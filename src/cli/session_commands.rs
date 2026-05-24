use std::path::{Path, PathBuf};

use rmux_client::connect;
use rmux_client::{detect_context, ClientContext};
use rmux_proto::request::{AttachSessionExt2Request, SwitchClientExt3Request};
use rmux_proto::request::{KillSessionRequest, ListSessionsRequest, NewSessionExtRequest};
use rmux_proto::{ClientTerminalContext, ErrorResponse, Response};

use super::{attach_with_connection, current_terminal_size, run_switch_client_on_connection};
use super::{
    build_terminal_size, connect_with_startserver, expect_command_success, optional_client_flags,
    resolve_current_session_target, resolve_session_target_or_current, resolve_session_target_spec,
    run_command_resolved, run_payload_command, unexpected_response, write_command_output,
    ExitFailure, StartupOptions,
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
    let working_directory = args
        .working_directory
        .or_else(current_working_directory_string);
    let response = connection
        .new_session_extended(NewSessionExtRequest {
            session_name: args.session_name.clone(),
            detached: args.detached,
            size: build_terminal_size(args.cols, args.rows),
            environment: (!args.environment.is_empty()).then_some(args.environment),
            group_target: args.group_target,
            working_directory,
            attach_if_exists: args.attach_if_exists,
            detach_other_clients: args.detach_other_clients || args.kill_other_clients,
            kill_other_clients: args.kill_other_clients,
            flags: client_flags.clone(),
            window_name: args.window_name,
            print_session_info: args.print_session_info,
            print_format: args.print_format,
            command: (!args.command.is_empty()).then_some(args.command),
            process_command: None,
            // Atomically set passthrough server-side. A separate
            // post-create `set-option` would race with the initial
            // pane reader spawn: the reader captures its replay log
            // at spawn time, so a later flip would leave the
            // already-spawned reader holding `None` and losing all
            // bytes that flow through window 0.
            passthrough: args.passthrough,
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
        if args.passthrough {
            eprintln!("rmux: session '{target}' created in passthrough mode (detached)");
        }
        return Ok(0);
    }

    // For passthrough sessions, print a status line both before and
    // after attach so the user has *something* to read regardless of
    // how the inner program behaves.
    //
    // We write directly to /dev/tty (with stderr as fallback) because:
    // - the inner program commonly runs `clear` on startup (zsh,
    //   fish, ... — they all `\x1b[H\x1b[2J` to a fresh terminal),
    //   which wipes anything we wrote to stderr before attach;
    // - stderr may be redirected by the user (`2>/tmp/log`) — they'd
    //   still want to see *that the session ended* in the terminal;
    // - writing through /dev/tty pierces both of those and lands
    //   visibly in the user's terminal.
    let passthrough_attach = args.passthrough;
    if passthrough_attach {
        write_tty_line(&format!(
            "rmux: attaching to '{target}' (passthrough mode, no chrome)",
        ));
    }
    let result = match detect_context() {
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
    };
    if passthrough_attach {
        match &result {
            Ok(code) => write_tty_line(&format!(
                "rmux: detached from passthrough session '{target}' (exit {code})",
            )),
            Err(error) => write_tty_line(&format!(
                "rmux: passthrough attach to '{target}' failed: {}",
                error.message(),
            )),
        }
    }
    result
}

/// Writes `line` followed by `\r\n` to the controlling terminal.
/// Falls back to stderr if `/dev/tty` is unavailable (e.g. the
/// process has no controlling terminal — CI, daemon contexts).
///
/// **Why /dev/tty specifically:** the inner program at attach time
/// typically clears the screen on startup (`zsh`, `fish` and most
/// shells run something equivalent to `\x1b[H\x1b[2J`), wiping
/// anything we wrote to stderr beforehand. /dev/tty is a
/// process-controlling-terminal handle that bypasses both stderr
/// redirection and screen-clear side effects: the bytes land
/// directly on the terminal device. We also append `\r\n` instead
/// of just `\n` so the line renders cleanly even if the terminal
/// is still in raw mode when this runs.
fn write_tty_line(line: &str) {
    use std::io::Write;
    if let Ok(mut tty) = std::fs::OpenOptions::new().write(true).open("/dev/tty") {
        let _ = tty.write_all(line.as_bytes());
        let _ = tty.write_all(b"\r\n");
        let _ = tty.flush();
    } else {
        eprintln!("{line}");
    }
}

fn current_working_directory_string() -> Option<String> {
    current_working_directory().map(|path| path.to_string_lossy().into_owned())
}

fn current_working_directory() -> Option<PathBuf> {
    std::env::current_dir().ok().filter(|path| path.is_dir())
}

pub(super) fn run_has_session(
    args: SessionTargetArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    let mut connection = connect(socket_path)
        .map_err(|error| ExitFailure::from_client_connect(socket_path, error))?;
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
        Response::Error(ErrorResponse { error }) => Err(ExitFailure::new(1, error.to_string())),
        other => Err(unexpected_response("has-session", &other)),
    }
}

fn map_has_session_lookup_error(error: ExitFailure) -> ExitFailure {
    normalize_session_lookup_error(error, "can't find session: {}")
}

pub(super) fn run_kill_session(
    args: KillSessionArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    let mut connection = connect(socket_path)
        .map_err(|error| ExitFailure::from_client_connect(socket_path, error))?;
    let target =
        resolve_session_target_or_current(&mut connection, args.target.as_ref(), "kill-session")
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
