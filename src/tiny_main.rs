//! Tiny public CLI dispatcher for release package builds.
#![cfg_attr(test, allow(dead_code))]

use std::env;
use std::ffi::{OsStr, OsString};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

#[cfg(not(windows))]
use rmux_client::attach_terminal_with_initial_bytes;
#[cfg(unix)]
use rmux_client::attach_terminal_with_initial_bytes_and_resize_geometry;
#[cfg(windows)]
use rmux_client::attach_terminal_with_initial_bytes_and_windows_console_key;
#[cfg(windows)]
use rmux_client::connect_for_server_shutdown;
use rmux_client::{
    connect, connect_or_absent, ensure_server_running_with_config_outcome, resolve_socket_path,
    resolve_tmux_compatible_socket_path, wait_for_server_endpoint_cleanup, AttachTransition,
    AutoStartConfig, ClientError, ConnectResult, Connection, EnsuredServerConnection,
    ServerConnectionProvenance, StartServerError,
};
use rmux_core::formats::{DEFAULT_LIST_PANES_ALL_FORMAT, DEFAULT_LIST_PANES_WINDOW_FORMAT};
use rmux_core::{
    command_inventory::RMUX_EXTENSION_COMMANDS,
    command_parser::{CommandParser, ParsedCommands},
};
use rmux_proto::request::{
    AttachSessionExt2Request, AttachSessionExt3Request, KillSessionRequest, NewSessionExtRequest,
};
#[cfg(unix)]
use rmux_proto::CAPABILITY_ATTACH_SERVER_EXITED;
use rmux_proto::{
    CapturePaneTargetActionRequest, JoinPaneRequest, ListSessionsRequest, OptionScopeSelector,
    ResizePaneTargetActionRequest, ResolveTargetType, Response, RmuxError, SetOptionMode,
    SourceFileResponse, SplitDirection, SplitWindowTargetActionRequest, Target,
    CAPABILITY_ATTACH_RENDER, CAPABILITY_ATTACH_RESIZE_GEOMETRY, RMUX_WIRE_VERSION,
};
#[cfg(windows)]
use rmux_proto::{CAPABILITY_ATTACH_WINDOWS_CONSOLE_KEY, CAPABILITY_DAEMON_STATUS};

mod helper;
mod output;
mod parse;
mod runtime_execution;
mod trace;

use crate::cli_top_level as top_level;
use crate::client_terminal::require_attach_terminal;
use crate::command_alias_snapshot::{decode_command_alias_definitions, definition_matches_name};
use crate::runtime_command_expansion::{
    expand_runtime_command_segment, RuntimeCommandExpansionError,
};
use crate::tmux_error_surface::{source_file_error_uses_stdout, tmux_cli_error_message};
#[cfg(not(windows))]
use helper::daemon_helper_path;
use helper::exec_full_helper;
use output::{client_error, write_response_output_or_error, write_stderr, write_stdout};
use parse::{
    parse_attach_session, parse_capture_pane, parse_display_message, parse_has_session,
    parse_join_pane, parse_kill_pane, parse_kill_session, parse_list_panes, parse_list_windows,
    parse_new_session, parse_new_window, parse_rename_window, parse_resize_pane,
    parse_select_window, parse_send_keys, parse_set_option, parse_show_options, parse_source_file,
    parse_split_window, TinyDisplayMessage, TinyHasSession, TinyJoinPane, TinyKillPane,
    TinyKillSession, TinyListPanes, TinyListWindows, TinyNewWindow, TinyRenameWindow,
    TinySelectWindow, TinySendKeys, TinySetOption, TinyShowOptions, TinySourceFile,
};
use trace::{trace_direct, trace_fallback};

pub(crate) fn main() {
    let args: Vec<OsString> = env::args_os().collect();
    if let Some(code) = crate::source_stdin::reader_exit_code(&args) {
        std::process::exit(code);
    }
    let result = match TinyInvocation::parse(&args) {
        TinyInvocation::Version => {
            let version = if invoked_as_tmux(&args) {
                "tmux 3.4".to_owned()
            } else {
                format!("rmux {}", env!("CARGO_PKG_VERSION"))
            };
            write_stdout(format!("{version}\n").as_bytes())
                .map(|()| 0)
                .map_err(|error| error.to_string())
        }
        TinyInvocation::Direct(command) => {
            let mut command_connection = TinyCommandConnection::disconnected(command.socket_path());
            match command.runtime_alias_dispatch(&args, &mut command_connection) {
                Ok(RuntimeAliasDispatch::FullHelper) => {
                    trace_fallback("runtime command-alias");
                    exec_full_helper(&args)
                }
                Ok(RuntimeAliasDispatch::Complete(code)) => Ok(code),
                Ok(RuntimeAliasDispatch::Direct) => {
                    trace_direct(command.name());
                    command.run(&args, &mut command_connection)
                }
                Err(error) => Err(error),
            }
        }
        TinyInvocation::Fallback => {
            trace_fallback("unsupported invocation");
            exec_full_helper(&args)
        }
    };

    match result {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            if !error.is_empty() {
                let _ = writeln!(io::stderr().lock(), "{error}");
            }
            std::process::exit(1);
        }
    }
}

