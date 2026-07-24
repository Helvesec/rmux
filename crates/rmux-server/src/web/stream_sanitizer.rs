//! Fragmentation-safe WebShare policy boundary.
//!
//! Visual OSC commands on the allowlist survive. Clipboard OSC 52, unknown
//! OSC commands, and every APC/DCS/PM/SOS string (including Kitty graphics and
//! SIXEL) are dropped rather than delegated to browser-terminal behavior.

const MAX_BUFFERED_OSC_BYTES: usize = 1024 * 1024;

#[derive(Debug, Default)]
pub(crate) struct WebTerminalSanitizer {
    state: State,
    utf8_continuations: u8,
}

#[derive(Debug, Default)]
enum State {
    #[default]
    Ground,
    Escape,
    Osc {
        bytes: Vec<u8>,
        escaped: bool,
    },
    DiscardString {
        escaped: bool,
    },
}

impl WebTerminalSanitizer {
    pub(crate) fn push(&mut self, input: &[u8], output: &mut Vec<u8>) {
        for byte in input.iter().copied() {
            self.push_byte(byte, output);
        }
    }

    pub(crate) fn reset(&mut self) {
        self.state = State::Ground;
        self.utf8_continuations = 0;
    }

    fn push_byte(&mut self, byte: u8, output: &mut Vec<u8>) {
        let standalone_control = if self.utf8_continuations > 0 {
            if byte & 0xc0 == 0x80 {
                self.utf8_continuations -= 1;
                false
            } else {
                self.utf8_continuations = utf8_continuations(byte);
                true
            }
        } else {
            self.utf8_continuations = utf8_continuations(byte);
            self.utf8_continuations == 0
        };
        let state = std::mem::take(&mut self.state);
        self.state = match state {
            State::Ground => ground(byte, output, standalone_control),
            State::Escape => escaped(byte, output),
            State::Osc { mut bytes, escaped } => {
                if string_ended(byte, escaped, true, standalone_control) {
                    bytes.push(byte);
                    if allowed_osc(&bytes) {
                        output.extend_from_slice(&bytes);
                    }
                    State::Ground
                } else if bytes.len() >= MAX_BUFFERED_OSC_BYTES {
                    State::DiscardString {
                        escaped: byte == 0x1b,
                    }
                } else {
                    bytes.push(byte);
                    State::Osc {
                        bytes,
                        escaped: byte == 0x1b,
                    }
                }
            }
            State::DiscardString { escaped } => {
                if string_ended(byte, escaped, false, standalone_control) {
                    State::Ground
                } else {
                    State::DiscardString {
                        escaped: byte == 0x1b,
                    }
                }
            }
        };
    }
}

fn ground(byte: u8, output: &mut Vec<u8>, standalone_control: bool) -> State {
    match byte {
        0x1b => State::Escape,
        0x9d if standalone_control => State::Osc {
            bytes: vec![byte],
            escaped: false,
        },
        0x90 | 0x98 | 0x9e | 0x9f if standalone_control => State::DiscardString { escaped: false },
        _ => {
            output.push(byte);
            State::Ground
        }
    }
}

fn escaped(byte: u8, output: &mut Vec<u8>) -> State {
    match byte {
        b']' => State::Osc {
            bytes: vec![0x1b, b']'],
            escaped: false,
        },
        b'P' | b'X' | b'^' | b'_' => State::DiscardString { escaped: false },
        0x1b => {
            output.push(0x1b);
            State::Escape
        }
        _ => {
            output.extend_from_slice(&[0x1b, byte]);
            State::Ground
        }
    }
}

fn string_ended(byte: u8, escaped: bool, bell_terminates: bool, standalone_control: bool) -> bool {
    (standalone_control && byte == 0x9c)
        || (escaped && byte == b'\\')
        || (bell_terminates && byte == 0x07)
}

const fn utf8_continuations(byte: u8) -> u8 {
    match byte {
        0xc2..=0xdf => 1,
        0xe0..=0xef => 2,
        0xf0..=0xf4 => 3,
        _ => 0,
    }
}

fn allowed_osc(sequence: &[u8]) -> bool {
    let payload = if sequence.starts_with(b"\x1b]") {
        &sequence[2..]
    } else if sequence.first() == Some(&0x9d) {
        &sequence[1..]
    } else {
        return false;
    };
    let code_end = payload
        .iter()
        .position(|byte| *byte == b';' || *byte == 0x07 || *byte == 0x9c || *byte == 0x1b)
        .unwrap_or(payload.len());
    let Ok(code) = std::str::from_utf8(&payload[..code_end]) else {
        return false;
    };
    if code == "8" {
        return allowed_hyperlink(&payload[code_end..]);
    }
    matches!(
        code,
        "0" | "1" | "2" | "4" | "7" | "10" | "11" | "12" | "104" | "110" | "111" | "112" | "133"
    )
}

