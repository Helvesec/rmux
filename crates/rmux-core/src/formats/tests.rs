use super::{
    bounded_format_padding_width, format_skip, format_skip_delimiter, format_variable_descriptor,
    format_variable_descriptors, is_known_format_variable_name, is_truthy, render_template,
    signed_i32_abs_usize, tmux_format_variable_descriptors, tmux_time_format_variable_descriptors,
    FormatContext, FormatVariable, FormatVariableContext, FormatVariableProvider, FormatVariables,
    DEFAULT_DISPLAY_MESSAGE_FORMAT, DEFAULT_LIST_PANES_FORMAT, DEFAULT_LIST_SESSIONS_FORMAT,
    DEFAULT_LIST_WINDOWS_FORMAT, FORMAT_PADDING_LIMIT, FORMAT_VARIABLES, TMUX_FORMAT_TABLE_NAMES,
    TMUX_TIME_FORMAT_VARIABLE_NAMES,
};
use crate::Session;
use rmux_proto::TerminalSize;

fn session_name(value: &str) -> rmux_proto::SessionName {
    rmux_proto::SessionName::new(value).expect("valid session name")
}

fn sample_window_layout() -> String {
    let body = "120x40,0,0{80x40,0,0,4,39x40,81,0[39x20,81,0,5,39x19,81,21,6]}";
    format!("{:04x},{}", crate::layout::layout_checksum(body), body)
}

struct StaticWindowValues;

impl FormatVariables for StaticWindowValues {
    fn format_value(&self, variable: FormatVariable) -> Option<String> {
        Some(match variable {
            FormatVariable::SessionName => "alpha".to_owned(),
            FormatVariable::SessionWindows => "2".to_owned(),
            FormatVariable::SessionAttached => "1".to_owned(),
            FormatVariable::SessionWidth => "120".to_owned(),
            FormatVariable::SessionHeight => "40".to_owned(),
            FormatVariable::WindowIndex => "5".to_owned(),
            FormatVariable::WindowId => "@9".to_owned(),
            FormatVariable::WindowName => "logs".to_owned(),
            FormatVariable::WindowRawFlags => "*".to_owned(),
            FormatVariable::WindowPanes => "3".to_owned(),
            FormatVariable::WindowWidth => "120".to_owned(),
            FormatVariable::WindowHeight => "40".to_owned(),
            FormatVariable::WindowLayout => sample_window_layout(),
            FormatVariable::WindowActive => "1".to_owned(),
            FormatVariable::WindowLastFlag => "0".to_owned(),
            FormatVariable::PaneIndex => "0".to_owned(),
            FormatVariable::PaneId => "%4".to_owned(),
            FormatVariable::PaneActive => "1".to_owned(),
            FormatVariable::PaneWidth => "80".to_owned(),
            FormatVariable::PaneHeight => "24".to_owned(),
        })
    }

    fn format_value_by_name(&self, name: &str) -> Option<String> {
        match name {
            "history_bytes" => Some("512".to_owned()),
            "history_limit" => Some("2000".to_owned()),
            "history_size" => Some("10".to_owned()),
            "pane_dead" => Some("0".to_owned()),
            "pane_title" => Some("build logs".to_owned()),
            "session_created" => Some("1713180600".to_owned()),
            "session_group" => Some("dev".to_owned()),
            "session_grouped" => Some("0".to_owned()),
            "window_flags" => Some("*".to_owned()),
            _ => FormatVariable::from_name(name).and_then(|variable| self.format_value(variable)),
        }
    }

    fn format_name_exists(&self, scope: Option<char>, name: &str) -> Option<bool> {
        Some(match (scope, name) {
            (Some('s'), "alpha") => true,
            (Some('s'), _) => false,
            (None | Some('w'), "logs") => true,
            (None | Some('w'), _) => false,
            _ => false,
        })
    }
}

// -----------------------------------------------------------------------
// Existing tests — preserved with updated expectations for tmux semantics
// -----------------------------------------------------------------------

#[path = "tests/defaults_context.rs"]
mod defaults_context;

#[path = "tests/skip_and_modifiers.rs"]
mod skip_and_modifiers;

#[path = "tests/operators.rs"]
mod operators;

#[path = "tests/transformations.rs"]
mod transformations;

#[path = "tests/path_basename.rs"]
mod path_basename;

#[path = "tests/path_basename_limit.rs"]
mod path_basename_limit;

#[path = "tests/path_dirname_limit.rs"]
mod path_dirname_limit;

#[path = "tests/path_dirname_separators.rs"]
mod path_dirname_separators;

#[path = "tests/tables_time.rs"]
mod tables_time;

#[path = "tests/advanced_values.rs"]
mod advanced_values;

#[path = "tests/build_capability.rs"]
mod build_capability;

#[path = "tests/hexadecimal_operands.rs"]
mod hexadecimal_operands;

#[path = "tests/glob_alnum_class.rs"]
mod glob_alnum_class;
#[path = "tests/glob_alpha_class.rs"]
mod glob_alpha_class;
#[path = "tests/glob_case_class.rs"]
mod glob_case_class;
#[path = "tests/glob_classes.rs"]
mod glob_classes;
#[path = "tests/glob_digit_class.rs"]
mod glob_digit_class;
#[path = "tests/glob_print_class.rs"]
mod glob_print_class;
#[path = "tests/glob_punct_class.rs"]
mod glob_punct_class;
#[path = "tests/glob_space_class.rs"]
mod glob_space_class;
#[path = "tests/glob_star_escape.rs"]
mod glob_star_escape;
#[path = "tests/glob_xdigit_class.rs"]
mod glob_xdigit_class;

#[path = "tests/invalid_substitution.rs"]
mod invalid_substitution;

#[path = "tests/precision_c_space.rs"]
mod precision_c_space;

#[path = "tests/expansion_budget.rs"]
mod expansion_budget;
