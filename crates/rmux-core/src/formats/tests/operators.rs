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
    assert_eq!(render_template("#{!:0}", &StaticWindowValues), "1");
    assert_eq!(render_template("#{!:1}", &StaticWindowValues), "0");
    assert_eq!(render_template("#{!!:0}", &StaticWindowValues), "0");
    assert_eq!(render_template("#{!!:1}", &StaticWindowValues), "1");
    assert_eq!(render_template("#{!!:foo}", &StaticWindowValues), "1");
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
    // '%' is not a tmux e-operator (oracle probe 2026-07-08: empty).
    assert_eq!(render_template("#{e|%|:5,2}", &StaticWindowValues), "");
    assert_eq!(render_template("#{e|+|:2.9,3.9}", &StaticWindowValues), "5");
    assert_eq!(render_template("#{e|*|:2.9,3.9}", &StaticWindowValues), "6");
    assert_eq!(render_template("#{e|m|:7,2}", &StaticWindowValues), "1");
}

#[test]
fn expression_integer_overflow_matches_tmux() {
    // Oracle probe 2026-07-08: out-of-range operands saturate (arm64
    // long long semantics), so the sum renders as (double)LLONG_MAX.
    assert_eq!(
        render_template("#{e|+|:999999999999999999999,1}", &StaticWindowValues),
        "9223372036854775808"
    );
    assert_eq!(
        render_template("#{e|+|:9223372036854775807,1}", &StaticWindowValues),
        "9223372036854775808"
    );
    assert_eq!(
        render_template("#{e|+|:9223372036854775807,2}", &StaticWindowValues),
        "9223372036854775808"
    );
    assert_eq!(
        render_template("#{e|-|:-9223372036854775808,1}", &StaticWindowValues),
        i64::MIN.to_string()
    );
    assert_eq!(
        render_template("#{e|-|:0,9223372036854775806}", &StaticWindowValues),
        i64::MIN.to_string()
    );
    assert_eq!(
        render_template("#{e|*|:4611686018427387904,2}", &StaticWindowValues),
        "9223372036854775808"
    );
    assert_eq!(
        render_template("#{e|*|:3037000500,3037000500}", &StaticWindowValues),
        "9223372036854775808"
    );
    assert_eq!(
        render_template("#{e|*|:3037000499,3037000499}", &StaticWindowValues),
        "9223372030926248960"
    );
}

#[test]
fn expression_integer_minimum_division_overflow_does_not_panic() {
    assert_eq!(
        render_template("#{e|/|:-9223372036854775808,-1}", &StaticWindowValues),
        "9223372036854775808"
    );
    assert_eq!(
        render_template("#{e|m|:-9223372036854775808,-1}", &StaticWindowValues),
        "0"
    );
    // '%' is not a tmux e-operator (oracle renders empty).
    assert_eq!(
        render_template("#{e|%|:-9223372036854775808,-1}", &StaticWindowValues),
        ""
    );
}

#[test]
fn expression_division_by_zero_matches_tmux_oracle() {
    // Oracle probe 2026-07-08: division by zero saturates, integer modulo
    // by zero renders 0, float modulo by zero renders "nan" (unsigned),
    // and '%' is not a valid e-operator (renders empty).
    assert_eq!(
        render_template("#{e|/|:5,0}", &StaticWindowValues),
        "9223372036854775808"
    );
    assert_eq!(render_template("#{e|m|:5,2}", &StaticWindowValues), "1");
    assert_eq!(render_template("#{e|%|:5,2}", &StaticWindowValues), "");
    assert_eq!(render_template("#{e|%|:5,0}", &StaticWindowValues), "");
    assert_eq!(render_template("#{e|m|:5,0}", &StaticWindowValues), "0");
    assert_eq!(render_template("#{e|/|f:5,0}", &StaticWindowValues), "inf");
    assert_eq!(render_template("#{e|m|f:5,0}", &StaticWindowValues), "nan");
    assert_eq!(render_template("#{e|%|f:5,2}", &StaticWindowValues), "");
    assert_eq!(render_template("#{e|%|f:5,0}", &StaticWindowValues), "");
}

#[test]
fn expression_empty_and_prefixed_integer_operands_match_tmux() {
    assert_eq!(render_template("#{e|+|:5,}", &StaticWindowValues), "5");
    assert_eq!(render_template("#{e|+|:0x10,1}", &StaticWindowValues), "17");
    assert_eq!(
        render_template("#{e|+|:-0x10,1}", &StaticWindowValues),
        "-15"
    );
    assert_eq!(
        render_template("#{e|+|:inf,1}", &StaticWindowValues),
        "9223372036854775808"
    );
    assert_eq!(render_template("#{e|q|:inf,1}", &StaticWindowValues), "");
}

