const ESC: u8 = 0x1b;
const MAX_PRIVATE_MODE_SEQUENCE: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum OutputWriteKind {
    Normal,
    ScopedVtInput,
}

#[derive(Debug, Eq, PartialEq)]
pub(super) struct OutputChunk {
    pub(super) kind: OutputWriteKind,
    pub(super) bytes: Vec<u8>,
}

#[derive(Debug, Default)]
pub(super) struct VtModeScanner {
    pending: Vec<u8>,
}

impl VtModeScanner {
    /// Splits output into ordinary bytes and the small, typed set of writes
    /// which require the Windows 11 scoped VT-input bridge. A possible
    /// sequence is retained across calls so fragmentation at any byte
    /// boundary is safe.
    pub(super) fn push(&mut self, bytes: &[u8]) -> Vec<OutputChunk> {
        let mut input = std::mem::take(&mut self.pending);
        input.extend_from_slice(bytes);

        let mut chunks = Vec::new();
        let mut scan_at = 0;
        let mut normal_at = 0;
        while let Some(relative_escape) = input[scan_at..].iter().position(|byte| *byte == ESC) {
            let escape_at = scan_at + relative_escape;
            match inspect_scoped_sequence(&input[escape_at..]) {
                ScopedSequenceInspection::Incomplete => {
                    push_chunk(
                        &mut chunks,
                        OutputWriteKind::Normal,
                        &input[normal_at..escape_at],
                    );
                    self.pending.extend_from_slice(&input[escape_at..]);
                    return chunks;
                }
                ScopedSequenceInspection::Complete {
                    len,
                    requires_scoped_vt_input: true,
                } => {
                    push_chunk(
                        &mut chunks,
                        OutputWriteKind::Normal,
                        &input[normal_at..escape_at],
                    );
                    push_chunk(
                        &mut chunks,
                        OutputWriteKind::ScopedVtInput,
                        &input[escape_at..escape_at + len],
                    );
                    scan_at = escape_at + len;
                    normal_at = scan_at;
                }
                ScopedSequenceInspection::Complete {
                    len,
                    requires_scoped_vt_input: false,
                } => {
                    scan_at = escape_at + len;
                }
                ScopedSequenceInspection::NotScopedSequence => {
                    scan_at = escape_at + 1;
                }
            }
        }

        push_chunk(&mut chunks, OutputWriteKind::Normal, &input[normal_at..]);
        chunks
    }

