use std::borrow::Cow;

use crate::style::{style_parse, Style};
use crate::utf8::{text_width, truncate_right_to_width, truncate_to_width, Utf8Config};

use super::scan::format_skip_delimiter;

/// Return the display-cell width of expanded format text, excluding embedded
/// `#[...]` style clauses.
#[must_use]
pub fn styled_text_width(value: &str, utf8: &Utf8Config) -> usize {
    text_width(&visible_text(&inline_tokens(value)), utf8)
}

/// Keep the leftmost display cells of expanded format text while preserving
/// the style and range clauses needed to render them.
#[must_use]
pub fn truncate_styled_text_to_width(value: &str, max_width: usize, utf8: &Utf8Config) -> String {
    if value.is_empty() || max_width == 0 {
        return String::new();
    }

    let tokens = inline_tokens(value);
    let visible = visible_text(&tokens);
    let clipped = truncate_to_width(&visible, max_width, utf8);
    if clipped.len() == visible.len() {
        return value.to_owned();
    }

    let mut remaining = clipped.len();
    let mut output = String::with_capacity(value.len().min(remaining + 32));
    for token in tokens {
        if token.visible.is_empty() {
            if remaining > 0 {
                output.push_str(token.source);
            }
            continue;
        }

        if remaining >= token.visible.len() {
            output.push_str(token.source);
            remaining -= token.visible.len();
            continue;
        }

        push_partial_visible(&mut output, &token, 0, remaining);
        break;
    }
    output
}

/// Keep the rightmost display cells of expanded format text. Style clauses
/// before the retained suffix are preserved so its first cell keeps the style
/// it had in the untruncated value, matching tmux's format modifiers.
#[must_use]
pub(super) fn truncate_styled_text_right_to_width(
    value: &str,
    max_width: usize,
    utf8: &Utf8Config,
) -> String {
    if value.is_empty() || max_width == 0 {
        return String::new();
    }

    let tokens = inline_tokens(value);
    let visible = visible_text(&tokens);
    let clipped = truncate_right_to_width(&visible, max_width, utf8);
    if clipped.len() == visible.len() {
        return value.to_owned();
    }

    let mut skip = visible.len().saturating_sub(clipped.len());
    let mut output = String::with_capacity(value.len().min(clipped.len() + 32));
    for token in tokens {
        if token.visible.is_empty() {
            output.push_str(token.source);
            continue;
        }

        if skip >= token.visible.len() {
            skip -= token.visible.len();
            continue;
        }

        if skip > 0 {
            push_partial_visible(&mut output, &token, skip, token.visible.len());
            skip = 0;
        } else {
            output.push_str(token.source);
        }
    }
    output
}

fn visible_text(tokens: &[InlineToken<'_>]) -> String {
    let capacity = tokens.iter().map(|token| token.visible.len()).sum();
    let mut visible = String::with_capacity(capacity);
    for token in tokens {
        visible.push_str(token.visible.as_ref());
    }
    visible
}

fn push_partial_visible(output: &mut String, token: &InlineToken<'_>, start: usize, end: usize) {
    let partial = &token.visible[start..end];
    if matches!(token.visible, Cow::Borrowed(_)) {
        output.push_str(partial);
    } else {
        // Owned token text came from tmux hash-doubling. Escape each retained
        // hash again so the later format-draw pass renders it literally.
        output.push_str(&partial.replace('#', "##"));
    }
}

#[derive(Debug)]
struct InlineToken<'a> {
    source: &'a str,
    visible: Cow<'a, str>,
}

