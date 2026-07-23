use rmux_core::TerminalPaletteIndex;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum TerminalResponseDecode {
    NotResponse,
    Partial,
    PaneBound {
        size: usize,
    },
    PaletteResponse {
        size: usize,
        index: TerminalPaletteIndex,
    },
    ClipboardResponse {
        size: usize,
        selection: Option<u8>,
        content: Vec<u8>,
    },
    Matched {
        size: usize,
        event: Option<TerminalControlEvent>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TerminalControlEvent {
    FocusIn,
    FocusOut,
    ClientLightTheme,
    ClientDarkTheme,
}

#[cfg(test)]
pub(super) fn decode_attached_terminal_control(
    input: &[u8],
    focus_passthrough: bool,
) -> TerminalResponseDecode {
    decode_attached_terminal_control_after_append(input, focus_passthrough, 0)
}

pub(super) fn decode_attached_terminal_control_after_append(
    input: &[u8],
    focus_passthrough: bool,
    new_input_at: usize,
) -> TerminalResponseDecode {
    // `new_input_at` is the retained length before the current append. Each
    // decoder backs up only as far as its split terminator requires.
    match decode_osc_sequence(input, new_input_at) {
        TerminalResponseDecode::NotResponse => {}
        matched => return matched,
    }

    if !focus_passthrough {
        match decode_focus_response(input) {
            TerminalResponseDecode::NotResponse => {}
            matched => return matched,
        }
    }

    decode_terminal_response_after_append(input, new_input_at)
}

#[cfg(test)]
pub(super) fn decode_terminal_response(input: &[u8]) -> TerminalResponseDecode {
    decode_terminal_response_after_append(input, 0)
}

fn decode_terminal_response_after_append(
    input: &[u8],
    new_input_at: usize,
) -> TerminalResponseDecode {
    if !input.starts_with(b"\x1b[") {
        return TerminalResponseDecode::NotResponse;
    }

    let search_at = new_input_at.max(2).min(input.len());
    let mut final_index = None;
    for (offset, byte) in input[search_at..].iter().copied().enumerate() {
        if is_csi_final(byte) {
            final_index = Some(search_at + offset);
            break;
        }
        // A CSI body may contain only parameter/intermediate bytes before its
        // final byte. Treat C0 controls, DEL, ESC, and non-ASCII bytes as raw
        // pane input immediately instead of retaining a "terminal response"
        // that the pending-escape scheduler correctly cannot classify.
        if !(0x20..=0x3f).contains(&byte) {
            return TerminalResponseDecode::NotResponse;
        }
    }
    let Some(final_index) = final_index else {
        return if is_plausible_terminal_response_prefix(input) {
            TerminalResponseDecode::Partial
        } else {
            TerminalResponseDecode::NotResponse
        };
    };
    match input[final_index] {
        b'n' => matched(final_index + 1, decode_theme_report(input, final_index)),
        b'c' | b't' => matched(final_index + 1, None),
        b'y' if is_decrpm_response(input, final_index) => matched(final_index + 1, None),
        b'R' => TerminalResponseDecode::PaneBound {
            size: final_index + 1,
        },
        _ => TerminalResponseDecode::NotResponse,
    }
}

const fn matched(size: usize, event: Option<TerminalControlEvent>) -> TerminalResponseDecode {
    TerminalResponseDecode::Matched { size, event }
}

fn is_decrpm_response(input: &[u8], final_index: usize) -> bool {
    final_index > 2 && input.get(final_index - 1) == Some(&b'$')
}

pub(super) fn decode_focus_event(input: &[u8]) -> Option<TerminalControlEvent> {
    if input.starts_with(b"\x1b[I") {
        return Some(TerminalControlEvent::FocusIn);
    }
    if input.starts_with(b"\x1b[O") {
        return Some(TerminalControlEvent::FocusOut);
    }
    None
}

fn decode_focus_response(input: &[u8]) -> TerminalResponseDecode {
    match decode_focus_event(input) {
        Some(event) => matched(3, Some(event)),
        None => TerminalResponseDecode::NotResponse,
    }
}

fn decode_theme_report(input: &[u8], final_index: usize) -> Option<TerminalControlEvent> {
    match &input[..=final_index] {
        b"\x1b[?997;1n" => Some(TerminalControlEvent::ClientDarkTheme),
        b"\x1b[?997;2n" => Some(TerminalControlEvent::ClientLightTheme),
        _ => None,
    }
}

fn decode_osc_sequence(input: &[u8], new_input_at: usize) -> TerminalResponseDecode {
    if !input.starts_with(b"\x1b]") {
        return TerminalResponseDecode::NotResponse;
    }
    const CONSUMED_OSC_PREFIXES: &[&[u8]] = &[
        b"\x1b]4;",
        b"\x1b]10;",
        b"\x1b]11;",
        b"\x1b]12;",
        b"\x1b]52;",
    ];
    if CONSUMED_OSC_PREFIXES
        .iter()
        .any(|prefix| input.len() < prefix.len() && prefix.starts_with(input))
    {
        return TerminalResponseDecode::Partial;
    }
    if !CONSUMED_OSC_PREFIXES
        .iter()
        .any(|prefix| input.starts_with(prefix))
    {
        return TerminalResponseDecode::NotResponse;
    }

    let mut index = new_input_at.saturating_sub(1).max(2).min(input.len());
    while index < input.len() {
        match input[index] {
            b'\x07' => return decode_complete_osc_sequence(input, index, index + 1),
            b'\x1b' if input.get(index + 1) == Some(&b'\\') => {
                return decode_complete_osc_sequence(input, index, index + 2);
            }
            _ => index += 1,
        }
    }
    TerminalResponseDecode::Partial
}

fn decode_complete_osc_sequence(
    input: &[u8],
    body_end: usize,
    size: usize,
) -> TerminalResponseDecode {
    if input.starts_with(b"\x1b]4;") {
        if let Some(index) = decode_palette_response_body(&input[b"\x1b]4;".len()..body_end]) {
            return TerminalResponseDecode::PaletteResponse { size, index };
        }
    }
    if let Some(body) = input
        .strip_prefix(b"\x1b]52;")
        .and_then(|body| body.get(..body_end.saturating_sub(b"\x1b]52;".len())))
    {
        if let Some(response) = crate::clipboard_protocol::decode_clipboard_response_body(body) {
            return TerminalResponseDecode::ClipboardResponse {
                size,
                selection: response.selection,
                content: response.content,
            };
        }
    }
    matched(size, None)
}

fn decode_palette_response_body(body: &[u8]) -> Option<TerminalPaletteIndex> {
    let separator = body.iter().position(|byte| *byte == b';')?;
    let (index, value) = body.split_at(separator);
    let index = std::str::from_utf8(index)
        .ok()
        .and_then(TerminalPaletteIndex::parse)?;
    let rgb = value.get(1..)?.strip_prefix(b"rgb:")?;
    let mut channels = rgb.split(|byte| *byte == b'/');
    for _ in 0..3 {
        let channel = channels.next()?;
        if channel.is_empty() || channel.len() > 4 || !channel.iter().all(u8::is_ascii_hexdigit) {
            return None;
        }
    }
    if channels.next().is_some() {
        return None;
    }
    Some(index)
}

fn is_plausible_terminal_response_prefix(input: &[u8]) -> bool {
    input
        .get(2)
        .is_some_and(|byte| *byte == b'?' || *byte == b'>' || byte.is_ascii_digit())
}

fn is_csi_final(byte: u8) -> bool {
    (0x40..=0x7e).contains(&byte)
}

#[cfg(test)]
mod tests {
    use super::{
        decode_attached_terminal_control, decode_attached_terminal_control_after_append,
        decode_focus_event, decode_terminal_response, TerminalControlEvent, TerminalPaletteIndex,
        TerminalResponseDecode,
    };

    #[test]
    fn matches_primary_device_attributes_response() {
        assert_eq!(
            decode_terminal_response(b"\x1b[?62;52;ctail"),
            TerminalResponseDecode::Matched {
                size: 10,
                event: None
            }
        );
    }

    #[test]
    fn marks_cursor_position_response_as_pane_bound() {
        assert_eq!(
            decode_terminal_response(b"\x1b[12;40R"),
            TerminalResponseDecode::PaneBound { size: 8 }
        );
    }

    #[test]
    fn matches_decrpm_response() {
        assert_eq!(
            decode_terminal_response(b"\x1b[?2004;1$y"),
            TerminalResponseDecode::Matched {
                size: 11,
                event: None
            }
        );
    }

    #[test]
    fn matches_theme_reports() {
        assert_eq!(
            decode_terminal_response(b"\x1b[?997;1n"),
            TerminalResponseDecode::Matched {
                size: 9,
                event: Some(TerminalControlEvent::ClientDarkTheme)
            }
        );
        assert_eq!(
            decode_terminal_response(b"\x1b[?997;2n"),
            TerminalResponseDecode::Matched {
                size: 9,
                event: Some(TerminalControlEvent::ClientLightTheme)
            }
        );
        assert_eq!(
            decode_terminal_response(b"\x1b[?2031;1$y"),
            TerminalResponseDecode::Matched {
                size: 11,
                event: None
            }
        );
    }

    #[test]
    fn retains_fragmented_responses() {
        assert_eq!(
            decode_terminal_response(b"\x1b[?62;52"),
            TerminalResponseDecode::Partial
        );
    }

    #[test]
    fn invalid_csi_body_bytes_are_never_retained_as_terminal_responses() {
        for leader in [b'?', b'>', b'1'] {
            for invalid in [b'\0', b'\r', b'\x1b', b'\x7f', b'\x80', b'\xff'] {
                let input = [b'\x1b', b'[', leader, invalid];
                assert_eq!(
                    decode_attached_terminal_control_after_append(&input, false, 3),
                    TerminalResponseDecode::NotResponse,
                    "leader={leader:#04x}, invalid={invalid:#04x}"
                );
            }
        }
    }

    #[test]
    fn leaves_arrow_keys_for_key_decoder() {
        assert_eq!(
            decode_terminal_response(b"\x1b[A"),
            TerminalResponseDecode::NotResponse
        );
    }

    #[test]
    fn leaves_extended_keys_for_key_decoder() {
        assert_eq!(
            decode_terminal_response(b"\x1b[27;2;65u"),
            TerminalResponseDecode::NotResponse
        );
    }

    #[test]
    fn attached_terminal_control_consumes_focus_events_by_default() {
        assert_eq!(
            decode_attached_terminal_control(b"\x1b[Irest", false),
            TerminalResponseDecode::Matched {
                size: 3,
                event: Some(TerminalControlEvent::FocusIn)
            }
        );
        assert_eq!(
            decode_attached_terminal_control(b"\x1b[Orest", false),
            TerminalResponseDecode::Matched {
                size: 3,
                event: Some(TerminalControlEvent::FocusOut)
            }
        );
        assert_eq!(
            decode_focus_event(b"\x1b[Irest"),
            Some(TerminalControlEvent::FocusIn)
        );
    }

    #[test]
    fn attached_terminal_control_preserves_focus_events_for_focus_mode() {
        assert_eq!(
            decode_attached_terminal_control(b"\x1b[Irest", true),
            TerminalResponseDecode::NotResponse
        );
        assert_eq!(
            decode_attached_terminal_control(b"\x1b[Orest", true),
            TerminalResponseDecode::NotResponse
        );
    }

    #[test]
    fn attached_terminal_control_consumes_osc_sequences() {
        assert_eq!(
            decode_attached_terminal_control(b"\x1b]52;c;AAAA\x07tail", false),
            TerminalResponseDecode::ClipboardResponse {
                size: 12,
                selection: Some(b'c'),
                content: vec![0, 0, 0],
            }
        );
        assert_eq!(
            decode_attached_terminal_control(b"\x1b]52;c;AAAA\x1b\\tail", false),
            TerminalResponseDecode::ClipboardResponse {
                size: 13,
                selection: Some(b'c'),
                content: vec![0, 0, 0],
            }
        );
        assert_eq!(
            decode_attached_terminal_control(b"\x1b]52;c;AAAA", false),
            TerminalResponseDecode::Partial
        );
    }

    #[test]
    fn decodes_bounded_palette_responses_with_bel_or_st() {
        for (response, index) in [
            (b"\x1b]4;0;rgb:0000/1111/ffff\x07".as_slice(), 0),
            (b"\x1b]4;255;rgb:0/a/FFFF\x1b\\".as_slice(), 255),
        ] {
            assert_eq!(
                decode_attached_terminal_control(response, false),
                TerminalResponseDecode::PaletteResponse {
                    size: response.len(),
                    index: TerminalPaletteIndex::from(index),
                }
            );
        }
    }

    #[test]
    fn fragmented_palette_response_is_partial_until_its_terminator() {
        let response = b"\x1b]4;7;rgb:1111/2222/3333\x1b\\";
        for split in 2..response.len() {
            assert_eq!(
                decode_attached_terminal_control(&response[..split], false),
                TerminalResponseDecode::Partial,
                "palette response split at byte {split}"
            );
        }
        assert_eq!(
            decode_attached_terminal_control(response, false),
            TerminalResponseDecode::PaletteResponse {
                size: response.len(),
                index: TerminalPaletteIndex::from(7),
            }
        );
    }

    #[test]
    fn malformed_or_out_of_range_palette_sequences_are_consumed_not_forwarded() {
        for response in [
            b"\x1b]4;256;rgb:0000/0000/0000\x07".as_slice(),
            b"\x1b]4;0;not-rgb\x07".as_slice(),
            b"\x1b]4;0;rgb:00000/0/0\x07".as_slice(),
            b"\x1b]4;0;rgb:0/0/0;echo\x07".as_slice(),
        ] {
            assert_eq!(
                decode_attached_terminal_control(response, false),
                TerminalResponseDecode::Matched {
                    size: response.len(),
                    event: None,
                },
                "malformed OSC 4 must stay at the attach boundary: {response:?}"
            );
        }
    }

    #[test]
    fn incremental_osc_search_finds_terminators_at_append_boundary() {
        let bell_terminated = b"\x1b]52;c;AAAA\x07tail";
        let bell_at = b"\x1b]52;c;AAAA".len();
        assert_eq!(
            decode_attached_terminal_control_after_append(bell_terminated, false, bell_at),
            TerminalResponseDecode::ClipboardResponse {
                size: bell_at + 1,
                selection: Some(b'c'),
                content: vec![0, 0, 0],
            }
        );

        let string_terminated = b"\x1b]52;c;AAAA\x1b\\tail";
        let terminator_at = b"\x1b]52;c;AAAA".len();
        assert_eq!(
            decode_attached_terminal_control_after_append(
                string_terminated,
                false,
                terminator_at + 1,
            ),
            TerminalResponseDecode::ClipboardResponse {
                size: terminator_at + 2,
                selection: Some(b'c'),
                content: vec![0, 0, 0],
            }
        );
    }

    #[test]
    fn incremental_control_search_starts_at_the_append_boundary_overlap() {
        // Earlier final bytes are impossible in retained partial state. They
        // are sentinels that expose any scan which restarts before the cursor.
        let osc = b"\x1b]52;c;old\x07padding-padding-new\x07tail";
        let osc_new_input_at = b"\x1b]52;c;old\x07padding-padding-new".len();
        assert_eq!(
            decode_attached_terminal_control_after_append(osc, false, osc_new_input_at),
            TerminalResponseDecode::Matched {
                size: osc_new_input_at + 1,
                event: None,
            }
        );

        let csi = b"\x1b[?62;52cpadding-padding997;1n";
        let csi_new_input_at = b"\x1b[?62;52cpadding-padding".len();
        assert_eq!(
            decode_attached_terminal_control_after_append(csi, false, csi_new_input_at),
            TerminalResponseDecode::Matched {
                size: csi.len(),
                event: None,
            }
        );
    }

    #[test]
    fn attached_terminal_control_retains_ambiguous_alt_right_bracket_prefix() {
        assert_eq!(
            decode_attached_terminal_control(b"\x1b]", false),
            TerminalResponseDecode::Partial
        );
        assert_eq!(
            decode_attached_terminal_control(b"\x1b]X\x07", false),
            TerminalResponseDecode::NotResponse
        );
    }

    #[test]
    fn attached_terminal_control_retains_every_fragmented_osc52_prefix() {
        let response = b"\x1b]52;c;AAAA\x07";
        for split in 2..b"\x1b]52;".len() {
            assert_eq!(
                decode_attached_terminal_control(&response[..split], false),
                TerminalResponseDecode::Partial,
                "OSC52 prefix split at byte {split}"
            );
        }
    }
}
