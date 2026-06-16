use std::path::Path;

use rmux_client::connect;
use rmux_proto::{
    ErrorResponse, ProcessCommand, Request, Response, SplitWindowExtRequest, SplitWindowRequest,
};

use super::super::format_print::print_target_format;
use super::super::{
    resolve_current_pane_target, resolve_split_window_target_spec, unexpected_response, ExitFailure,
};
use crate::cli_args::SplitWindowArgs;

const DEFAULT_SPLIT_WINDOW_PRINT_FORMAT: &str = "#{session_name}:#{window_index}.#{pane_index}";

pub(in crate::cli) fn run_split_window(
    args: SplitWindowArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    let direction = args.direction();
    let print_target = args.print_target;
    let print_format = args
        .format
        .clone()
        .unwrap_or_else(|| DEFAULT_SPLIT_WINDOW_PRINT_FORMAT.to_owned());
    let mut connection = connect(socket_path)
        .map_err(|error| ExitFailure::from_client_connect(socket_path, error))?;
    let target = match args.target.as_ref() {
        Some(target) => resolve_split_window_target_spec(&mut connection, target)?,
        None => rmux_proto::SplitWindowTarget::Pane(resolve_current_pane_target(
            &mut connection,
            "split-window",
        )?),
    };
    let size = args.size_spec();
    let environment = (!args.environment.is_empty()).then_some(args.environment);
    let command = (!args.command.is_empty()).then_some(args.command);
    let stdin_to_empty_pane = args.stdin && command.is_none();
    let stdin_payload = if stdin_to_empty_pane {
        Some(read_stdin_payload()?)
    } else {
        None
    };
    let process_command = if stdin_to_empty_pane {
        Some(ProcessCommand::Shell(String::new()))
    } else {
        None
    };
    let response = if command.is_some()
        || process_command.is_some()
        || args.start_directory.is_some()
        || args.detached
        || size.is_some()
        || args.full_size
        || args.preserve_zoom
        || stdin_payload.is_some()
    {
        connection
            .roundtrip(&Request::SplitWindowExt(SplitWindowExtRequest {
                target: target.clone(),
                direction,
                before: args.before,
                environment,
                command,
                process_command,
                start_directory: args.start_directory,
                keep_alive_on_exit: stdin_to_empty_pane.then_some(true),
                detached: args.detached,
                size,
                preserve_zoom: args.preserve_zoom,
                full_size: args.full_size,
                stdin_payload,
            }))
            .map_err(ExitFailure::from_client)?
    } else {
        connection
            .roundtrip(&Request::SplitWindow(SplitWindowRequest {
                target: target.clone(),
                direction,
                before: args.before,
                environment,
            }))
            .map_err(ExitFailure::from_client)?
    };
    let pane = match response {
        Response::SplitWindow(response) => response.pane,
        Response::Error(ErrorResponse { error }) => {
            return Err(ExitFailure::new(1, error.to_string()))
        }
        other => return Err(unexpected_response("split-window", &other)),
    };

    if print_target {
        print_target_format(
            &mut connection,
            "split-window",
            rmux_proto::Target::Pane(pane.clone()),
            &print_format,
        )?;
    }
    Ok(0)
}

fn read_stdin_payload() -> Result<Vec<u8>, ExitFailure> {
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut std::io::stdin(), &mut bytes)
        .map_err(|error| ExitFailure::new(1, format!("failed to read stdin: {error}")))?;
    Ok(bytes)
}
