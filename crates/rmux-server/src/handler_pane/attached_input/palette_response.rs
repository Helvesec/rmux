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
    Complete(usize),
    Partial,
}

pub(super) enum ModalPaletteInputSegment {
    Input(Vec<u8>),
    Response {
        index: TerminalPaletteIndex,
        bytes: Vec<u8>,
    },
}

impl ModalPaletteInputSegment {
    pub(super) fn into_bytes(self) -> Vec<u8> {
        match self {
            Self::Input(bytes) | Self::Response { bytes, .. } => bytes,
        }
    }
}

pub(super) struct ModalPaletteInput {
    pub(super) segments: Vec<ModalPaletteInputSegment>,
    pub(super) retained: Vec<u8>,
}

impl RequestHandler {
    /// Splits complete OSC 4 response candidates from input about to enter an
    /// attached modal surface. The caller processes these segments in wire
    /// order and bypasses the modal key decoder only after correlation. Other
    /// terminal controls and malformed OSC 4 input stay byte-for-byte in the
    /// ordinary modal input segments.
    pub(super) fn split_attached_modal_palette_input(
        &self,
        pending_input: &[u8],
        bytes: &[u8],
    ) -> Option<ModalPaletteInput> {
        if !pending_input.contains(&b'\x1b') && !bytes.contains(&b'\x1b') {
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
            if candidate.starts_with(PALETTE_RESPONSE_PREFIX) {
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
                    TerminalResponseDecode::Matched { size, .. }
                    | TerminalResponseDecode::PaneBound { size } => {
                        // A malformed OSC 4 sequence is not a correlated
                        // palette response. Skip over the whole OSC while
                        // retaining every byte in the modal input.
                        offset += size.max(1);
                    }
                    TerminalResponseDecode::NotResponse => offset += 1,
                }
                continue;
            }

            match opaque_input_span(candidate) {
                Some(OpaqueInputSpan::Complete(size)) => {
                    offset += size;
                    continue;
                }
                Some(OpaqueInputSpan::Partial) => break,
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

fn opaque_input_span(input: &[u8]) -> Option<OpaqueInputSpan> {
    if input.starts_with(BRACKETED_PASTE_START) {
        return find_subslice(&input[BRACKETED_PASTE_START.len()..], BRACKETED_PASTE_END)
            .map(|end| {
                OpaqueInputSpan::Complete(
                    BRACKETED_PASTE_START.len() + end + BRACKETED_PASTE_END.len(),
                )
            })
            .or(Some(OpaqueInputSpan::Partial));
    }
    if input.len() >= 3
        && input.len() < BRACKETED_PASTE_START.len()
        && BRACKETED_PASTE_START.starts_with(input)
    {
        return Some(OpaqueInputSpan::Partial);
    }

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
            _ => return None,
        }
    };
    find_opaque_terminator(input, body_start, bell_terminated)
        .map(OpaqueInputSpan::Complete)
        .or(Some(OpaqueInputSpan::Partial))
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
    use super::RequestHandler;

    #[test]
    fn modal_palette_splitter_ignores_osc4_bytes_inside_opaque_frames() {
        let handler = RequestHandler::new();
        for opaque in [
            b"\x1b[200~paste\x1b]4;1;rgb:1/2/3\x07body\x1b[201~".as_slice(),
            b"\x1bPtmux;\x1b\x1b]4;1;rgb:1/2/3\x07payload\x1b\\".as_slice(),
            b"\x1b_Gi=1;payload\x1b]4;1;rgb:1/2/3\x07more\x1b\\".as_slice(),
            b"\x1b]52;c;payload\x1b]4;1;rgb:1/2/3\x07".as_slice(),
        ] {
            assert!(
                handler
                    .split_attached_modal_palette_input(&[], opaque)
                    .is_none(),
                "opaque payload must not expose a nested OSC 4: {opaque:?}"
            );
        }
    }

    #[test]
    fn modal_palette_splitter_does_not_search_in_partial_opaque_frames() {
        let handler = RequestHandler::new();
        for opaque in [
            b"\x1b[200~paste\x1b]4;1;rgb:1/2/3\x07".as_slice(),
            b"\x1bPtmux;\x1b\x1b]4;1;rgb:1/2/3\x07".as_slice(),
            b"\x1b_Gi=1;payload\x1b]4;1;rgb:1/2/3\x07".as_slice(),
        ] {
            assert!(
                handler
                    .split_attached_modal_palette_input(&[], opaque)
                    .is_none(),
                "partial opaque payload must remain indivisible: {opaque:?}"
            );
        }
    }
}
