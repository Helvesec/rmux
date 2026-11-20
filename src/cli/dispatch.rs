use std::path::Path;

use rmux_client::connect;
use rmux_proto::{ClientTerminalContext, Response};

use super::attach_transport::{queued_attach_session_is_active, QueuedAttachSession};
use super::command_runner::{
    finish_command_success, with_command_connection_cache, write_command_output,
};
#[cfg(test)]
pub(super) use super::session_commands::default_client_command;
use super::target_resolution::{
    resolve_canfail_pane_target_spec, resolve_select_layout_target_spec,
    resolve_window_target_or_current,
};
use super::{connect_with_startserver, ExitFailure, StartupOptions};
use crate::cli_args::Command;
use crate::empty_server_lifecycle::shutdown_started_empty_server_at;

pub(super) fn dispatch_command_queue(
    commands: Vec<Command>,
    socket_path: &Path,
    startup: StartupOptions,
    client_terminal: ClientTerminalContext,
) -> Result<i32, ExitFailure> {
    let mut initial_connection = None;
    let commands = if commands.is_empty() {
        let mut connection = connect_with_startserver(socket_path, startup.clone())?;
        let commands =
            super::session_commands::configured_default_client_commands(&mut connection)?;
        if commands.first().is_some_and(command_has_start_server_flag) {
            startup.endpoint.retain_connection(connection);
        } else {
            initial_connection = Some(connection);
        }
        commands
    } else {
        commands
    };

    if commands.len() > 1
        && commands
            .iter()
            .any(|command| matches!(command, Command::Unsupported(args) if args.name == "new-pane"))
    {
        return Err(super::alias_fallback::new_pane::batch_error());
    }
    if commands.len() > 1
        && commands.iter().any(|command| {
            matches!(command, Command::ResizePane(args) if args.position_x.is_some() || args.position_y.is_some())
        })
    {
        return Err(super::alias_fallback::resize_pane_position::batch_error());
    }
    if commands.len() > 1
        && commands.iter().any(
            |command| matches!(command, Command::Unsupported(args) if args.name == "set-pane-placement"),
        )
    {
        return Err(super::alias_fallback::set_pane_placement::batch_error());
    }
    let requires_split_key = commands.len() > 1
        && commands.iter().any(
            |command| matches!(command, Command::SplitWindow(args) if args.keep_alive_on_exit),
        );
    if initial_connection.is_some() {
        return super::command_runner::with_initial_command_connection(
            socket_path,
            initial_connection,
            || dispatch_commands(commands, startup, client_terminal),
        );
    }

    let can_reuse_connection = commands.len() > 1
        && commands
            .iter()
            .all(command_allows_detached_connection_reuse);
    if can_reuse_connection || requires_split_key {
        return with_command_connection_cache(socket_path, || {
            if requires_split_key {
                super::command_runner::with_command_connection(socket_path, |connection| {
                    if !connection
                        .supports_capability(rmux_proto::CAPABILITY_SPLIT_WINDOW_KEY_INPUT_V1)
                        .map_err(ExitFailure::from_client)?
                    {
                        return Err(ExitFailure::new(1, "batch requires commands.split_window.key_input.v1; no commands executed"));
                    }
                    Ok(())
                })?;
            }
            dispatch_commands(commands, startup, client_terminal)
        });
    }
    dispatch_commands(commands, startup, client_terminal)
}

fn dispatch_commands(
    commands: Vec<Command>,
    startup: StartupOptions,
    client_terminal: ClientTerminalContext,
) -> Result<i32, ExitFailure> {
    let mut exit_code = 0;
    let queued_commands = commands
        .iter()
        .filter(|command| !matches!(command, Command::ApplyParseTimeAssignments(_)))
        .count()
        > 1;
    let mut queued_attach_session = None::<QueuedAttachSession>;
    let queue_includes_start_server = commands
        .iter()
        .any(|command| matches!(command, Command::StartServer(_)));
    for (index, command) in commands.iter().cloned().enumerate() {
        // A queued command may auto-start the daemon on a different endpoint
        // than the one resolved at process start (Windows rotates a stale
        // managed generation), so every later command follows the daemon the
        // queue is actually talking to.
        let current_socket_path = startup.socket_path();
        let socket_path = current_socket_path.as_path();
        let command_is_detach_client = matches!(&command, Command::DetachClient(_));
        let command_is_kill_server = matches!(&command, Command::KillServer);
        let queue_attach_sequence = queued_commands
            && matches!(&command, Command::AttachSession(_))
            && attach_sequence_has_terminal_tail(&commands[index + 1..]);
        let queued_attach_active = queued_attach_session.is_some();
        let dispatch_result = dispatch(
            command,
            socket_path,
            startup.clone(),
            client_terminal.clone(),
            queue_attach_sequence,
            &mut queued_attach_session,
        );
        let mut command_exit_code = match dispatch_result {
            Ok(exit_code) => exit_code,
            // tmux makes a queued kill-server terminal and successful when
            // the server is already absent; a standalone kill-server still
            // reports the connection error.
            Err(error) if command_is_kill_server && queued_commands && error.is_server_absent() => {
                0
            }
            Err(error) => return Err(error),
        };
        if command_is_kill_server && queued_attach_active && command_exit_code == 0 {
            command_exit_code = 1;
        }
        if command_exit_code != 0 {
            exit_code = command_exit_code;
        }
        if command_is_detach_client
            && queued_attach_active
            && command_exit_code == 0
            && !queued_attach_session_is_active(socket_path)?
        {
            queued_attach_session = None;
        }
        if command_is_kill_server {
            queued_attach_session = None;
        }
        if command_is_kill_server {
            // tmux treats kill-server as terminal for the current command
            // queue, even when there was no server before the invocation.
            // Continuing would feed startup options to the tail and recreate
            // the daemon that this command just stopped.
            break;
        }
    }
    if let Some(queued_attach_session) = queued_attach_session {
        let attach_exit_code = queued_attach_session.run()?;
        if attach_exit_code != 0 {
            exit_code = attach_exit_code;
        }
    }
    if queue_includes_start_server {
        shutdown_started_empty_server_at(&startup.socket_path(), startup.endpoint.provenance())
            .map_err(|error| ExitFailure::new(1, error.to_string()))?;
    }
    Ok(exit_code)
}

