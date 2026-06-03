use super::*;
use crate::formats::expand_time_tokens;

#[test]
fn tmux_format_table_names_is_sorted_and_unique() {
    for pair in super::TMUX_FORMAT_TABLE_NAMES.windows(2) {
        assert!(
            pair[0] < pair[1],
            "TMUX_FORMAT_TABLE_NAMES not sorted: {:?} >= {:?}",
            pair[0],
            pair[1]
        );
    }
}

#[test]
fn tmux_time_format_variable_names_is_sorted_and_subset() {
    for pair in super::TMUX_TIME_FORMAT_VARIABLE_NAMES.windows(2) {
        assert!(
            pair[0] < pair[1],
            "TMUX_TIME_FORMAT_VARIABLE_NAMES not sorted: {:?} >= {:?}",
            pair[0],
            pair[1]
        );
    }
    for name in super::TMUX_TIME_FORMAT_VARIABLE_NAMES {
        assert!(
            super::TMUX_FORMAT_TABLE_NAMES.binary_search(&name).is_ok(),
            "TIME variable {name:?} not in TMUX_FORMAT_TABLE_NAMES"
        );
    }
}

#[test]
fn is_known_format_variable_name_covers_enum_and_table() {
    for variable in super::FORMAT_VARIABLES {
        assert!(
            super::is_known_format_variable_name(variable.name()),
            "enum variable {:?} not recognized by is_known",
            variable.name()
        );
    }
    for name in super::TMUX_FORMAT_TABLE_NAMES {
        assert!(
            super::is_known_format_variable_name(name),
            "table variable {name:?} not recognized by is_known"
        );
    }
    assert!(!super::is_known_format_variable_name("nonexistent_var"));
    assert!(!super::is_known_format_variable_name("@user_option"));
}

// -----------------------------------------------------------------------
// Hardening tests — time formatting
// -----------------------------------------------------------------------

#[test]
fn time_modifier_with_epoch_zero() {
    struct TimeVars;
    impl FormatVariables for TimeVars {
        fn format_value(&self, _: FormatVariable) -> Option<String> {
            None
        }
        fn format_value_by_name(&self, name: &str) -> Option<String> {
            match name {
                "session_created" => Some("0".to_owned()),
                _ => None,
            }
        }
    }
    let result = render_template("#{t:session_created}", &TimeVars);
    assert_eq!(result, "", "tmux renders epoch 0 time formats as empty");
}

#[test]
fn time_modifier_with_non_numeric_value() {
    struct TimeVars;
    impl FormatVariables for TimeVars {
        fn format_value(&self, _: FormatVariable) -> Option<String> {
            None
        }
        fn format_value_by_name(&self, name: &str) -> Option<String> {
            match name {
                "session_created" => Some("not-a-number".to_owned()),
                _ => None,
            }
        }
    }
    // Non-numeric input to `t` modifier should produce empty (graceful).
    let result = render_template("#{t:session_created}", &TimeVars);
    assert_eq!(result, "");
}

#[test]
fn invalid_strftime_percent_is_rendered_literally() {
    assert_eq!(expand_time_tokens("cpu 100%"), "cpu 100%");
}

#[test]
fn bare_percent_does_not_disable_later_time_expansion() {
    let rendered = expand_time_tokens("CPU 50% %H:%M");

    assert!(rendered.starts_with("CPU 50% "));
    assert!(!rendered.ends_with("%H:%M"));
}

#[test]
fn invalid_chrono_strftime_edge_cases_are_literal_not_panics() {
    for template in [
        "%5f", "%12f", "%.5f", "%.7f", "%::::z", "%:::::z", "%#z", "%-A", "%_B", "%0Z", "%-z",
    ] {
        let rendered = std::panic::catch_unwind(|| expand_time_tokens(template))
            .expect("invalid strftime item must not panic");
        assert_eq!(rendered, template);
    }
}

#[test]
fn valid_epoch_and_fraction_modifiers_still_expand() {
    for template in ["%-s", "%0s", "%-f", "%0f"] {
        let rendered = std::panic::catch_unwind(|| expand_time_tokens(template))
            .expect("supported strftime modifier must not panic");
        assert_ne!(rendered, template);
        assert!(
            rendered.bytes().all(|byte| byte.is_ascii_digit()),
            "expected numeric expansion for {template}, got {rendered:?}"
        );
    }

    for template in ["%_s", "%_f"] {
        let rendered = std::panic::catch_unwind(|| expand_time_tokens(template))
            .expect("supported strftime modifier must not panic");
        assert_ne!(rendered, template);
        assert!(
            rendered
                .bytes()
                .all(|byte| byte == b' ' || byte.is_ascii_digit()),
            "expected space-padded numeric expansion for {template}, got {rendered:?}"
        );
        let trimmed = rendered.trim_start();
        assert!(!trimmed.is_empty());
        assert!(
            trimmed.bytes().all(|byte| byte.is_ascii_digit()),
            "expected numeric expansion after padding for {template}, got {rendered:?}"
        );
    }
}

#[test]
fn invalid_strftime_items_do_not_disable_later_time_expansion() {
    for template in ["%5f %H:%M", "%.7f %H:%M", "%::::z %H:%M", "%#z %H:%M"] {
        let rendered = expand_time_tokens(template);

        assert!(rendered.starts_with(template.split_once(' ').expect("space").0));
        assert!(!rendered.ends_with("%H:%M"));
    }
}

// -----------------------------------------------------------------------
// Hardening tests — truncation edge cases
// -----------------------------------------------------------------------
