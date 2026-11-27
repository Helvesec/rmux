use std::path::Path;

use rmux_client::{connect, Connection};
use rmux_proto::{NewWindowRequest, ProcessCommand, Request, ResolveTargetType, Response};

use super::super::command_runner::run_queued_server_command_at_target_with_connection;
use super::super::format_print::print_target_format;
use super::super::{
    expect_command_output, expect_command_success, resolve_current_pane_target,
    resolve_current_session_target, resolve_session_target_spec, resolve_target_spec,
    response_name_for_target, unexpected_response, ExitFailure,
};
use super::destination::{
    is_special_window_token, resolve_bare_relative_window_target,
    resolve_window_placement_anchor_target, signed_window_index_target,
};
use super::LIST_WINDOWS_FILTER_SEPARATOR;
use crate::cli_args::{NewWindowArgs, TargetSpec};

const DEFAULT_NEW_WINDOW_PRINT_FORMAT: &str = "#{session_name}:#{window_index}.#{pane_index}";

pub(in crate::cli) fn run_new_window(
    args: NewWindowArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    if args.kill_existing || args.select_existing {
        let mut connection = connect(socket_path)
            .map_err(|error| ExitFailure::from_client_connect(socket_path, error))?;
        let target = resolve_current_pane_target(&mut connection, "new-window")?;
        return run_queued_server_command_at_target_with_connection(
            &mut connection,
            "new-window",
            args.queue_command,
            target,
        );
    }

    let start_directory = args
        .start_directory
        .or_else(|| std::env::current_dir().ok());
    let print_target = args.print_target;
    let print_format = args
        .format
        .clone()
        .unwrap_or_else(|| DEFAULT_NEW_WINDOW_PRINT_FORMAT.to_owned());
    let mut connection = connect(socket_path)
        .map_err(|error| ExitFailure::from_client_connect(socket_path, error))?;
    let insert_at_target = args.after || args.before;
    let (target, target_window_index) = if insert_at_target {
        resolve_new_window_placement_target(
            &mut connection,
            args.target.as_ref(),
            args.after,
            "new-window",
        )?
    } else {
        resolve_new_window_target_spec(&mut connection, args.target.as_ref())?
    };
    let response = connection
        .roundtrip(&Request::NewWindow(Box::new(NewWindowRequest {
            target: target.clone(),
            target_window_index,
            name: args.name,
            detached: args.detached,
            environment: (!args.environment.is_empty()).then_some(args.environment),
            start_directory,
            process_command: ProcessCommand::from_legacy_command(Some(&args.command)),
            command: (!args.command.is_empty()).then_some(args.command),
            insert_at_target,
        })))
        .map_err(ExitFailure::from_client)?;
    let target = match response {
        Response::NewWindow(response) => response.target,
        response @ Response::Error(_) => {
            expect_command_success(response, "new-window")?;
            unreachable!("new-window error response should return from expect_command_success")
        }
        other => return Err(unexpected_response("new-window", &other)),
    };

    if print_target {
        let pane = rmux_proto::PaneTarget::with_window(
            target.session_name().clone(),
            target.window_index(),
            0,
        );
        print_target_format(
            &mut connection,
            "new-window",
            rmux_proto::Target::Pane(pane),
            &print_format,
        )?;
    }

    Ok(0)
}

fn resolve_new_window_placement_target(
    connection: &mut Connection,
    target: Option<&TargetSpec>,
    after: bool,
    command_name: &str,
) -> Result<(rmux_proto::SessionName, Option<u32>), ExitFailure> {
    let window = resolve_window_placement_anchor_target(connection, target, command_name)?;
    let window_index = if after {
        window.window_index().checked_add(1).ok_or_else(|| {
            ExitFailure::new(
                1,
                format!(
                    "window index space exhausted for session {}",
                    window.session_name()
                ),
            )
        })?
    } else {
        window.window_index()
    };
    Ok((window.session_name().clone(), Some(window_index)))
}

