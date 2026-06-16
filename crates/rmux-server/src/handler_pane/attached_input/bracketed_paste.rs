const BRACKETED_PASTE_START: &[u8] = b"\x1b[200~";
const BRACKETED_PASTE_END: &[u8] = b"\x1b[201~";

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

pub(super) fn decode_bracketed_paste(input: &[u8]) -> BracketedPasteDecode {
    if input.starts_with(BRACKETED_PASTE_START) {
        let body = &input[BRACKETED_PASTE_START.len()..];
        if let Some(end_offset) = find_subslice(body, BRACKETED_PASTE_END) {
            let body_start = BRACKETED_PASTE_START.len();
            let body_end = body_start + end_offset;
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

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::{decode_bracketed_paste, BracketedPasteDecode};

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
    fn ignores_other_escape_sequences() {
        assert_eq!(
            decode_bracketed_paste(b"\x1b[201~"),
            BracketedPasteDecode::NotPaste
        );
    }
}
