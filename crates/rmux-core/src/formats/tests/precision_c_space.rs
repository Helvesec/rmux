use super::{render_template, StaticWindowValues};

// tmux 3.7b, LC_ALL=C, display-message -p, measured on Mac for S3-285.
// All six C spaces and their mixed run accept an initial position only.
const LEADING: [&str; 8] = [
    " ",
    "\t",
    "\n",
    "\u{b}",
    "\u{c}",
    "\r",
    " \t\n\u{b}\u{c}\r ",
    "",
];
const MODES: [(&str, &str, &str, &str); 4] = [
    ("+", "", "1,2", "3"),
    ("+", "f", "1,2", "3"),
    ("==", "", "2.9,2.1", "1"),
    ("==", "f", "2.9,2.1", "0"),
];

#[test]
fn initial_c_spaces_reach_arithmetic_and_comparison_in_both_modes() {
    let mut mismatches = Vec::new();
    for leading in &LEADING[..7] {
        for (operator, options, operands, expected) in MODES {
            let template = format!("#{{e|{operator}|{options}|{leading}0:{operands}}}");
            let actual = render_template(&template, &StaticWindowValues);
            if actual != expected {
                mismatches.push((template, actual, expected));
            }
        }
    }
    assert!(mismatches.is_empty(), "{mismatches:?}");
}

#[test]
fn signed_precision_and_range_keep_their_measured_meaning() {
    for leading in LEADING {
        for (precision, decimals) in [
            ("-100", Some(6)),
            ("-1", Some(6)),
            ("0", Some(0)),
            ("+0", Some(0)),
            ("2", Some(2)),
            ("100", Some(100)),
            ("-101", None),
            ("101", None),
        ] {
            for (operator, options, operands, integer) in MODES {
                let template =
                    format!("#{{e|{operator}|{options}|{leading}{precision}:{operands}}}");
                let expected = match decimals {
                    None => String::new(),
                    Some(0) => integer.to_owned(),
                    Some(n) => format!("{integer}.{}", "0".repeat(n)),
                };
                assert_eq!(
                    render_template(&template, &StaticWindowValues),
                    expected,
                    "{template:?}"
                );
            }
        }
    }
}

#[test]
fn only_an_initial_c_space_run_is_ignored() {
    for precision in [
        "",
        " ",
        "\t",
        " \t\n\u{b}\u{c}\r ",
        "+",
        "-",
        " +",
        " -",
        "0x",
        " 0x",
        "0 ",
        " 0 ",
        "0\t",
        "0\n",
        "0\u{b}",
        "0\u{c}",
        "0\r",
        "\u{a0}0",
        "\u{2003}0",
        "\u{85}0",
        "\u{2028}0",
        " \u{a0}0",
        "0\u{a0}",
        "9223372036854775808",
        "-9223372036854775809",
        " 9223372036854775808",
        " -9223372036854775809",
        "0.0",
        " 0.0",
        "+ 0",
        "--0",
    ] {
        for (operator, options, operands, _) in MODES {
            let end = if precision.is_empty() { "|:" } else { ":" };
            let template = format!("#{{e|{operator}|{options}|{precision}{end}{operands}}}");
            assert_eq!(
                render_template(&template, &StaticWindowValues),
                "",
                "{template:?}"
            );
        }
    }
}

#[test]
fn absent_empty_and_operand_modes_remain_distinct() {
    for (template, expected) in [
        ("#{e|+||:1,2}", "3"),
        ("#{e|+|f|:1,2}", "3.00"),
        ("#{e|==||:2.9,2.1}", "1"),
        ("#{e|==|f|:2.9,2.1}", "0.00"),
        ("#{e|+|||:1,2}", ""),
        ("#{e|+|f||:1,2}", ""),
        ("#{e|==|||:2.9,2.1}", ""),
        ("#{e|==|f||:2.9,2.1}", ""),
        ("#{e|/||2:5.9,2.9}", "2.00"),
        ("#{e|/|f|2:5.9,2.9}", "2.03"),
        ("#{e|+|| 0:0x1p1,0x1p0}", "3"),
        ("#{e|+|f| 0:0x1p1,0x1p0}", "3"),
        ("#{e|+|| 0:\u{a0}1,2}", ""),
        ("#{e|+|f| 0:\u{a0}1,2}", ""),
    ] {
        assert_eq!(
            render_template(template, &StaticWindowValues),
            expected,
            "{template:?}"
        );
    }
}
