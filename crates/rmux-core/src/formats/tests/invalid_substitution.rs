//! A substitution whose pattern will not compile keeps the value.
//!
//! Every expectation here is the stdout of the pinned tmux 3.7b, measured in one
//! small fixture before this correction was written
//! (`fms1/native/oracle-fms1.json`): the user option `@fms1` holds
//! the text and the whole format travels as a single argument, exactly as it
//! does on the command line.
//!
//! The witnesses go through the public `render_template`, not through a
//! simulated replacement.

use super::*;

/// The text under `@fms1`, the whole format, and what tmux 3.7b printed.
struct Measured {
    value: &'static str,
    format: &'static str,
    tmux_stdout: &'static str,
}

fn rendered(measured: &Measured) -> String {
    let context = FormatContext::new().with_named_value("@fms1", measured.value);
    render_template(measured.format, &context)
}

#[test]
fn an_invalid_pattern_keeps_a_value_that_contains_it_literally() {
    // Both patterns are real regex errors, and both texts contain the pattern
    // as plain text: a literal replacement would be visible here.
    for measured in [
        Measured {
            value: "a[b",
            format: "#{s/[/X/:@fms1}",
            tmux_stdout: "a[b",
        },
        Measured {
            value: "a(b",
            format: "#{s/(/X/:@fms1}",
            tmux_stdout: "a(b",
        },
    ] {
        assert_eq!(
            rendered(&measured),
            measured.tmux_stdout,
            "{}",
            measured.format
        );
    }
}

#[test]
fn an_invalid_pattern_keeps_an_empty_value_and_a_value_without_it() {
    for measured in [
        Measured {
            value: "",
            format: "#{s/[/X/:@fms1}",
            tmux_stdout: "",
        },
        Measured {
            value: "zzz",
            format: "#{s/[/X/:@fms1}",
            tmux_stdout: "zzz",
        },
    ] {
        assert_eq!(
            rendered(&measured),
            measured.tmux_stdout,
            "{:?}",
            measured.value
        );
    }
}

#[test]
fn the_modifiers_after_an_invalid_one_still_run() {
    // The chain the mandate names: a valid step, the step that cannot compile,
    // then another valid step. tmux printed A[B, so the failing step keeps its
    // own value and stops nothing.
    let measured = Measured {
        value: "a[b",
        format: "#{s/a/A/;s/[/X/;s/b/B/:@fms1}",
        tmux_stdout: "A[B",
    };

    assert_eq!(rendered(&measured), measured.tmux_stdout);
}

#[test]
fn a_valid_substitution_on_the_same_text_is_untouched() {
    let measured = Measured {
        value: "a[b",
        format: "#{s/b/B/:@fms1}",
        tmux_stdout: "a[B",
    };

    assert_eq!(rendered(&measured), measured.tmux_stdout);
}
