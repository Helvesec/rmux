use std::path::Path;

use rmux_client::connect;
use rmux_proto::{ErrorResponse, Request, Response, SplitWindowExtRequest, SplitWindowRequest};

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
    if args.stdin {
        return Err(ExitFailure::new(1, "split-window: unsupported flag -I"));
    }

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
    let environment = (!args.environment.is_empty()).then_some(args.environment);
    let command = (!args.command.is_empty()).then_some(args.command);
    let response = if command.is_some()
        || args.start_directory.is_some()
        || args.detached
        || args.size.is_some()
        || args.preserve_zoom
    {
        connection
            .roundtrip(&Request::SplitWindowExt(SplitWindowExtRequest {
                target: target.clone(),
                direction,
                before: args.before,
                environment,
                command,
                process_command: None,
                start_directory: args.start_directory,
                keep_alive_on_exit: None,
                detached: args.detached,
                size: args.size,
                preserve_zoom: args.preserve_zoom,
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
            rmux_proto::Target::Pane(pane),
            &print_format,
        )?;
    }

    Ok(0)
}
