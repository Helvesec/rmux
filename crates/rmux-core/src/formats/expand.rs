use std::rc::Rc;

use super::time::expand_time_tokens;
use super::{
    format_replace, format_skip, resolve_variable, ExpandBudget, ExpandState, FormatVariables,
};

/// Recursion depth limit matching tmux `FORMAT_LOOP_LIMIT`.
pub(super) const FORMAT_LOOP_LIMIT: u32 = 100;

/// Renders status text with NUL standing for a literal hash supplied by OSC 7501.
#[must_use]
pub fn render_status_template<V: FormatVariables + ?Sized>(
    template: &str,
    variables: &V,
) -> String {
    render_output(template, variables, true)
}

pub(super) fn render_output<V: FormatVariables + ?Sized>(
    template: &str,
    variables: &V,
    status: bool,
) -> String {
    let mut state = ExpandState {
        status,
        loop_depth: 0,
        expand_time: false,
        stop_expansion: false,
        preserve_jobs: false,
        disable_jobs: false,
        budget: Rc::new(ExpandBudget::start()),
    };
    format_expand1(&mut state, template, variables)
}

/// The main expansion loop, matching tmux `format_expand1`.
pub(super) fn format_expand1<V>(state: &mut ExpandState, fmt: &str, variables: &V) -> String
where
    V: FormatVariables + ?Sized,
{
    if fmt.is_empty() || state.budget.is_exhausted() {
        return String::new();
    }
    state.budget.charge(fmt.len());

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

    let status = state.status && !state.disable_jobs;
    let bytes = fmt.as_bytes();
    let mut out = String::with_capacity(fmt.len());
    let mut i = 0;
    let mut style_end = None;

    while i < bytes.len() {
        if state.stop_expansion {
            break;
        }
        if bytes[i] != b'#' {
            let start = i;
            while i < bytes.len() && bytes[i] != b'#' {
                i += 1;
            }
            push_raw(&mut out, &fmt[start..i], status);
            continue;
        }

        // We have a `#`. Peek at next char.
        i += 1;
        if i >= bytes.len() {
            // tmux stops at an incomplete final marker (format_expand1).
            // Escaped pairs have already emitted their literal hash.
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
                    if state.preserve_jobs {
                        out.push_str(&fmt[start - 2..=i]);
                    } else if !state.disable_jobs {
                        // Keep the caller's recursion guard and current loop item.
                        // A job command cannot recursively launch another job.
                        let expand_time = state.expand_time;
                        state.expand_time = false;
                        state.disable_jobs = true;
                        let source = &fmt[start..i];
                        let command = format_expand1(state, source, variables);
                        state.disable_jobs = false;
                        state.expand_time = expand_time;
                        if !state.stop_expansion {
                            push_raw(
                                &mut out,
                                &variables.format_job(source, &command).unwrap_or_default(),
                                status,
                            );
                        }
                    }
                    i += 1; // skip `)`
                } else {
                    // No matching `)` — break out of loop (tmux behavior).
                    break;
                }
            }
            b'{' => {
                // tmux `format_skip1` finds no end once the time budget is
                // spent, and `format_expand1` stops scanning there.
                if state.budget.is_exhausted() {
                    break;
                }
                // `#{...}` — format expression.
                // Use format_skip to find matching `}`, starting from the `#`.
                let skip_start = i - 2; // points at `#`
                let skip_bytes = &bytes[skip_start..];
                match format_skip(skip_bytes, b"}") {
                    Some(off) => {
                        let key_start = i; // first char after `{`
                        let key_end = skip_start + off;
                        let key = &fmt[key_start..key_end];
                        let display_field = status && style_end.is_none_or(|end| skip_start > end);
                        let previous = state.status;
                        state.status = display_field;
                        let result = format_replace(state, key, variables);
                        let forwarded = std::mem::replace(&mut state.status, previous);
                        if state.stop_expansion {
                            break;
                        }
                        push_raw(&mut out, &result, status && !forwarded);
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
                    style_end = format_skip(skip_bytes, b"]").map(|off| style_start + off);
                    out.push_str(&fmt[style_start..=i]);
                    i += 1;
                    continue;
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
                // Preserve style syntax while expanding expressions in its
                // current context. Single-character aliases remain literal.
                let skip_start = i - 2; // points at `#`
                let skip_bytes = &bytes[skip_start..];
                style_end = format_skip(skip_bytes, b"]").map(|off| skip_start + off);
                out.push_str("#[");
            }
            b'}' | b',' => {
                // `#}` -> `}`, `#,` -> `,`
                out.push(ch as char);
            }
            _ => {
                if let Some(alias) =
                    single_char_alias(ch).filter(|_| style_end.is_none_or(|end| i - 1 > end))
                {
                    push_raw(&mut out, &resolve_variable(alias, variables), status);
                } else {
                    out.push('#');
                    // `ch` was the byte right after `#` and `i` is already past
                    // it. If ch is ASCII, push it directly. If it starts a
                    // multi-byte UTF-8 sequence, copy the whole character.
                    if ch < 0x80 {
                        out.push(if status && ch == 0 {
                            '\x01'
                        } else {
                            ch as char
                        });
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

/// Mark reported text before any modifier transforms it.
pub(super) fn status_field<V: FormatVariables + ?Sized>(
    status: bool,
    body: &str,
    variables: &V,
) -> Option<String> {
    if !status
        || !matches!(
            body,
            "pane_program_status_title" | "pane_program_status_message"
        )
    {
        return None;
    }
    let (text, reported) = variables.format_status_field(body);
    reported.then(|| text.replace('#', "\0"))
}

fn push_raw(out: &mut String, text: &str, status: bool) {
    out.push_str(&raw_text(text, status));
}

pub(super) fn raw_text(text: &str, status: bool) -> std::borrow::Cow<'_, str> {
    // Keep a genuine NUL nonprinting when the status renderer reserves it for '#'.
    if status && text.contains('\0') {
        text.replace('\0', "\x01").into()
    } else {
        text.into()
    }
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
