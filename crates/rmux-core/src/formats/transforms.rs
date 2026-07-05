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
    if pattern.is_empty() {
        return value.to_owned();
    }
    if pattern == "^" {
        return value
            .chars()
            .next()
            .map_or_else(String::new, |first| value[first.len_utf8()..].to_owned());
    }
    if pattern == "$" {
        if value.is_empty() {
            return String::new();
        }
        let mut output = String::with_capacity(value.len() + replacement.len());
        output.push_str(value);
        push_tmux_replacement_without_captures(replacement, &mut output);
        return output;
    }
    let case_insensitive = modifier
        .argv
        .get(2)
        .is_some_and(|flags| flags.contains('i'));

    match cached_regex(pattern, case_insensitive) {
        Ok(regex) => substitute_regex(value, &regex, replacement),
        Err(_) => value.replace(pattern, replacement),
    }
}

fn substitute_regex(value: &str, regex: &regex::Regex, replacement: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut offset = 0;

    while offset < value.len() {
        if !value.is_char_boundary(offset) {
            let byte = value.as_bytes()[offset];
            push_tmux_byte(byte, &mut output);
            push_tmux_replacement_without_captures(replacement, &mut output);
            offset += 1;
            continue;
        }

        let suffix = &value[offset..];
        let Some(suffix_captures) = regex.captures(suffix) else {
            output.push_str(&value[offset..]);
            return output;
        };
        let Some(suffix_match) = suffix_captures.get(0) else {
            output.push_str(&value[offset..]);
            return output;
        };

        if suffix_match.is_empty() {
            let match_start = offset + suffix_match.start();
            output.push_str(&value[offset..match_start]);

            if match_start >= value.len() {
                push_tmux_replacement(&suffix_captures, replacement, &mut output);
                offset = value.len();
                continue;
            }
            let byte = value.as_bytes()[match_start];
            push_tmux_byte(byte, &mut output);
            let next_offset = match_start + 1;
            if !has_non_empty_match_at(regex, value, next_offset) {
                push_tmux_replacement(&suffix_captures, replacement, &mut output);
            }
            offset = next_offset;
            continue;
        }

        let Some(captures) = regex.captures_at(value, offset) else {
            output.push_str(suffix);
            return output;
        };
        let Some(match_) = captures.get(0) else {
            output.push_str(suffix);
            return output;
        };

        let match_start = match_.start();
        let match_end = match_.end();
        output.push_str(&value[offset..match_start]);

        push_tmux_replacement(&captures, replacement, &mut output);
        offset = match_end;
    }

    output
}

fn has_non_empty_match_at(regex: &regex::Regex, value: &str, offset: usize) -> bool {
    if offset >= value.len() {
        return false;
    }
    if !value.is_char_boundary(offset) {
        return false;
    }

    regex
        .captures(&value[offset..])
        .and_then(|captures| captures.get(0))
        .is_some_and(|match_| match_.start() == 0 && !match_.is_empty())
}

fn push_tmux_replacement(captures: &regex::Captures<'_>, replacement: &str, output: &mut String) {
    let mut chars = replacement.chars().peekable();

    while let Some(character) = chars.next() {
        match character {
            '\\' if chars.peek().is_some_and(char::is_ascii_digit) => {
                let digit = chars.next().expect("peeked digit exists");
                let capture_index = digit.to_digit(10).expect("ascii digit") as usize;
                match captures.get(capture_index) {
                    Some(capture) if !capture.as_str().is_empty() => {
                        output.push_str(capture.as_str())
                    }
                    _ => output.push(digit),
                }
            }
            _ => output.push(character),
        }
    }
}

fn push_tmux_replacement_without_captures(replacement: &str, output: &mut String) {
    let mut chars = replacement.chars().peekable();

    while let Some(character) = chars.next() {
        match character {
            '\\' if chars.peek().is_some_and(char::is_ascii_digit) => {
                output.push(chars.next().expect("peeked digit exists"));
            }
            _ => output.push(character),
        }
    }
}

fn push_tmux_byte(byte: u8, output: &mut String) {
    if byte.is_ascii() {
        output.push(char::from(byte));
    } else {
        output.push('\\');
        output.push_str(&format!("{byte:03o}"));
    }
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