enum TinyInvocation {
    Version,
    Direct(Box<TinyCommand>),
    Fallback,
}

enum RuntimeAliasDispatch {
    FullHelper,
    Direct,
    Complete(i32),
}

#[derive(Debug)]
struct TinyCommandConnection<C = Connection> {
    socket_path: PathBuf,
    connection: Option<C>,
}

impl<C> TinyCommandConnection<C> {
    fn disconnected(socket_path: &Path) -> Self {
        Self {
            socket_path: socket_path.to_path_buf(),
            connection: None,
        }
    }

    fn reusable(socket_path: &Path, connection: C) -> Self {
        Self {
            socket_path: socket_path.to_path_buf(),
            connection: Some(connection),
        }
    }

    fn take_reusable(&mut self, socket_path: &Path) -> Option<C> {
        (self.socket_path == socket_path)
            .then(|| self.connection.take())
            .flatten()
    }
}

impl TinyCommandConnection {
    fn connect(&mut self, socket_path: &Path) -> Result<Connection, String> {
        if let Some(connection) = self.take_reusable(socket_path) {
            return Ok(connection);
        }
        connect(socket_path).map_err(|error| client_error(socket_path, error))
    }
}

enum TinyCommand {
    StartServer {
        socket_path: PathBuf,
    },
    ListSessions {
        socket_path: PathBuf,
    },
    HasSession {
        socket_path: PathBuf,
        request: TinyHasSession,
    },
    ListWindows {
        socket_path: PathBuf,
        request: TinyListWindows,
    },
    ListPanes {
        socket_path: PathBuf,
        request: TinyListPanes,
    },
    KillServer {
        socket_path: PathBuf,
    },
    CapturePane {
        socket_path: PathBuf,
        request: CapturePaneTargetActionRequest,
    },
    AttachSession {
        socket_path: PathBuf,
        request: AttachSessionExt2Request,
    },
    SplitWindow {
        socket_path: PathBuf,
        request: SplitWindowTargetActionRequest,
    },
    NewWindow {
        socket_path: PathBuf,
        request: TinyNewWindow,
    },
    NewSession {
        socket_path: PathBuf,
        request: NewSessionExtRequest,
    },
    KillSession {
        socket_path: PathBuf,
        request: TinyKillSession,
    },
    ShowOptions {
        socket_path: PathBuf,
        request: TinyShowOptions,
        command_name: &'static str,
    },
    RenameWindow {
        socket_path: PathBuf,
        request: TinyRenameWindow,
    },
    SelectWindow {
        socket_path: PathBuf,
        request: TinySelectWindow,
    },
    KillPane {
        socket_path: PathBuf,
        request: TinyKillPane,
    },
    JoinPane {
        socket_path: PathBuf,
        request: TinyJoinPane,
    },
    SetOption {
        socket_path: PathBuf,
        request: TinySetOption,
        command_name: &'static str,
    },
    ResizePane {
        socket_path: PathBuf,
        request: ResizePaneTargetActionRequest,
    },
    DisplayMessage {
        socket_path: PathBuf,
        request: TinyDisplayMessage,
    },
    SendKeys {
        socket_path: PathBuf,
        request: TinySendKeys,
    },
    SourceFile {
        socket_path: PathBuf,
        request: TinySourceFile,
    },
}

mod attach_transport;
mod command;
mod invocation;
mod pane_transport;
mod server_transport;
use attach_transport::*;
use pane_transport::*;
use server_transport::*;

fn tiny_invoked_command_arguments(args: &[OsString]) -> Option<Vec<String>> {
    let mut index = 1;
    while index < args.len() {
        match args[index].to_str()? {
            "-L" | "-S" => index += 2,
            _ => {
                return args[index..]
                    .iter()
                    .map(|argument| argument.to_str().map(str::to_owned))
                    .collect();
            }
        }
    }
    Some(Vec::new())
}