fn attach_sequence_has_terminal_tail(tail: &[Command]) -> bool {
    tail.iter()
        .any(|command| matches!(command, Command::DetachClient(_) | Command::KillServer))
}

#[cfg(all(test, unix))]
mod display_panes_canonical_tests;
#[cfg(all(test, unix))]
mod display_panes_rows_tests;
#[cfg(all(test, unix))]
mod display_panes_tests;
mod reuse;
use reuse::command_allows_detached_connection_reuse;
mod command;
use command::dispatch;

fn run_shell_foreground(
    socket_path: &Path,
    args: crate::cli_args::RunShellArgs,
) -> Result<i32, ExitFailure> {
    let (command, arguments) = run_shell_command_and_arguments(args.command, args.as_commands)?;
    let mut connection = connect(socket_path)
        .map_err(|error| ExitFailure::from_client_connect(socket_path, error))?;
    let target = resolve_canfail_pane_target(&mut connection, args.target.as_ref())?;
    let response = connection
        .run_shell(
            command,
            arguments,
            false,
            args.as_commands,
            args.show_stderr,
            args.delay_seconds,
            args.start_directory,
            target,
        )
        .map_err(ExitFailure::from_client)?;

    match response {
        Response::RunShell(response) => {
            if let Some(output) = response.command_output() {
                write_command_output(output)?;
            }
            Ok(response.exit_status().unwrap_or(0))
        }
        other => finish_command_success(other, "run-shell"),
    }
}

fn run_shell_command_and_arguments(
    command: Vec<String>,
    as_commands: bool,
) -> Result<(String, Vec<String>), ExitFailure> {
    let mut command = command.into_iter();
    let Some(shell_command) = command.next() else {
        return Ok((String::new(), Vec::new()));
    };
    if as_commands {
        return Ok((shell_command, Vec::new()));
    }
    Ok((shell_command, command.collect()))
}

fn resolve_canfail_pane_target(
    connection: &mut rmux_client::Connection,
    target: Option<&crate::cli_args::TargetSpec>,
) -> Result<Option<rmux_proto::PaneTarget>, ExitFailure> {
    match target {
        Some(target) => resolve_canfail_pane_target_spec(connection, target),
        None => Ok(None),
    }
}

fn run_select_layout_noop(
    target: Option<&crate::cli_args::TargetSpec>,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    let mut connection = connect(socket_path)
        .map_err(|error| ExitFailure::from_client_connect(socket_path, error))?;
    match target {
        Some(target) => {
            let _ = resolve_select_layout_target_spec(&mut connection, target)?;
        }
        None => {
            let _ = resolve_window_target_or_current(&mut connection, None, "select-layout")?;
        }
    }
    Ok(0)
}

fn run_noop(socket_path: &Path) -> Result<i32, ExitFailure> {
    let _connection = connect(socket_path)
        .map_err(|error| ExitFailure::from_client_connect(socket_path, error))?;
    Ok(0)
}

fn looks_like_custom_layout(layout: &str) -> bool {
    layout.contains(',')
}

fn invalid_layout_failure(layout: &str) -> ExitFailure {
    ExitFailure::new(1, format!("invalid layout: {layout}"))
}

pub(super) fn command_has_start_server_flag(command: &Command) -> bool {
    match command {
        Command::Noop | Command::ApplyParseTimeAssignments(_) => false,
        Command::NewSession(_) | Command::StartServer(_) | Command::AttachSession(_) => true,
        Command::WebShare(args) => web_share_creates_share(args),
        _ => false,
    }
}

fn web_share_creates_share(args: &crate::cli_args::WebShareArgs) -> bool {
    !args.list
        && args.stop.is_none()
        && args.disconnect.is_none()
        && !args.stop_all
        && args.lookup.is_none()
        && !args.config
}

fn command_requires_web_daemon(command: &Command) -> bool {
    matches!(command, Command::WebShare(args) if web_share_creates_share(args))
}

fn unsupported_argument_suffix(arguments: &[String]) -> String {
    if arguments.is_empty() {
        String::new()
    } else {
        format!(" {}", arguments.join(" "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_shell_as_commands_accepts_single_command_string() {
        let (command, arguments) =
            run_shell_command_and_arguments(vec!["display-message ok".to_owned()], true)
                .expect("single command string is valid");

        assert_eq!(command, "display-message ok");
        assert!(arguments.is_empty());
    }

    #[test]
    fn run_shell_as_commands_ignores_trailing_positional_arguments_like_tmux() {
        let (command, arguments) = run_shell_command_and_arguments(
            vec!["display-message ok".to_owned(), "discarded".to_owned()],
            true,
        )
        .expect("tmux accepts and ignores trailing -C arguments");

        assert_eq!(command, "display-message ok");
        assert!(arguments.is_empty());
    }
}
