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
fn expression_double_percent_is_a_modulo_alias_like_linux_tmux() {
    // The Linux glibc oracle evaluates both '%' and '%%' as modulo; the
    // darwin oracle's BSD strftime doubles '%%' into '%' before the format
    // parser, so both oracles agree the doubled spelling must evaluate.
    assert_eq!(render_template("#{e|%%|:5,2}", &StaticWindowValues), "1");
    assert_eq!(
        render_template("#{e|%%|f:5,2}", &StaticWindowValues),
        "1.00"
    );
}

#[test]
fn float_comparisons_apply_requested_precision_like_tmux_3_7b() {
    let comparisons = [
        ("==", "5,5", "5,6"),
        ("!=", "5,6", "5,5"),
        ("<", "5,6", "6,5"),
        ("<=", "5,5", "6,5"),
        (">", "6,5", "5,6"),
        (">=", "5,5", "5,6"),
    ];
    let precisions = [("f", "1.00", "0.00"), ("f|4", "1.0000", "0.0000")];

    for (operator, true_operands, false_operands) in comparisons {
        for (float_options, expected_true, expected_false) in precisions {
            assert_eq!(
                render_template(
                    &format!("#{{e|{operator}|{float_options}:{true_operands}}}"),
                    &StaticWindowValues,
                ),
                expected_true,
                "{operator} true with {float_options}",
            );
            assert_eq!(
                render_template(
                    &format!("#{{e|{operator}|{float_options}:{false_operands}}}"),
                    &StaticWindowValues,
                ),
                expected_false,
                "{operator} false with {float_options}",
            );
        }
    }
}

#[test]
fn float_comparisons_use_untruncated_operands_like_tmux_3_7b() {
    let cases = [
        ("==", "1.5,1.5", "1.5,1.6"),
        ("!=", "1.5,1.6", "1.5,1.5"),
        ("<", "1.5,1.6", "1.6,1.5"),
        ("<=", "1.5,1.6", "1.6,1.5"),
        (">", "1.6,1.5", "1.5,1.6"),
        (">=", "1.6,1.5", "1.5,1.6"),
    ];

    for (operator, true_operands, false_operands) in cases {
        assert_eq!(
            render_template(
                &format!("#{{e|{operator}|f:{true_operands}}}"),
                &StaticWindowValues,
            ),
            "1.00",
            "{operator} true with decimal operands",
        );
        assert_eq!(
            render_template(
                &format!("#{{e|{operator}|f:{false_operands}}}"),
                &StaticWindowValues,
            ),
            "0.00",
            "{operator} false with decimal operands",
        );
    }
}

#[test]
fn float_equality_uses_tmux_3_7b_epsilon_and_non_finite_semantics() {
    assert_eq!(
        render_template("#{e|==|f:1.0000000005,1}", &StaticWindowValues),
        "1.00"
    );
    assert_eq!(
        render_template("#{e|!=|f:1.0000000005,1}", &StaticWindowValues),
        "0.00"
    );
    assert_eq!(
        render_template("#{e|==|f:inf,inf}", &StaticWindowValues),
        "0.00"
    );
    assert_eq!(
        render_template("#{e|!=|f:nan,nan}", &StaticWindowValues),
        "0.00"
    );
}

#[test]
fn nested_float_comparison_uses_rendered_boolean_like_tmux_3_7b() {
    // tmux only treats the exact string "0" as false. Requested floating-point
    // rendering therefore makes the false comparison result "0.00" truthy.
    assert_eq!(
        render_template("#{?#{e|==|f:5,6},then,else}", &StaticWindowValues),
        "then"
    );
    assert_eq!(
        render_template("#{?#{e|==|f|4:5,6},then,else}", &StaticWindowValues),
        "then"
    );
}

#[test]
fn expression_operands_trim_spaces_product_divergence() {
    assert_eq!(render_template("#{e|+|: 5 , 3 }", &StaticWindowValues), "8");
}

