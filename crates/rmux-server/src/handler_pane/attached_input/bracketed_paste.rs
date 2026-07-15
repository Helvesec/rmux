const BRACKETED_PASTE_START: &[u8] = b"\x1b[200~";
const BRACKETED_PASTE_END: &[u8] = b"\x1b[201~";

/// Takes a bounded body segment from an incomplete bracketed paste while
/// retaining the opener and any possible split closing delimiter. The caller
/// can forward the returned bytes through the paste path and continue parsing
/// later chunks without retaining an unbounded clipboard payload.
pub(super) fn take_incomplete_bracketed_paste_segment(
    input: &mut Vec<u8>,
    maximum: usize,
) -> Option<Vec<u8>> {
    if input.len() <= maximum || !input.starts_with(BRACKETED_PASTE_START) {
        return None;
    }

    let delimiter_tail = longest_proper_suffix(input, BRACKETED_PASTE_END).unwrap_or(0);
    let utf8_tail = incomplete_utf8_suffix_len(input);
    let tail_len = delimiter_tail.max(utf8_tail);
    let body_end = input.len().saturating_sub(tail_len);
    if body_end <= BRACKETED_PASTE_START.len() {
        return None;
    }

    let body = input[BRACKETED_PASTE_START.len()..body_end].to_vec();
    let tail = input[body_end..].to_vec();
    input.truncate(BRACKETED_PASTE_START.len());
    input.extend_from_slice(&tail);
    Some(body)
}

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
    let mut out = Vec::with_capacity(input.len());
    for &byte in input {
        out.push(byte);
        loop {
            let marker_len = if out.ends_with(BRACKETED_PASTE_START) {
                Some(BRACKETED_PASTE_START.len())
            } else if out.ends_with(BRACKETED_PASTE_END) {
                Some(BRACKETED_PASTE_END.len())
            } else {
                None
            };
            let Some(marker_len) = marker_len else {
                break;
            };
            out.truncate(out.len() - marker_len);
        }
    }
    out
}

/// Removes markers introduced by the latest append without rescanning the
/// already-scrubbed prefix. The retained prefix remains the reduction stack,
/// so markers split across the append boundary and arbitrarily deep deletion
/// cascades are handled while visiting each newly appended byte once.
pub(in crate::handler) fn strip_bracketed_paste_markers_after_append(
    input: &mut Vec<u8>,
    new_input_at: usize,
) {
    let _ = strip_bracketed_paste_markers_after_append_inner(input, new_input_at);
}

/// Continues the same stack reduction used by
/// [`strip_bracketed_paste_markers`] from an already-scrubbed prefix. Keeping
/// that prefix as the reduction stack is important: a deletion in the newly
/// appended bytes can expose another marker across the append boundary, but
/// replaying the prefix after every such deletion makes a nested cascade
/// quadratic. Each appended byte is instead visited exactly once here.
fn strip_bracketed_paste_markers_after_append_inner(
    input: &mut Vec<u8>,
    new_input_at: usize,
) -> usize {
    let appended = input.split_off(new_input_at.min(input.len()));
    let scanned = appended.len();
    for byte in appended {
        input.push(byte);
        if let Some(marker_len) = trailing_bracketed_paste_marker_len(input) {
            input.truncate(input.len() - marker_len);
        }
    }
    scanned
}

fn trailing_bracketed_paste_marker_len(input: &[u8]) -> Option<usize> {
    if input.ends_with(BRACKETED_PASTE_START) {
        Some(BRACKETED_PASTE_START.len())
    } else if input.ends_with(BRACKETED_PASTE_END) {
        Some(BRACKETED_PASTE_END.len())
    } else {
        None
    }
}

pub(super) fn neutralize_timed_out_bracketed_paste(input: &[u8]) -> Option<Vec<u8>> {
    input.starts_with(BRACKETED_PASTE_START).then(|| {
        let mut body = strip_bracketed_paste_markers(input);
        // The timeout proves this was already inside a bracketed paste, so a
        // trailing proper prefix of either protocol marker is not user text:
        // it is a delimiter fragment cut by the idle deadline. Remove the
        // longest suffix repeatedly. Keeping even a lone trailing ESC would
        // move an incomplete CSI/APC into a non-bracketed child parser when
        // the sanitized body is forwarded.
        while let Some(suffix_len) = longest_partial_marker_suffix(&body) {
            body.truncate(body.len() - suffix_len);
        }
        body
    })
}

fn longest_partial_marker_suffix(input: &[u8]) -> Option<usize> {
    let mut longest = None;
    for marker in [BRACKETED_PASTE_START, BRACKETED_PASTE_END] {
        for prefix_len in 1..marker.len() {
            if input.ends_with(&marker[..prefix_len]) {
                longest = Some(longest.map_or(prefix_len, |found: usize| found.max(prefix_len)));
            }
        }
    }
    longest
}

fn longest_proper_suffix(input: &[u8], marker: &[u8]) -> Option<usize> {
    (1..marker.len())
        .rev()
        .find(|&prefix_len| input.ends_with(&marker[..prefix_len]))
}

