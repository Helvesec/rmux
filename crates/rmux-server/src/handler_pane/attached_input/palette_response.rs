use std::io;

use rmux_core::TerminalPaletteIndex;

use super::terminal_response::{
    decode_attached_terminal_control_after_append, TerminalResponseDecode,
};
use super::{
    io_other, prepare_pane_input_write, write_attached_bytes_to_target_io, ActiveAttachIdentity,
    PaneInputLiveness, RequestHandler,
};

const PALETTE_RESPONSE_PREFIX: &[u8] = b"\x1b]4;";
const BRACKETED_PASTE_START: &[u8] = b"\x1b[200~";
const BRACKETED_PASTE_END: &[u8] = b"\x1b[201~";

enum OpaqueInputSpan {
    ModalInputComplete(usize),
    ModalInputPartial,
    PaneBoundComplete(usize),
    PaneBoundPartial,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PaneBoundTerminalStringDecode {
    Matched { size: usize },
    Partial,
    NotString,
}

pub(super) enum ModalPaletteInputSegment {
    Input(Vec<u8>),
    PaneBound(Vec<u8>),
    Response {
        index: TerminalPaletteIndex,
        bytes: Vec<u8>,
    },
}

impl ModalPaletteInputSegment {
    pub(super) fn into_bytes(self) -> Vec<u8> {
        match self {
            Self::Input(bytes) | Self::PaneBound(bytes) | Self::Response { bytes, .. } => bytes,
        }
    }
}

pub(super) struct ModalPaletteInput {
    pub(super) segments: Vec<ModalPaletteInputSegment>,
    pub(super) retained: Vec<u8>,
}

impl RequestHandler {
    /// Separates terminal-protocol strings from input about to enter an
    /// attached modal surface. Correlated OSC 4 responses and opaque
    /// pane-bound strings bypass modal key decoding; consumed outer-terminal
    /// responses are removed, while ordinary keys and bracketed-paste bodies
    /// retain their modal behavior. All segments preserve wire order.
    pub(super) fn split_attached_modal_palette_input(
        &self,
        pending_input: &[u8],
        bytes: &[u8],
    ) -> Option<ModalPaletteInput> {
        if !contains_terminal_string_leader(pending_input)
            && !contains_terminal_string_leader(bytes)
        {
            return None;
        }
        let mut input = Vec::with_capacity(pending_input.len().saturating_add(bytes.len()));
        input.extend_from_slice(pending_input);
        input.extend_from_slice(bytes);

        let mut segments = Vec::new();
        let mut retained = Vec::new();
        let mut copy_from = 0;
        let mut offset = 0;
        let mut changed = false;

        while offset < input.len() {
            let candidate = &input[offset..];
            if candidate.len() < PALETTE_RESPONSE_PREFIX.len()
                && PALETTE_RESPONSE_PREFIX.starts_with(candidate)
            {
                if copy_from < offset {
                    segments.push(ModalPaletteInputSegment::Input(
                        input[copy_from..offset].to_vec(),
                    ));
                }
                retained.extend_from_slice(candidate);
                copy_from = input.len();
                changed = true;
                break;
            }
            if candidate.starts_with(b"\x1b]") {
                match decode_attached_terminal_control_after_append(candidate, false, 0) {
                    TerminalResponseDecode::PaletteResponse { size, index } => {
                        if copy_from < offset {
                            segments.push(ModalPaletteInputSegment::Input(
                                input[copy_from..offset].to_vec(),
                            ));
                        }
                        segments.push(ModalPaletteInputSegment::Response {
                            index,
                            bytes: candidate[..size].to_vec(),
                        });
                        offset += size;
                        copy_from = offset;
                        changed = true;
                        continue;
                    }
                    TerminalResponseDecode::Partial => {
                        if copy_from < offset {
                            segments.push(ModalPaletteInputSegment::Input(
                                input[copy_from..offset].to_vec(),
                            ));
                        }
                        retained.extend_from_slice(candidate);
                        copy_from = input.len();
                        changed = true;
                        break;
                    }
                    TerminalResponseDecode::Matched { size, .. } => {
                        if copy_from < offset {
                            segments.push(ModalPaletteInputSegment::Input(
                                input[copy_from..offset].to_vec(),
                            ));
                        }
                        // Outer-terminal OSC responses (including malformed
                        // OSC 4 and OSC 10/11/12/52) are consumed on the
                        // ordinary live-input path. Preserve that behavior
                        // before a modal key decoder can interpret their ESC.
                        offset += size;
                        copy_from = offset;
                        changed = true;
                        continue;
                    }
                    TerminalResponseDecode::PaneBound { size } => {
                        if copy_from < offset {
                            segments.push(ModalPaletteInputSegment::Input(
                                input[copy_from..offset].to_vec(),
                            ));
                        }
                        segments.push(ModalPaletteInputSegment::PaneBound(
                            candidate[..size].to_vec(),
                        ));
                        offset += size;
                        copy_from = offset;
                        changed = true;
                        continue;
                    }
                    TerminalResponseDecode::NotResponse => {}
                }
            }

            match opaque_input_span(candidate) {
                Some(OpaqueInputSpan::ModalInputComplete(size)) => {
                    offset += size;
                    continue;
                }
                Some(OpaqueInputSpan::ModalInputPartial) => break,
                Some(OpaqueInputSpan::PaneBoundComplete(size)) => {
                    if copy_from < offset {
                        segments.push(ModalPaletteInputSegment::Input(
                            input[copy_from..offset].to_vec(),
                        ));
                    }
                    segments.push(ModalPaletteInputSegment::PaneBound(
                        candidate[..size].to_vec(),
                    ));
                    offset += size;
                    copy_from = offset;
                    changed = true;
                }
                Some(OpaqueInputSpan::PaneBoundPartial) => {
                    if copy_from < offset {
                        segments.push(ModalPaletteInputSegment::Input(
                            input[copy_from..offset].to_vec(),
                        ));
                    }
                    retained.extend_from_slice(candidate);
                    copy_from = input.len();
                    changed = true;
                    break;
                }
                None => offset += 1,
            }
        }

        if !changed {
            return None;
        }
        if copy_from < input.len() {
            segments.push(ModalPaletteInputSegment::Input(input[copy_from..].to_vec()));
        }
        Some(ModalPaletteInput { segments, retained })
    }