    /// Releases an incomplete candidate as ordinary output when the writer is
    /// being destroyed. Normal `flush` deliberately does not call this: attach
    /// frames can split a DECSET sequence and the output worker flushes every
    /// frame.
    pub(super) fn finish(&mut self) -> Option<OutputChunk> {
        (!self.pending.is_empty()).then(|| OutputChunk {
            kind: OutputWriteKind::Normal,
            bytes: std::mem::take(&mut self.pending),
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ScopedSequenceInspection {
    Incomplete,
    Complete {
        len: usize,
        requires_scoped_vt_input: bool,
    },
    NotScopedSequence,
}

fn inspect_scoped_sequence(bytes: &[u8]) -> ScopedSequenceInspection {
    debug_assert_eq!(bytes.first(), Some(&ESC));
    if bytes.len() == 1 {
        return ScopedSequenceInspection::Incomplete;
    }
    match bytes[1] {
        b'[' => inspect_private_mode(bytes),
        b']' => inspect_palette_query(bytes),
        _ => ScopedSequenceInspection::NotScopedSequence,
    }
}

fn inspect_private_mode(bytes: &[u8]) -> ScopedSequenceInspection {
    debug_assert!(bytes.starts_with(b"\x1b["));
    if bytes.len() == 2 {
        return ScopedSequenceInspection::Incomplete;
    }
    if bytes[2] != b'?' {
        return ScopedSequenceInspection::NotScopedSequence;
    }
    if bytes.len() == 3 {
        return ScopedSequenceInspection::Incomplete;
    }

    let mut value = Some(0_u32);
    let mut has_digit = false;
    let mut targeted = false;
    for (offset, byte) in bytes[3..].iter().copied().enumerate() {
        let len = offset + 4;
        if len > MAX_PRIVATE_MODE_SEQUENCE {
            return ScopedSequenceInspection::NotScopedSequence;
        }
        match byte {
            b'0'..=b'9' => {
                has_digit = true;
                value = value.and_then(|current| {
                    current
                        .checked_mul(10)
                        .and_then(|current| current.checked_add(u32::from(byte - b'0')))
                });
            }
            b';' if has_digit => {
                targeted |= value.is_some_and(is_scoped_vt_input_mode);
                value = Some(0);
                has_digit = false;
            }
            b'h' | b'l' if has_digit => {
                targeted |= value.is_some_and(is_scoped_vt_input_mode);
                return ScopedSequenceInspection::Complete {
                    len,
                    requires_scoped_vt_input: targeted,
                };
            }
            // A different CSI final byte proves this is complete ordinary
            // output. It must not be held waiting for another attach frame.
            0x40..=0x7e => {
                return ScopedSequenceInspection::Complete {
                    len,
                    requires_scoped_vt_input: false,
                };
            }
            _ => return ScopedSequenceInspection::NotScopedSequence,
        }
    }

    if bytes.len() >= MAX_PRIVATE_MODE_SEQUENCE {
        ScopedSequenceInspection::NotScopedSequence
    } else {
        ScopedSequenceInspection::Incomplete
    }
}

/// Recognizes only one bounded OSC 4 palette query. Palette-setting writes,
/// multi-query extensions, malformed indices, and every other OSC family stay
/// on the ordinary output path. Server-side construction canonicalizes one
/// query per sequence, so this is deliberately narrower than a generic OSC
/// parser.
fn inspect_palette_query(bytes: &[u8]) -> ScopedSequenceInspection {
    debug_assert!(bytes.starts_with(b"\x1b]"));
    const PREFIX: &[u8] = b"\x1b]4;";
    if bytes.len() < PREFIX.len() {
        return if PREFIX.starts_with(bytes) {
            ScopedSequenceInspection::Incomplete
        } else {
            ScopedSequenceInspection::NotScopedSequence
        };
    }
    if !bytes.starts_with(PREFIX) {
        return ScopedSequenceInspection::NotScopedSequence;
    }

    let mut cursor = PREFIX.len();
    let mut index = 0_u16;
    let mut digits = 0_u8;
    while let Some(byte @ b'0'..=b'9') = bytes.get(cursor).copied() {
        if digits == 3 {
            return ScopedSequenceInspection::NotScopedSequence;
        }
        index = index * 10 + u16::from(byte - b'0');
        digits += 1;
        cursor += 1;
    }
    if digits == 0 {
        return if cursor == bytes.len() {
            ScopedSequenceInspection::Incomplete
        } else {
            ScopedSequenceInspection::NotScopedSequence
        };
    }
    if index > u16::from(u8::MAX) {
        return ScopedSequenceInspection::NotScopedSequence;
    }
    if cursor == bytes.len() {
        return ScopedSequenceInspection::Incomplete;
    }
    if bytes[cursor] != b';' {
        return ScopedSequenceInspection::NotScopedSequence;
    }
    cursor += 1;
    if cursor == bytes.len() {
        return ScopedSequenceInspection::Incomplete;
    }
    if bytes[cursor] != b'?' {
        return ScopedSequenceInspection::NotScopedSequence;
    }
    cursor += 1;
    if cursor == bytes.len() {
        return ScopedSequenceInspection::Incomplete;
    }

    match bytes[cursor] {
        0x07 => ScopedSequenceInspection::Complete {
            len: cursor + 1,
            requires_scoped_vt_input: true,
        },
        ESC if cursor + 1 == bytes.len() => ScopedSequenceInspection::Incomplete,
        ESC if bytes[cursor + 1] == b'\\' => ScopedSequenceInspection::Complete {
            len: cursor + 2,
            requires_scoped_vt_input: true,
        },
        _ => ScopedSequenceInspection::NotScopedSequence,
    }
}

const fn is_scoped_vt_input_mode(mode: u32) -> bool {
    matches!(mode, 1000 | 1002 | 1003 | 1004 | 1005 | 1006 | 2004 | 2031)
}

fn push_chunk(chunks: &mut Vec<OutputChunk>, kind: OutputWriteKind, bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
    if let Some(last) = chunks.last_mut().filter(|chunk| chunk.kind == kind) {
        last.bytes.extend_from_slice(bytes);
    } else {
        chunks.push(OutputChunk {
            kind,
            bytes: bytes.to_vec(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::{OutputChunk, OutputWriteKind, VtModeScanner};

    fn flatten(chunks: &[OutputChunk]) -> Vec<u8> {
        chunks
            .iter()
            .flat_map(|chunk| chunk.bytes.iter().copied())
            .collect()
    }

    fn scoped(chunks: &[OutputChunk]) -> Vec<Vec<u8>> {
        chunks
            .iter()
            .filter(|chunk| chunk.kind == OutputWriteKind::ScopedVtInput)
            .map(|chunk| chunk.bytes.clone())
            .collect()
    }

    #[test]
    fn every_target_mode_and_reset_is_classified() {
        for mode in [1000, 1002, 1003, 1004, 1005, 1006, 2004, 2031] {
            for final_byte in [b'h', b'l'] {
                let sequence = format!("\x1b[?{mode}{}", char::from(final_byte)).into_bytes();
                let chunks = VtModeScanner::default().push(&sequence);
                assert_eq!(scoped(&chunks), vec![sequence.clone()]);
                assert_eq!(flatten(&chunks), sequence);
            }
        }
    }

    #[test]
    fn targeted_sequence_survives_every_fragmentation_boundary() {
        let sequence = b"left \x1b[?1002;1006h right";
        for split in 0..=sequence.len() {
            let mut scanner = VtModeScanner::default();
            let mut chunks = scanner.push(&sequence[..split]);
            chunks.extend(scanner.push(&sequence[split..]));
            if let Some(final_chunk) = scanner.finish() {
                chunks.push(final_chunk);
            }
            assert_eq!(flatten(&chunks), sequence, "split at {split}");
            assert_eq!(
                scoped(&chunks),
                vec![b"\x1b[?1002;1006h".to_vec()],
                "split at {split}"
            );
        }
    }

    #[test]
    fn bounded_palette_queries_with_bel_or_st_are_classified() {
        for sequence in [
            b"\x1b]4;0;?\x07".as_slice(),
            b"\x1b]4;255;?\x1b\\".as_slice(),
        ] {
            let chunks = VtModeScanner::default().push(sequence);
            assert_eq!(scoped(&chunks), vec![sequence.to_vec()]);
            assert_eq!(flatten(&chunks), sequence);
        }
    }

    #[test]
    fn palette_query_survives_every_fragmentation_boundary() {
        for sequence in [
            b"left \x1b]4;7;?\x07 right".as_slice(),
            b"left \x1b]4;255;?\x1b\\ right".as_slice(),
        ] {
            for split in 0..=sequence.len() {
                let mut scanner = VtModeScanner::default();
                let mut chunks = scanner.push(&sequence[..split]);
                chunks.extend(scanner.push(&sequence[split..]));
                if let Some(final_chunk) = scanner.finish() {
                    chunks.push(final_chunk);
                }
                assert_eq!(flatten(&chunks), sequence, "split at {split}");
                assert_eq!(scoped(&chunks).len(), 1, "split at {split}");
            }
        }
    }

    #[test]
    fn palette_sets_and_opaque_osc_remain_ordinary() {
        for sequence in [
            b"\x1b]4;0;rgb:0000/0000/0000\x07".as_slice(),
            b"\x1b]4;0;#000000\x1b\\".as_slice(),
            b"\x1b]52;c;AAAA\x07".as_slice(),
            b"\x1b]0;title\x1b\\".as_slice(),
        ] {
            let chunks = VtModeScanner::default().push(sequence);
            assert_eq!(flatten(&chunks), sequence);
            assert!(scoped(&chunks).is_empty(), "ordinary OSC: {sequence:?}");
        }
    }

    #[test]
    fn malformed_or_extended_palette_queries_remain_ordinary() {
        for sequence in [
            b"\x1b]4;;?\x07".as_slice(),
            b"\x1b]4;256;?\x07".as_slice(),
            b"\x1b]4;0000;?\x07".as_slice(),
            b"\x1b]4;0;?;1;?\x07".as_slice(),
            b"\x1b]4;0;?x".as_slice(),
        ] {
            let chunks = VtModeScanner::default().push(sequence);
            assert_eq!(flatten(&chunks), sequence);
            assert!(scoped(&chunks).is_empty(), "invalid OSC: {sequence:?}");
        }
    }

    #[test]
    fn unicode_and_non_target_sequences_remain_ordinary() {
        let bytes = "début \u{1b}[?25l 中 \u{1b}[31m fin".as_bytes();
        let chunks = VtModeScanner::default().push(bytes);
        assert_eq!(flatten(&chunks), bytes);
        assert!(scoped(&chunks).is_empty());
    }

    #[test]
    fn target_digits_inside_text_or_other_modes_do_not_match() {
        let bytes = b"1000h \x1b[?11000h \x1b[?20040l";
        let chunks = VtModeScanner::default().push(bytes);
        assert_eq!(flatten(&chunks), bytes);
        assert!(scoped(&chunks).is_empty());
    }

    #[test]
    fn invalid_candidate_does_not_hide_a_following_target() {
        let bytes = b"\x1b[?10:x\x1b[?1000h";
        let chunks = VtModeScanner::default().push(bytes);
        assert_eq!(flatten(&chunks), bytes);
        assert_eq!(scoped(&chunks), vec![b"\x1b[?1000h".to_vec()]);
    }

    #[test]
    fn incomplete_candidate_is_bounded_and_released_on_finish() {
        let mut scanner = VtModeScanner::default();
        let prefix = b"text\x1b[?100";
        let chunks = scanner.push(prefix);
        assert_eq!(flatten(&chunks), b"text");
        let final_chunk = scanner.finish().expect("candidate is pending");
        assert_eq!(final_chunk.kind, OutputWriteKind::Normal);
        assert_eq!(final_chunk.bytes, b"\x1b[?100");

        let oversized = [b'1'; 80];
        let mut bytes = b"\x1b[?".to_vec();
        bytes.extend_from_slice(&oversized);
        let chunks = VtModeScanner::default().push(&bytes);
        assert_eq!(flatten(&chunks), bytes);

        let mut scanner = VtModeScanner::default();
        let chunks = scanner.push(b"text\x1b]4;255;?\x1b");
        assert_eq!(flatten(&chunks), b"text");
        let final_chunk = scanner.finish().expect("palette query is pending");
        assert_eq!(final_chunk.kind, OutputWriteKind::Normal);
        assert_eq!(final_chunk.bytes, b"\x1b]4;255;?\x1b");
    }
}