fn incomplete_utf8_suffix_len(input: &[u8]) -> usize {
    let mut continuation_len = 0_usize;
    for &byte in input.iter().rev().take(3) {
        if byte & 0b1100_0000 == 0b1000_0000 {
            continuation_len += 1;
        } else {
            break;
        }
    }

    let lead_at = input.len().saturating_sub(continuation_len + 1);
    let Some(&lead) = input.get(lead_at) else {
        return 0;
    };
    let expected = match lead {
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => return 0,
    };
    let present = continuation_len + 1;
    if present < expected {
        present
    } else {
        0
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
        encode_bracketed_paste_for_mode, neutralize_timed_out_bracketed_paste,
        strip_bracketed_paste_markers, strip_bracketed_paste_markers_after_append,
        strip_bracketed_paste_markers_after_append_inner, take_incomplete_bracketed_paste_segment,
        BracketedPasteDecode, BRACKETED_PASTE_START,
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
    fn overflowing_incomplete_paste_keeps_only_opener_and_split_close() {
        let mut input = b"\x1b[200~body-body\x1b[20".to_vec();

        let body = take_incomplete_bracketed_paste_segment(&mut input, 8)
            .expect("over-limit incomplete paste yields a bounded segment");

        assert_eq!(body, b"body-body");
        assert_eq!(input, b"\x1b[200~\x1b[20");
    }

    #[test]
    fn overflowing_incomplete_paste_never_splits_a_utf8_scalar() {
        let mut input = b"\x1b[200~body\xe2\x82".to_vec();

        let body = take_incomplete_bracketed_paste_segment(&mut input, 8)
            .expect("over-limit incomplete paste yields a bounded segment");

        assert_eq!(body, b"body");
        assert_eq!(input, b"\x1b[200~\xe2\x82");
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

    #[test]
    fn incremental_strip_handles_markers_split_across_every_append_boundary() {
        let source = b"prefix\x1b[200~body\x1b[201~suffix";
        let expected = strip_bracketed_paste_markers(source);

        for chunk_size in 1..=source.len() {
            let mut pending = Vec::new();
            for chunk in source.chunks(chunk_size) {
                let new_input_at = pending.len();
                pending.extend_from_slice(chunk);
                strip_bracketed_paste_markers_after_append(&mut pending, new_input_at);
            }
            assert_eq!(pending, expected, "chunk size {chunk_size}");
        }
    }

    #[test]
    fn incremental_strip_expands_left_for_fixed_point_cascades() {
        let source = b"prefix\x1b[20\x1b[201~1~body";
        let expected = strip_bracketed_paste_markers(source);
        let mut pending = b"prefix\x1b[20".to_vec();
        let new_input_at = pending.len();
        pending.extend_from_slice(b"\x1b[201~1~body");

        strip_bracketed_paste_markers_after_append(&mut pending, new_input_at);

        assert_eq!(pending, expected);
    }

    #[test]
    fn incremental_strip_visits_each_appended_byte_once_under_deep_cascades() {
        // `ESC[200` is a proper five-byte prefix of the six-byte start marker.
        // Every appended `~` removes one prefix, exposing the previous prefix
        // for the next byte. A rescan-and-back-up implementation performs one
        // increasingly deep pass per deletion (quadratic work); the retained
        // prefix as a reduction stack handles the same cascade in one pass.
        const CASCADE_DEPTH: usize = 16 * 1024;
        let marker_prefix = &BRACKETED_PASTE_START[..BRACKETED_PASTE_START.len() - 1];
        let mut pending = marker_prefix.repeat(CASCADE_DEPTH);
        let new_input_at = pending.len();
        pending.extend(std::iter::repeat_n(b'~', CASCADE_DEPTH));

        let scanned = strip_bracketed_paste_markers_after_append_inner(&mut pending, new_input_at);

        assert_eq!(scanned, CASCADE_DEPTH);
        assert!(pending.is_empty(), "the entire marker cascade must reduce");
    }

    #[test]
    fn timed_out_paste_drops_every_partial_marker_suffix() {
        for suffix in [
            b"\x1b".as_slice(),
            b"\x1b[".as_slice(),
            b"\x1b[2".as_slice(),
            b"\x1b[20".as_slice(),
            b"\x1b[200".as_slice(),
            b"\x1b[201".as_slice(),
        ] {
            let mut retained = b"\x1b[200~body".to_vec();
            retained.extend_from_slice(suffix);
            assert_eq!(
                neutralize_timed_out_bracketed_paste(&retained),
                Some(b"body".to_vec()),
                "cut marker suffix {suffix:?} must not escape into the pane"
            );
        }
    }

    #[test]
    fn timed_out_paste_stabilizes_repeated_partial_suffixes() {
        assert_eq!(
            neutralize_timed_out_bracketed_paste(b"\x1b[200~body\x1b[20\x1b["),
            Some(b"body".to_vec())
        );
    }
}
