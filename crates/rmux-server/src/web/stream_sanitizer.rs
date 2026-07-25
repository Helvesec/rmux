//! Fragmentation-safe WebShare policy boundary.
//!
//! Visual OSC commands on the allowlist survive. Clipboard OSC 52, unknown
//! OSC commands, and every APC/DCS/PM/SOS string (including Kitty graphics and
//! SIXEL) are dropped rather than delegated to browser-terminal behavior.
//!
//! A control is recognized exactly where the viewer recognizes one. The viewer
//! decodes the stream as UTF-8, so a C1 control only exists in its encoded
//! `0xc2 0x80..=0x9f` form. A bare 8-bit `0x80..=0x9f` byte is invalid UTF-8:
//! the browser, rmux's own emulator and tmux all render it as U+FFFD. Reading
//! such a byte as an OSC/DCS/PM/APC/SOS introducer — or as a string terminator
//! — would delete pane output the owner can plainly see, so bare C1 bytes are
//! forwarded as the data bytes they are.

const MAX_BUFFERED_OSC_BYTES: usize = 1024 * 1024;

#[derive(Debug, Default)]
pub(crate) struct WebTerminalSanitizer {
    state: State,
    pending_c2: bool,
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
        bell_terminates: bool,
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
        self.pending_c2 = false;
    }

    fn push_byte(&mut self, byte: u8, output: &mut Vec<u8>) {
        // `0xc2` is the only lead byte that can encode a C1 control, and it can
        // never be a UTF-8 continuation byte, so one byte of lookahead decides
        // whether a control code point is on the wire.
        if self.pending_c2 {
            self.pending_c2 = false;
            match byte {
                0x80..=0x9f => {
                    self.process_byte(InputByte::encoded_c1(byte), output);
                    return;
                }
                // A complete two-byte character: both halves stay adjacent.
                0xa0..=0xbf => self.process_byte(InputByte::plain(0xc2), output),
                // A truncated sequence. Forwarding the orphaned lead byte would
                // let a later `0x80..=0x9f` byte recombine with it once this
                // boundary deletes what sat between them — a C1 control the
                // pane never emitted and the owner's screen never shows.
                _ => {}
            }
        }
        if byte == 0xc2 {
            self.pending_c2 = true;
            return;
        }

        self.process_byte(InputByte::plain(byte), output);
    }

    fn process_byte(&mut self, input: InputByte, output: &mut Vec<u8>) {
        let state = std::mem::take(&mut self.state);
        self.state = match state {
            State::Ground => ground(input, output),
            State::Escape => escape_sequence(input, output),
            State::Osc { mut bytes, escaped } => {
                if cancelled(input) {
                    State::Ground
                } else if escaped && input.byte != b'\\' {
                    escape_sequence(input, output)
                } else if string_ended(input, escaped, true) {
                    input.append_to(&mut bytes);
                    if allowed_osc(&bytes) {
                        output.extend_from_slice(&bytes);
                    }
                    State::Ground
                } else if let Some(state) = string_introducer(input) {
                    state
                } else if bytes.len() >= MAX_BUFFERED_OSC_BYTES {
                    State::DiscardString {
                        escaped: input.byte == 0x1b,
                        bell_terminates: true,
                    }
                } else {
                    input.append_to(&mut bytes);
                    State::Osc {
                        bytes,
                        escaped: input.byte == 0x1b,
                    }
                }
            }
            State::DiscardString {
                escaped,
                bell_terminates,
            } => {
                if cancelled(input) {
                    State::Ground
                } else if escaped && input.byte != b'\\' {
                    escape_sequence(input, output)
                } else if string_ended(input, escaped, bell_terminates) {
                    State::Ground
                } else if let Some(state) = string_introducer(input) {
                    state
                } else {
                    State::DiscardString {
                        escaped: input.byte == 0x1b,
                        bell_terminates,
                    }
                }
            }
        };
    }
}

#[derive(Clone, Copy)]
struct InputByte {
    byte: u8,
    /// The byte is the trailing half of a `0xc2 0x80..=0x9f` sequence, i.e. a
    /// C1 code point the viewer's UTF-8 decoder really produces.
    encoded_c1: bool,
}

impl InputByte {
    const fn plain(byte: u8) -> Self {
        Self {
            byte,
            encoded_c1: false,
        }
    }

    const fn encoded_c1(byte: u8) -> Self {
        Self {
            byte,
            encoded_c1: true,
        }
    }

    fn append_to(self, output: &mut Vec<u8>) {
        if self.encoded_c1 {
            output.extend_from_slice(&[0xc2, self.byte]);
        } else {
            output.push(self.byte);
        }
    }
}

fn ground(input: InputByte, output: &mut Vec<u8>) -> State {
    if let Some(state) = string_introducer(input) {
        return state;
    }
    match input.byte {
        0x1b => State::Escape,
        0x18 | 0x1a => State::Ground,
        _ => {
            input.append_to(output);
            State::Ground
        }
    }
}

