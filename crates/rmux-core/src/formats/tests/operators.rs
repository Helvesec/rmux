use super::*;

#[test]
fn comparison_equal() {
    assert_eq!(
        render_template("#{==:alpha,alpha}", &StaticWindowValues),
        "1"
    );
    assert_eq!(
        render_template("#{==:alpha,beta}", &StaticWindowValues),
        "0"
    );
}

#[test]
fn comparison_not_equal() {
    assert_eq!(
        render_template("#{!=:alpha,beta}", &StaticWindowValues),
        "1"
    );
    assert_eq!(
        render_template("#{!=:alpha,alpha}", &StaticWindowValues),
        "0"
    );
}

#[test]
fn comparison_less_than() {
    assert_eq!(render_template("#{<:abc,def}", &StaticWindowValues), "1");
    assert_eq!(render_template("#{<:def,abc}", &StaticWindowValues), "0");
}

#[test]
fn comparison_greater_than() {
    assert_eq!(render_template("#{>:def,abc}", &StaticWindowValues), "1");
    assert_eq!(render_template("#{>:abc,def}", &StaticWindowValues), "0");
}

#[test]
fn comparison_less_equal() {
    assert_eq!(render_template("#{<=:abc,abc}", &StaticWindowValues), "1");
    assert_eq!(render_template("#{<=:abc,def}", &StaticWindowValues), "1");
    assert_eq!(render_template("#{<=:def,abc}", &StaticWindowValues), "0");
}

#[test]
fn comparison_greater_equal() {
    assert_eq!(render_template("#{>=:def,def}", &StaticWindowValues), "1");
    assert_eq!(render_template("#{>=:def,abc}", &StaticWindowValues), "1");
    assert_eq!(render_template("#{>=:abc,def}", &StaticWindowValues), "0");
}

#[test]
fn comparison_with_variable_expansion() {
    // Compare expanded variables.
    assert_eq!(
        render_template("#{==:#{session_name},alpha}", &StaticWindowValues),
        "1"
    );
    assert_eq!(
        render_template("#{!=:#{session_name},beta}", &StaticWindowValues),
        "1"
    );
}

// -----------------------------------------------------------------------
// New tests — fnmatch
// -----------------------------------------------------------------------

#[test]
fn fnmatch_basic() {
    assert_eq!(render_template("#{m:al*,alpha}", &StaticWindowValues), "1");
    assert_eq!(render_template("#{m:be*,alpha}", &StaticWindowValues), "0");
}

#[test]
fn fnmatch_regex_flag() {
    assert_eq!(
        render_template("#{m/r:^al[a-z]+$,alpha}", &StaticWindowValues),
        "1"
    );
    assert_eq!(
        render_template("#{m/r:^be[a-z]+$,alpha}", &StaticWindowValues),
        "0"
    );
}

#[test]
fn fnmatch_regex_flag_can_be_case_insensitive() {
    assert_eq!(
        render_template("#{m/ri:^AL[A-Z]+$,alpha}", &StaticWindowValues),
        "1"
    );
}

#[test]
fn fnmatch_question_mark() {
    assert_eq!(
        render_template("#{m:alph?,alpha}", &StaticWindowValues),
        "1"
    );
    assert_eq!(render_template("#{m:alp?,alpha}", &StaticWindowValues), "0");
}

// -----------------------------------------------------------------------
// New tests — boolean operators
// -----------------------------------------------------------------------

#[test]
fn boolean_and() {
    // Both truthy — operands are format expressions that get expanded.
    assert_eq!(
        render_template(
            "#{&&:#{window_active},#{session_name}}",
            &StaticWindowValues
        ),
        "1"
    );
    // One falsy (window_last_flag = "0").
    assert_eq!(
        render_template(
            "#{&&:#{window_last_flag},#{window_active}}",
            &StaticWindowValues
        ),
        "0"
    );
}

#[test]
fn boolean_or() {
    // One truthy.
    assert_eq!(
        render_template(
            "#{||:#{window_last_flag},#{window_active}}",
            &StaticWindowValues
        ),
        "1"
    );
    // Both falsy (window_last_flag="0", missing="").
    assert_eq!(
        render_template("#{||:#{window_last_flag},#{missing}}", &StaticWindowValues),
        "0"
    );
}

