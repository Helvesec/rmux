use super::*;

#[test]
fn format_skip_basic_delimiter() {
    assert_eq!(format_skip(b"hello,world", b","), Some(5));
    // Bare `}` at depth 0 decrements brackets to -1, so it's not found.
    // This matches tmux behavior — `format_skip` for `}` is always called
    // starting from `#{` where brackets is already 1.
    assert_eq!(format_skip(b"hello}", b"}"), None);
    // With the `#{` prefix to set brackets=1:
    assert_eq!(format_skip(b"#{hello}", b"}"), Some(7));
    assert_eq!(format_skip(b"no-match", b","), None);
}

#[test]
fn format_skip_nested_brackets() {
    // `#{a,b},after` — the `,` inside `#{...}` is at depth 1 (skipped).
    // The `,` after `}` is at depth 0, position 6.
    assert_eq!(format_skip(b"#{a,b},after", b","), Some(6));
}

#[test]
fn format_skip_escape_sequences() {
    // `#,` escapes the first comma. The second `,` at position 2 is the delimiter.
    assert_eq!(format_skip(b"#,,real", b","), Some(2));
    // `#}` escapes the brace — but the second `}` still decrements,
    // making brackets=-1, so it's not found.
    assert_eq!(format_skip(b"#},real}", b"}"), None);
    // With `#{` prefix: brackets goes 0→1 at `#{`, escape skips inner `#}`,
    // then `,` at depth 1 not found for `}`, then `}` decrements to 0 → found.
    assert_eq!(format_skip(b"#{#},real}", b"}"), Some(9));
    // `##` escapes the hash.
    assert_eq!(format_skip(b"##,real", b","), Some(2));
    // `#:` escapes the colon.
    assert_eq!(format_skip(b"#:,end", b","), Some(2));
}

#[test]
fn format_skip_multiple_end_chars() {
    assert_eq!(format_skip(b"a;b:c", b";:"), Some(1));
    assert_eq!(format_skip(b"ab:c", b";:"), Some(2));
}

#[test]
fn public_format_skip_delimiter_matches_nested_scanner() {
    assert_eq!(super::format_skip_delimiter("#{a,#{b,c}}]", b"]"), Some(11));
    assert_eq!(
        super::format_skip_delimiter("#[literal] tail", b"]"),
        Some(9)
    );
}

// -----------------------------------------------------------------------
// New tests — modifier parsing
// -----------------------------------------------------------------------

#[test]
fn modifier_literal() {
    // `#{l:hello}` → literal "hello".
    assert_eq!(render_template("#{l:hello}", &StaticWindowValues), "hello");
    // With escape sequences.
    assert_eq!(
        render_template("#{l:he#,llo}", &StaticWindowValues),
        "he,llo"
    );
}

#[test]
fn modifier_expand() {
    // `#{E:#{session_name}}` → expand "alpha" → "alpha".
    assert_eq!(
        render_template("#{E:session_name}", &StaticWindowValues),
        "alpha"
    );

    struct TimeVars;
    impl FormatVariables for TimeVars {
        fn format_value(&self, variable: FormatVariable) -> Option<String> {
            match variable {
                FormatVariable::SessionName => Some("alpha".to_owned()),
                _ => None,
            }
        }

        fn format_value_by_name(&self, name: &str) -> Option<String> {
            match name {
                "@foo" => Some("time=%H name=#{session_name}".to_owned()),
                _ => {
                    FormatVariable::from_name(name).and_then(|variable| self.format_value(variable))
                }
            }
        }
    }
    let expanded = render_template("#{E:@foo}", &TimeVars);
    assert!(expanded.starts_with("time="), "{expanded:?}");
    assert!(expanded.ends_with(" name=alpha"), "{expanded:?}");
    assert!(!expanded.contains("%H"), "{expanded:?}");
    let time_expanded = render_template("#{T:@foo}", &TimeVars);
    assert!(time_expanded.ends_with(" name=alpha"));
    assert!(!time_expanded.contains("%H"));
}

#[test]
fn modifier_shell_quote() {
    assert_eq!(
        render_template("#{q:session_name}", &StaticWindowValues),
        "alpha"
    );
}

#[test]
fn modifier_shell_quote_with_single_quote() {
    struct QuoteVars;
    impl FormatVariables for QuoteVars {
        fn format_value(&self, variable: FormatVariable) -> Option<String> {
            match variable {
                FormatVariable::SessionName => Some("it's".to_owned()),
                _ => None,
            }
        }
    }
    assert_eq!(render_template("#{q:session_name}", &QuoteVars), "it\\'s");
}

#[test]
fn modifier_shell_quote_escapes_tmux_specials() {
    struct QuoteVars;
    impl FormatVariables for QuoteVars {
        fn format_value(&self, variable: FormatVariable) -> Option<String> {
            match variable {
                FormatVariable::SessionName => Some("a b$c#d%".to_owned()),
                _ => None,
            }
        }
    }
    assert_eq!(
        render_template("#{q:session_name}", &QuoteVars),
        "a\\ b\\$c\\#d\\%"
    );
}

#[test]
fn modifier_shell_quote_leaves_single_nested_expansion_unescaped() {
    struct UserOptionVars;
    impl FormatVariables for UserOptionVars {
        fn format_value(&self, variable: FormatVariable) -> Option<String> {
            match variable {
                FormatVariable::SessionName => Some("alpha beta".to_owned()),
                _ => None,
            }
        }

        fn format_value_by_name(&self, name: &str) -> Option<String> {
            match name {
                "@v" => Some("Hello World".to_owned()),
                _ => {
                    FormatVariable::from_name(name).and_then(|variable| self.format_value(variable))
                }
            }
        }
    }

    assert_eq!(
        render_template("#{q:#{@v}}", &UserOptionVars),
        "Hello World"
    );
    assert_eq!(render_template("#{q:@v}", &UserOptionVars), "Hello\\ World");
    assert_eq!(
        render_template("#{q:session_name}", &UserOptionVars),
        "alpha\\ beta"
    );
}

#[test]
fn modifier_style_quote() {
    struct StyleVars;
    impl FormatVariables for StyleVars {
        fn format_value(&self, variable: FormatVariable) -> Option<String> {
            match variable {
                FormatVariable::SessionName => Some("a#b".to_owned()),
                _ => None,
            }
        }
    }
    assert_eq!(render_template("#{q/e/:session_name}", &StyleVars), "a##b");
}

// -----------------------------------------------------------------------
// New tests — comparisons
// -----------------------------------------------------------------------
