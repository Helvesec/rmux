//! Scan pane→client byte streams for terminal queries.
//!
//! Counterpart of [`super::terminal_response`].  Where the response
//! scanner sits on the client→pane direction looking for `\x1b[?…c`,
//! this one sits on the pane→client direction looking for `\x1b[c`,
//! `\x1b[6n`, and friends.  Each query found bumps a counter on the
//! attached client; the response scanner consumes that counter when a
//! matching reply lands.  A reply that arrives with the counter at 0
//! is an orphan (pane has changed tenant; usually vim exited before
//! the reply round-tripped back across SSH) and gets dropped.
//!
//! We deliberately under-detect rather than over-detect: any query we
//! miss leaks one orphaned reply at worst, while any query we
//! mis-count would cause us to drop a *legitimate* reply.  So this
//! scanner only matches well-known terminal queries with unambiguous
//! shapes.  When in doubt — don't increment.

/// Count the terminal-capability queries present in `bytes`.
///
/// Queries currently recognised:
///   * `\x1b[c`           — Primary DA (DA1)
///   * `\x1b[0c`          — DA1 with explicit parameter
///   * `\x1b[>c`          — Secondary DA (DA2)
///   * `\x1b[>0c`, `\x1b[>0;0c` — DA2 variants
///   * `\x1b[=c`          — Tertiary DA (DA3)
///   * `\x1b[5n`          — Device status report
///   * `\x1b[6n`          — Cursor position report (DSR-CPR)
///   * `\x1b[?6n`         — CPR with origin-mode bit
///   * `\x1b[14t`, `\x1b[18t`, `\x1b[19t` — Window size queries
///
/// Returns the number of queries seen.  The caller adds this to the
/// per-attach outstanding counter.
#[must_use]
pub(crate) fn count_terminal_queries(bytes: &[u8]) -> u32 {
    let mut count: u32 = 0;
    let mut i = 0;
    while i + 2 < bytes.len() {
        if bytes[i] != 0x1b || bytes[i + 1] != b'[' {
            i += 1;
            continue;
        }
        // bytes[i..] starts with `\x1b[`.  Walk forward over CSI
        // parameters (digits / `;` / leading private markers `?>=`)
        // until the final byte (0x40..=0x7e).
        let mut j = i + 2;
        // Optional private marker.
        if j < bytes.len() && matches!(bytes[j], b'?' | b'>' | b'=') {
            j += 1;
        }
        // Parameter bytes.
        while j < bytes.len() && (bytes[j].is_ascii_digit() || bytes[j] == b';') {
            j += 1;
        }
        if j >= bytes.len() {
            break; // Partial CSI — let it accumulate on next call.
        }
        let final_byte = bytes[j];
        if !(0x40..=0x7e).contains(&final_byte) {
            i += 1;
            continue;
        }
        // We have a complete CSI from i to j (inclusive).
        if is_terminal_query(&bytes[i..=j]) {
            count += 1;
        }
        i = j + 1;
    }
    count
}

fn is_terminal_query(csi: &[u8]) -> bool {
    // csi is `\x1b[<params><final>`.  Strip `\x1b[`.
    let body = &csi[2..];
    let final_byte = *body.last().expect("csi has final byte");
    let params = &body[..body.len() - 1];
    match final_byte {
        // Device attributes (`c`): only with empty or `0` params for DA1,
        // `>` or `>0` or `>0;0` for DA2, `=` for DA3.
        b'c' => matches!(
            params,
            b"" | b"0" | b">" | b">0" | b">0;0" | b"="
        ),
        // Device status report (`n`): only `5n` (status) and `6n` /
        // `?6n` (cursor position).
        b'n' => matches!(params, b"5" | b"6" | b"?6"),
        // Window manipulation (`t`): only the few "report" variants.
        // Setters / resizes share the `t` final but with different
        // parameter shapes, so we whitelist tightly.
        b't' => matches!(params, b"14" | b"18" | b"19"),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::count_terminal_queries;

    #[test]
    fn counts_primary_device_attributes() {
        assert_eq!(count_terminal_queries(b"\x1b[c"), 1);
        assert_eq!(count_terminal_queries(b"\x1b[0c"), 1);
    }

    #[test]
    fn counts_secondary_device_attributes() {
        assert_eq!(count_terminal_queries(b"\x1b[>c"), 1);
        assert_eq!(count_terminal_queries(b"\x1b[>0c"), 1);
        assert_eq!(count_terminal_queries(b"\x1b[>0;0c"), 1);
    }

    #[test]
    fn counts_cursor_position_request() {
        assert_eq!(count_terminal_queries(b"\x1b[6n"), 1);
        assert_eq!(count_terminal_queries(b"\x1b[?6n"), 1);
    }

    #[test]
    fn counts_window_size_request() {
        assert_eq!(count_terminal_queries(b"\x1b[18t"), 1);
        assert_eq!(count_terminal_queries(b"\x1b[14t"), 1);
    }

    #[test]
    fn counts_multiple_queries() {
        // vim's startup typically emits DA1 + DA2 + window-size + CPR.
        let input = b"\x1b[c\x1b[>c\x1b[18t\x1b[6n";
        assert_eq!(count_terminal_queries(input), 4);
    }

    #[test]
    fn ignores_normal_csi_sequences() {
        // Cursor movement, SGR, mode-set — none should count as queries.
        assert_eq!(count_terminal_queries(b"\x1b[2J\x1b[H\x1b[0m"), 0);
        assert_eq!(count_terminal_queries(b"\x1b[?1049h\x1b[?25l"), 0);
    }

    #[test]
    fn ignores_arrow_keys() {
        // Arrow keys are CSI A/B/C/D — not query finals.
        assert_eq!(count_terminal_queries(b"\x1b[A\x1b[B\x1b[C\x1b[D"), 0);
    }

    #[test]
    fn ignores_responses_in_pane_to_client_direction() {
        // Just in case a pane echoed something resembling a reply;
        // a reply parameter shape (`?65;4;6;18;22` for DA1, `12;40`
        // for CPR) should NOT count as a query.
        assert_eq!(count_terminal_queries(b"\x1b[?65;4;6;18;22c"), 0);
        assert_eq!(count_terminal_queries(b"\x1b[12;40R"), 0);
    }

    #[test]
    fn embedded_in_other_output() {
        let mut input = b"some text \x1b[1;32mgreen ".to_vec();
        input.extend_from_slice(b"\x1b[c");
        input.extend_from_slice(b" more text");
        assert_eq!(count_terminal_queries(&input), 1);
    }

    #[test]
    fn partial_csi_at_end_does_not_count() {
        // Unterminated `\x1b[6` — could become `\x1b[6n` next call.
        assert_eq!(count_terminal_queries(b"\x1b[6"), 0);
    }
}
