use super::*;

#[test]
fn truncation_left() {
    assert_eq!(
        render_template("#{=3:session_name}", &StaticWindowValues),
        "alp"
    );
}

#[test]
fn truncation_with_marker() {
    assert_eq!(
        render_template("#{=/3/...:session_name}", &StaticWindowValues),
        "alp..."
    );
}

#[test]
fn styled_format_width_truncation_and_padding_ignore_inline_clauses() {
    let context = FormatContext::new()
        .with_named_value("left", "#[fg=red]AB#[fg=blue]CDEF")
        .with_named_value("right", "#[fg=red]AB#[fg=blue]CDEF");

    assert_eq!(render_template("#{w:left}", &context), "6");
    assert_eq!(
        render_template("#{=/4:left}", &context),
        "#[fg=red]AB#[fg=blue]CD"
    );
    assert_eq!(
        render_template("#{=-3:right}", &context),
        "#[fg=red]#[fg=blue]DEF"
    );
    assert_eq!(
        render_template("#{p8:left}", &context),
        "#[fg=red]AB#[fg=blue]CDEF  "
    );
}

#[test]
fn negative_modifier_bounds_do_not_overflow() {
    assert_eq!(signed_i32_abs_usize(i32::MIN), 2_147_483_648);
    assert_eq!(
        bounded_format_padding_width(FORMAT_PADDING_LIMIT as i32),
        Some(FORMAT_PADDING_LIMIT)
    );
    assert_eq!(bounded_format_padding_width(i32::MIN), None);
    assert_eq!(
        render_template("#{=-2147483648:session_name}", &StaticWindowValues),
        "alpha"
    );
}

// -----------------------------------------------------------------------
// New tests — substitution
// -----------------------------------------------------------------------

#[test]
fn substitution_basic() {
    assert_eq!(
        render_template("#{s/alpha/beta/:session_name}", &StaticWindowValues),
        "beta"
    );

    let context = FormatContext::new().with_named_value("text", "foofoo");
    assert_eq!(render_template("#{s/^foo/bar/:text}", &context), "barfoo");
}

#[test]
fn substitution_empty_pattern_is_noop_and_zero_width_patterns_match_tmux() {
    assert_eq!(
        render_template("#{s//Z/:session_name}", &StaticWindowValues),
        "alpha"
    );
    assert_eq!(
        render_template("#{s/^/>/:session_name}", &StaticWindowValues),
        "lpha"
    );
    assert_eq!(
        render_template("#{s/$/Z/:session_name}", &StaticWindowValues),
        "alphaZ"
    );
    assert_eq!(
        render_template("#{s/[0-9]*/Z/:session_name}", &StaticWindowValues),
        "aZlZpZhZaZ"
    );
    assert_eq!(
        render_template("#{s/[0-9]?/Z/:session_name}", &StaticWindowValues),
        "aZlZpZhZaZ"
    );
    assert_eq!(
        render_template("#{s/(^|$)/Z/:session_name}", &StaticWindowValues),
        "aZlZpZhZaZ"
    );
    assert_eq!(
        render_template("#{s/ *$//:session_name}", &StaticWindowValues),
        "alpha"
    );
    assert_eq!(
        render_template("#{s/ *$/X/:session_name}", &StaticWindowValues),
        "alphaX"
    );

    let context = FormatContext::new()
        .with_named_value("aba", "aba")
        .with_named_value("baa", "baa")
        .with_named_value("abc", "abc")
        .with_named_value("empty", "");
    assert_eq!(render_template("#{s/a*/Z/:aba}", &context), "ZbZ");
    assert_eq!(render_template("#{s/b*/Z/:aba}", &context), "aZaZ");
    assert_eq!(render_template("#{s/a*/Z/:baa}", &context), "bZ");
    assert_eq!(render_template("#{s/b*/Z/:abc}", &context), "aZcZ");
    assert_eq!(render_template("#{s/$/Z/:empty}", &context), "");
}

#[test]
fn substitution_zero_width_backrefs_match_tmux() {
    assert_eq!(
        render_template(r"#{s/(a*)/<\1>/:session_name}", &StaticWindowValues),
        "<a>l<1>p<1>h<a>"
    );
    assert_eq!(
        render_template(r"#{s/(b*)/<\1>/:session_name}", &StaticWindowValues),
        "a<1>l<1>p<1>h<1>a<1>"
    );
    assert_eq!(
        render_template(r"#{s/$/\1/:session_name}", &StaticWindowValues),
        "alpha1"
    );
}