fn resolve_new_window_target_spec(
    connection: &mut Connection,
    target: Option<&TargetSpec>,
) -> Result<(rmux_proto::SessionName, Option<u32>), ExitFailure> {
    let Some(target) = target else {
        return resolve_current_session_target(connection).map(|session| (session, None));
    };

    if let Some(window) =
        resolve_bare_relative_window_target(connection, target.raw(), "new-window")?
    {
        return Ok((window.session_name().clone(), Some(window.window_index())));
    }

    if new_window_target_requests_window_index(target.raw()) {
        return match resolve_target_spec(
            connection,
            target,
            ResolveTargetType::Window,
            true,
            false,
        )? {
            rmux_proto::Target::Window(window) => {
                Ok((window.session_name().clone(), Some(window.window_index())))
            }
            other => Err(ExitFailure::new(
                1,
                format!(
                    "resolve-target produced {} where a new-window target was required",
                    response_name_for_target(&other)
                ),
            )),
        };
    }

    if let Some(window_target) = resolve_new_window_bare_window_target(connection, target)? {
        return Ok((
            window_target.session_name().clone(),
            Some(window_target.window_index()),
        ));
    }

    if let Some(session_name) = resolve_new_window_session_only_target(connection, target)? {
        return Ok((session_name, None));
    }

    match resolve_target_spec(connection, target, ResolveTargetType::Session, false, false)? {
        rmux_proto::Target::Session(session_name) => Ok((session_name, None)),
        other => Err(ExitFailure::new(
            1,
            format!(
                "resolve-target produced {} where a new-window target was required",
                response_name_for_target(&other)
            ),
        )),
    }
}

pub(super) fn new_window_target_requests_window_index(raw_target: &str) -> bool {
    if is_special_window_token(raw_target) {
        return true;
    }
    if !raw_target.is_empty() && raw_target.bytes().all(|byte| byte.is_ascii_digit()) {
        return true;
    }
    if signed_window_index_target(raw_target) {
        return true;
    }
    raw_target.split_once(':').is_some_and(|(_, window_part)| {
        (!window_part.is_empty() && !window_part.contains('.'))
            || signed_window_index_target(window_part)
    })
}

fn resolve_new_window_bare_window_target(
    connection: &mut Connection,
    target: &TargetSpec,
) -> Result<Option<rmux_proto::WindowTarget>, ExitFailure> {
    let raw = target.raw();
    if !new_window_target_is_bare_lookup(raw)
        || !matches!(target.exact(), None | Some(rmux_proto::Target::Session(_)))
    {
        return Ok(None);
    }

    match resolve_target_spec(connection, target, ResolveTargetType::Window, false, false) {
        Ok(rmux_proto::Target::Window(window)) => {
            if window_name_matches_target(connection, &window, raw)? {
                Ok(Some(window))
            } else {
                Ok(None)
            }
        }
        Ok(_) => Ok(None),
        Err(window_error) => match resolve_session_target_spec(connection, target, false) {
            Ok(_) => Ok(None),
            Err(session_error) if session_error.is_ambiguous_target() => Err(session_error),
            Err(_) => Err(window_error),
        },
    }
}

fn new_window_target_is_bare_lookup(raw_target: &str) -> bool {
    !raw_target.is_empty()
        && !raw_target.starts_with(['@', '$', '%', '+', '-', '='])
        && !raw_target.contains([':', '.'])
}

fn window_name_matches_target(
    connection: &mut Connection,
    window: &rmux_proto::WindowTarget,
    target: &str,
) -> Result<bool, ExitFailure> {
    let response = connection
        .list_windows(
            window.session_name().clone(),
            Some(format!(
                "#{{window_index}}{LIST_WINDOWS_FILTER_SEPARATOR}#{{window_name}}"
            )),
        )
        .map_err(ExitFailure::from_client)?;
    let output = expect_command_output(&response, "list-windows")?;
    for line in String::from_utf8_lossy(output.stdout()).lines() {
        let Some((index, window_name)) = line.split_once(LIST_WINDOWS_FILTER_SEPARATOR) else {
            continue;
        };
        if index != window.window_index().to_string() {
            continue;
        }
        return Ok(window_name == target
            || window_name.starts_with(target)
            || rmux_core::fnmatch(target, window_name));
    }
    Ok(false)
}

fn resolve_new_window_session_only_target(
    connection: &mut Connection,
    target: &TargetSpec,
) -> Result<Option<rmux_proto::SessionName>, ExitFailure> {
    let raw_target = target.raw();
    let Some((session_name, window_part)) = raw_target.split_once(':') else {
        return Ok(None);
    };
    if !window_part.is_empty() {
        return Ok(None);
    }
    let session_target = crate::cli_args::parse_target_spec(session_name)
        .map_err(|error| ExitFailure::new(1, error))?;
    resolve_session_target_spec(connection, &session_target, false).map(Some)
}
