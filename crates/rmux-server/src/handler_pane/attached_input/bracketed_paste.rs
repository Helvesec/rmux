const BRACKETED_PASTE_START: &[u8] = b"\x1b[200~";
const BRACKETED_PASTE_END: &[u8] = b"\x1b[201~";

pub(super) fn encode_bracketed_paste_for_mode(body: &[u8], bracketed: bool) -> Vec<u8> {
    if !bracketed {
        return body.to_vec();
    }

    let mut encoded =
        Vec::with_capacity(BRACKETED_PASTE_START.len() + body.len() + BRACKETED_PASTE_END.len());
    encoded.extend_from_slice(BRACKETED_PASTE_START);
    encoded.extend_from_slice(body);
    encoded.extend_from_slice(BRACKETED_PASTE_END);
    encoded
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BracketedPasteDecode {
    NotPaste,
    Partial,
    Matched {
        size: usize,
        body_start: usize,
        body_end: usize,
    },
}

#[cfg(test)]
pub(super) fn decode_bracketed_paste(input: &[u8]) -> BracketedPasteDecode {
    decode_bracketed_paste_after_append(input, 0)
}

pub(super) fn decode_bracketed_paste_after_append(
    input: &[u8],
    new_input_at: usize,
) -> BracketedPasteDecode {
    if input.starts_with(BRACKETED_PASTE_START) {
        // A retained partial paste was already searched through `new_input_at`.
        // Resume close enough before that boundary to catch a delimiter split
        // across reads without rescanning the accumulated body.
        let search_at = new_input_at
            .saturating_sub(BRACKETED_PASTE_END.len() - 1)
            .max(BRACKETED_PASTE_START.len())
            .min(input.len());
        if let Some(end_offset) = find_subslice(&input[search_at..], BRACKETED_PASTE_END) {
            let body_start = BRACKETED_PASTE_START.len();
            let body_end = search_at + end_offset;
            return BracketedPasteDecode::Matched {
                size: body_end + BRACKETED_PASTE_END.len(),
                body_start,
                body_end,
            };
        }
        return BracketedPasteDecode::Partial;
    }

    if input.len() >= 3 && BRACKETED_PASTE_START.starts_with(input) {
        return BracketedPasteDecode::Partial;
    }

    BracketedPasteDecode::NotPaste
}

/// Removes complete bracketed-paste markers from `input`, leaving the pasted
/// body as literal bytes. Used when routing input to an overlay such as the
/// command prompt, which treats a paste as literal text: without stripping, the
/// leading `ESC` of `ESC[200~` cancels the overlay and the body leaks to the
/// pane. Only whole `ESC[200~` / `ESC[201~` sequences are removed; any other
/// bytes (including the pasted content's own control characters) are preserved.
///
/// The scan iterates to a fixed point so crafted clipboard fragments that
/// reassemble into a live marker after a single removal pass (for example
/// `b"\x1b[20" + BRACKETED_PASTE_START + b"0~body"` collapses to a live
/// `ESC[200~` if the middle marker is removed in isolation) are also
/// eliminated.
pub(in crate::handler) fn strip_bracketed_paste_markers(input: &[u8]) -> Vec<u8> {
    let mut current = input.to_vec();
    loop {
        let mut out = Vec::with_capacity(current.len());
        let mut index = 0;
        let mut removed = false;
        while index < current.len() {
            let rest = &current[index..];
            if rest.starts_with(BRACKETED_PASTE_START) {
                index += BRACKETED_PASTE_START.len();
                removed = true;
            } else if rest.starts_with(BRACKETED_PASTE_END) {
                index += BRACKETED_PASTE_END.len();
                removed = true;
            } else {
                out.push(current[index]);
                index += 1;
            }
        }
        if !removed {
            return out;
        }
        current = out;
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::{
        decode_bracketed_paste, decode_bracketed_paste_after_append,
        encode_bracketed_paste_for_mode, strip_bracketed_paste_markers, BracketedPasteDecode,
    };

    #[test]
    fn pane_mode_controls_bracketed_paste_wrappers() {
        let body = b"line one\nline two";
        assert_eq!(encode_bracketed_paste_for_mode(body, false), body);
        assert_eq!(
            encode_bracketed_paste_for_mode(body, true),
            b"\x1b[200~line one\nline two\x1b[201~"
        );
    }

    #[test]
    fn detects_chunked_start_as_partial() {
        assert_eq!(
            decode_bracketed_paste(b"\x1b[20"),
            BracketedPasteDecode::Partial
        );
    }

    #[test]
    fn leaves_lone_escape_for_key_decoder() {
        assert_eq!(
            decode_bracketed_paste(b"\x1b"),
            BracketedPasteDecode::NotPaste
        );
    }

    #[test]
    fn leaves_ambiguous_csi_prefix_for_key_decoder() {
        assert_eq!(
            decode_bracketed_paste(b"\x1b["),
            BracketedPasteDecode::NotPaste
        );
    }

    #[test]
    fn matches_through_the_closing_delimiter() {
        assert_eq!(
            decode_bracketed_paste(b"\x1b[200~line\r\n\x02\x1b[201~tail"),
            BracketedPasteDecode::Matched {
                size: 19,
                body_start: 6,
                body_end: 13,
            }
        );
    }

    #[test]
    fn incremental_search_finds_closing_delimiter_split_at_append_boundary() {
        let input = b"\x1b[200~body\x1b[201~tail";
        let closing_at = b"\x1b[200~body".len();
        for new_input_at in closing_at + 1..closing_at + b"\x1b[201~".len() {
            assert_eq!(
                decode_bracketed_paste_after_append(input, new_input_at),
                BracketedPasteDecode::Matched {
                    size: closing_at + b"\x1b[201~".len(),
                    body_start: b"\x1b[200~".len(),
                    body_end: closing_at,
                },
                "delimiter split before byte {new_input_at}"
            );
        }
    }

    #[test]
    fn incremental_search_starts_at_the_append_boundary_overlap() {
        // The earlier close is an impossible retained-state sentinel: choosing
        // it would prove that the append cursor was ignored and the prefix was
        // rescanned.
        let input = b"\x1b[200~old\x1b[201~padding-padding\x1b[201~tail";
        let new_input_at = b"\x1b[200~old\x1b[201~padding-padding".len();
        let body_end = new_input_at;
        assert_eq!(
            decode_bracketed_paste_after_append(input, new_input_at),
            BracketedPasteDecode::Matched {
                size: body_end + b"\x1b[201~".len(),
                body_start: b"\x1b[200~".len(),
                body_end,
            }
        );
    }

    #[test]
    fn ignores_other_escape_sequences() {
        assert_eq!(
            decode_bracketed_paste(b"\x1b[201~"),
            BracketedPasteDecode::NotPaste
        );
    }

    #[test]
    fn strip_removes_complete_markers() {
        assert_eq!(
            strip_bracketed_paste_markers(b"\x1b[200~hello\x1b[201~"),
            b"hello".to_vec()
        );
    }

    #[test]
    fn strip_preserves_ordinary_bytes_and_lone_escape() {
        assert_eq!(
            strip_bracketed_paste_markers(b"hi\x1bworld"),
            b"hi\x1bworld".to_vec()
        );
    }

    #[test]
    fn strip_collapses_fragmented_end_marker_into_nothing() {
        // Hostile clipboard: ESC[20 + ESC[201~ + 1~body.
        // A single-pass strip would remove the middle complete marker and leave
        // ESC[201~body — a live end-marker; the fixed-point loop must remove it.
        let mut input = Vec::new();
        input.extend_from_slice(b"\x1b[20");
        input.extend_from_slice(b"\x1b[201~");
        input.extend_from_slice(b"1~body");
        assert_eq!(strip_bracketed_paste_markers(&input), b"body".to_vec());
    }

    #[test]
    fn strip_collapses_fragmented_start_marker_into_nothing() {
        let mut input = Vec::new();
        input.extend_from_slice(b"\x1b[");
        input.extend_from_slice(b"\x1b[200~");
        input.extend_from_slice(b"200~body");
        assert_eq!(strip_bracketed_paste_markers(&input), b"body".to_vec());
    }

    #[test]
    fn strip_is_idempotent_on_pasted_control_characters() {
        // ESC-less control characters must be preserved verbatim so a pasted
        // Ctrl-C stays a Ctrl-C in the overlay's decoded stream.
        assert_eq!(
            strip_bracketed_paste_markers(b"\x1b[200~a\x03b\x1b[201~"),
            b"a\x03b".to_vec()
        );
    }
}
