//! Explicit command-only split/key transport. Never falls back to boolean keep.
use std::io::Write;
use std::path::Path;

use rmux_proto::{Response, SplitWindowCommandV1Request, SplitWindowCommandV1Response};

use crate::cli::command_runner::with_command_connection;
use crate::cli::{unexpected_response, ExitFailure};
use crate::cli_args::SplitWindowArgs;
use crate::cli_response::tmux_cli_error_message;

pub(super) fn run(args: SplitWindowArgs, socket_path: &Path) -> Result<i32, ExitFailure> {
    let arguments = arguments(&args)?;
    let payload = if args.stdin && args.command.is_empty() {
        Some(super::split::read_stdin_payload()?)
    } else {
        None
    };
    with_command_connection(socket_path, |connection| {
        let response = connection.split_window_command_v1(SplitWindowCommandV1Request::new(
            arguments, std::env::current_dir().ok(), payload,
        )).map_err(|error| ExitFailure::new(1, format!("split-window: admission failed or result unknown; command not retried: {error}")))?;
        match response {
            Response::SplitWindowCommandV1(result) => match *result {
                SplitWindowCommandV1Response::NotAdmitted { error } => Err(ExitFailure::new(
                    1,
                    tmux_cli_error_message("split-window", &error),
                )),
                SplitWindowCommandV1Response::Complete {
                    stdout,
                    error,
                    exit_status,
                } => {
                    std::io::stdout()
                        .write_all(&stdout)
                        .map_err(|error| ExitFailure::new(1, error.to_string()))?;
                    if let Some(error) = error {
                        return Err(ExitFailure::new(
                            exit_status.max(1),
                            tmux_cli_error_message("split-window", &error),
                        ));
                    }
                    Ok(exit_status)
                }
            },
            Response::Error(error) => Err(ExitFailure::new(
                1,
                tmux_cli_error_message("split-window", &error.error),
            )),
            other => Err(unexpected_response("split-window", &other)),
        }
    })
}

fn arguments(args: &SplitWindowArgs) -> Result<Vec<String>, ExitFailure> {
    let mut argv = vec!["split-window".to_owned(), "-k".to_owned()];
    for (flag, enabled) in [
        ("-b", args.before),
        ("-d", args.detached),
        ("-f", args.full_size),
        ("-h", args.horizontal),
        ("-v", args.vertical),
        ("-Z", args.preserve_zoom),
        ("-P", args.print_target),
        ("-I", args.stdin),
    ] {
        if enabled {
            argv.push(flag.to_owned());
        }
    }
    if let Some(cwd) = &args.start_directory {
        let cwd = cwd.to_str().ok_or_else(|| {
            ExitFailure::new(1, "split-window -k requires a UTF-8 start directory")
        })?;
        argv.extend(["-c".to_owned(), cwd.to_owned()]);
    }
    for (flag, value) in [
        ("-t", args.target.as_ref().map(|target| target.raw())),
        ("-l", args.size.as_deref()),
        ("-p", args.percentage.as_deref()),
        ("-F", args.format.as_deref()),
    ] {
        if let Some(value) = value {
            argv.extend([flag.to_owned(), value.to_owned()]);
        }
    }
    for environment in &args.environment {
        argv.extend(["-e".to_owned(), environment.clone()]);
    }
    if !args.command.is_empty() {
        argv.push("--".to_owned());
        argv.extend(args.command.iter().cloned());
    }
    // argv parser's only delimiter is an unescaped trailing semicolon. These
    // values have already been parsed by the full CLI, so preserve them literally.
    for value in &mut argv {
        if value.ends_with(';') {
            value.insert(value.len() - 1, '\\');
        }
    }
    Ok(argv)
}

#[cfg(all(test, unix))]
#[path = "split_key_transport_tests.rs"]
mod transport_tests;
