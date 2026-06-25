use super::regex_cache::cached_regex;
use super::FormatModifier;

pub(super) fn format_fnmatch(pattern: &str, text: &str, fm: &FormatModifier) -> bool {
    let flags = fm.argv.first().map(String::as_str).unwrap_or_default();
    let case_insensitive = flags.contains('i');

    if flags.contains('r') {
        return cached_regex(pattern, case_insensitive).is_ok_and(|regex| regex.is_match(text));
    }
    if case_insensitive {
        glob_match(&pattern.to_lowercase(), &text.to_lowercase())
    } else {
        glob_match(pattern, text)
    }
}

/// Minimal glob matching supporting `*`, `?`, and character classes `[...]`.
fn glob_match(pattern: &str, text: &str) -> bool {
    let pat: Vec<char> = pattern.chars().collect();
    let txt: Vec<char> = text.chars().collect();
    glob_match_impl(&pat, &txt)
}

fn glob_match_impl(pat: &[char], txt: &[char]) -> bool {
    let mut pi = 0;
    let mut ti = 0;
    let mut star_pi = usize::MAX;
    let mut star_ti = 0;

    while ti < txt.len() {
        if pi < pat.len() && (pat[pi] == '?' || pat[pi] == txt[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < pat.len() && pat[pi] == '*' {
            star_pi = pi;
            star_ti = ti;
            pi += 1;
        } else if pi < pat.len() && pat[pi] == '[' {
            if let Some((matches, end)) = match_char_class(&pat[pi..], txt[ti]) {
                if matches {
                    pi += end;
                    ti += 1;
                } else if star_pi != usize::MAX {
                    star_ti += 1;
                    pi = star_pi + 1;
                    ti = star_ti;
                } else {
                    return false;
                }
            } else if star_pi != usize::MAX {
                star_ti += 1;
                pi = star_pi + 1;
                ti = star_ti;
            } else {
                return false;
            }
        } else if star_pi != usize::MAX {
            star_ti += 1;
            pi = star_pi + 1;
            ti = star_ti;
        } else {
            return false;
        }
    }

    while pi < pat.len() && pat[pi] == '*' {
        pi += 1;
    }
    pi == pat.len()
}

/// Matches a character class `[...]` at the start of `pat`, returning
/// `(matches, length_consumed)` or `None` if malformed.
fn match_char_class(pat: &[char], ch: char) -> Option<(bool, usize)> {
    if pat.is_empty() || pat[0] != '[' {
        return None;
    }
    let mut i = 1;
    let negate = i < pat.len() && (pat[i] == '!' || pat[i] == '^');
    if negate {
        i += 1;
    }

    let mut matched = false;
    while i < pat.len() && pat[i] != ']' {
        if i + 2 < pat.len() && pat[i + 1] == '-' {
            if ch >= pat[i] && ch <= pat[i + 2] {
                matched = true;
            }
            i += 3;
        } else {
            if pat[i] == ch {
                matched = true;
            }
            i += 1;
        }
    }

    if i >= pat.len() {
        return None;
    }
    let result = if negate { !matched } else { matched };
    Some((result, i + 1))
}
