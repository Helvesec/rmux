use super::{format_expand1, format_skip, ExpandState, FormatVariables};

/// A parsed format modifier.
#[derive(Debug)]
pub(super) struct FormatModifier {
    /// The modifier string, e.g. `"l"`, `"=="`, `"||"`.
    pub(super) modifier: String,
    /// Arguments to the modifier (already expanded).
    pub(super) argv: Vec<String>,
}

/// Single-char modifiers that take no arguments when followed by `;` or `:`.
const SINGLE_NO_ARG: &[u8] = b"labcdnwETSWPL<>";

/// Double-char modifiers that take no arguments.
const DOUBLE_NO_ARG: &[&str] = &["||", "&&", "!=", "==", "<=", ">="];

/// Single-char modifiers that may take arguments.
const SINGLE_WITH_ARG: &[u8] = b"mCLNPSst=pReqW";

fn is_modifier_end(ch: u8) -> bool {
    ch == b';' || ch == b':'
}

/// Parses the modifier chain from `body`, returning the modifiers and the
/// remaining body after the `:` separator. If no valid modifier chain is found,
/// returns an empty modifier list and the original body.
pub(super) fn parse_modifiers<'a, V>(
    state: &mut ExpandState,
    body: &'a str,
    variables: &V,
) -> (Vec<FormatModifier>, &'a str)
where
    V: FormatVariables + ?Sized,
{
    let bytes = body.as_bytes();
    let mut modifiers = Vec::new();
    let mut i = 0;

    while i < bytes.len() && bytes[i] != b':' {
        // Skip separator.
        if bytes[i] == b';' {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }

        // Single-char modifier with no arguments.
        if SINGLE_NO_ARG.contains(&bytes[i]) && i + 1 < bytes.len() && is_modifier_end(bytes[i + 1])
        {
            modifiers.push(FormatModifier {
                modifier: String::from(bytes[i] as char),
                argv: Vec::new(),
            });
            i += 1;
            continue;
        }

        // Double-char modifier with no arguments.
        if i + 2 <= bytes.len() {
            let pair = &body[i..i + 2.min(bytes.len() - i)];
            if pair.len() == 2
                && DOUBLE_NO_ARG.contains(&pair)
                && i + 2 < bytes.len()
                && is_modifier_end(bytes[i + 2])
            {
                modifiers.push(FormatModifier {
                    modifier: pair.to_owned(),
                    argv: Vec::new(),
                });
                i += 2;
                continue;
            }
        }

        // Single-char with arguments.
        if !SINGLE_WITH_ARG.contains(&bytes[i]) {
            break;
        }
        let c = bytes[i] as char;

        // No arguments provided (followed by end).
        if i + 1 < bytes.len() && is_modifier_end(bytes[i + 1]) {
            modifiers.push(FormatModifier {
                modifier: String::from(c),
                argv: Vec::new(),
            });
            i += 1;
            continue;
        }
        if i + 1 >= bytes.len() {
            break;
        }

        // Single argument with no wrapper character.
        // If char after modifier is not punctuation, or is '-', use bare form.
        let next = bytes[i + 1];
        if !next.is_ascii_punctuation() || next == b'-' {
            let rest = &bytes[i + 1..];
            if let Some(end_off) = format_skip(rest, b":;") {
                let raw = &body[i + 1..i + 1 + end_off];
                let expanded = format_expand1(state, raw, variables);
                modifiers.push(FormatModifier {
                    modifier: String::from(c),
                    argv: vec![expanded],
                });
                i = i + 1 + end_off;
                continue;
            }
            break;
        }

        // Multiple arguments with a wrapper character.
        let wrapper = next;
        let mut argv = Vec::new();
        let mut j = i + 1; // points at wrapper char
        loop {
            // Check for empty wrapper at end: wrapper followed by `;` or `:`.
            if j < bytes.len()
                && bytes[j] == wrapper
                && j + 1 < bytes.len()
                && is_modifier_end(bytes[j + 1])
            {
                j += 1;
                break;
            }

            // Find next occurrence of wrapper.
            let rest = &bytes[j + 1..];
            let end_chars = [wrapper, b';', b':'];
            let end_off = match format_skip(rest, &end_chars) {
                Some(off) => off,
                None => break,
            };
            let raw = &body[j + 1..j + 1 + end_off];
            let expanded = format_expand1(state, raw, variables);
            argv.push(expanded);

            j = j + 1 + end_off;

            if j >= bytes.len() || is_modifier_end(bytes[j]) {
                break;
            }
        }
        modifiers.push(FormatModifier {
            modifier: String::from(c),
            argv,
        });
        i = j;
        continue;
    }

    if i < bytes.len() && bytes[i] == b':' {
        (modifiers, &body[i + 1..])
    } else {
        // No `:` found — no valid modifier chain.
        (Vec::new(), body)
    }
}
