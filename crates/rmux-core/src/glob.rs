//! Shared tmux-style fnmatch glob matching.

/// Returns whether `text` matches the tmux-style `pattern`.
///
/// Runs in O(pattern x text) time. Every atom other than `*` consumes exactly
/// one character, so when an atom fails only the most recent `*` needs to
/// absorb one more character: an earlier star can never make a match possible
/// that the later one cannot. A recursive search that retried every star
/// independently was exponential in the number of stars, and a `-t` target of
/// a few dozen characters froze the whole server.
#[must_use]
pub fn fnmatch(pattern: &str, text: &str) -> bool {
    let pattern = pattern.chars().collect::<Vec<_>>();
    let text = text.chars().collect::<Vec<_>>();

    let mut pattern_index = 0;
    let mut text_index = 0;
    // Pattern index just past the latest star, and the text index that star
    // currently stops before.
    let mut latest_star: Option<(usize, usize)> = None;

    while text_index < text.len() {
        if pattern.get(pattern_index) == Some(&'*') {
            pattern_index = skip_stars(&pattern, pattern_index);
            latest_star = Some((pattern_index, text_index));
            continue;
        }
        if let Some(next_pattern) = match_single(&pattern, pattern_index, text[text_index]) {
            pattern_index = next_pattern;
            text_index += 1;
            continue;
        }
        let Some((star_pattern, star_text)) = latest_star else {
            return false;
        };
        latest_star = Some((star_pattern, star_text + 1));
        pattern_index = star_pattern;
        text_index = star_text + 1;
    }

    skip_stars(&pattern, pattern_index) == pattern.len()
}

fn skip_stars(pattern: &[char], mut pattern_index: usize) -> usize {
    while pattern.get(pattern_index) == Some(&'*') {
        pattern_index += 1;
    }
    pattern_index
}

/// Matches the one-character atom at `pattern_index` against `value` and
/// returns the pattern index after the atom, or `None` when it does not match
/// (including when the pattern is exhausted).
fn match_single(pattern: &[char], pattern_index: usize, value: char) -> Option<usize> {
    match *pattern.get(pattern_index)? {
        '?' => Some(pattern_index + 1),
        '[' => match bracket_match(pattern, pattern_index, value) {
            Some((matched, next_pattern)) => matched.then_some(next_pattern),
            // An unterminated class is a literal `[`.
            None => (value == '[').then_some(pattern_index + 1),
        },
        '\\' => {
            // A trailing backslash matches itself.
            let literal = pattern.get(pattern_index + 1).copied().unwrap_or('\\');
            let next_pattern = (pattern_index + 2).min(pattern.len());
            (value == literal).then_some(next_pattern)
        }
        literal => (value == literal).then_some(pattern_index + 1),
    }
}

fn bracket_match(pattern: &[char], start: usize, value: char) -> Option<(bool, usize)> {
    let mut index = start + 1;
    if index >= pattern.len() {
        return None;
    }
    let negated = matches!(pattern[index], '!' | '^');
    if negated {
        index += 1;
    }

    let mut matched = false;
    let mut saw_entry = false;
    while index < pattern.len() {
        if pattern[index] == ']' && saw_entry {
            return Some((matched != negated, index + 1));
        }

        let first = pattern[index];
        if index + 2 < pattern.len() && pattern[index + 1] == '-' && pattern[index + 2] != ']' {
            let last = pattern[index + 2];
            if first <= value && value <= last {
                matched = true;
            }
            index += 3;
        } else {
            if first == value {
                matched = true;
            }
            index += 1;
        }
        saw_entry = true;
    }

    None
}

#[cfg(test)]
mod tests {
    use super::fnmatch;

    #[test]
    fn fnmatch_handles_globs_character_classes_and_escapes() {
        assert!(fnmatch("xterm*", "xterm-256color"));
        assert!(fnmatch("foo[0-9]", "foo7"));
        assert!(fnmatch("literal\\*", "literal*"));
    }

    #[test]
    fn fnmatch_rejects_non_matching_values() {
        assert!(!fnmatch("xterm?", "xterm-256color"));
        assert!(!fnmatch("foo[!0-9]", "foo7"));
    }

    #[test]
    fn fnmatch_empty_patterns_and_texts() {
        assert!(fnmatch("", ""));
        assert!(!fnmatch("", "a"));
        assert!(!fnmatch("a", ""));
        assert!(fnmatch("*", ""));
        assert!(fnmatch("*", "anything"));
    }

    #[test]
    fn fnmatch_consecutive_stars_collapse() {
        assert!(fnmatch("**", "abc"));
        assert!(fnmatch("***", ""));
        assert!(fnmatch("a**b", "ab"));
        assert!(fnmatch("a**b", "aXYZb"));
        assert!(!fnmatch("a**b", "aXYZc"));
    }

    #[test]
    fn fnmatch_unterminated_bracket_matches_literal() {
        assert!(fnmatch("[", "["));
        assert!(!fnmatch("[", "a"));
        assert!(fnmatch("[abc", "[abc"));
        assert!(!fnmatch("[abc", "a"));
    }

    #[test]
    fn fnmatch_bracket_with_caret_negation() {
        assert!(fnmatch("[^a]", "b"));
        assert!(!fnmatch("[^a]", "a"));
    }

    #[test]
    fn fnmatch_bracket_closing_bracket_as_first_entry() {
        // `]` as first character in bracket is treated as literal until
        // a subsequent `]` closes the class.
        assert!(fnmatch("[]]", "]"));
    }

    #[test]
    fn fnmatch_trailing_backslash_matches_literal() {
        assert!(fnmatch("abc\\", "abc\\"));
        assert!(!fnmatch("abc\\", "abc"));
    }

    #[test]
    fn fnmatch_question_mark_matches_single_char() {
        assert!(fnmatch("?", "x"));
        assert!(!fnmatch("?", ""));
        assert!(!fnmatch("?", "xy"));
    }

    #[test]
    fn fnmatch_terminal_feature_patterns() {
        // Real patterns from terminal-features defaults.
        assert!(fnmatch("xterm*", "xterm-256color"));
        assert!(fnmatch("xterm*", "xterm-kitty"));
        assert!(fnmatch("screen*", "screen-256color"));
        assert!(fnmatch("rxvt*", "rxvt-unicode-256color"));
        assert!(fnmatch("linux*", "linux"));
        assert!(!fnmatch("xterm*", "tmux-256color"));
    }

    /// R3396 A4: `*a*a...*a*b` against a run of `a` made the recursive matcher
    /// retry every star at every position, O(text^stars). Twenty stars over
    /// forty characters would run for years; the linear matcher answers in
    /// microseconds, so the deadline only has to survive a loaded host.
    #[test]
    fn fnmatch_many_stars_against_a_long_text_finish_quickly() {
        use std::sync::mpsc;
        use std::time::Duration;

        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let text = "a".repeat(40);
            let mut answers = Vec::new();
            for stars in [12, 20, 39, 41] {
                let pattern = format!("{}*b", "*a".repeat(stars));
                answers.push(fnmatch(&pattern, &text));
                answers.push(fnmatch(&pattern, &format!("{text}b")));
            }
            let _ = sender.send(answers);
        });

        let answers = receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("a glob with many stars must not backtrack exponentially");
        assert_eq!(
            answers,
            [false, true, false, true, false, true, false, false]
        );
    }
}
