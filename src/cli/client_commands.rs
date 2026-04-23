use std::path::Path;

use rmux_client::{
    attach_terminal, connect, connect_or_absent, detect_context, drive_control_mode,
    AttachTransition, ClientContext, ConnectResult, Connection, ControlTransition,
};
use rmux_proto::request::{
    AttachSessionExt2Request, DetachClientExtRequest, ListClientsRequest, RefreshClientRequest,
    SuspendClientRequest, SwitchClientExt3Request,
};
use rmux_proto::{ClientTerminalContext, ControlMode, ErrorResponse, Response};

use super::{
    connect_with_startserver, current_terminal_size, expect_command_success,
    finish_command_success, resolve_session_target_spec, run_command, run_payload_command_resolved,
    unexpected_response, ExitFailure, StartupOptions,
};
use crate::cli_args::{
    AttachSessionArgs, Cli, DetachClientArgs, ListClientsArgs, RefreshClientArgs,
    SuspendClientArgs, SwitchClientArgs,
};

pub(super) fn client_terminal_context_from_cli(cli: &Cli) -> ClientTerminalContext {
    let mut terminal_features = cli
        .terminal_features()
        .iter()
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|feature| !feature.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    if cli.assume_256_colors {
        terminal_features.push("256".to_owned());
    }

    ClientTerminalContext {
        terminal_features,
        utf8: cli.utf8,
    }
}

pub(super) fn run_attach_session(
    args: AttachSessionArgs,
    socket_path: &Path,
    startup: StartupOptions,
    client_terminal: ClientTerminalContext,
) -> Result<i32, ExitFailure> {
    let nested_context = detect_context() == ClientContext::Nested;
    if nested_context {
        validate_nested_attach_session(&args)?;
    }
    let nested_target = args.target.as_ref().map(ToString::to_string);
    let target_spec = args.target.as_ref().map(ToString::to_string);
    let nested_skip_environment_update = args.skip_environment_update;
    let nested_toggle_read_only = args.read_only;
    let mut connection = connect_with_startserver(socket_path, startup)?;
    let target = args
        .target
        .as_ref()
        .map(|target| resolve_session_target_spec(&mut connection, target, false))
        .transpose()?;
    let request = AttachSessionExt2Request {
        target,
        target_spec,
        detach_other_clients: args.detach_other_clients || args.kill_other_clients,
        kill_other_clients: args.kill_other_clients,
        read_only: args.read_only,
        skip_environment_update: args.skip_environment_update,
        flags: optional_client_flags(args.flags),
        working_directory: args.working_directory,
        client_terminal,
        client_size: current_terminal_size(),
    };

    if nested_context {
        return run_switch_client_on_connection(
            &mut connection,
            SwitchClientExt3Request {
                target_client: None,
                target: nested_target,
                key_table: None,
                last_session: false,
                next_session: false,
                previous_session: false,
                toggle_read_only: nested_toggle_read_only,
                sort_order: None,
                skip_environment_update: nested_skip_environment_update,
                zoom: false,
            },
        );
    }

    attach_with_connection(connection, request)
}

fn validate_nested_attach_session(args: &AttachSessionArgs) -> Result<(), ExitFailure> {
    let mut unsupported = Vec::new();
    if args.working_directory.is_some() {
        unsupported.push("-c");
    }
    if args.detach_other_clients {
        unsupported.push("-d");
    }
    if !args.flags.is_empty() {
        unsupported.push("-f");
    }
    if args.read_only {
        unsupported.push("-r");
    }
    if args.kill_other_clients {
        unsupported.push("-x");
    }

    if !unsupported.is_empty() {
        return Err(ExitFailure::new(
            1,
            format!(
                "attach-session inside an attached client supports only -E and -t; unsupported: {}",
                unsupported.join(", ")
            ),
        ));
    }

    if args.target.is_none() {
        return Err(ExitFailure::new(
            1,
            "attach-session inside an attached client requires -t",
        ));
    }

    Ok(())
}