#[test]
fn expression_float_nan_results_render_deterministically() {
    // Every NaN producer must render "-nan" (Linux deployment oracle), not
    // the platform-dependent hardware NaN sign.
    assert_eq!(render_template("#{e|m|f:5,0}", &StaticWindowValues), "-nan");
    assert_eq!(
        render_template("#{e|m|f:inf,2}", &StaticWindowValues),
        "-nan"
    );
    assert_eq!(render_template("#{e|/|f:0,0}", &StaticWindowValues), "-nan");
    assert_eq!(
        render_template("#{e|-|f:inf,inf}", &StaticWindowValues),
        "-nan"
    );
    assert_eq!(
        render_template("#{e|*|f:inf,0}", &StaticWindowValues),
        "-nan"
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
fn expression_integer_overflow_uses_deterministic_sentinel() {
    assert_eq!(
        render_template("#{e|+|:999999999999999999999,1}", &StaticWindowValues),
        i64::MIN.to_string()
    );
    assert_eq!(
        render_template("#{e|+|:9223372036854775807,1}", &StaticWindowValues),
        i64::MIN.to_string()
    );
    assert_eq!(
        render_template("#{e|+|:9223372036854775807,2}", &StaticWindowValues),
        i64::MIN.to_string()
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
        i64::MIN.to_string()
    );
    assert_eq!(
        render_template("#{e|*|:3037000500,3037000500}", &StaticWindowValues),
        i64::MIN.to_string()
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
        i64::MIN.to_string()
    );
    assert_eq!(
        render_template("#{e|m|:-9223372036854775808,-1}", &StaticWindowValues),
        "0"
    );
    assert_eq!(
        render_template("#{e|%|:-9223372036854775808,-1}", &StaticWindowValues),
        "0"
    );
}

#[test]
fn expression_division_by_zero_uses_deterministic_sentinel() {
    assert_eq!(
        render_template("#{e|/|:5,0}", &StaticWindowValues),
        i64::MIN.to_string()
    );
    assert_eq!(render_template("#{e|m|:5,2}", &StaticWindowValues), "1");
    assert_eq!(render_template("#{e|%|:5,2}", &StaticWindowValues), "1");
    assert_eq!(
        render_template("#{e|%|:5,0}", &StaticWindowValues),
        i64::MIN.to_string()
    );
    assert_eq!(
        render_template("#{e|m|:5,0}", &StaticWindowValues),
        i64::MIN.to_string()
    );
    assert_eq!(render_template("#{e|/|f:5,0}", &StaticWindowValues), "inf");
    assert_eq!(render_template("#{e|m|f:5,0}", &StaticWindowValues), "-nan");
    assert_eq!(render_template("#{e|%|f:5,2}", &StaticWindowValues), "1.00");
    assert_eq!(render_template("#{e|%|f:5,0}", &StaticWindowValues), "-nan");
}

#[test]
fn expression_empty_and_prefixed_integer_operands_use_tmux_parsing() {
    assert_eq!(render_template("#{e|+|:5,}", &StaticWindowValues), "5");
    assert_eq!(render_template("#{e|+|:0x10,1}", &StaticWindowValues), "17");
    assert_eq!(
        render_template("#{e|+|:-0x10,1}", &StaticWindowValues),
        "-15"
    );
    assert_eq!(
        render_template("#{e|+|:inf,1}", &StaticWindowValues),
        i64::MIN.to_string()
    );
    assert_eq!(render_template("#{e|q|:inf,1}", &StaticWindowValues), "");
}

#[test]
fn expression_non_finite_and_wide_integer_operands_use_deterministic_sentinel() {
    assert_eq!(
        render_template("#{e|/|:9223372036854775808,2}", &StaticWindowValues),
        "-4611686018427387904"
    );
    assert_eq!(
        render_template("#{e|*|:9223372036854775808,2}", &StaticWindowValues),
        i64::MIN.to_string()
    );
    assert_eq!(
        render_template("#{e|m|:9223372036854775808,3}", &StaticWindowValues),
        "-2"
    );
    assert_eq!(
        render_template("#{e|/|:inf,2}", &StaticWindowValues),
        "-4611686018427387904"
    );
    assert_eq!(
        render_template("#{e|*|:nan,2}", &StaticWindowValues),
        i64::MIN.to_string()
    );
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
fn expression_float_precision_is_bounded_like_tmux_3_7b() {
    let precision_100 = render_template("#{e|+|f|100:1,2}", &StaticWindowValues);
    assert_eq!(precision_100.len(), 102);
    assert!(precision_100.starts_with("3."));
    assert!(precision_100[2..].bytes().all(|byte| byte == b'0'));

    assert_eq!(render_template("#{e|+|f|101:1,2}", &StaticWindowValues), "");
    assert_eq!(
        render_template("#{e|+|f|999999999999999999999:1,2}", &StaticWindowValues),
        ""
    );
    assert_eq!(
        render_template("#{e|+|f|invalid:1,2}", &StaticWindowValues),
        ""
    );
    assert_eq!(
        render_template("#{e|+|f|-1:1,2}", &StaticWindowValues),
        "3.000000"
    );
    assert_eq!(
        render_template("#{e|+|f|-100:1,2}", &StaticWindowValues),
        "3.000000"
    );
    assert_eq!(
        render_template("#{e|+|f|-101:1,2}", &StaticWindowValues),
        ""
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
fn expression_non_finite_comparisons_use_deterministic_sentinel() {
    assert_eq!(render_template("#{e|>|:inf,1}", &StaticWindowValues), "0");
    assert_eq!(render_template("#{e|<|:inf,1}", &StaticWindowValues), "1");
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
    assert_eq!(render_template("#{e|==|:nan,0}", &StaticWindowValues), "0");
    assert_eq!(render_template("#{e|!=|:nan,0}", &StaticWindowValues), "1");
    assert_eq!(render_template("#{e|<|:nan,1}", &StaticWindowValues), "1");
    assert_eq!(render_template("#{e|>|:nan,1}", &StaticWindowValues), "0");
    assert_eq!(render_template("#{e|<=|:nan,1}", &StaticWindowValues), "1");
    assert_eq!(
        render_template("#{e|==|:inf,-inf}", &StaticWindowValues),
        "1"
    );
    assert_eq!(
        render_template("#{e|>|:inf,-inf}", &StaticWindowValues),
        "0"
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
