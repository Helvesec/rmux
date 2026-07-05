use std::path::{Path, PathBuf};

use rmux_client::connect;
use rmux_client::{detect_context, detect_parent, ClientContext, ClientContextParent};
use rmux_proto::request::{AttachSessionExt2Request, SwitchClientExt3Request};
use rmux_proto::request::{KillSessionRequest, ListSessionsRequest, NewSessionExtRequest};
use rmux_proto::{ClientTerminalContext, ErrorResponse, Response};

use super::json_output::{list_sessions_json_format, write_list_sessions_json};
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
    validate_new_session_size(args.cols, args.rows)?;

    if !args.detached && detect_parent() == ClientContextParent::Tmux {
        return Err(ExitFailure::new(
            1,
            "sessions should be nested with care, unset $TMUX to force",
        ));
    }

    let mut connection = connect_with_startserver(socket_path, startup)?;
    let client_flags = optional_client_flags(args.flags.clone());
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
            client_environment: invoking_client_environment(),
            skip_environment_update: args.skip_environment_update,
        })
        .map_err(ExitFailure::from_client)?;
    let output = response.command_output().cloned();
    let (target, detached) = match response {
        Response::NewSession(response) => (response.session_name, response.detached),
        other => {
            expect_command_success(other, "new-session")?;
            unreachable!("new-session success must return a new-session response")
        }
    };

    if let Some(output) = output {
        write_command_output(&output)?;
    }

    if detached {
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
                client_size: current_terminal_size(),
            },
        ),
    }
}

fn validate_new_session_size(cols: Option<u16>, rows: Option<u16>) -> Result<(), ExitFailure> {
    if cols == Some(0) {
        return Err(ExitFailure::new(1, "width too small"));
    }
    if rows == Some(0) {
        return Err(ExitFailure::new(1, "height too small"));
    }
    Ok(())
}

fn current_working_directory_string() -> Option<String> {
    current_working_directory().map(|path| path.to_string_lossy().into_owned())
}

#[cfg(windows)]
const RMUX_CLIENT_SHELL_ENV: &str = "RMUX_CLIENT_SHELL";
#[cfg(windows)]
const INTERNAL_CLIENT_SHELL_ENV: &str = "RMUX_INTERNAL_CLIENT_SHELL";
#[cfg(windows)]
const PUBLIC_BINARY_OVERRIDE_ENV: &str = "RMUX_INTERNAL_PUBLIC_BINARY_PATH";
#[cfg(windows)]
const INTERNAL_TMUX_COMPAT_ENV: &str = "RMUX_INTERNAL_INVOKED_AS_TMUX";

#[cfg(windows)]
fn invoking_client_environment() -> Option<Vec<String>> {
    let shell = invoking_client_shell().or_else(internal_client_shell_handoff);
    Some(windows_invoking_client_environment(
        std::env::vars_os(),
        shell,
    ))
}

#[cfg(windows)]
fn windows_invoking_client_environment<I>(vars: I, shell: Option<String>) -> Vec<String>
where
    I: IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
{
    let mut environment = vars
        .into_iter()
        .map(|(name, value)| {
            (
                name.to_string_lossy().into_owned(),
                value.to_string_lossy().into_owned(),
            )
        })
        .filter(|(name, _)| !name.starts_with('='))
        .filter(|(name, _)| !name.eq_ignore_ascii_case(RMUX_CLIENT_SHELL_ENV))
        .filter(|(name, _)| !name.eq_ignore_ascii_case(INTERNAL_CLIENT_SHELL_ENV))
        .filter(|(name, _)| !name.eq_ignore_ascii_case(INTERNAL_TMUX_COMPAT_ENV))
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>();

    if let Some(shell) = shell {
        environment.push(format!("{RMUX_CLIENT_SHELL_ENV}={shell}"));
    }

    environment
}

#[cfg(windows)]
fn invoking_client_shell() -> Option<String> {
    let parent_pid = rmux_os::process::parent_pid(std::process::id())?;
    let parent_name = rmux_os::process::command_name(parent_pid)?;
    windows_client_shell_for_parent_name(&parent_name)
}

#[cfg(windows)]
fn internal_client_shell_handoff() -> Option<String> {
    internal_client_shell_handoff_from_vars(std::env::vars_os())
}

#[cfg(windows)]
fn internal_client_shell_handoff_from_vars<I>(vars: I) -> Option<String>
where
    I: IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
{
    let mut public_binary_seen = false;
    let mut shell = None;

    for (name, value) in vars {
        if name.eq_ignore_ascii_case(PUBLIC_BINARY_OVERRIDE_ENV) && !value.is_empty() {
            public_binary_seen = true;
        } else if name.eq_ignore_ascii_case(INTERNAL_CLIENT_SHELL_ENV) && !value.is_empty() {
            shell = Some(value.to_string_lossy().into_owned());
        }
    }

    public_binary_seen.then_some(shell).flatten()
}

