use clap::error::{ContextKind, ContextValue, ErrorKind};
use rmux_proto::{ControlMode, CONTROL_CONTROL_END, CONTROL_CONTROL_START};

use super::ExitFailure;

pub(super) fn parse_failure(error: clap::Error, control_mode: u8) -> ExitFailure {
    if error.kind() == ErrorKind::UnknownArgument {
        let command_name = match error.get(ContextKind::Custom) {
            Some(ContextValue::String(value)) => Some(value.as_str()),
            _ => None,
        };
        let invalid_argument = match error.get(ContextKind::InvalidArg) {
            Some(ContextValue::String(value)) => Some(value.as_str()),
            _ => None,
        };
        if let (Some(command_name), Some(flag)) = (command_name, invalid_argument) {
            if flag.starts_with('-') && flag != "-" && flag != "--" {
                let flag = escape_diagnostic_token(flag);
                return exit_failure_for_count(
                    1,
                    &format!("command {command_name}: unknown flag {flag}"),
                    control_mode,
                );
            }
        }
    }

    let failure = ExitFailure::from_clap(error);
    exit_failure_for_count(failure.exit_code(), failure.message(), control_mode)
}

fn escape_diagnostic_token(token: &str) -> String {
    let mut escaped = String::with_capacity(token.len());
    for character in token.chars() {
        if character.is_control() {
            escaped.extend(character.escape_default());
        } else {
            escaped.push(character);
        }
    }
    escaped
}

pub(super) fn exit_failure_for_count(
    exit_code: i32,
    message: &str,
    control_mode: u8,
) -> ExitFailure {
    let message = message.trim_end();
    let mut escaped = String::with_capacity(message.len() + 6);
    for (index, line) in message.split('\n').enumerate() {
        if index != 0 {
            escaped.push('\n');
        }
        if line.starts_with('%') {
            escaped.push('\\');
        }
        for character in line.chars() {
            if character.is_control() {
                escaped.extend(character.escape_default());
            } else {
                escaped.push(character);
            }
        }
    }
    escaped.push_str("\n%exit");
    if ControlMode::from_count(control_mode).is_control_control() {
        let mut framed = String::with_capacity(
            CONTROL_CONTROL_START.len() + escaped.len() + 1 + CONTROL_CONTROL_END.len(),
        );
        framed.push_str(CONTROL_CONTROL_START);
        framed.push_str(&escaped);
        framed.push('\n');
        framed.push_str(CONTROL_CONTROL_END);
        ExitFailure::new_stdout_exact(exit_code, framed)
    } else {
        ExitFailure::new_stdout(exit_code, escaped)
    }
}
