use super::time::expand_time_tokens;
use super::{format_replace, format_skip, resolve_variable, ExpandState, FormatVariables};

/// Recursion depth limit matching tmux `FORMAT_LOOP_LIMIT`.
pub(super) const FORMAT_LOOP_LIMIT: u32 = 100;

/// The main expansion loop, matching tmux `format_expand1`.
pub(super) fn format_expand1<V>(state: &mut ExpandState, fmt: &str, variables: &V) -> String
where
    V: FormatVariables + ?Sized,
{
    if fmt.is_empty() {
        return String::new();
    }

    if state.stop_expansion || state.loop_depth >= FORMAT_LOOP_LIMIT {
        return String::new();
    }
    state.loop_depth += 1;

    let expanded_time;
    let fmt = if state.expand_time && fmt.contains('%') {
        expanded_time = expand_time_tokens(fmt);
        expanded_time.as_str()
    } else {
        fmt
    };

    let bytes = fmt.as_bytes();
    let mut out = String::with_capacity(fmt.len());
    let mut i = 0;

    while i < bytes.len() {
        if state.stop_expansion {
            break;
        }
        if bytes[i] != b'#' {
            let start = i;
            while i < bytes.len() && bytes[i] != b'#' {
                i += 1;
            }
            out.push_str(&fmt[start..i]);
            continue;
        }

        // We have a `#`. Peek at next char.
        i += 1;
        if i >= bytes.len() {
            // Keep trailing `#` literal. tmux drops it, but rmux treats a bare
            // marker as text unless it starts a complete expansion.
            out.push('#');
            break;
        }

        let ch = bytes[i];
        i += 1;

        match ch {
            b'(' => {
                // `#(cmd)` — job expansion. Find matching `)`.
                let mut depth = 1;
                let start = i;
                while i < bytes.len() {
                    if bytes[i] == b'(' {
                        depth += 1;
                    } else if bytes[i] == b')' {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    i += 1;
                }
                if i < bytes.len() {
                    // Found matching `)`. Job expansion is not run in this
                    // path, but status rendering can preserve it for a later
                    // runtime execution pass.
                    if state.preserve_jobs {
                        out.push_str(&fmt[start - 2..=i]);
                    }
                    i += 1; // skip `)`
                } else {
                    // No matching `)` — break out of loop (tmux behavior).
                    break;
                }
            }
            b'{' => {
                // `#{...}` — format expression.
                // Use format_skip to find matching `}`, starting from the `#`.
                let skip_start = i - 2; // points at `#`
                let skip_bytes = &bytes[skip_start..];
                match format_skip(skip_bytes, b"}") {
                    Some(off) => {
                        let key_start = i; // first char after `{`
                        let key_end = skip_start + off;
                        let key = &fmt[key_start..key_end];
                        let result = format_replace(state, key, variables);
                        if state.stop_expansion {
                            break;
                        }
                        out.push_str(&result);
                        i = key_end + 1; // skip past `}`
                    }
                    None => {
                        // No matching `}` — tmux breaks out of the while loop.
                        break;
                    }
                }
            }
            b'#' => {
                // `##` — check if followed by more `#` and then `[` for styles.
                // For now, `##` produces literal `#`. Style pass-through is
                // handled below.
                let hash_start = i - 2; // first `#`
                let _ = hash_start;

                // Count consecutive `#` chars including the ones we consumed.
                // We already consumed `##` (two hashes).
                let mut n = 2;
                while i < bytes.len() && bytes[i] == b'#' {
                    n += 1;
                    i += 1;
                }
                if i < bytes.len() && bytes[i] == b'[' {
                    let style_start = i - n;
                    let skip_bytes = &bytes[style_start..];
                    if let Some(off) = format_skip(skip_bytes, b"]") {
                        let end = style_start + off + 1;
                        out.push_str(&fmt[style_start..end]);
                        i = end;
                        continue;
                    }
                }
                // Plain `##...` — output literal `#` characters.
                // `##` = `#`, `###` = `##`, `####` = `##`, etc.
                // tmux: falls through to the `}`, `,` case which outputs
                // one `#` for the second `#`, then the remaining chars
                // are re-processed. Let me match tmux exactly.
                //
                // In tmux, `##` case: the second `#` is `ch`, then it
                // checks if there are more `#`s followed by `[`.
                // If not, it falls through to the `,`/`}`/`#` handler
                // which outputs `ch` (which is `#`).
                //
                // For `###`: first pair `##` produces `#`. Then the third
                // `#` is the new start of the while loop, processed as
                // another `#` prefix.
                //
                // To match: we consumed 2 `#`s minimum. If we consumed
                // extra `#`s above (for the style check), we need to put
                // them back. Since `##` outputs one `#`, and additional
                // `#`s were consumed speculatively, we need to re-process.
                //
                // Simplest correct approach: output one `#`, and rewind
                // `i` to after the second `#`.
                i -= n - 2; // rewind extra `#`s
                out.push('#');
            }
            b'[' => {
                // `#[...]` — style sequence. Pass through for format_draw.
                // Find matching `]` using format_skip.
                let skip_start = i - 2; // points at `#`
                let skip_bytes = &bytes[skip_start..];
                if let Some(off) = format_skip(skip_bytes, b"]") {
                    // Output `#[` through `]`.
                    let end = skip_start + off + 1;
                    out.push_str(&fmt[skip_start..end]);
                    i = end;
                } else {
                    // No matching `]` — output `#[` and continue.
                    out.push('#');
                    out.push('[');
                }
            }
            b'}' | b',' => {
                // `#}` -> `}`, `#,` -> `,`
                out.push(ch as char);
            }
            _ => {
                if let Some(alias) = single_char_alias(ch) {
                    out.push_str(&resolve_variable(alias, variables));
                } else {
                    out.push('#');
                    // `ch` was the byte right after `#` and `i` is already past
                    // it. If ch is ASCII, push it directly. If it starts a
                    // multi-byte UTF-8 sequence, copy the whole character.
                    if ch < 0x80 {
                        out.push(ch as char);
                    } else {
                        let start = i - 1;
                        let char_len = if ch & 0xE0 == 0xC0 {
                            2
                        } else if ch & 0xF0 == 0xE0 {
                            3
                        } else {
                            4
                        };
                        let end = (start + char_len).min(bytes.len());
                        out.push_str(&fmt[start..end]);
                        i = end;
                    }
                }
            }
        }
    }

    state.loop_depth -= 1;
    out
}

fn single_char_alias(ch: u8) -> Option<&'static str> {
    Some(match ch {
        b'D' => "pane_id",
        b'F' => "window_flags",
        b'H' => "host",
        b'I' => "window_index",
        b'P' => "pane_index",
        b'S' => "session_name",
        b'T' => "pane_title",
        b'W' => "window_name",
        b'h' => "host_short",
        _ => return None,
    })
}