#[cfg(windows)]
fn windows_client_shell_for_parent_name(parent_name: &str) -> Option<String> {
    let lower = parent_name.to_ascii_lowercase();
    match lower.as_str() {
        "cmd.exe" | "cmd" => Some(
            std::env::var_os("COMSPEC")
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| "cmd.exe".into())
                .to_string_lossy()
                .into_owned(),
        ),
        "powershell.exe" | "powershell" => {
            if windows_command_available_on_path("pwsh.exe") {
                Some("pwsh.exe".to_owned())
            } else {
                Some("powershell.exe".to_owned())
            }
        }
        "pwsh.exe" | "pwsh" => Some("pwsh.exe".to_owned()),
        "bash.exe" | "bash" | "sh.exe" | "sh" | "zsh.exe" | "zsh" | "nu.exe" | "nu" => {
            Some(parent_name.to_owned())
        }
        _ => None,
    }
}

#[cfg(windows)]
fn windows_command_available_on_path(name: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|directory| directory.join(name).is_file())
}

#[cfg(not(windows))]
fn invoking_client_environment() -> Option<Vec<String>> {
    None
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
            .map_err(|error| map_has_session_lookup_error(error, target.raw()))?,
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

fn map_has_session_lookup_error(error: ExitFailure, raw_target: &str) -> ExitFailure {
    if error.message().contains("ambiguous session match") {
        return ExitFailure::new(1, format!("can't find session: {raw_target}"));
    }
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
            kill_group: args.kill_group,
        })
        .map_err(ExitFailure::from_client)?;
    expect_command_success(response, "kill-session")?;
    Ok(0)
}

fn map_kill_session_lookup_error(error: ExitFailure) -> ExitFailure {
    normalize_session_lookup_error(error, "can't find session: {}")
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
    if args.json {
        let mut connection = connect(socket_path)
            .map_err(|error| ExitFailure::from_client_connect(socket_path, error))?;
        let response = connection
            .list_sessions(ListSessionsRequest {
                format: Some(list_sessions_json_format()),
                filter: args.filter,
                sort_order: args.sort_order,
                reversed: args.reversed,
            })
            .map_err(ExitFailure::from_client)?;
        let output = super::expect_command_output(&response, "list-sessions")?;
        return write_list_sessions_json(output);
    }

    run_payload_command(socket_path, "list-sessions", move |connection| {
        connection.list_sessions(ListSessionsRequest {
            format: args.format,
            filter: args.filter,
            sort_order: args.sort_order,
            reversed: args.reversed,
        })
    })
}

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    use std::ffi::OsString;

    #[cfg(windows)]
    use super::{
        internal_client_shell_handoff_from_vars, windows_invoking_client_environment,
        INTERNAL_CLIENT_SHELL_ENV, INTERNAL_TMUX_COMPAT_ENV, PUBLIC_BINARY_OVERRIDE_ENV,
        RMUX_CLIENT_SHELL_ENV,
    };

    #[cfg(windows)]
    fn env_pair(name: &str, value: &str) -> (OsString, OsString) {
        (OsString::from(name), OsString::from(value))
    }

    #[cfg(windows)]
    #[test]
    fn windows_full_cli_uses_trusted_tiny_client_shell_handoff() {
        let vars = [
            env_pair("Path", r"C:\bin"),
            env_pair(PUBLIC_BINARY_OVERRIDE_ENV, r"C:\rmux\rmux.exe"),
            env_pair(INTERNAL_CLIENT_SHELL_ENV, "pwsh.exe"),
        ];

        assert_eq!(
            internal_client_shell_handoff_from_vars(vars).as_deref(),
            Some("pwsh.exe")
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_full_cli_ignores_untrusted_client_shell_handoff() {
        let vars = [
            env_pair("Path", r"C:\bin"),
            env_pair(INTERNAL_CLIENT_SHELL_ENV, "pwsh.exe"),
        ];

        assert_eq!(internal_client_shell_handoff_from_vars(vars), None);
    }

    #[cfg(windows)]
    #[test]
    fn windows_full_cli_filters_internal_handoff_environment() {
        let environment = windows_invoking_client_environment(
            [
                env_pair("Path", r"C:\bin"),
                env_pair(RMUX_CLIENT_SHELL_ENV, "stale.exe"),
                env_pair(INTERNAL_CLIENT_SHELL_ENV, "pwsh.exe"),
                env_pair(INTERNAL_TMUX_COMPAT_ENV, "1"),
            ],
            Some("pwsh.exe".to_owned()),
        );

        assert!(environment.iter().any(|entry| entry == r"Path=C:\bin"));
        assert!(environment
            .iter()
            .any(|entry| entry == "RMUX_CLIENT_SHELL=pwsh.exe"));
        assert!(!environment
            .iter()
            .any(|entry| entry == "RMUX_CLIENT_SHELL=stale.exe"));
        assert!(!environment
            .iter()
            .any(|entry| entry.starts_with("RMUX_INTERNAL_CLIENT_SHELL=")));
        assert!(!environment
            .iter()
            .any(|entry| entry.starts_with("RMUX_INTERNAL_INVOKED_AS_TMUX=")));
    }
}
