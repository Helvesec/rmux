use crate::cli::ExitFailure;
use crate::cli_args::{SetOptionArgs, SetOptionCommandKind};

pub(super) fn command_error(command: SetOptionCommandKind, error: ExitFailure) -> ExitFailure {
    // SourceFile diagnostics carry the Display form of the option error. Use
    // the same CLI rendering as the direct named-option response.
    let Some(message) = error.message().strip_prefix("invalid set-option request: ") else {
        return error;
    };
    ExitFailure::new(
        error.exit_code(),
        crate::tmux_error_surface::tmux_cli_error_message(
            command.command_name(),
            &rmux_proto::RmuxError::InvalidSetOption(message.to_owned()),
        ),
    )
}

pub(super) fn command_arguments(
    command: SetOptionCommandKind,
    args: &SetOptionArgs,
) -> Result<String, ExitFailure> {
    // The named-option wire request has no quiet flag. Preserve it through the
    // command queue so the server decides the no-op and its success hooks.
    let mut arguments = vec![command.command_name().to_owned()];
    for (enabled, flag) in [
        (args.global, "-g"),
        (args.server, "-s"),
        (args.window, "-w"),
        (args.pane, "-p"),
        (args.append, "-a"),
        (args.format, "-F"),
        (args.only_if_unset, "-o"),
        (args.quiet, "-q"),
        (args.unset, "-u"),
        (args.unset_pane_overrides, "-U"),
    ] {
        if enabled {
            arguments.push(flag.to_owned());
        }
    }
    if let Some(target) = &args.target {
        arguments.extend(["-t".to_owned(), target.raw().to_owned()]);
    }
    arguments.push("--".to_owned());
    arguments.push(args.option.clone());
    arguments.extend(args.value.iter().cloned());
    // CLI argv splitting already decoded escaped trailing semicolons. Serialize
    // these scalar values without interpreting them as command separators again.
    let command = arguments
        .into_iter()
        .map(|argument| {
            rmux_core::command_parser::CommandArgument::String(argument).to_tmux_reparse_string()
        })
        .collect::<Vec<_>>()
        .join(" ");
    rmux_core::command_parser::CommandParser::new()
        .with_max_command_bytes(rmux_core::command_parser::SOURCE_FILE_MAX_COMMAND_BYTES)
        .parse_one_group(&command)
        .map(|parsed| parsed.to_tmux_reparse_string())
        .map_err(|error| ExitFailure::new(1, error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queued_option_errors_use_direct_diagnostics_and_keep_exit_status() {
        for command in [
            SetOptionCommandKind::SetOption,
            SetOptionCommandKind::SetWindowOption,
        ] {
            for message in ["unknown value: invalid-size", "bad value: invalid-flag"] {
                let error = command_error(
                    command,
                    ExitFailure::new(7, format!("invalid set-option request: {message}")),
                );
                assert_eq!(error.exit_code(), 7);
                assert_eq!(error.message(), message);
            }
        }
    }

    #[test]
    fn queued_option_errors_preserve_unrecognized_diagnostics() {
        for message in [
            "can't find window: missing",
            "invalid set-option request: custom error",
        ] {
            let error = command_error(
                SetOptionCommandKind::SetOption,
                ExitFailure::new(1, message),
            );
            assert_eq!(error.exit_code(), 1);
            assert_eq!(error.message(), message);
        }
    }
}
