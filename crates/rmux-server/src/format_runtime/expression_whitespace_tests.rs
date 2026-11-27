use super::super::{render_runtime_template, RuntimeFormatContext};
use rmux_core::formats::FormatContext;

#[test]
fn format_runtime_expressions_use_c_operand_whitespace_boundaries() {
    // Linux /usr/local/bin/tmux 3.7b expectations, except the C whitespace
    // tolerance protected by expression_operands_trim_spaces_product_divergence.
    for (value, expected) in [
        ("2", ["3", "3.00", "1", "1.00", "\u{e9}\u{9}\u{754c}"]),
        (" 2", ["3", "3.00", "1", "1.00", "\u{e9}\u{9}\u{754c}"]),
        ("\u{9}2", ["3", "3.00", "1", "1.00", "\u{e9}\u{9}\u{754c}"]),
        ("\u{a}2", ["3", "3.00", "1", "1.00", "\u{e9}\u{9}\u{754c}"]),
        ("\u{b}2", ["3", "3.00", "1", "1.00", "\u{e9}\u{9}\u{754c}"]),
        ("\u{c}2", ["3", "3.00", "1", "1.00", "\u{e9}\u{9}\u{754c}"]),
        ("\u{d}2", ["3", "3.00", "1", "1.00", "\u{e9}\u{9}\u{754c}"]),
        // tmux 3.7b: ["", "", "", "", "other"].
        ("2 ", ["3", "3.00", "1", "1.00", "\u{e9}\u{9}\u{754c}"]),
        // tmux 3.7b: ["", "", "", "", "other"].
        ("2\u{9}", ["3", "3.00", "1", "1.00", "\u{e9}\u{9}\u{754c}"]),
        ("\u{a0}2", ["", "", "", "", "other"]),
        ("2\u{a0}", ["", "", "", "", "other"]),
        ("\u{2003}2", ["", "", "", "", "other"]),
        ("2\u{2003}", ["", "", "", "", "other"]),
        ("", ["1", "1.00", "0", "0.00", "other"]),
        // tmux 3.7b: ["", "", "", "", "other"].
        (" ", ["1", "1.00", "0", "0.00", "other"]),
        // tmux 3.7b: ["", "", "", "", "other"].
        ("\u{9}", ["1", "1.00", "0", "0.00", "other"]),
        ("\u{a0}", ["", "", "", "", "other"]),
    ] {
        let runtime =
            RuntimeFormatContext::new(FormatContext::new().with_named_value("@value", value));
        for (template, expected) in [
            ("#{e|+|:#{@value},1}", expected[0]),
            ("#{e|+|f|:#{@value},1}", expected[1]),
            ("#{e|==|:#{@value},2}", expected[2]),
            ("#{e|==|f|:2,#{@value}}", expected[3]),
            (
                "#{?#{e|==|:#{@value},2},#{l:\u{e9}\u{9}\u{754c}},#{l:other}}",
                expected[4],
            ),
        ] {
            assert_eq!(
                render_runtime_template(template, &runtime, false).as_bytes(),
                expected.as_bytes(),
                "value {value:?}, template {template}"
            );
        }
    }
}
