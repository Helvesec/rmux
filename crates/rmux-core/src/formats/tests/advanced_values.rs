use super::*;

#[test]
fn truncation_right_from_end() {
    // Negative limit truncates from the right (keeps last N chars).
    assert_eq!(
        render_template("#{=-3:session_name}", &StaticWindowValues),
        "pha"
    );
}

#[test]
fn truncation_no_op_when_within_limit() {
    // String shorter than limit — no truncation.
    assert_eq!(
        render_template("#{=100:session_name}", &StaticWindowValues),
        "alpha"
    );
}

#[test]
fn truncation_zero_limit_is_no_op() {
    // Zero limit means no truncation applied.
    assert_eq!(
        render_template("#{=0:session_name}", &StaticWindowValues),
        "alpha"
    );
}

// -----------------------------------------------------------------------
// Hardening tests — padding edge cases
// -----------------------------------------------------------------------

#[test]
fn padding_right_align() {
    // Negative width pads from the left (right-aligns).
    let result = render_template("#{p-10:session_name}", &StaticWindowValues);
    assert_eq!(result, "     alpha");
}

#[test]
fn padding_left_align() {
    // Positive width pads from the right (left-aligns).
    let result = render_template("#{p10:session_name}", &StaticWindowValues);
    assert_eq!(result, "alpha     ");
}

#[test]
fn truncation_uses_display_width_for_wide_characters() {
    let context = FormatContext::new()
        .with_named_value("left", "表A")
        .with_named_value("right", "A表");

    assert_eq!(render_template("#{=2:left}", &context), "表");
    assert_eq!(render_template("#{=-2:right}", &context), "表");
}

#[test]
fn padding_uses_display_width_for_wide_characters() {
    let context = FormatContext::new().with_named_value("wide", "表A");

    assert_eq!(render_template("#{p5:wide}", &context), "表A  ");
    assert_eq!(render_template("#{p-5:wide}", &context), "  表A");
}

// -----------------------------------------------------------------------
// Hardening tests — substitution edge cases
// -----------------------------------------------------------------------

#[test]
fn substitution_no_match() {
    assert_eq!(
        render_template("#{s/xyz/abc/:session_name}", &StaticWindowValues),
        "alpha"
    );
}

#[test]
fn substitution_invalid_regex_falls_back_to_literal() {
    // An invalid regex pattern should fall back to literal replacement.
    assert_eq!(
        render_template("#{s/[invalid/abc/:session_name}", &StaticWindowValues),
        "alpha"
    );
}

#[test]
fn substitution_with_regex_groups() {
    assert_eq!(
        render_template(r"#{s/(al)(pha)/\2\1/:session_name}", &StaticWindowValues),
        "phaal"
    );
}

#[test]
fn substitution_backreference_before_word_character_is_unambiguous() {
    assert_eq!(
        render_template(
            r"#{s/(al)(pha)/\2_word_\1/:session_name}",
            &StaticWindowValues
        ),
        "pha_word_al"
    );
}

#[test]
fn substitution_keeps_dollar_references_literal() {
    assert_eq!(
        render_template("#{s/(al)(pha)/$2$1/:session_name}", &StaticWindowValues),
        "$2$1"
    );
}

#[test]
fn status_substitution_preserves_captured_hashes_and_configured_styles() {
    struct ReportedText;
    impl FormatVariables for ReportedText {
        fn format_value(&self, _: FormatVariable) -> Option<String> {
            None
        }

        fn format_status_field(&self, _: &str) -> (String, bool) {
            ("foo#bar".to_owned(), true)
        }
    }

    for (modifiers, expected) in [
        (r"s/(##)(bar)/X\2/", "#[fg=green]fooXbar"),
        (r"s/(##)(bar)/\1\1\2/", "#[fg=green]foo\0\0bar"),
    ] {
        let template = format!("#{{{modifiers}:#[fg=green]#{{pane_program_status_message}}}}");
        assert_eq!(
            super::super::render_status_template(&template, &ReportedText),
            expected,
            "{template}"
        );
    }

    // Zero-width matches use captures relative to their remaining suffix.
    let template = r"#{s/x*(##)?/\1/:#{pane_program_status_message}}";
    let plain = FormatContext::new().with_named_value("pane_program_status_message", "foo#bar");
    let marked = super::super::render_status_template(template, &ReportedText);
    assert_eq!(marked.replace('\0', "#"), render_template(template, &plain));
    assert!(marked.contains('\0'));

    let raw = FormatContext::new().with_named_value("@raw", "foo\0bar");
    assert_eq!(
        super::super::render_status_template("#{s/##/X/:@raw}", &raw),
        "foo\x01bar"
    );
}

// -----------------------------------------------------------------------
// Hardening tests — named_values precedence
// -----------------------------------------------------------------------

#[test]
fn named_value_overrides_enum_variable() {
    let context = FormatContext::new().with_named_value("session_name", "override");
    assert_eq!(render_template("#{session_name}", &context), "override");
}

#[test]
fn named_value_last_wins() {
    let context = FormatContext::new()
        .with_named_value("custom", "first")
        .with_named_value("custom", "second");
    assert_eq!(render_template("#{custom}", &context), "second");
}

// -----------------------------------------------------------------------
// Hardening tests — single-char aliases with format_value_by_name
// -----------------------------------------------------------------------

#[test]
fn single_char_alias_d_resolves_pane_id() {
    assert_eq!(render_template("#D", &StaticWindowValues), "%4");
}

#[test]
fn single_char_alias_h_resolves_host_short() {
    struct HostVars;
    impl FormatVariables for HostVars {
        fn format_value(&self, _: FormatVariable) -> Option<String> {
            None
        }
        fn format_value_by_name(&self, name: &str) -> Option<String> {
            match name {
                "host_short" => Some("myhost".to_owned()),
                _ => None,
            }
        }
    }
    assert_eq!(render_template("#h", &HostVars), "myhost");
}

// -----------------------------------------------------------------------
// Hardening tests — nested job expansion
// -----------------------------------------------------------------------

#[test]
fn nested_parentheses_in_job_expansion() {
    // `#(echo (foo))` — the inner `(` increments depth, inner `)` decrements.
    assert_eq!(
        render_template("x#(echo (foo))y", &StaticWindowValues),
        "xy"
    );
}

#[test]
fn colour_modifier_renders_non_ascii_operands_as_empty_like_tmux() {
    // tmux 3.7b printed "" for each non-ASCII operand and "800000" for red.
    let context = FormatContext::new();
    for format in ["#{c:abcde\u{e9}}", "#{c:\u{e9}}", "#{c:colour\u{e9}}"] {
        assert_eq!(render_template(format, &context), "", "{format:?}");
    }
    assert_eq!(render_template("#{c:red}", &context), "800000");
}
