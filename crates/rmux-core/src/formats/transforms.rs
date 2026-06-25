use crate::utf8::{text_width, truncate_right_to_width, truncate_to_width, Utf8Config};

use super::regex_cache::cached_regex;
use super::FormatModifier;

/// Shell quoting: backslash-escapes tmux shell special characters.
pub(super) fn shell_quote(s: &str) -> String {
    const SHELL_SPECIALS: &[u8] = b"|&;<>()$`\\\"'*?[# =%";

    let mut out = String::with_capacity(s.len() * 2);
    for ch in s.chars() {
        if ch.is_ascii() && SHELL_SPECIALS.contains(&(ch as u8)) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// Style quoting: escapes `#` as `##`.
pub(super) fn style_quote(s: &str) -> String {
    s.replace('#', "##")
}

pub(super) fn format_unescape(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut brackets = 0_i32;
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'#' && bytes.get(i + 1) == Some(&b'{') {
            brackets += 1;
        }
        if brackets == 0
            && bytes[i] == b'#'
            && i + 1 < bytes.len()
            && b",#{}:".contains(&bytes[i + 1])
        {
            out.push(bytes[i + 1] as char);
            i += 2;
            continue;
        }
        if bytes[i] == b'}' {
            brackets -= 1;
        }

        let ch = s[i..]
            .chars()
            .next()
            .expect("format_unescape index must be at a character boundary");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

pub(super) fn apply_substitution(value: &str, modifier: &FormatModifier) -> String {
    let Some(pattern) = modifier.argv.first() else {
        return value.to_owned();
    };
    let Some(replacement) = modifier.argv.get(1) else {
        return value.to_owned();
    };
    let case_insensitive = modifier
        .argv
        .get(2)
        .is_some_and(|flags| flags.contains('i'));

    match cached_regex(pattern, case_insensitive) {
        Ok(regex) => regex
            .replace_all(value, tmux_regex_replacement(replacement).as_str())
            .into_owned(),
        Err(_) => value.replace(pattern, replacement),
    }
}

fn tmux_regex_replacement(replacement: &str) -> String {
    let mut translated = String::with_capacity(replacement.len());
    let mut chars = replacement.chars().peekable();

    while let Some(character) = chars.next() {
        match character {
            '\\' if chars.peek().is_some_and(char::is_ascii_digit) => {
                translated.push_str("${");
                translated.push(chars.next().expect("peeked digit exists"));
                translated.push('}');
            }
            '$' => translated.push_str("$$"),
            _ => translated.push(character),
        }
    }

    translated
}

pub(super) fn truncate_left(s: &str, max: usize) -> String {
    let config = Utf8Config::default();
    if text_width(s, &config) <= max {
        s.to_owned()
    } else {
        truncate_to_width(s, max, &config)
    }
}

pub(super) fn truncate_right(s: &str, max: usize) -> String {
    let config = Utf8Config::default();
    if text_width(s, &config) <= max {
        return s.to_owned();
    }

    truncate_right_to_width(s, max, &config)
}