fn escape_sequence(input: InputByte, output: &mut Vec<u8>) -> State {
    if cancelled(input) {
        return State::Ground;
    }
    if let Some(state) = string_introducer(input) {
        return state;
    }
    match input.byte {
        b']' => State::Osc {
            bytes: vec![0x1b, b']'],
            escaped: false,
        },
        b'P' | b'X' | b'^' | b'_' => State::DiscardString {
            escaped: false,
            bell_terminates: false,
        },
        0x1b => State::Escape,
        _ => {
            output.push(0x1b);
            input.append_to(output);
            State::Ground
        }
    }
}

fn string_introducer(input: InputByte) -> Option<State> {
    // Only a decoded C1 code point introduces a control string. A bare
    // `0x90`/`0x98`/`0x9d`/`0x9e`/`0x9f` byte is invalid UTF-8 that the viewer
    // renders as U+FFFD, so opening a string here would silently discard every
    // following byte of ordinary pane output.
    if !input.encoded_c1 {
        return None;
    }
    match input.byte {
        0x9d => {
            let mut bytes = Vec::with_capacity(2);
            input.append_to(&mut bytes);
            Some(State::Osc {
                bytes,
                escaped: false,
            })
        }
        0x90 | 0x98 | 0x9e | 0x9f => Some(State::DiscardString {
            escaped: false,
            bell_terminates: false,
        }),
        _ => None,
    }
}

fn cancelled(input: InputByte) -> bool {
    // CAN and SUB are C0 bytes: they are never part of a UTF-8 sequence, so the
    // viewer always sees them as controls.
    !input.encoded_c1 && matches!(input.byte, 0x18 | 0x1a)
}

fn string_ended(input: InputByte, escaped: bool, bell_terminates: bool) -> bool {
    // A bare `0x9c` is data, not ST: the viewer keeps the string open, so
    // ending it here would forward its tail as visible text.
    (input.encoded_c1 && input.byte == 0x9c)
        || (escaped && input.byte == b'\\')
        || (bell_terminates && input.byte == 0x07)
}