fn tiny_socket(
    tmux_compatible: bool,
    socket_name: Option<&OsStr>,
    socket_path: Option<&Path>,
) -> Option<PathBuf> {
    if tmux_compatible {
        resolve_tmux_compatible_socket_path(socket_name, socket_path).ok()
    } else {
        resolve_socket_path(socket_name, socket_path).ok()
    }
}

fn invoked_as_tmux(args: &[OsString]) -> bool {
    invoked_as_tmux_from(
        args,
        env::var_os("RMUX_INTERNAL_INVOKED_AS_TMUX").as_deref(),
    )
}

fn invoked_as_tmux_from(args: &[OsString], internal_override: Option<&OsStr>) -> bool {
    if internal_override.is_some_and(|value| value == "1") {
        return true;
    }
    args.first()
        .and_then(|arg| Path::new(arg).file_stem())
        .and_then(OsStr::to_str)
        .is_some_and(|stem| stem.eq_ignore_ascii_case("tmux"))
}

fn env_var_nonempty(name: &str) -> bool {
    env::var_os(name).is_some_and(|value| !value.is_empty())
}

#[derive(Clone, Copy)]
enum RetryPolicy {
    DecodeOnly,
    DecodeOrEof,
}

fn run_target_action(
    original_args: &[OsString],
    socket_path: &Path,
    command: &str,
    retry_policy: RetryPolicy,
    command_connection: &mut TinyCommandConnection,
    send: impl FnOnce(&mut rmux_client::Connection) -> Result<Response, ClientError>,
) -> Result<i32, String> {
    let mut connection = command_connection.connect(socket_path)?;
    let response = send(&mut connection);
    if response_needs_full_helper(&response, retry_policy) {
        return exec_full_helper(original_args);
    }
    let response = response.map_err(|error| error.to_string())?;
    write_response_output_or_error(response, command)
}

fn connect_with_validated_startup(socket_path: &Path) -> Result<Connection, String> {
    connect_with_validated_startup_outcome(socket_path)
        .map(EnsuredServerConnection::into_connection)
}

fn connect_with_validated_startup_outcome(
    socket_path: &Path,
) -> Result<EnsuredServerConnection, String> {
    ensure_server_running_with_config_outcome(socket_path, default_auto_start_config()?)
        .map_err(|error| error.to_string())
}

fn default_auto_start_config() -> Result<AutoStartConfig, String> {
    let config = AutoStartConfig::default_files(true, env::current_dir().ok());

    #[cfg(windows)]
    {
        Ok(config)
    }

    #[cfg(not(windows))]
    {
        let helper = daemon_helper_path()?;
        Ok(config.with_binary_override(helper))
    }
}

fn server_has_sessions(connection: &mut Connection) -> Result<bool, String> {
    let response = connection
        .list_sessions(ListSessionsRequest {
            format: None,
            filter: None,
            sort_order: None,
            reversed: false,
        })
        .map_err(|error| error.to_string())?;
    match response {
        Response::ListSessions(response) => Ok(!response.command_output().stdout().is_empty()),
        Response::Error(error) => {
            write_response_output_or_error(Response::Error(error), "list-sessions").map(|_| false)
        }
        other => write_response_output_or_error(other, "list-sessions").map(|_| false),
    }
}

fn write_source_file_response(response: Response) -> Result<i32, String> {
    match response {
        Response::SourceFile(response) => write_source_file_success_response(response),
        Response::Error(error) if source_file_error_uses_stdout(&error.error) => {
            let message = tmux_cli_error_message("source-file", &error.error);
            write_stdout(format!("{message}\n").as_bytes()).map_err(|error| error.to_string())?;
            Ok(1)
        }
        other => write_response_output_or_error(other, "source-file"),
    }
}

fn write_source_file_success_response(response: SourceFileResponse) -> Result<i32, String> {
    if let Some(output) = response.command_output() {
        write_stdout(output.stdout()).map_err(|error| error.to_string())?;
    }
    if !response.stderr().is_empty() {
        write_stderr(response.stderr()).map_err(|error| error.to_string())?;
    }
    Ok(response.exit_status().unwrap_or(0))
}

