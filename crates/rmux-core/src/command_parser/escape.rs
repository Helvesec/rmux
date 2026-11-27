//! Rendering of command arguments back into tmux command syntax, choosing
//! unquoted, single-quoted or double-quoted spelling and escaping as tmux does.

pub(crate) fn escape_argument(value: &str) -> String {
    if value.is_empty() {
        return "''".to_owned();
    }
    if is_single_char_escaped_argument(value) {
        return escape_unquoted_argument(value);
    }
    if !value.chars().any(argument_needs_double_quotes) {
        if value.contains('"') {
            return format!("'{}'", escape_single_quoted_argument(value));
        }
        return escape_unquoted_argument(value);
    }

    format!("\"{}\"", escape_double_quoted_argument(value))
}

fn is_single_char_escaped_argument(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(ch) = chars.next() else {
        return false;
    };
    chars.next().is_none() && matches!(ch, ';' | '{' | '}' | '\'' | '"' | '#' | '$' | '%')
}

pub(super) fn argument_needs_double_quotes(ch: char) -> bool {
    matches!(ch, ' ' | ';' | '{' | '}' | '\'' | '#' | '$' | '%')
        || (ch.is_whitespace() && !matches!(ch, '\n' | '\r' | '\t'))
}

fn escape_unquoted_argument(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for (index, ch) in value.chars().enumerate() {
        match ch {
            '~' if index == 0 => escaped.push_str(r"\~"),
            ';' | '{' | '}' | '\'' | '"' | '#' | '$' | '%' => {
                escaped.push('\\');
                escaped.push(ch);
            }
            '\n' => escaped.push_str(r"\n"),
            '\r' => escaped.push_str(r"\r"),
            '\t' => escaped.push_str(r"\t"),
            '\\' => escaped.push_str(r"\\"),
            _ => escape_control_argument_char(&mut escaped, ch),
        }
    }
    escaped
}

fn escape_single_quoted_argument(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\n' => escaped.push_str(r"\n"),
            '\r' => escaped.push_str(r"\r"),
            '\t' => escaped.push_str(r"\t"),
            '\\' => escaped.push_str(r"\\"),
            _ => escape_control_argument_char(&mut escaped, ch),
        }
    }
    escaped
}

pub(super) fn escape_double_quoted_argument(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    let mut chars = value.chars().enumerate().peekable();
    while let Some((index, ch)) = chars.next() {
        match ch {
            '~' if index == 0 => escaped.push_str(r"\~"),
            '\n' => escaped.push_str(r"\n"),
            '\r' => escaped.push_str(r"\r"),
            '\t' => escaped.push_str(r"\t"),
            '\u{7}' => escaped.push_str(r"\a"),
            '\u{8}' => escaped.push_str(r"\b"),
            '\u{b}' => escaped.push_str(r"\v"),
            '\u{c}' => escaped.push_str(r"\f"),
            '\u{1b}' => escaped.push_str(r"\033"),
            '$' if chars
                .peek()
                .is_some_and(|(_, next)| dollar_starts_variable(*next)) =>
            {
                escaped.push_str(r"\$")
            }
            '\\' | '"' => {
                escaped.push('\\');
                escaped.push(ch);
            }
            _ => escape_control_argument_char(&mut escaped, ch),
        }
    }
    escaped
}

fn escape_control_argument_char(escaped: &mut String, ch: char) {
    match ch {
        '\u{7}' => escaped.push_str(r"\a"),
        '\u{8}' => escaped.push_str(r"\b"),
        '\u{b}' => escaped.push_str(r"\v"),
        '\u{c}' => escaped.push_str(r"\f"),
        '\u{1b}' => escaped.push_str(r"\033"),
        '\0'..='\u{1f}' | '\u{7f}' => {
            escaped.push('\\');
            escaped.push_str(&format!("{:03o}", ch as u32));
        }
        _ => escaped.push(ch),
    }
}

fn dollar_starts_variable(ch: char) -> bool {
    ch == '{' || ch == '_' || ch.is_ascii_alphabetic()
}