#[test]
fn substitution_zero_width_matches_non_ascii_bytes_like_tmux_3_4() {
    let context = FormatContext::new().with_named_value("unicode", "éx");

    assert_eq!(render_template("#{s//Z/:unicode}", &context), "éx");
    assert_eq!(
        render_template("#{s/[0-9]*/Z/:unicode}", &context),
        "\\303Z\\251ZxZ"
    );
    assert_eq!(
        render_template("#{s/(^|$)/Z/:unicode}", &context),
        "\\303Z\\251ZxZ"
    );
}

#[test]
fn substitution_uses_regex_and_case_insensitive_flag() {
    assert_eq!(
        render_template("#{s/AL.HA/beta/i:session_name}", &StaticWindowValues),
        "beta"
    );
}

#[test]
fn trailing_bare_hash_is_literal_product_divergence() {
    assert_eq!(render_template("prefix#", &StaticWindowValues), "prefix#");
}

// -----------------------------------------------------------------------
// New tests — basename and dirname
// -----------------------------------------------------------------------

#[test]
fn modifier_basename() {
    struct PathVars;
    impl FormatVariables for PathVars {
        fn format_value(&self, variable: FormatVariable) -> Option<String> {
            match variable {
                FormatVariable::SessionName => Some("/home/user/test".to_owned()),
                _ => None,
            }
        }
    }
    assert_eq!(render_template("#{b:session_name}", &PathVars), "test");
}

#[test]
fn modifier_dirname() {
    struct PathVars;
    impl FormatVariables for PathVars {
        fn format_value(&self, variable: FormatVariable) -> Option<String> {
            match variable {
                FormatVariable::SessionName => Some("/home/user/test".to_owned()),
                _ => None,
            }
        }
    }
    assert_eq!(
        render_template("#{d:session_name}", &PathVars),
        "/home/user"
    );
}

#[test]
fn modifier_dirname_matches_empty_and_trailing_slash_semantics() {
    let context = FormatContext::new()
        .with_named_value("path", "/a/b/c")
        .with_named_value("slash", "/a/b/")
        .with_named_value("single", "file")
        .with_named_value("root", "/");

    assert_eq!(
        render_template(
            "#{d:path}|#{d:slash}|#{d:single}|#{d:missing}|#{d:}|#{d:root}",
            &context
        ),
        "/a/b|/a|.|||/"
    );
}

// -----------------------------------------------------------------------
// New tests — length
// -----------------------------------------------------------------------

#[test]
fn modifier_length() {
    assert_eq!(
        render_template("#{n:session_name}", &StaticWindowValues),
        "5" // "alpha".len() == 5
    );
}

// -----------------------------------------------------------------------
// New tests — style pass-through
// -----------------------------------------------------------------------

#[test]
fn style_passthrough() {
    assert_eq!(
        render_template("#[fg=red]hello#[default]", &StaticWindowValues),
        "#[fg=red]hello#[default]"
    );
}

// -----------------------------------------------------------------------
// New tests — interaction of modifiers and conditionals
// -----------------------------------------------------------------------

#[test]
fn conditional_inside_comparison() {
    // Compare session_name with "alpha" using ==, then use result in conditional.
    assert_eq!(
        render_template(
            "#{?#{==:#{session_name},alpha},match,no-match}",
            &StaticWindowValues
        ),
        "match"
    );
}

// -----------------------------------------------------------------------
// Hardening tests — escape sequences in conditionals and nesting
// -----------------------------------------------------------------------

#[test]
fn escaped_comma_inside_conditional_value() {
    // `#,` inside a conditional value should produce a literal `,`.
    // #{l:a#,b} → "a,b" (literal mode unescapes), but inside a conditional
    // the `#,` should prevent the comma from acting as a field separator.
    assert_eq!(
        render_template("#{?window_active,a#,b,fallback}", &StaticWindowValues),
        "a,b"
    );
}

#[test]
fn escaped_comma_in_boolean_operand() {
    // Escaped commas stay inside a single truthy operand.
    assert_eq!(
        render_template("#{||:hello#,world}", &StaticWindowValues),
        "1"
    );
}

#[test]
fn escaped_brace_in_literal() {
    // `#}` inside literal mode should produce `}`.
    assert_eq!(render_template("#{l:a#}b}", &StaticWindowValues), "a}b");
}

#[test]
fn literal_unescape_respects_nested_format_depth() {
    // tmux only unescapes `#` sequences at bracket depth zero.
    assert_eq!(
        render_template("#{l:outer #{inner#,value} end}", &StaticWindowValues),
        "outer #{inner#,value} end"
    );
}