pub(super) fn run_control_mode(
    cli: &Cli,
    socket_path: &Path,
    startup: StartupOptions,
) -> Result<i32, ExitFailure> {
    let connection = connect_with_startserver(socket_path, startup)?;
    match connection
        .begin_control_mode(
            ControlMode::from_count(cli.control_mode),
            client_terminal_context_from_cli(cli),
        )
        .map_err(ExitFailure::from_client)?
    {
        ControlTransition::Upgraded(upgrade) => {
            drive_control_mode(upgrade, cli.control_command_lines())
                .map_err(ExitFailure::from_client)?;
            Ok(0)
        }
        ControlTransition::Rejected(Response::Error(ErrorResponse { error })) => {
            Err(ExitFailure::new(1, error.to_string()))
        }
        ControlTransition::Rejected(response) => {
            Err(unexpected_response("control-mode", &response))
        }
    }
}

pub(super) fn run_switch_client(
    args: SwitchClientArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    let mut connection = connect(socket_path)
        .map_err(|error| ExitFailure::from_client_connect(socket_path, error))?;
    run_switch_client_on_connection(
        &mut connection,
        SwitchClientExt3Request {
            target_client: args.target_client,
            target: args.target,
            key_table: args.key_table,
            last_session: args.last_session,
            next_session: args.next_session,
            previous_session: args.previous_session,
            toggle_read_only: args.toggle_read_only,
            sort_order: args.sort_order,
            skip_environment_update: args.skip_environment_update,
            zoom: args.zoom,
        },
    )
}

pub(super) fn run_switch_client_on_connection(
    connection: &mut Connection,
    request: SwitchClientExt3Request,
) -> Result<i32, ExitFailure> {
    let response = connection
        .switch_client_with_target_selector(request)
        .map_err(ExitFailure::from_client)?;
    expect_command_success(response, "switch-client")?;
    Ok(0)
}

pub(super) fn run_refresh_client(
    args: RefreshClientArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    run_command(socket_path, "refresh-client", move |connection| {
        connection.refresh_client(RefreshClientRequest {
            target_client: args.target_client,
            adjustment: args.adjustment,
            clear_pan: args.clear_pan,
            pan_left: args.pan_left,
            pan_right: args.pan_right,
            pan_up: args.pan_up,
            pan_down: args.pan_down,
            status_only: args.status_only,
            clipboard_query: args.clipboard_query,
            flags: args.flags,
            flags_alias: args.flags_alias,
            subscriptions: args.subscriptions,
            subscriptions_format: args.subscriptions_format,
            control_size: args.control_size,
            colour_report: args.colour_report,
        })
    })
}

pub(super) fn run_list_clients(
    args: ListClientsArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    run_payload_command_resolved(socket_path, "list-clients", move |connection| {
        let target_session = args
            .target_session
            .as_ref()
            .map(|target| resolve_session_target_spec(connection, target, false))
            .transpose()?;
        connection
            .list_clients(ListClientsRequest {
                format: args.format,
                filter: args.filter,
                sort_order: args.sort_order,
                reversed: args.reversed,
                target_session,
            })
            .map_err(ExitFailure::from_client)
    })
}

pub(super) fn run_detach_client(
    args: DetachClientArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    match connect_or_absent(socket_path).map_err(ExitFailure::from_client)? {
        ConnectResult::Absent => Err(ExitFailure::new(1, "rmux server is not running")),
        ConnectResult::Connected(mut connection) => {
            let response = connection
                .detach_client_extended(DetachClientExtRequest {
                    target_client: args.target_client,
                    all_other_clients: args.all_other_clients,
                    target_session: args.target_session,
                    kill_on_detach: args.kill_on_detach,
                    exec_command: args.exec_command,
                })
                .map_err(ExitFailure::from_client)?;
            finish_command_success(response, "detach-client")
        }
    }
}

pub(super) fn run_suspend_client(
    args: SuspendClientArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    run_command(socket_path, "suspend-client", move |connection| {
        connection.suspend_client(SuspendClientRequest {
            target_client: args.target_client,
        })
    })
}

pub(super) fn attach_with_connection(
    connection: Connection,
    request: AttachSessionExt2Request,
) -> Result<i32, ExitFailure> {
    match connection
        .begin_attach_with_target_spec(request)
        .map_err(ExitFailure::from_client)?
    {
        AttachTransition::Upgraded(upgrade) => {
            let stream = upgrade.into_stream();
            attach_terminal(stream).map_err(ExitFailure::from_client)?;
            Ok(0)
        }
        AttachTransition::Rejected(response) => {
            expect_command_success(response, "attach-session")?;
            Ok(0)
        }
    }
}

pub(super) fn optional_client_flags(flags: Vec<String>) -> Option<Vec<String>> {
    (!flags.is_empty()).then_some(flags)
}