#[test]
fn bang_prefix_is_not_a_boolean_modifier() {
    assert_eq!(render_template("#{!:0}", &StaticWindowValues), "");
    assert_eq!(render_template("#{!:1}", &StaticWindowValues), "");
    assert_eq!(render_template("#{!!:0}", &StaticWindowValues), "");
    assert_eq!(render_template("#{!!:1}", &StaticWindowValues), "");
    assert_eq!(
        render_template("#{!#{window_active}}", &StaticWindowValues),
        "!1"
    );
    assert_eq!(
        render_template("#{!#{window_last_flag}}", &StaticWindowValues),
        "!0"
    );
}

#[test]
fn expression_arithmetic_defaults_to_integer_output() {
    assert_eq!(render_template("#{e|+|:2,3}", &StaticWindowValues), "5");
    assert_eq!(render_template("#{e|-|:2,3}", &StaticWindowValues), "-1");
    assert_eq!(render_template("#{e|*|:2,3}", &StaticWindowValues), "6");
    assert_eq!(render_template("#{e|/|:5,2}", &StaticWindowValues), "2");
    assert_eq!(render_template("#{e|%|:5,2}", &StaticWindowValues), "1");
    assert_eq!(render_template("#{e|+|:2.9,3.9}", &StaticWindowValues), "5");
    assert_eq!(render_template("#{e|*|:2.9,3.9}", &StaticWindowValues), "6");
    assert_eq!(render_template("#{e|m|:7,2}", &StaticWindowValues), "1");
}

#[test]
fn expression_integer_overflow_saturates() {
    assert_eq!(
        render_template("#{e|+|:999999999999999999999,1}", &StaticWindowValues),
        i64::MAX.to_string()
    );
    assert_eq!(
        render_template("#{e|+|:9223372036854775807,1}", &StaticWindowValues),
        i64::MAX.to_string()
    );
}

#[test]
fn expression_integer_minimum_division_overflow_does_not_panic() {
    assert_eq!(
        render_template("#{e|/|:-9223372036854775808,-1}", &StaticWindowValues),
        i64::MAX.to_string()
    );
    assert_eq!(
        render_template("#{e|%|:-9223372036854775808,-1}", &StaticWindowValues),
        "0"
    );
}

#[test]
fn expression_division_by_zero_is_invalid_not_tmux_sentinel() {
    assert_eq!(render_template("#{e|/|:5,0}", &StaticWindowValues), "");
    assert_eq!(render_template("#{e|%|:5,0}", &StaticWindowValues), "");
    assert_eq!(render_template("#{e|m|:5,0}", &StaticWindowValues), "");
    assert_eq!(render_template("#{e|/|f:5,0}", &StaticWindowValues), "");
}

#[test]
fn expression_arithmetic_float_option_renders_two_decimals() {
    assert_eq!(
        render_template("#{e|+|f:1.23,2.34}", &StaticWindowValues),
        "3.57"
    );
    assert_eq!(render_template("#{e|/|f:5,2}", &StaticWindowValues), "2.50");
    assert_eq!(
        render_template("#{e|+|f|4:1.2345,2.3456}", &StaticWindowValues),
        "3.5801"
    );
    assert_eq!(
        render_template("#{e|+|f|0:1.9,2.9}", &StaticWindowValues),
        "5"
    );
}

#[test]
fn expression_numeric_comparisons() {
    assert_eq!(render_template("#{e|==|:2,2}", &StaticWindowValues), "1");
    assert_eq!(render_template("#{e|!=|:2,3}", &StaticWindowValues), "1");
    assert_eq!(render_template("#{e|>|:5,2}", &StaticWindowValues), "1");
    assert_eq!(render_template("#{e|<=|:5,2}", &StaticWindowValues), "0");
}

#[test]
fn boolean_and_matches_tmux_binary_first_comma_split() {
    assert_eq!(
        render_template(
            "#{&&:#{window_active},#{session_name},#{window_panes}}",
            &StaticWindowValues
        ),
        "1"
    );
    assert_eq!(
        render_template(
            "#{&&:#{window_active},#{window_last_flag},#{session_name}}",
            &StaticWindowValues
        ),
        "1"
    );
    assert_eq!(
        render_template(
            "#{&&:0,#{window_active},#{session_name}}",
            &StaticWindowValues
        ),
        "0"
    );
}