fn allowed_hyperlink(payload: &[u8]) -> bool {
    let payload = payload.strip_prefix(b";").unwrap_or(payload);
    let payload = strip_osc_terminator(payload);
    let Some(separator) = payload.iter().position(|byte| *byte == b';') else {
        return false;
    };
    let uri = &payload[separator + 1..];
    if uri.is_empty() {
        return true;
    }
    let Ok(uri) = std::str::from_utf8(uri) else {
        return false;
    };
    let Some((scheme, _)) = uri.split_once(':') else {
        return false;
    };
    matches!(
        scheme.to_ascii_lowercase().as_str(),
        "http" | "https" | "mailto"
    )
}

fn strip_osc_terminator(mut payload: &[u8]) -> &[u8] {
    if payload.ends_with(b"\x1b\\") {
        payload = &payload[..payload.len() - 2];
    } else if payload
        .last()
        .is_some_and(|byte| matches!(byte, 0x07 | 0x9c))
    {
        payload = &payload[..payload.len() - 1];
    }
    payload
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sanitize(chunks: &[&[u8]]) -> Vec<u8> {
        let mut sanitizer = WebTerminalSanitizer::default();
        let mut output = Vec::new();
        for chunk in chunks {
            sanitizer.push(chunk, &mut output);
        }
        output
    }

    #[test]
    fn osc_52_is_removed_at_every_fragmentation_boundary() {
        let input = b"before\x1b]52;c;Zm9v\x1b\\after";
        for split in 0..=input.len() {
            assert_eq!(
                sanitize(&[&input[..split], &input[split..]]),
                b"beforeafter",
                "split {split}"
            );
        }
    }

    #[test]
    fn allowed_visual_osc_survives_all_fragmentation_boundaries() {
        let input = b"A\x1b]8;;https://example.test\x1b\\link\x1b]8;;\x07B";
        for split in 0..=input.len() {
            assert_eq!(
                sanitize(&[&input[..split], &input[split..]]),
                input,
                "split {split}"
            );
        }
    }

    #[test]
    fn osc_8_allows_web_links_and_closures_but_drops_active_content_schemes() {
        for input in [
            b"A\x1b]8;;javascript:alert(1)\x1b\\B".as_slice(),
            b"A\x1b]8;;data:text/html,unsafe\x07B".as_slice(),
            b"A\x1b]8;;file:///etc/passwd\x1b\\B".as_slice(),
            b"A\x1b]8;;relative/path\x1b\\B".as_slice(),
        ] {
            for split in 0..=input.len() {
                assert_eq!(
                    sanitize(&[&input[..split], &input[split..]]),
                    b"AB",
                    "split {split}"
                );
            }
        }
        for input in [
            b"A\x1b]8;;https://example.test\x1b\\B".as_slice(),
            b"A\x1b]8;id=1;MAILTO:user@example.test\x07B".as_slice(),
            b"A\x1b]8;;\x1b\\B".as_slice(),
        ] {
            assert_eq!(sanitize(&[input]), input);
        }
    }

    #[test]
    fn apc_dcs_pm_and_sos_are_explicitly_dropped() {
        let input = b"a\x1b_Gi=1;kitty\x1b\\b\x1bPqSIXEL\x1b\\c\x1b^pm\x1b\\d\x1bXsos\x1b\\e";
        for first in 0..=input.len() {
            for second in first..=input.len() {
                assert_eq!(
                    sanitize(&[&input[..first], &input[first..second], &input[second..]]),
                    b"abcde",
                    "splits {first}/{second}"
                );
            }
        }
    }

    #[test]
    fn c1_string_forms_follow_the_same_policy() {
        let input = b"a\x9d52;c;Zm9v\x9cb\x9fGkitty\x9cc\x90qsixel\x9cd";
        for split in 0..=input.len() {
            assert_eq!(
                sanitize(&[&input[..split], &input[split..]]),
                b"abcd",
                "split {split}"
            );
        }
    }

    #[test]
    fn utf8_continuations_that_overlap_c1_controls_are_never_reinterpreted() {
        let input = "aНÜ\u{259c}b".as_bytes();
        for split in 0..=input.len() {
            assert_eq!(
                sanitize(&[&input[..split], &input[split..]]),
                input,
                "split {split}"
            );
        }
    }

    #[test]
    fn utf8_inside_an_allowed_osc_cannot_terminate_it_as_a_c1_string() {
        let input = "\u{1b}]2;Н\u{259c} title\u{1b}\\safe".as_bytes();
        for split in 0..=input.len() {
            assert_eq!(
                sanitize(&[&input[..split], &input[split..]]),
                input,
                "split {split}"
            );
        }
    }

    #[test]
    fn unknown_osc_is_removed_at_every_fragmentation_boundary() {
        let input = b"before\x1b]999;not-a-web-contract\x07after";
        for split in 0..=input.len() {
            assert_eq!(
                sanitize(&[&input[..split], &input[split..]]),
                b"beforeafter",
                "split {split}"
            );
        }
    }

    #[test]
    fn reset_discards_an_incomplete_control_string() {
        let mut sanitizer = WebTerminalSanitizer::default();
        let mut output = Vec::new();
        sanitizer.push(b"safe\x1b]52;c;partial", &mut output);
        sanitizer.reset();
        sanitizer.push(b"fresh", &mut output);
        assert_eq!(output, b"safefresh");
    }
}