#[test]
fn job_expansion_returns_empty() {
    // `#(cmd)` is recognized here but not executed.
    assert_eq!(
        render_template("before#(echo hello)after", &StaticWindowValues),
        "beforeafter"
    );
}

#[test]
fn job_expansion_unclosed_breaks_out() {
    // `#(cmd` with no matching `)` — tmux breaks out of loop.
    assert_eq!(
        render_template("before#(no close", &StaticWindowValues),
        "before"
    );
}

#[test]
fn session_loop_keeps_commas_in_body() {
    struct LoopValues;

    impl FormatVariables for LoopValues {
        fn format_value(&self, _variable: FormatVariable) -> Option<String> {
            None
        }

        fn format_loop(
            &self,
            scope: char,
            body: &str,
            current_body: Option<&str>,
            _count_only: bool,
        ) -> Option<String> {
            Some(format!(
                "{scope}:{body}:{}",
                current_body.unwrap_or("<none>")
            ))
        }
    }

    assert_eq!(
        render_template("#{S:#{session_name},CURRENT}", &LoopValues),
        "S:#{session_name},CURRENT:<none>"
    );
    assert_eq!(
        render_template("#{W:#{window_index},CURRENT}", &LoopValues),
        "W:#{window_index}:CURRENT"
    );
}

#[test]
fn comparison_both_sides_expanded() {
    // Both sides of a comparison get expanded.
    assert_eq!(
        render_template("#{==:#{window_index},#{window_index}}", &StaticWindowValues),
        "1"
    );
    assert_eq!(
        render_template(
            "#{!=:#{window_index},#{session_windows}}",
            &StaticWindowValues
        ),
        "1"
    );
}

#[test]
fn comparison_no_comma_returns_empty() {
    // If there's no comma to split on, comparison returns empty.
    assert_eq!(
        render_template("#{==:no-comma-here}", &StaticWindowValues),
        ""
    );
}

#[test]
fn fnmatch_character_class() {
    assert_eq!(
        render_template("#{m:[a-z]*,alpha}", &StaticWindowValues),
        "1"
    );
    assert_eq!(
        render_template("#{m:[0-9]*,alpha}", &StaticWindowValues),
        "0"
    );
}

#[test]
fn multi_pair_conditional_with_nested_expansion() {
    // tmux 3.7 treats additional condition,value pairs in the false arm as
    // chained else-if branches.
    assert_eq!(
        render_template(
            "#{?#{window_last_flag},first,#{window_active},#{window_name},default}",
            &StaticWindowValues
        ),
        "logs"
    );
}

#[test]
fn modifier_chain_basename_and_length() {
    // Chain: basename then length.
    struct PathVars;
    impl FormatVariables for PathVars {
        fn format_value(&self, variable: FormatVariable) -> Option<String> {
            match variable {
                FormatVariable::SessionName => Some("/home/user/test".to_owned()),
                _ => None,
            }
        }
    }
    assert_eq!(
        render_template("#{b;n:session_name}", &PathVars),
        "4" // basename "test" → length 4
    );
}

#[test]
fn modifier_dirname_no_slash() {
    // dirname of a name with no slash → "."
    assert_eq!(
        render_template("#{d:session_name}", &StaticWindowValues),
        "." // "alpha" has no `/`
    );
}

#[test]
fn trailing_hash_in_various_positions() {
    assert_eq!(render_template("abc#", &StaticWindowValues), "abc#");
    assert_eq!(
        render_template("#{session_name}#", &StaticWindowValues),
        "alpha#"
    );
    assert_eq!(render_template("#####", &StaticWindowValues), "###");
}

#[test]
fn format_skip_colon_escape() {
    // `#:` escapes a colon — format_skip should skip it.
    assert_eq!(format_skip(b"a#:b:c", b":"), Some(4));
}

#[test]
fn style_with_nested_expression() {
    // `#[fg=red]text#[default]` passes through style markers.
    assert_eq!(
        render_template("before#[fg=red]middle#[default]after", &StaticWindowValues),
        "before#[fg=red]middle#[default]after"
    );
}

#[test]
fn double_hash_before_style_passthrough() {
    // `##[fg=red]` — the `##` + `[` triggers style passthrough with
    // all the `#` chars preserved.
    assert_eq!(
        render_template("##[fg=red]text", &StaticWindowValues),
        "##[fg=red]text"
    );
}

// -----------------------------------------------------------------------
// Hardening tests — TMUX_FORMAT_TABLE_NAMES invariants
// -----------------------------------------------------------------------
