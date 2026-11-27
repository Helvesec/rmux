use super::super::{render_runtime_template, RuntimeFormatContext};
use rmux_core::formats::FormatContext;

#[test]
fn format_runtime_expressions_reject_unicode_operand_whitespace() {
    // Linux /usr/local/bin/tmux 3.7b rejects these operands byte for byte.
    for value in ["\u{a0}2", "2\u{a0}", "\u{2003}2", "2\u{2003}", "\u{a0}"] {
        let runtime =
            RuntimeFormatContext::new(FormatContext::new().with_named_value("@value", value));
        for (template, expected) in [
            ("#{e|+|:#{@value},1}", ""),
            ("#{e|+|f|:#{@value},1}", ""),
            ("#{e|==|:#{@value},2}", ""),
            ("#{e|==|f|:2,#{@value}}", ""),
            (
                "#{?#{e|==|:#{@value},2},#{l:\u{e9}\u{9}\u{754c}},#{l:other}}",
                "other",
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