fn inline_tokens(expanded: &str) -> Vec<InlineToken<'_>> {
    let bytes = expanded.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0_usize;
    let mut style = Style::default();
    let default = style.cell;

    while index < bytes.len() {
        // Match format_draw's hash-doubling rules. Odd runs before `[` leave
        // the final `#[` for the style-clause path; even runs render `#[`.
        if bytes[index] == b'#' && index + 1 < bytes.len() && bytes[index + 1] != b'[' {
            let mut count = 1_usize;
            while index + count < bytes.len() && bytes[index + count] == b'#' {
                count += 1;
            }

            let followed_by_bracket = bytes.get(index + count).copied() == Some(b'[');
            let (consumed, mut visible) = if followed_by_bracket && count.is_multiple_of(2) {
                let mut visible = "#".repeat(count / 2);
                visible.push('[');
                (count + 1, visible)
            } else if followed_by_bracket {
                (count - 1, "#".repeat(count / 2))
            } else {
                (count, "#".repeat(count.div_ceil(2)))
            };

            if style.ignore {
                visible.clear();
            }
            tokens.push(InlineToken {
                source: &expanded[index..index + consumed],
                visible: Cow::Owned(visible),
            });
            index += consumed;
            continue;
        }

        if bytes[index] == b'#' && bytes.get(index + 1).copied() == Some(b'[') && !style.ignore {
            let Some(offset) = format_skip_delimiter(&expanded[index + 2..], b"]") else {
                // format_draw stops at an unterminated style clause.
                break;
            };
            let end = index + 2 + offset;
            let source = &expanded[index..=end];
            let clause = &expanded[index + 2..end];
            let _ = style_parse(&mut style, &default, clause);
            tokens.push(InlineToken {
                source,
                visible: Cow::Borrowed(""),
            });
            index = end + 1;
            continue;
        }

        let start = index;
        while index < bytes.len() && bytes[index] != b'#' {
            let Some(character) = expanded[index..].chars().next() else {
                break;
            };
            index += character.len_utf8();
        }
        if start == index {
            let character = expanded[index..]
                .chars()
                .next()
                .expect("index is inside expanded text");
            index += character.len_utf8();
        }
        let source = &expanded[start..index];
        tokens.push(InlineToken {
            source,
            visible: Cow::Borrowed(source),
        });
    }

    tokens
}

#[cfg(test)]
mod tests {
    use super::{
        styled_text_width, truncate_styled_text_right_to_width, truncate_styled_text_to_width,
    };
    use crate::Utf8Config;

    #[test]
    fn width_ignores_style_and_range_clauses() {
        assert_eq!(
            styled_text_width(
                "#[fg=red]AB#[range=control|7]CD#[norange]",
                &Utf8Config::default(),
            ),
            4
        );
    }

    #[test]
    fn nested_format_syntax_inside_a_style_clause_stays_zero_width() {
        let value = "#[fg=#{?client_prefix,red,blue},bold]ABCD";

        assert_eq!(styled_text_width(value, &Utf8Config::default()), 4);
        assert_eq!(
            truncate_styled_text_to_width(value, 2, &Utf8Config::default()),
            "#[fg=#{?client_prefix,red,blue},bold]AB"
        );
    }

    #[test]
    fn left_truncation_preserves_clauses_and_unicode_cells() {
        assert_eq!(
            truncate_styled_text_to_width(
                "#[fg=red]表A#[range=control|7]👋🏽B",
                5,
                &Utf8Config::default(),
            ),
            "#[fg=red]表A#[range=control|7]👋🏽"
        );
    }

    #[test]
    fn right_truncation_preserves_style_before_the_retained_suffix() {
        assert_eq!(
            truncate_styled_text_right_to_width(
                "#[fg=red]AB#[fg=blue]CDEF",
                3,
                &Utf8Config::default(),
            ),
            "#[fg=red]#[fg=blue]DEF"
        );
    }

    #[test]
    fn hash_doubling_is_measured_as_rendered_text() {
        assert_eq!(
            truncate_styled_text_to_width("##[ABCD", 3, &Utf8Config::default()),
            "##[A"
        );
        assert_eq!(
            truncate_styled_text_right_to_width("####[AB", 4, &Utf8Config::default()),
            "##[AB"
        );
    }
}