#[test]
fn boolean_or_matches_tmux_binary_first_comma_split() {
    assert_eq!(
        render_template(
            "#{||:#{window_last_flag},#{missing},#{missing2}}",
            &StaticWindowValues
        ),
        "1"
    );
    assert_eq!(
        render_template(
            "#{||:#{window_last_flag},#{window_active},#{missing}}",
            &StaticWindowValues
        ),
        "1"
    );
}

// -----------------------------------------------------------------------
// New tests — ternary conditionals
// -----------------------------------------------------------------------

#[test]
fn conditional_selects_true_or_false_branch() {
    assert_eq!(
        render_template("#{?window_active,first,second}", &StaticWindowValues),
        "first"
    );
    assert_eq!(
        render_template("#{?window_last_flag,first,second}", &StaticWindowValues),
        "second"
    );
}

#[test]
fn conditional_false_branch_preserves_commas() {
    assert_eq!(
        render_template(
            "#{?window_last_flag,first,missing,second,default}tail",
            &StaticWindowValues
        ),
        "missing,second,defaulttail"
    );
    assert_eq!(
        render_template(
            "#{?window_last_flag,first,session_name,second,default}tail",
            &StaticWindowValues
        ),
        "session_name,second,defaulttail"
    );
}

#[test]
fn conditional_without_false_branch_stops_expansion_like_tmux() {
    assert_eq!(
        render_template("pre#{?window_last_flag,first}tail", &StaticWindowValues),
        "pre"
    );
    assert_eq!(
        render_template("#{?window_active,first}tail", &StaticWindowValues),
        ""
    );
}

#[test]
fn incomplete_conditional_inside_selected_branch_does_not_stop_outer_expansion() {
    assert_eq!(
        render_template("A#{?#{==:1,0},B,#{?#{==:1,1},C}}D", &StaticWindowValues),
        "AD"
    );
    assert_eq!(
        render_template("A#{?#{==:1,1},#{?#{==:1,1},B},C}D", &StaticWindowValues),
        "AD"
    );
}

#[test]
fn conditional_format_chain_is_iterative_and_bounded() {
    fn chained_format_conditionals(count: usize) -> String {
        let mut body = "default".to_owned();
        for _ in 0..count {
            body = format!("zz_format,t,{body}");
        }
        format!("#{{?{body}}}")
    }

    assert_eq!(
        render_template(&chained_format_conditionals(64), &StaticWindowValues),
        "default"
    );
    assert_eq!(
        render_template(&chained_format_conditionals(512), &StaticWindowValues),
        ""
    );
}

// -----------------------------------------------------------------------
// New tests — escape sequences in expansion
// -----------------------------------------------------------------------

#[test]
fn escape_comma() {
    // `#,` in template produces literal `,`.
    assert_eq!(render_template("a#,b", &StaticWindowValues), "a,b");
}

#[test]
fn escape_closing_brace() {
    // `#}` in template produces literal `}`.
    assert_eq!(render_template("a#}b", &StaticWindowValues), "a}b");
}

// -----------------------------------------------------------------------
// New tests — recursion limit
// -----------------------------------------------------------------------

#[test]
fn recursion_limit_produces_empty() {
    // Deeply nested expand modifiers should hit the limit and return empty.
    // Create a template that re-expands many times.
    struct RecurseVars;
    impl FormatVariables for RecurseVars {
        fn format_value(&self, variable: FormatVariable) -> Option<String> {
            match variable {
                FormatVariable::SessionName => Some("#{E:session_name}".to_owned()),
                _ => None,
            }
        }
    }
    // This will try to expand session_name → "#{E:session_name}" → expand
    // again → ... until the recursion limit is hit.
    let result = render_template("#{E:session_name}", &RecurseVars);
    // Should eventually produce empty string when limit is hit.
    assert!(result.len() < 1000, "recursion should be bounded");
}

// -----------------------------------------------------------------------
// New tests — truncation and padding
// -----------------------------------------------------------------------
