#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TerminalResponseDecode {
    NotResponse,
    Partial,
    Matched { size: usize },
}

pub(super) fn decode_attached_terminal_control(
    input: &[u8],
    focus_passthrough: bool,
) -> TerminalResponseDecode {
    match decode_osc_sequence(input) {
        TerminalResponseDecode::NotResponse => {}
        matched => return matched,
    }

    if !focus_passthrough {
        match decode_focus_event(input) {
            TerminalResponseDecode::NotResponse => {}
            matched => return matched,
        }
    }

    decode_terminal_response(input)
}

pub(super) fn decode_terminal_response(input: &[u8]) -> TerminalResponseDecode {
    if !input.starts_with(b"\x1b[") {
        return TerminalResponseDecode::NotResponse;
    }

    let Some(final_offset) = input[2..].iter().position(|byte| is_csi_final(*byte)) else {
        return if is_plausible_terminal_response_prefix(input) {
            TerminalResponseDecode::Partial
        } else {
            TerminalResponseDecode::NotResponse
        };
    };
    let final_index = final_offset + 2;
    match input[final_index] {
        b'c' | b'n' | b't' => TerminalResponseDecode::Matched {
            size: final_index + 1,
        },
        b'y' if is_decrpm_response(input, final_index) => TerminalResponseDecode::Matched {
            size: final_index + 1,
        },
        _ => TerminalResponseDecode::NotResponse,
    }
}

fn is_decrpm_response(input: &[u8], final_index: usize) -> bool {
    final_index > 2 && input.get(final_index - 1) == Some(&b'$')
}

fn decode_focus_event(input: &[u8]) -> TerminalResponseDecode {
    if input.starts_with(b"\x1b[I") || input.starts_with(b"\x1b[O") {
        return TerminalResponseDecode::Matched { size: 3 };
    }
    TerminalResponseDecode::NotResponse
}

fn decode_osc_sequence(input: &[u8]) -> TerminalResponseDecode {
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
    if !CONSUMED_OSC_PREFIXES
        .iter()
        .any(|prefix| input.starts_with(prefix))
    {
        return TerminalResponseDecode::NotResponse;
    }

    let mut index = 2;
    while index < input.len() {
        match input[index] {
            b'\x07' => return TerminalResponseDecode::Matched { size: index + 1 },
            b'\x1b' if input.get(index + 1) == Some(&b'\\') => {
                return TerminalResponseDecode::Matched { size: index + 2 };
            }
            _ => index += 1,
        }
    }
    TerminalResponseDecode::Partial
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
        decode_attached_terminal_control, decode_terminal_response, TerminalResponseDecode,
    };

    #[test]
    fn matches_primary_device_attributes_response() {
        assert_eq!(
            decode_terminal_response(b"\x1b[?62;52;ctail"),
            TerminalResponseDecode::Matched { size: 10 }
        );
    }

    #[test]
    fn leaves_cursor_position_response_for_pane() {
        assert_eq!(
            decode_terminal_response(b"\x1b[12;40R"),
            TerminalResponseDecode::NotResponse
        );
    }

    #[test]
    fn matches_decrpm_response() {
        assert_eq!(
            decode_terminal_response(b"\x1b[?2004;1$y"),
            TerminalResponseDecode::Matched { size: 11 }
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
            TerminalResponseDecode::Matched { size: 3 }
        );
        assert_eq!(
            decode_attached_terminal_control(b"\x1b[Orest", false),
            TerminalResponseDecode::Matched { size: 3 }
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
            TerminalResponseDecode::Matched { size: 12 }
        );
        assert_eq!(
            decode_attached_terminal_control(b"\x1b]52;c;AAAA\x1b\\tail", false),
            TerminalResponseDecode::Matched { size: 13 }
        );
        assert_eq!(
            decode_attached_terminal_control(b"\x1b]52;c;AAAA", false),
            TerminalResponseDecode::Partial
        );
    }

    #[test]
    fn attached_terminal_control_preserves_alt_right_bracket_input() {
        assert_eq!(
            decode_attached_terminal_control(b"\x1b]", false),
            TerminalResponseDecode::NotResponse
        );
        assert_eq!(
            decode_attached_terminal_control(b"\x1b]X\x07", false),
            TerminalResponseDecode::NotResponse
        );
    }
}
