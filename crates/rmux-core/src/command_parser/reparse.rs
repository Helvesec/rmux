use super::escape::{argument_needs_double_quotes, escape_argument, escape_double_quoted_argument};

pub(super) fn escape_argument_for_reparse(value: &str) -> String {
    let single_quoted_display =
        value.contains('"') && !value.chars().any(argument_needs_double_quotes);
    let rendered =
        if single_quoted_display && value.chars().any(|ch| ch == '\\' || ch.is_ascii_control()) {
            format!("\"{}\"", escape_double_quoted_argument(value))
        } else {
            escape_argument(value)
        };

    #[cfg(windows)]
    if rendered.contains('\\') {
        // Native Windows config paths allow literal backslashes. Start an
        // ambiguous canonical word with a numeric escape so the lexer keeps
        // interpreting escapes even after decoding a drive or slash prefix.
        // This spelling is also valid tmux syntax; no private marker is needed.
        let mut chars = value.chars();
        let first = chars.next().expect("ambiguous argument is not empty");
        let prefix = if first.is_ascii() {
            format!("\\{:03o}", first as u32)
        } else {
            format!("\\U{:08x}", first as u32)
        };
        return format!(
            "\"{prefix}{}\"",
            escape_double_quoted_argument(chars.as_str())
        );
    }

    rendered
}
