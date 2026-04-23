use std::path::Path;

use rmux_client::{connect_or_absent, detect_context, ClientContext, ConnectResult};
use rmux_proto::request::{
    AttachSessionExt2Request, KillSessionRequest, ListSessionsRequest, NewSessionExtRequest,
    SwitchClientExt3Request,
};
use rmux_proto::{ClientTerminalContext, ErrorResponse, Response};

use super::{
    attach_with_connection, build_terminal_size, connect_with_startserver, current_terminal_size,
    expect_command_success, optional_client_flags, resolve_current_session_target,
    resolve_session_target_or_current, resolve_session_target_spec, run_command_resolved,
    run_payload_command, run_switch_client_on_connection, unexpected_response,
    write_command_output, ExitFailure, StartupOptions,
};
use crate::cli_args::{
    KillSessionArgs, ListSessionsArgs, NewSessionArgs, RenameSessionArgs, SessionTargetArgs,
};

pub(super) fn run_new_session(
    args: NewSessionArgs,
    socket_path: &Path,
    startup: StartupOptions,
    client_terminal: ClientTerminalContext,
) -> Result<i32, ExitFailure> {
    let mut connection = connect_with_startserver(socket_path, startup)?;
    let client_flags = optional_client_flags(args.flags.clone());
    let client_size = current_terminal_size();
    let response = connection
        .new_session_extended(NewSessionExtRequest {
            session_name: args.session_name.clone(),
            detached: args.detached,
            size: build_terminal_size(args.cols, args.rows),
            environment: (!args.environment.is_empty()).then_some(args.environment),
            group_target: args.group_target,
            working_directory: args.working_directory,
            attach_if_exists: args.attach_if_exists,
            detach_other_clients: args.detach_other_clients || args.kill_other_clients,
            kill_other_clients: args.kill_other_clients,
            flags: client_flags.clone(),
            window_name: args.window_name,
            print_session_info: args.print_session_info,
            print_format: args.print_format,
            command: (!args.command.is_empty()).then_some(args.command),
        })
        .map_err(ExitFailure::from_client)?;
    let output = response.command_output().cloned();
    let target = match response {
        Response::NewSession(response) => response.session_name,
        other => {
            expect_command_success(other, "new-session")?;
            unreachable!("new-session success must return a new-session response")
        }
    };

    if let Some(output) = output {
        write_command_output(&output)?;
    }

    if args.detached {
        return Ok(0);
    }

    match detect_context() {
        ClientContext::Nested => run_switch_client_on_connection(
            &mut connection,
            SwitchClientExt3Request {
                target_client: None,
                target: Some(target.to_string()),
                key_table: None,
                last_session: false,
                next_session: false,
                previous_session: false,
                toggle_read_only: false,
                sort_order: None,
                skip_environment_update: false,
                zoom: false,
            },
        ),
        ClientContext::Outside => attach_with_connection(
            connection,
            AttachSessionExt2Request {
                target: Some(target.clone()),
                target_spec: Some(target.to_string()),
                detach_other_clients: false,
                kill_other_clients: false,
                read_only: false,
                skip_environment_update: false,
                flags: client_flags,
                working_directory: None,
                client_terminal,
                client_size,
            },
        ),
    }
}

pub(super) fn run_has_session(
    args: SessionTargetArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    match connect_or_absent(socket_path).map_err(ExitFailure::from_client)? {
        ConnectResult::Absent => Ok(1),
        ConnectResult::Connected(mut connection) => {
            let missing_message = args
                .target
                .as_ref()
                .map(|target| format!("can't find session: {target}"))
                .unwrap_or_else(|| "can't find session".to_owned());
            let target = match args.target.as_ref() {
                Some(target) => resolve_session_target_spec(&mut connection, target, false)
                    .map_err(map_has_session_lookup_error)?,
                None => resolve_current_session_target(&mut connection)?,
            };
            let response = connection
                .has_session(target)
                .map_err(ExitFailure::from_client)?;

            match response {
                Response::HasSession(response) => {
                    if response.exists {
                        Ok(0)
                    } else {
                        Err(ExitFailure::new(1, missing_message))
                    }
                }
                Response::Error(ErrorResponse { error }) => {
                    Err(ExitFailure::new(1, error.to_string()))
                }
                other => Err(unexpected_response("has-session", &other)),
            }
        }
    }
}

fn map_has_session_lookup_error(error: ExitFailure) -> ExitFailure {
    normalize_session_lookup_error(error, "can't find session: {}")
}

pub(super) fn run_kill_session(
    args: KillSessionArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    match connect_or_absent(socket_path).map_err(ExitFailure::from_client)? {
        ConnectResult::Absent => Ok(1),
        ConnectResult::Connected(mut connection) => {
            let target = resolve_session_target_or_current(
                &mut connection,
                args.target.as_ref(),
                "kill-session",
            )
            .map_err(map_kill_session_lookup_error)?;
            let response = connection
                .kill_session(KillSessionRequest {
                    target,
                    kill_all_except_target: args.kill_all_except_target,
                    clear_alerts: args.clear_alerts,
                })
                .map_err(ExitFailure::from_client)?;
            expect_command_success(response, "kill-session")?;
            Ok(0)
        }
    }
}

fn map_kill_session_lookup_error(error: ExitFailure) -> ExitFailure {
    normalize_session_lookup_error(error, "session not found: {}")
}

fn normalize_session_lookup_error(error: ExitFailure, format: &str) -> ExitFailure {
    const PREFIX: &str = "can't find session: ";

    if let Some((_, session_name)) = error.message().split_once(PREFIX) {
        return ExitFailure::new(1, format.replace("{}", session_name));
    }

    error
}

pub(super) fn run_rename_session(
    args: RenameSessionArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    run_command_resolved(socket_path, "rename-session", move |connection| {
        let target =
            resolve_session_target_or_current(connection, args.target.as_ref(), "rename-session")?;
        connection
            .rename_session(target, args.new_name)
            .map_err(ExitFailure::from_client)
    })
}

pub(super) fn run_list_sessions(
    args: ListSessionsArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    run_payload_command(socket_path, "list-sessions", move |connection| {
        connection.list_sessions(ListSessionsRequest {
            format: args.format,
            filter: args.filter,
            sort_order: args.sort_order,
            reversed: args.reversed,
        })
    })
}
