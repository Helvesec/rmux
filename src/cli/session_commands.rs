use std::path::{Path, PathBuf};

use rmux_client::connect;
use rmux_client::{detect_context, detect_parent, ClientContext, ClientContextParent};
use rmux_proto::request::{AttachSessionExt2Request, SwitchClientExt3Request};
use rmux_proto::request::{KillSessionRequest, ListSessionsRequest, NewSessionExtRequest};
use rmux_proto::{ClientTerminalContext, ErrorResponse, Response};

use super::json_output::{list_sessions_json_format, write_list_sessions_json};
use super::{
    attach_with_connection, current_terminal_size, require_attach_terminal,
    run_switch_client_on_connection,
};
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

    let client_context = detect_context();
    let mut connection = connect_with_startserver(socket_path, startup)?;
    if !args.detached && client_context == ClientContext::Outside {
        reject_existing_session_before_attach_preflight(&args, &mut connection)?;
        require_attach_terminal()?;
    }

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

    match client_context {
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

fn reject_existing_session_before_attach_preflight(
    args: &NewSessionArgs,
    connection: &mut rmux_client::Connection,
) -> Result<(), ExitFailure> {
    let Some(session_name) = args
        .session_name
        .as_ref()
        .filter(|_| !args.attach_if_exists)
    else {
        return Ok(());
    };
    let response = connection
        .has_session(session_name.clone())
        .map_err(ExitFailure::from_client)?;
    match response {
        Response::HasSession(response) if response.exists => Err(ExitFailure::new(
            1,
            format!("duplicate session: {session_name}"),
        )),
        Response::HasSession(_) => Ok(()),
        Response::Error(ErrorResponse { error }) => Err(ExitFailure::new(1, error.to_string())),
        other => Err(unexpected_response("has-session", &other)),
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
    let environment = crate::windows_shell::WindowsShellEnvironment::current();
    windows_client_shell_for_parent_name(&parent_name, &environment)
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
fn windows_client_shell_for_parent_name(
    parent_name: &str,
    environment: &crate::windows_shell::WindowsShellEnvironment,
) -> Option<String> {
    crate::windows_shell::client_shell_for_parent_name(parent_name, environment)
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
        internal_client_shell_handoff_from_vars, windows_client_shell_for_parent_name,
        windows_invoking_client_environment, INTERNAL_CLIENT_SHELL_ENV, INTERNAL_TMUX_COMPAT_ENV,
        PUBLIC_BINARY_OVERRIDE_ENV, RMUX_CLIENT_SHELL_ENV,
    };
    #[cfg(windows)]
    use crate::windows_shell::WindowsShellEnvironment;

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

    #[cfg(windows)]
    #[test]
    fn windows_full_cli_shell_mapping_skips_alias_and_uses_real_pwsh() {
        let root =
            std::env::temp_dir().join(format!("rmux-full-shell-hint-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let alias_dir = root.join("Microsoft").join("WindowsApps");
        let regular_dir = root.join("PowerShell").join("7");
        let powershell = root
            .join("System32")
            .join("WindowsPowerShell")
            .join("v1.0")
            .join("powershell.exe");
        let cmd = root.join("System32").join("cmd.exe");
        std::fs::create_dir_all(&alias_dir).expect("WindowsApps fixture directory");
        std::fs::create_dir_all(&regular_dir).expect("regular shell fixture directory");
        std::fs::create_dir_all(powershell.parent().expect("PowerShell fixture parent"))
            .expect("PowerShell fixture directory");
        std::fs::create_dir_all(cmd.parent().expect("cmd fixture parent"))
            .expect("cmd fixture directory");
        std::fs::write(alias_dir.join("pwsh.exe"), b"").expect("alias pwsh fixture");
        std::fs::write(regular_dir.join("pwsh.exe"), b"").expect("regular pwsh fixture");
        std::fs::write(&powershell, b"").expect("Windows PowerShell fixture");
        std::fs::write(&cmd, b"").expect("cmd fixture");

        let real_path = std::env::join_paths([alias_dir.as_os_str(), regular_dir.as_os_str()])
            .expect("PATH with real pwsh");
        let real_environment = WindowsShellEnvironment::for_test(
            Some(real_path),
            Some(root.clone().into_os_string()),
            Some(cmd.clone().into_os_string()),
        );
        assert_eq!(
            windows_client_shell_for_parent_name("powershell.exe", &real_environment).as_deref(),
            Some("pwsh.exe")
        );

        let alias_path = std::env::join_paths([alias_dir.as_os_str()]).expect("alias-only PATH");
        let alias_environment = WindowsShellEnvironment::for_test(
            Some(alias_path),
            Some(root.clone().into_os_string()),
            Some(cmd.into_os_string()),
        );
        assert_eq!(
            windows_client_shell_for_parent_name("pwsh.exe", &alias_environment).as_deref(),
            Some("powershell.exe")
        );

        std::fs::remove_dir_all(root).expect("remove full shell fixture");
    }
}