fn run_legacy_wire_kill_server(socket_path: &Path, wire_version: u32) -> Result<i32, String> {
    let mut connection =
        match connect_or_absent(socket_path).map_err(|error| client_error(socket_path, error))? {
            ConnectResult::Connected(connection) => connection,
            ConnectResult::Absent => {
                wait_for_killed_server_socket_cleanup(socket_path)?;
                return Ok(0);
            }
        };

    let shutdown = connection.kill_server_legacy_wire(wire_version);
    drop(connection);
    match shutdown {
        Ok(()) => {
            wait_for_killed_server_socket_cleanup(socket_path)?;
            Ok(0)
        }
        Err(error) if kill_server_connection_closed(&error) => {
            wait_for_killed_server_socket_cleanup(socket_path)?;
            Ok(0)
        }
        Err(error) => Err(error.to_string()),
    }
}

fn kill_server_connection_closed(error: &ClientError) -> bool {
    matches!(error, ClientError::UnexpectedEof)
        || matches!(
            error,
            ClientError::Io(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::BrokenPipe
                        | io::ErrorKind::ConnectionReset
                        | io::ErrorKind::UnexpectedEof
                )
        )
}

fn legacy_shutdown_fallback_wire_version(error: &ClientError) -> Option<u32> {
    match error {
        ClientError::Protocol(error) => previous_wire_error(error),
        _ => None,
    }
}

fn previous_wire_error(error: &RmuxError) -> Option<u32> {
    match error {
        RmuxError::UnsupportedWireVersion { got, .. } if (1..RMUX_WIRE_VERSION).contains(got) => {
            Some(*got)
        }
        _ => None,
    }
}

#[cfg(test)]
fn incompatible_daemon_error(socket_path: &Path) -> String {
    format!(
        "rmux: running daemon on '{}' uses an incompatible protocol.\nrmux: stopping this daemon ends its sessions and running processes. Save your work with the matching client first.\nrmux: run `{}` to stop it, then retry.",
        socket_path.display(),
        incompatible_daemon_kill_server_command(socket_path)
    )
}

#[cfg(test)]
fn incompatible_daemon_kill_server_command(socket_path: &Path) -> String {
    if rmux_client::default_socket_path()
        .ok()
        .as_deref()
        .is_some_and(|default_path| default_path == socket_path)
    {
        return "rmux kill-server".to_owned();
    }

    format!("rmux -S {} kill-server", shell_quote_path(socket_path))
}

#[cfg(test)]
fn shell_quote_path(path: &Path) -> String {
    let text = path.display().to_string();
    if !text.is_empty()
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"/._-+=:@".contains(&byte))
    {
        return text;
    }

    #[cfg(windows)]
    {
        format!("\"{}\"", text.replace('"', "\"\""))
    }
    #[cfg(not(windows))]
    format!("'{}'", text.replace('\'', "'\\''"))
}

fn response_needs_full_helper(
    response: &Result<Response, ClientError>,
    retry_policy: RetryPolicy,
) -> bool {
    let decode_error = matches!(
        response,
        Ok(Response::Error(error)) if matches!(error.error, RmuxError::Decode(_))
    );
    let eof_retry = matches!(retry_policy, RetryPolicy::DecodeOrEof)
        && matches!(response, Err(ClientError::UnexpectedEof));
    decode_error || eof_retry
}

fn response_needs_session_resolution_retry(response: &Response) -> bool {
    matches!(
        response,
        Response::Error(error)
            if matches!(
                error.error,
                RmuxError::SessionNotFound(_) | RmuxError::InvalidTarget { .. }
            )
    )
}

fn response_is_decode_error(response: &Response) -> bool {
    matches!(response, Response::Error(error) if matches!(error.error, RmuxError::Decode(_)))
}

fn wait_for_killed_server_socket_cleanup(socket_path: &Path) -> Result<(), String> {
    wait_for_server_endpoint_cleanup(socket_path).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests;

#[cfg(test)]
#[test]
fn l1_034_v2_and_sequential_error_fall_back_to_full_cli() {
    for argv in [
        vec!["rmux", "export-window-snapshot-v2", "-t", "sample:0"],
        vec!["rmux", "restore-window-snapshot-v2", "-t", "sample:0", "-"],
        vec!["rmux", "resize-pane", "-t", "%1", "-x10", "-ybad"],
        vec!["rmux", "resize-pane", "-t", "%1", "-x25%", "-ybad"],
    ] {
        let args: Vec<OsString> = argv.iter().map(OsString::from).collect();
        assert!(
            matches!(TinyInvocation::parse(&args), TinyInvocation::Fallback),
            "{argv:?}"
        );
        assert!(crate::cli_args::parse(argv).is_ok());
    }
}

#[cfg(test)]
mod delegate_help_tests;
