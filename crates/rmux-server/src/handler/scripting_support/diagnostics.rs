use super::{
    CommandArgument, CommandParseError, ParsedCommand, ParsedCommands, QueueCommandAction,
    QueueExecutionContext, QueueMode, RmuxError, CONTROL_QUEUE_STDOUT_LIMIT,
};

pub(super) fn append_queue_stdout(
    stdout: &mut Vec<u8>,
    bytes: &[u8],
    mode: QueueMode,
) -> Result<(), RmuxError> {
    let next_len = stdout.len().saturating_add(bytes.len());
    if mode == QueueMode::Control && next_len > CONTROL_QUEUE_STDOUT_LIMIT {
        return Err(RmuxError::Server(format!(
            "control command stdout exceeds {CONTROL_QUEUE_STDOUT_LIMIT} bytes"
        )));
    }
    stdout.extend_from_slice(bytes);
    Ok(())
}

pub(super) fn control_command_frame_outcome(
    action: &Result<QueueCommandAction, RmuxError>,
) -> (Vec<u8>, Option<RmuxError>) {
    match action {
        Ok(QueueCommandAction::Normal {
            output,
            error,
            source_file_error,
            ..
        })
        | Ok(QueueCommandAction::InsertAfter {
            output,
            error,
            source_file_error,
            ..
        }) => (
            output
                .as_ref()
                .map(|output| output.stdout().to_vec())
                .unwrap_or_default(),
            aggregate_rmux_errors(
                [error.clone(), source_file_error.clone()]
                    .into_iter()
                    .flatten()
                    .collect(),
            ),
        ),
        Err(error) => (Vec::new(), Some(error.clone())),
    }
}

pub(super) fn parsed_command_count(commands: &ParsedCommands) -> usize {
    commands.commands().iter().fold(0_usize, |count, command| {
        command
            .arguments()
            .iter()
            .fold(
                count.saturating_add(1),
                |nested_count, argument| match argument {
                    CommandArgument::Commands(nested) => {
                        nested_count.saturating_add(parsed_command_count(nested))
                    }
                    CommandArgument::String(_) => nested_count,
                },
            )
    })
}

pub(super) fn aggregate_rmux_errors(errors: Vec<RmuxError>) -> Option<RmuxError> {
    match errors.len() {
        0 => None,
        1 => Some(errors.into_iter().next().expect("single error")),
        _ => Some(RmuxError::Server(
            errors
                .into_iter()
                .map(rmux_error_message)
                .collect::<Vec<_>>()
                .join("\n"),
        )),
    }
}

pub(super) fn source_file_context_error(
    error: RmuxError,
    command: &ParsedCommand,
    context: &QueueExecutionContext,
) -> RmuxError {
    let Some(current_file) = context.current_file.as_deref() else {
        return error;
    };
    let include_line_prefix = source_file_error_uses_line_prefix(command.name(), &error);
    let message = source_file_command_error_message(command.name(), error);
    if !include_line_prefix || has_source_file_line_prefix(&message) {
        return RmuxError::Server(message);
    }
    RmuxError::Server(format!("{}:{}: {}", current_file, command.line(), message))
}

const TMUX_SET_OPTION_VALUE_ERRORS: &[&str] = &["invalid style: ", "value is ", "not an array: "];

fn source_file_command_error_message(command_name: &str, error: RmuxError) -> String {
    let message = match &error {
        RmuxError::InvalidTarget { value, reason }
            if matches!(command_name, "link-window" | "move-window")
                && reason == "window index already exists in session" =>
        {
            window_index_from_target(value)
                .map(|index| format!("index in use: {index}"))
                .unwrap_or_else(|| rmux_error_message_ref(&error))
        }
        // tmux's cmd_set_option_exec prints these texts without the
        // protocol prefix that `RmuxError::InvalidSetOption` displays.
        RmuxError::InvalidSetOption(message)
            if TMUX_SET_OPTION_VALUE_ERRORS
                .iter()
                .any(|prefix| message.starts_with(prefix)) =>
        {
            message.clone()
        }
        _ => rmux_error_message_ref(&error),
    };
    if let Some(flag) = message.strip_prefix(&format!("unsupported {command_name} flag: ")) {
        return format!("command {command_name}: unknown flag {flag}");
    }
    if let Some(flag) = unexpected_flag_argument_for_command(&message, command_name) {
        return format!("command {command_name}: unknown flag {flag}");
    }
    if let Some(flag) = unsupported_flag_for_command(&message, command_name) {
        return format!("command {command_name}: unknown flag {flag}");
    }
    message
}

fn source_file_error_uses_line_prefix(command_name: &str, error: &RmuxError) -> bool {
    if !matches!(command_name, "link-window" | "move-window") {
        return true;
    }
    match error {
        RmuxError::InvalidTarget { reason, .. } => {
            reason != "window index already exists in session"
        }
        RmuxError::Server(message) => !message.starts_with("index in use: "),
        _ => true,
    }
}

fn window_index_from_target(value: &str) -> Option<&str> {
    let (_, window) = value.split_once(':')?;
    let window = window.split_once('.').map_or(window, |(window, _)| window);
    (!window.is_empty() && window.bytes().all(|byte| byte.is_ascii_digit())).then_some(window)
}

fn unexpected_flag_argument_for_command<'a>(
    message: &'a str,
    command_name: &str,
) -> Option<&'a str> {
    let flag = message.strip_prefix("unexpected argument '")?;
    let suffix = format!("' for {command_name}");
    let flag = flag.strip_suffix(&suffix)?;
    flag.starts_with('-').then_some(flag)
}

fn unsupported_flag_for_command<'a>(message: &'a str, command_name: &str) -> Option<&'a str> {
    let flag = message.strip_prefix("unsupported flag '")?;
    let suffix = format!("' for {command_name}");
    flag.strip_suffix(&suffix)
}

fn has_source_file_line_prefix(message: &str) -> bool {
    let Some((_, rest)) = message.split_once(':') else {
        return false;
    };
    let Some((line, _)) = rest.split_once(':') else {
        return false;
    };
    !line.is_empty() && line.bytes().all(|byte| byte.is_ascii_digit())
}

fn rmux_error_message(error: RmuxError) -> String {
    match error {
        RmuxError::Server(message) => message,
        other => other.to_string(),
    }
}

fn rmux_error_message_ref(error: &RmuxError) -> String {
    match error {
        RmuxError::Server(message) => message.clone(),
        other => other.to_string(),
    }
}

pub(super) fn command_parse_error_to_rmux(error: CommandParseError) -> RmuxError {
    RmuxError::Server(error.to_string())
}
