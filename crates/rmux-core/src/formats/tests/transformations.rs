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

// -----------------------------------------------------------------------
// New tests — substitution
// -----------------------------------------------------------------------

#[test]
fn substitution_basic() {
    assert_eq!(
        render_template("#{s/alpha/beta/:session_name}", &StaticWindowValues),
        "beta"
    );
}

#[test]
fn substitution_uses_regex_and_case_insensitive_flag() {
    assert_eq!(
        render_template("#{s/AL.HA/beta/i:session_name}", &StaticWindowValues),
        "beta"
    );
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
    // An escaped comma leaves the boolean operator without a real delimiter.
    assert_eq!(
        render_template("#{||:hello#,world}", &StaticWindowValues),
        ""
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
    // tmux does not treat an expanded true condition in the false arm as a
    // new chained condition; the remaining body is returned as the false arm.
    assert_eq!(
        render_template(
            "#{?#{window_last_flag},first,#{window_active},#{window_name},default}",
            &StaticWindowValues
        ),
        "1,logs,default"
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
