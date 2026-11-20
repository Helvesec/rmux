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
fn registered_descriptors_have_unique_names_and_metadata() {
    let mut names = Vec::new();
    for descriptor in super::format_variable_descriptors() {
        assert!(
            !descriptor.contexts().is_empty(),
            "descriptor {:?} has no context",
            descriptor.name()
        );
        assert!(
            names.iter().all(|name| *name != descriptor.name()),
            "duplicate descriptor name {:?}",
            descriptor.name()
        );
        names.push(descriptor.name());
    }

    assert!(names.contains(&"pane_created"));
    assert!(names.contains(&"default-shell"));
}

#[test]
fn tmux_inventory_is_derived_from_registered_descriptors() {
    let names = super::tmux_format_variable_descriptors()
        .map(|descriptor| {
            assert!(descriptor.is_tmux_table_entry());
            descriptor.name()
        })
        .collect::<Vec<_>>();

    assert_eq!(names, super::TMUX_FORMAT_TABLE_NAMES);
    assert_eq!(names.len(), 195);
    assert!(!names.contains(&"command"));
    assert!(!names.contains(&"session_width"));
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
fn time_inventory_is_derived_from_registered_descriptors() {
    let names = super::tmux_time_format_variable_descriptors()
        .map(|descriptor| {
            assert!(descriptor.is_time());
            descriptor.name()
        })
        .collect::<Vec<_>>();

    assert_eq!(names, super::TMUX_TIME_FORMAT_VARIABLE_NAMES);
}

#[test]
fn format_variable_api_is_backed_by_registered_descriptors() {
    for variable in super::FORMAT_VARIABLES {
        let descriptor = super::format_variable_descriptor(variable.name())
            .expect("enum-backed variable must have a descriptor");

        assert_eq!(descriptor.format_variable(), Some(variable));
        assert_eq!(
            super::FormatVariable::from_name(descriptor.name()),
            Some(variable)
        );
        assert!(descriptor.has_known_empty_fallback());
    }
}

#[test]
fn descriptor_context_and_provider_classify_real_resolution_paths() {
    let sixel = super::format_variable_descriptor("sixel_support").expect("sixel descriptor");
    assert_eq!(
        sixel.provider(),
        super::FormatVariableProvider::BuildCapability
    );
    assert!(sixel
        .contexts()
        .contains(super::FormatVariableContext::Server));

    let pane_created =
        super::format_variable_descriptor("pane_created").expect("pane_created descriptor");
    assert_eq!(
        pane_created.provider(),
        super::FormatVariableProvider::Runtime
    );
    assert!(pane_created
        .contexts()
        .contains(super::FormatVariableContext::Pane));
    assert!(!pane_created.has_known_empty_fallback());

    let session_width =
        super::format_variable_descriptor("session_width").expect("session_width descriptor");
    assert_eq!(
        session_width.provider(),
        super::FormatVariableProvider::Runtime
    );
    assert_eq!(
        session_width.format_variable(),
        Some(super::FormatVariable::SessionWidth)
    );
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
fn templates_without_percent_are_returned_unchanged() {
    assert_eq!(expand_time_tokens("plain status text"), "plain status text");
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