#[test]
fn expression_non_finite_and_wide_integer_operands_saturate_like_tmux_oracle() {
    // Expectations recorded 2026-07-08 against the pinned tmux 3.7b oracle
    // (arm64): operands and results round-trip through a saturating
    // long long cast (NaN -> 0, +/-inf and out-of-range saturate).
    assert_eq!(
        render_template("#{e|/|:9223372036854775808,2}", &StaticWindowValues),
        "4611686018427387904"
    );
    assert_eq!(
        render_template("#{e|*|:9223372036854775808,2}", &StaticWindowValues),
        "9223372036854775808"
    );
    assert_eq!(
        render_template("#{e|m|:9223372036854775808,3}", &StaticWindowValues),
        "2"
    );
    assert_eq!(
        render_template("#{e|/|:inf,2}", &StaticWindowValues),
        "4611686018427387904"
    );
    assert_eq!(render_template("#{e|*|:nan,2}", &StaticWindowValues), "0");
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
fn expression_non_finite_comparisons_match_tmux_oracle() {
    // Oracle probe 2026-07-08: comparison operands take the same saturating
    // long long round-trip (inf -> LLONG_MAX, nan -> 0).
    assert_eq!(render_template("#{e|>|:inf,1}", &StaticWindowValues), "1");
    assert_eq!(render_template("#{e|<|:inf,1}", &StaticWindowValues), "0");
    assert_eq!(
        render_template("#{e|==|:inf,inf}", &StaticWindowValues),
        "1"
    );
    assert_eq!(
        render_template("#{e|!=|:inf,inf}", &StaticWindowValues),
        "0"
    );
    assert_eq!(
        render_template("#{e|==|:nan,nan}", &StaticWindowValues),
        "1"
    );
    assert_eq!(
        render_template("#{e|!=|:nan,nan}", &StaticWindowValues),
        "0"
    );
    assert_eq!(render_template("#{e|==|:nan,0}", &StaticWindowValues), "1");
    assert_eq!(render_template("#{e|!=|:nan,0}", &StaticWindowValues), "0");
    assert_eq!(render_template("#{e|<|:nan,1}", &StaticWindowValues), "1");
    assert_eq!(render_template("#{e|>|:nan,1}", &StaticWindowValues), "0");
    assert_eq!(render_template("#{e|<=|:nan,1}", &StaticWindowValues), "1");
    assert_eq!(
        render_template("#{e|==|:inf,-inf}", &StaticWindowValues),
        "0"
    );
    assert_eq!(
        render_template("#{e|>|:inf,-inf}", &StaticWindowValues),
        "1"
    );
}

#[test]
fn boolean_and_matches_tmux_3_7_variadic_semantics() {
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
        "0"
    );
    assert_eq!(
        render_template(
            "#{&&:0,#{window_active},#{session_name}}",
            &StaticWindowValues
        ),
        "0"
    );
    assert_eq!(render_template("#{&&:}", &StaticWindowValues), "0");
    assert_eq!(render_template("#{&&:1}", &StaticWindowValues), "1");
    assert_eq!(render_template("#{&&:1,1,1}", &StaticWindowValues), "1");
    assert_eq!(render_template("#{&&:1,1,0}", &StaticWindowValues), "0");
    assert_eq!(
        render_template("#{&&:1,#{&&:1,0},1}", &StaticWindowValues),
        "0"
    );
}

#[test]
fn boolean_or_matches_tmux_3_7_variadic_semantics() {
    assert_eq!(
        render_template(
            "#{||:#{window_last_flag},#{missing},#{missing2}}",
            &StaticWindowValues
        ),
        "0"
    );
    assert_eq!(
        render_template(
            "#{||:#{window_last_flag},#{window_active},#{missing}}",
            &StaticWindowValues
        ),
        "1"
    );
    assert_eq!(render_template("#{||:}", &StaticWindowValues), "0");
    assert_eq!(render_template("#{||:0}", &StaticWindowValues), "0");
    assert_eq!(render_template("#{||:1}", &StaticWindowValues), "1");
    assert_eq!(render_template("#{||:0,0,0}", &StaticWindowValues), "0");
    assert_eq!(render_template("#{||:0,1,0}", &StaticWindowValues), "1");
    assert_eq!(
        render_template("#{||:0,#{||:0,0},0}", &StaticWindowValues),
        "0"
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
        "defaulttail"
    );
    assert_eq!(
        render_template(
            "#{?window_last_flag,first,session_name,second,default}tail",
            &StaticWindowValues
        ),
        "secondtail"
    );
}

#[test]
fn conditional_else_if_matches_tmux_3_7() {
    assert_eq!(
        render_template("#{?#{==:a,b},X,#{==:c,c},Y,Z}", &StaticWindowValues),
        "Y"
    );
    assert_eq!(
        render_template("#{?#{==:a,b},X,#{==:c,d},Y,Z}", &StaticWindowValues),
        "Z"
    );
}

#[test]
fn repeat_modifier_matches_tmux_3_7() {
    assert_eq!(render_template("#{R:ab,3}", &StaticWindowValues), "ababab");
    assert_eq!(render_template("#{R:x,0}", &StaticWindowValues), "");
    assert_eq!(render_template("#{R:x,-1}", &StaticWindowValues), "");
    assert_eq!(render_template("#{R:x,2.9}", &StaticWindowValues), "");
    assert_eq!(
        render_template("#{R: ,#{n:#{session_name}}}", &StaticWindowValues),
        "     "
    );
    assert_eq!(
        render_template("#{R:0123456789,1000}", &StaticWindowValues).len(),
        10_000
    );
    assert_eq!(
        render_template("#{R:0123456789,1001}", &StaticWindowValues).len(),
        10_010
    );
    assert_eq!(
        render_template("#{n:#{R:#{p10000:a},100}}", &StaticWindowValues),
        "1000000"
    );
    assert_eq!(
        render_template("#{n:#{R:#{p10000:a},1000}}", &StaticWindowValues),
        "0"
    );
    assert_eq!(
        render_template(
            "#{n:#{R:#{R:#{p10000:a},10000},10000}}",
            &StaticWindowValues
        ),
        "0"
    );
    assert_eq!(render_template("#{R:x,10001}", &StaticWindowValues), "");
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