    pub(super) async fn write_attached_palette_response_for_identity(
        &self,
        identity: ActiveAttachIdentity,
        index: TerminalPaletteIndex,
        bytes: &[u8],
    ) -> io::Result<bool> {
        if self
            .attached_client_input_is_read_only_for_identity(identity)
            .await?
        {
            return Ok(false);
        }
        let (target, session_id) = self
            .attached_input_target_identity(identity)
            .await
            .map_err(io_other)?;
        let Some(write) = self
            .with_live_input_state(identity, &target, session_id, |state| {
                let transcript = state.transcript_handle(&target).map_err(io_other)?;
                let mut transcript = transcript
                    .lock()
                    .expect("pane transcript mutex must not be poisoned");
                if !transcript.consume_palette_query_response(index) {
                    return Ok(None);
                }
                drop(transcript);
                let write = prepare_pane_input_write(
                    state,
                    &target,
                    bytes,
                    PaneInputLiveness::TolerateDead,
                )
                .map_err(io_other)?;
                Ok(Some(write))
            })
            .await?
            .flatten()
        else {
            return Ok(false);
        };
        write_attached_bytes_to_target_io(write, bytes.to_vec())
            .await
            .map_err(io_other)?;
        Ok(true)
    }
}

fn contains_terminal_string_leader(input: &[u8]) -> bool {
    input
        .iter()
        .any(|byte| matches!(*byte, b'\x1b' | 0x90 | 0x98 | 0x9d | 0x9e | 0x9f))
}

fn opaque_input_span(input: &[u8]) -> Option<OpaqueInputSpan> {
    if input.starts_with(BRACKETED_PASTE_START) {
        return find_subslice(&input[BRACKETED_PASTE_START.len()..], BRACKETED_PASTE_END)
            .map(|end| {
                OpaqueInputSpan::ModalInputComplete(
                    BRACKETED_PASTE_START.len() + end + BRACKETED_PASTE_END.len(),
                )
            })
            .or(Some(OpaqueInputSpan::ModalInputPartial));
    }
    if input.len() >= 3
        && input.len() < BRACKETED_PASTE_START.len()
        && BRACKETED_PASTE_START.starts_with(input)
    {
        return Some(OpaqueInputSpan::ModalInputPartial);
    }

    match decode_pane_bound_terminal_string(input) {
        PaneBoundTerminalStringDecode::Matched { size } => {
            Some(OpaqueInputSpan::PaneBoundComplete(size))
        }
        PaneBoundTerminalStringDecode::Partial => Some(OpaqueInputSpan::PaneBoundPartial),
        PaneBoundTerminalStringDecode::NotString => None,
    }
}

pub(super) fn decode_pane_bound_terminal_string(input: &[u8]) -> PaneBoundTerminalStringDecode {
    let (body_start, bell_terminated) = if input.starts_with(b"\x1b]") {
        (2, true)
    } else if input.starts_with(b"\x1bP")
        || input.starts_with(b"\x1b_")
        || input.starts_with(b"\x1b^")
        || input.starts_with(b"\x1bX")
    {
        (2, false)
    } else {
        match input.first().copied() {
            Some(0x9d) => (1, true),
            Some(0x90 | 0x98 | 0x9e | 0x9f) => (1, false),
            _ => return PaneBoundTerminalStringDecode::NotString,
        }
    };
    find_opaque_terminator(input, body_start, bell_terminated)
        .map(|size| PaneBoundTerminalStringDecode::Matched { size })
        .unwrap_or(PaneBoundTerminalStringDecode::Partial)
}

fn find_opaque_terminator(input: &[u8], mut offset: usize, bell_terminated: bool) -> Option<usize> {
    while offset < input.len() {
        match input[offset] {
            b'\x07' if bell_terminated => return Some(offset + 1),
            0x9c => return Some(offset + 1),
            b'\x1b' if input.get(offset + 1) == Some(&b'\\') => return Some(offset + 2),
            _ => offset += 1,
        }
    }
    None
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::{ModalPaletteInputSegment, RequestHandler};

    fn assert_single_pane_bound(input: &[u8]) {
        let handler = RequestHandler::new();
        let split = handler
            .split_attached_modal_palette_input(&[], input)
            .expect("terminal string is separated from modal keys");
        assert!(split.retained.is_empty());
        assert_eq!(split.segments.len(), 1);
        match &split.segments[0] {
            ModalPaletteInputSegment::PaneBound(bytes) => assert_eq!(bytes, input),
            _ => panic!("terminal string must remain one pane-bound segment"),
        }
    }

    #[test]
    fn modal_palette_splitter_keeps_pane_bound_strings_indivisible() {
        for opaque in [
            b"\x1bPtmux;\x1b\x1b]4;1;rgb:1/2/3\x07payload\x1b\\".as_slice(),
            b"\x1b_Gi=1;payload\x1b]4;1;rgb:1/2/3\x07more\x1b\\".as_slice(),
            b"\x1b^private\x1b]4;1;rgb:1/2/3\x07message\x1b\\".as_slice(),
            b"\x90dcs\x1b]4;1;rgb:1/2/3\x07body\x9c".as_slice(),
        ] {
            assert_single_pane_bound(opaque);
        }
    }

    #[test]
    fn modal_palette_splitter_leaves_bracketed_paste_for_the_modal() {
        let handler = RequestHandler::new();
        for paste in [
            b"\x1b[200~paste\x1b]4;1;rgb:1/2/3\x07body\x1b[201~".as_slice(),
            b"\x1b[200~partial\x1b]4;1;rgb:1/2/3\x07".as_slice(),
        ] {
            assert!(
                handler
                    .split_attached_modal_palette_input(&[], paste)
                    .is_none(),
                "paste remains ordinary modal input: {paste:?}"
            );
        }
    }

    #[test]
    fn modal_palette_splitter_retains_partial_pane_bound_strings() {
        let handler = RequestHandler::new();
        for opaque in [
            b"\x1bPtmux;\x1b\x1b]4;1;rgb:1/2/3\x07".as_slice(),
            b"\x1bXpartial-sos".as_slice(),
            b"\x1b_Gi=1;payload\x1b]4;1;rgb:1/2/3\x07".as_slice(),
            b"\x1b^partial-pm".as_slice(),
            b"\x1b_Qpartial-apc".as_slice(),
            b"\x1b]777;payload".as_slice(),
            b"\x90partial-dcs".as_slice(),
            b"\x98partial-sos".as_slice(),
            b"\x9dpartial-osc".as_slice(),
            b"\x9epartial-pm".as_slice(),
            b"\x9fpartial-apc".as_slice(),
        ] {
            let split = handler
                .split_attached_modal_palette_input(&[], opaque)
                .expect("partial terminal string is retained before modal decoding");
            assert!(split.segments.is_empty());
            assert_eq!(split.retained, opaque);
        }
    }

    #[test]
    fn modal_palette_splitter_consumes_outer_terminal_osc_responses() {
        let handler = RequestHandler::new();
        let response = b"\x1b]52;c;payload\x1b]4;1;rgb:1/2/3\x07";
        let split = handler
            .split_attached_modal_palette_input(&[], response)
            .expect("consumed outer-terminal response is removed before modal decoding");
        assert!(split.segments.is_empty());
        assert!(split.retained.is_empty());
    }
}
