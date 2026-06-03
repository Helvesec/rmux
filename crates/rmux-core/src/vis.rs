//! vis-style escaping helpers used by tmux-facing text surfaces.

/// Flags controlling vis-style byte escaping.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct VisFlags {
    /// Emit octal escapes for bytes without a shorter cstyle form.
    pub octal: bool,
    /// Prefer C-style escapes for common control characters.
    pub cstyle: bool,
    /// Escape tabs.
    pub tab: bool,
    /// Escape newlines.
    pub newline: bool,
    /// Escape control bytes unsafe for terminal output.
    pub safe: bool,
    /// Do not specially escape backslashes.
    pub noslash: bool,
}

/// Encodes bytes using the requested vis-style escaping rules.
#[must_use]
pub(crate) fn encode_bytes(input: &[u8], flags: VisFlags) -> String {
    let mut encoded = String::new();
    for &byte in input {
        encode_byte(byte, flags, &mut encoded);
    }
    encoded
}

/// Encodes a UTF-8 string using the requested vis-style escaping rules.
#[must_use]
pub(crate) fn encode_str(input: &str, flags: VisFlags) -> String {
    encode_bytes(input.as_bytes(), flags)
}

/// tmux-compatible `paste_make_sample` preview rendering.
#[must_use]
pub(crate) fn encode_buffer_sample(input: &[u8]) -> String {
    const WIDTH: usize = 200;

    let flags = VisFlags {
        octal: true,
        cstyle: true,
        tab: true,
        newline: true,
        safe: false,
        noslash: false,
    };

    let prefix_len = input.len().min(WIDTH);
    let mut encoded = encode_bytes(&input[..prefix_len], flags);
    if input.len() > WIDTH || encoded.len() > WIDTH {
        truncate_at_byte_boundary(&mut encoded, WIDTH);
        encoded.push_str("...");
    }
    encoded
}

/// Encodes bytes for tmux-style safe paste-buffer writes.
#[must_use]
pub fn encode_paste_bytes(input: &[u8]) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(input.len());
    for &byte in input {
        encode_paste_byte(byte, &mut encoded);
    }
    encoded
}

fn encode_paste_byte(byte: u8, output: &mut Vec<u8>) {
    if (0x20..=0x7e).contains(&byte) || byte >= 0x80 {
        output.push(byte);
        return;
    }

    match byte {
        b'\0' => output.extend_from_slice(b"\\000"),
        b'\x07' => output.extend_from_slice(b"\\a"),
        b'\x08' => output.extend_from_slice(b"\\b"),
        b'\t' => output.extend_from_slice(b"\\t"),
        b'\n' => output.extend_from_slice(b"\\n"),
        b'\x0b' => output.extend_from_slice(b"\\v"),
        b'\x0c' => output.extend_from_slice(b"\\f"),
        b'\r' => output.extend_from_slice(b"\\r"),
        _ => {
            output.push(b'\\');
            output.push(b'0' + ((byte >> 6) & 0x7));
            output.push(b'0' + ((byte >> 3) & 0x7));
            output.push(b'0' + (byte & 0x7));
        }
    }
}

fn encode_byte(byte: u8, flags: VisFlags, output: &mut String) {
    if should_keep_raw(byte, flags) {
        output.push(char::from(byte));
        return;
    }

    if flags.cstyle {
        match byte {
            b'\0' => {
                output.push_str("\\000");
                return;
            }
            b'\x07' => {
                output.push_str("\\a");
                return;
            }
            b'\x08' => {
                output.push_str("\\b");
                return;
            }
            b'\x09' => {
                output.push_str("\\t");
                return;
            }
            b'\x0a' => {
                output.push_str("\\n");
                return;
            }
            b'\x0b' => {
                output.push_str("\\v");
                return;
            }
            b'\x0c' => {
                output.push_str("\\f");
                return;
            }
            b'\x0d' => {
                output.push_str("\\r");
                return;
            }
            b'\\' if !flags.noslash => {
                output.push_str("\\\\");
                return;
            }
            _ => {}
        }
    }

    if byte == b'\\' && !flags.noslash {
        output.push_str("\\\\");
        return;
    }

    if flags.octal || flags.safe || flags.cstyle {
        output.push('\\');
        output.push(char::from(b'0' + ((byte >> 6) & 0x7)));
        output.push(char::from(b'0' + ((byte >> 3) & 0x7)));
        output.push(char::from(b'0' + (byte & 0x7)));
        return;
    }

    output.push(char::from(byte));
}

fn should_keep_raw(byte: u8, flags: VisFlags) -> bool {
    if byte == b'\\' && !flags.noslash {
        return false;
    }

    if flags.safe {
        return (0x20..=0x7e).contains(&byte);
    }

    if byte == b'\t' {
        return !flags.tab;
    }
    if byte == b'\n' {
        return !flags.newline;
    }

    (0x20..=0x7e).contains(&byte)
}

fn truncate_at_byte_boundary(value: &mut String, max_len: usize) {
    if value.len() <= max_len {
        return;
    }

    let mut boundary = max_len;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value.truncate(boundary);
}

#[cfg(test)]
mod tests {
    use super::{encode_buffer_sample, encode_bytes, encode_paste_bytes, VisFlags};

    #[test]
    fn sample_encoding_matches_tmux_style_for_common_controls() {
        let encoded = encode_buffer_sample(b"one\two\nthree\\");
        assert_eq!(encoded, "one\\two\\nthree\\\\");
    }

    #[test]
    fn sample_encoding_truncates_to_tmux_width() {
        let input = vec![b'a'; 205];
        let encoded = encode_buffer_sample(&input);
        assert_eq!(encoded.len(), 203);
        assert!(encoded.ends_with("..."));
    }

    #[test]
    fn safe_paste_encoding_keeps_printable_bytes() {
        assert_eq!(encode_paste_bytes(b"hello"), b"hello");
    }

    #[test]
    fn safe_paste_encoding_escapes_controls_without_backslash_escaping() {
        let encoded = String::from_utf8(encode_paste_bytes(b"\t\\\x1b")).expect("utf8");
        assert_eq!(encoded, "\\t\\\\033");
    }

    #[test]
    fn safe_paste_encoding_preserves_utf8_bytes() {
        let input = "echo PASTED_漢字".as_bytes();
        assert_eq!(encode_paste_bytes(input), input);
    }

    #[test]
    fn safe_paste_encoding_escapes_delete() {
        let encoded = String::from_utf8(encode_paste_bytes(b"\x7f")).expect("utf8");
        assert_eq!(encoded, "\\177");
    }

    #[test]
    fn generic_encoding_respects_noslash() {
        let encoded = encode_bytes(
            b"\\",
            VisFlags {
                noslash: true,
                ..VisFlags::default()
            },
        );
        assert_eq!(encoded, "\\");
    }
}