fn allowed_osc(sequence: &[u8]) -> bool {
    let Some(payload) = sequence
        .strip_prefix(b"\x1b]".as_slice())
        .or_else(|| sequence.strip_prefix([0xc2, 0x9d].as_slice()))
    else {
        return false;
    };
    let code_end = payload
        .iter()
        .enumerate()
        .position(|(index, byte)| {
            *byte == b';'
                || *byte == 0x07
                || *byte == 0x1b
                || (*byte == 0xc2 && payload.get(index + 1) == Some(&0x9c))
        })
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
    if payload.ends_with(b"\x1b\\") || payload.ends_with(&[0xc2, 0x9c]) {
        payload = &payload[..payload.len() - 2];
    } else if payload.last() == Some(&0x07) {
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
    fn bare_c1_bytes_are_data_and_never_introduce_a_control_string() {
        // Invalid UTF-8 for the viewer, so none of these open a string. tmux
        // 3.7b and rmux's own emulator both render each byte as U+FFFD, which
        // is exactly what the browser's UTF-8 decoder produces from them.
        let input = b"a\x9d52;c;Zm9v\x9cb\x9fGkitty\x9cc\x90qsixel\x9cd";
        for split in 0..=input.len() {
            assert_eq!(
                sanitize(&[&input[..split], &input[split..]]),
                input.as_slice(),
                "split {split}"
            );
        }
    }

    #[test]
    fn a_bare_c1_byte_never_swallows_the_rest_of_a_pane_stream() {
        // `cat` of a binary file, then ordinary shell output. Before the fix
        // the viewer received the first chunk's prefix and nothing else.
        let chunks: [&[u8]; 3] = [
            b"$ cat logo.bin\r\n\x9f\x8a\x00PNG",
            b"\r\n$ echo hello\r\nhello\r\n$ ",
            b"date\r\nFri Jul 25 12:00:00 CEST 2026\r\n$ ",
        ];
        assert_eq!(sanitize(&chunks), chunks.concat());
    }

    #[test]
    fn every_bare_c1_byte_reaches_the_viewer_unchanged() {
        for byte in 0x80..=0x9f_u8 {
            let input = [b'A', byte, b'B', b'C', b'\r', b'\n'];
            for split in 0..=input.len() {
                assert_eq!(
                    sanitize(&[&input[..split], &input[split..]]),
                    input.as_slice(),
                    "byte {byte:#04x}, split {split}"
                );
            }
        }
    }

    #[test]
    fn a_bare_c1_terminator_cannot_end_a_string_the_viewer_keeps_open() {
        // The viewer decodes 0x9c as U+FFFD and keeps buffering the OSC, so
        // treating it as ST would forward the clipboard tail as visible text.
        let input = b"a\x1b]52;c;Zm9vYmFy\x9cdGFpbA==\x07b";
        for split in 0..=input.len() {
            assert_eq!(
                sanitize(&[&input[..split], &input[split..]]),
                b"ab",
                "split {split}"
            );
        }
    }

    #[test]
    fn a_bare_c1_byte_inside_an_allowed_osc_stays_part_of_its_payload() {
        let input = b"a\x1b]2;ti\x9ctle\x07b";
        for split in 0..=input.len() {
            assert_eq!(
                sanitize(&[&input[..split], &input[split..]]),
                input.as_slice(),
                "split {split}"
            );
        }
    }

    #[test]
    fn a_bare_c1_byte_inside_an_osc_identifier_fails_closed() {
        // The viewer's OSC identifier would be `2<U+FFFD>` — not a number, so
        // its parser aborts the string. Forwarding it would be a divergence.
        let input = b"a\x1b]2\x9c;title\x07b";
        for split in 0..=input.len() {
            assert_eq!(
                sanitize(&[&input[..split], &input[split..]]),
                b"ab",
                "split {split}"
            );
        }
    }

    #[test]
    fn an_orphaned_c2_lead_byte_cannot_be_recombined_into_a_forged_c1_control() {
        // The input holds no C1 control: `0xc2` is a truncated lead and `0x9f`
        // is invalid UTF-8. Forwarding the lead byte after deleting what sat
        // between them would hand the viewer a real U+009F APC introducer and
        // swallow the rest of the pane's output.
        for input in [
            b"A\xc2\x18\x9fplain text".as_slice(),
            b"A\xc2\x1b]52;c;Zm9v\x07\x9fplain text".as_slice(),
            b"A\xc2\x1b\x1b\x9fplain text".as_slice(),
        ] {
            for split in 0..=input.len() {
                let output = sanitize(&[&input[..split], &input[split..]]);
                assert!(
                    !output
                        .windows(2)
                        .any(|pair| pair[0] == 0xc2 && (0x80..=0x9f).contains(&pair[1])),
                    "split {split}: forged C1 control in {output:02x?}"
                );
                assert!(
                    output.ends_with(b"plain text"),
                    "split {split}: pane output must survive, got {output:02x?}"
                );
            }
        }
    }

    #[test]
    fn a_complete_two_byte_character_keeps_both_halves() {
        let input = "A\u{a0}\u{ff}B".as_bytes();
        for split in 0..=input.len() {
            assert_eq!(
                sanitize(&[&input[..split], &input[split..]]),
                input,
                "split {split}"
            );
        }
    }

    #[test]
    fn utf8_encoded_c1_strings_follow_the_same_policy() {
        let blocked = b"a\xc2\x9d52;c;Zm9v\xc2\x9cb\xc2\x9fGkitty\xc2\x9cc";
        for split in 0..=blocked.len() {
            assert_eq!(
                sanitize(&[&blocked[..split], &blocked[split..]]),
                b"abc",
                "blocked split {split}"
            );
        }

        let allowed = b"a\xc2\x9d2;title\xc2\x9cb";
        for split in 0..=allowed.len() {
            assert_eq!(
                sanitize(&[&allowed[..split], &allowed[split..]]),
                allowed,
                "allowed split {split}"
            );
        }
    }

    #[test]
    fn utf8_continuations_that_overlap_c1_controls_are_never_reinterpreted() {
        let input = "a\u{a0}НÜ\u{259c}b".as_bytes();
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
    fn escape_reentry_never_emits_a_forbidden_string_prefix() {
        for input in [
            b"a\x1b\x1b]52;c;WA==\x07b".as_slice(),
            b"a\x1b]2;safe\x1b]52;c;WA==\x07b".as_slice(),
            b"a\x1bPignored\x1b]52;c;WA==\x07b".as_slice(),
        ] {
            for first in 0..=input.len() {
                for second in first..=input.len() {
                    assert_eq!(
                        sanitize(&[&input[..first], &input[first..second], &input[second..]]),
                        b"ab",
                        "splits {first}/{second}"
                    );
                }
            }
        }
    }

    #[test]
    fn can_and_sub_cancel_strings_before_following_input_is_reparsed() {
        for cancel in [0x18, 0x1a] {
            let mut input = b"a\x1b]2;safe".to_vec();
            input.push(cancel);
            input.extend_from_slice(b"\x1b]52;c;WA==\x07b");
            for split in 0..=input.len() {
                assert_eq!(
                    sanitize(&[&input[..split], &input[split..]]),
                    b"ab",
                    "cancel {cancel:#x}, split {split}"
                );
            }
        }
    }

    #[test]
    fn oversized_osc_discard_ends_at_bell() {
        let mut input = b"before\x1b]2;".to_vec();
        input.resize(input.len() + MAX_BUFFERED_OSC_BYTES + 1, b'x');
        input.extend_from_slice(b"\x07after");
        assert_eq!(sanitize(&[&input]), b"beforeafter");
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
