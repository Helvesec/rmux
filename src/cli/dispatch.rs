use std::io::{Read, Write};
use std::path::Path;

use rmux_client::connect;
use rmux_proto::{ClientTerminalContext, CopyModeRequest, ErrorResponse, LayoutName, Response};

use super::automation::{
    run_broadcast_keys, run_collect_pane_output, run_expect_pane, run_find_panes,
    run_find_sessions, run_locator, run_pane_snapshot, run_stream_pane, run_wait_pane,
    run_with_session,
};
use super::buffer_commands::{run_load_buffer, run_save_buffer};
use super::capture_pane::{capture_pane_request, send_capture_pane_request};
use super::client_commands::{
    run_attach_session, run_attach_session_queued, QueuedAttachSession, QueuedAttachSessionResult,
};
use super::command_inventory::run_list_commands;
use super::command_runner::{
    finish_command_success, inherited_pane_target, run_command, run_command_resolved,
    run_payload_command, run_payload_command_resolved, run_queued_server_command,
    with_command_connection_cache, write_command_output,
};
use super::config_commands::{
    run_set_environment, run_set_hook, run_set_option, run_show_environment, run_show_hooks,
    run_show_options,
};
use super::key_commands::{
    run_bind_key, run_list_keys, run_send_keys, run_send_prefix, run_unbind_key,
};
use super::message_commands::run_display_message;
use super::pane_commands::{
    run_break_pane, run_join_pane, run_last_pane, run_list_panes, run_move_pane, run_pipe_pane,
    run_resize_pane, run_respawn_pane, run_select_pane, run_split_window, run_swap_pane,
};
use super::server_commands::{
    run_kill_server, run_lock_client, run_lock_server, run_lock_session, run_server_access,
    run_start_server,
};
use super::session_commands::{
    run_has_session, run_kill_session, run_list_sessions, run_new_session, run_rename_session,
};
use super::target_resolution::{
    display_panes_client_target_error, resolve_current_pane_target, resolve_pane_target_or_current,
    resolve_pane_target_spec, resolve_select_layout_target_spec, resolve_session_target_or_current,
    resolve_window_target_or_current,
};
use super::web_commands::run_web_share;
use super::window_commands::{
    run_kill_window, run_last_window, run_link_window, run_list_windows, run_move_window,
    run_new_window, run_next_window, run_previous_window, run_rename_window, run_resize_window,
    run_respawn_window, run_rotate_window, run_select_window, run_swap_window, run_unlink_window,
};
use super::{connect_with_startserver, ExitFailure, StartupOptions};
use crate::cli_args::{Command, NewSessionArgs, SetOptionCommandKind, ShowOptionsCommandKind};
use crate::cli_response::tmux_cli_error_message;
use crate::tmux_error_surface::source_file_error_uses_stdout;

pub(super) fn default_client_command() -> Command {
    Command::NewSession(NewSessionArgs {
        attach_if_exists: false,
        working_directory: None,
        detach_other_clients: false,
        skip_environment_update: false,
        detached: false,
        session_name: None,
        environment: Vec::new(),
        flags: Vec::new(),
        print_format: None,
        window_name: None,
        print_session_info: false,
        group_target: None,
        kill_other_clients: false,
        cols: None,
        rows: None,
        command: Vec::new(),
    })
}

pub(super) fn dispatch_command_queue(
    commands: Vec<Command>,
    socket_path: &Path,
    startup: StartupOptions,
    client_terminal: ClientTerminalContext,
) -> Result<i32, ExitFailure> {
    let commands = if commands.is_empty() {
        vec![default_client_command()]
    } else {
        commands
    };

    let can_reuse_connection = commands.len() > 1
        && commands
            .iter()
            .all(command_allows_detached_connection_reuse);
    if can_reuse_connection {
        return with_command_connection_cache(socket_path, || {
            dispatch_commands(commands, socket_path, startup, client_terminal)
        });
    }
    dispatch_commands(commands, socket_path, startup, client_terminal)
}

fn dispatch_commands(
    commands: Vec<Command>,
    socket_path: &Path,
    startup: StartupOptions,
    client_terminal: ClientTerminalContext,
) -> Result<i32, ExitFailure> {
    let mut exit_code = 0;
    let queued_commands = commands.len() > 1;
    let mut queued_attach_session = None::<QueuedAttachSession>;
    for (index, command) in commands.iter().cloned().enumerate() {
        let command_is_detach_client = matches!(&command, Command::DetachClient(_));
        let command_is_kill_server = matches!(&command, Command::KillServer);
        let queue_attach_sequence = queued_commands
            && matches!(&command, Command::AttachSession(_))
            && attach_sequence_has_terminal_tail(&commands[index + 1..]);
        let queued_attach_active = queued_attach_session.is_some();
        exit_code = dispatch(
            command,
            socket_path,
            startup.clone(),
            client_terminal.clone(),
            queue_attach_sequence,
            &mut queued_attach_session,
        )?;
        if command_is_kill_server && queued_attach_active && exit_code == 0 {
            exit_code = 1;
        }
        if command_is_detach_client || command_is_kill_server {
            queued_attach_session = None;
        }
    }
    Ok(exit_code)
}

fn attach_sequence_has_terminal_tail(tail: &[Command]) -> bool {
    tail.iter()
        .any(|command| matches!(command, Command::DetachClient(_) | Command::KillServer))
}

fn command_allows_detached_connection_reuse(candidate: &Command) -> bool {
    match candidate {
        Command::SendKeys(args) if args.has_wait() => false,
        Command::Noop => true,
        Command::NewSession(_)
        | Command::StartServer(_)
        | Command::KillServer
        | Command::AttachSession(_)
        | Command::SourceFile(_)
        | Command::WaitPane(_)
        | Command::StreamPane(_)
        | Command::CollectPaneOutput(_)
        | Command::WithSession(_)
        | Command::WebShare(_)
        | Command::Unsupported(_) => false,
        Command::HasSession(_)
        | Command::KillSession(_)
        | Command::RenameSession(_)
        | Command::ServerAccess(_)
        | Command::LockServer
        | Command::LockSession(_)
        | Command::LockClient(_)
        | Command::NewWindow(_)
        | Command::KillWindow(_)
        | Command::SelectWindow(_)
        | Command::RenameWindow(_)
        | Command::NextWindow(_)
        | Command::PreviousWindow(_)
        | Command::LastWindow(_)
        | Command::ListSessions(_)
        | Command::ListWindows(_)
        | Command::MoveWindow(_)
        | Command::SwapWindow(_)
        | Command::RotateWindow(_)
        | Command::ResizeWindow(_)
        | Command::RespawnWindow(_)
        | Command::SplitWindow(_)
        | Command::SwapPane(_)
        | Command::LastPane(_)
        | Command::JoinPane(_)
        | Command::MovePane(_)
        | Command::BreakPane(_)
        | Command::PipePane(_)
        | Command::RespawnPane(_)
        | Command::KillPane(_)
        | Command::SelectLayout(_)
        | Command::NextLayout(_)
        | Command::PreviousLayout(_)
        | Command::ResizePane(_)
        | Command::DisplayPanes(_)
        | Command::ListPanes(_)
        | Command::SelectPane(_)
        | Command::CopyMode(_)
        | Command::ClockMode(_)
        | Command::PaneSnapshot(_)
        | Command::Locator(_)
        | Command::ExpectPane(_)
        | Command::FindPanes(_)
        | Command::FindSessions(_)
        | Command::BroadcastKeys(_)
        | Command::SendKeys(_)
        | Command::BindKey(_)
        | Command::UnbindKey(_)
        | Command::ListCommands(_)
        | Command::ListKeys(_)
        | Command::SendPrefix(_)
        | Command::Prompt(_)
        | Command::ConfirmBefore(_)
        | Command::FindWindow(_)
        | Command::LinkWindow(_)
        | Command::UnlinkWindow(_)
        | Command::ChooseTree(_)
        | Command::ChooseBuffer(_)
        | Command::ChooseClient(_)
        | Command::CustomizeMode(_)
        | Command::RefreshClient(_)
        | Command::ListClients(_)
        | Command::SwitchClient(_)
        | Command::DetachClient(_)
        | Command::SuspendClient(_)
        | Command::SetOption(_)
        | Command::SetWindowOption(_)
        | Command::SetEnvironment(_)
        | Command::ShowOptions(_)
        | Command::ShowWindowOptions(_)
        | Command::ShowEnvironment(_)
        | Command::SetHook(_)
        | Command::ShowHooks(_)
        | Command::SetBuffer(_)
        | Command::ShowBuffer(_)
        | Command::PasteBuffer(_)
        | Command::ListBuffers(_)
        | Command::DeleteBuffer(_)
        | Command::LoadBuffer(_)
        | Command::SaveBuffer(_)
        | Command::CapturePane(_)
        | Command::ClearHistory(_)
        | Command::DisplayMessage(_)
        | Command::ShowMessages(_)
        | Command::RunShell(_)
        | Command::IfShell(_)
        | Command::WaitFor(_)
        | Command::DisplayMenu(_)
        | Command::DisplayPopup(_)
        | Command::ClearPromptHistory(_)
        | Command::ShowPromptHistory(_) => true,
    }
}

fn dispatch(
    command: Command,
    socket_path: &Path,
    startup: StartupOptions,
    client_terminal: ClientTerminalContext,
    queue_attach_detach: bool,
    queued_attach_session: &mut Option<QueuedAttachSession>,
) -> Result<i32, ExitFailure> {
    let command_startup = startup.for_command(
        command_has_start_server_flag(&command),
        command_requires_web_daemon(&command),
    );

    match command {
        Command::Noop => run_noop(socket_path),
        Command::NewSession(args) => {
            run_new_session(args, socket_path, command_startup, client_terminal)
        }
        Command::StartServer(_) => run_start_server(socket_path, command_startup),
        Command::KillServer => run_kill_server(socket_path),
        Command::HasSession(args) => run_has_session(args, socket_path),
        Command::KillSession(args) => run_kill_session(args, socket_path),
        Command::RenameSession(args) => run_rename_session(args, socket_path),
        Command::ServerAccess(args) => run_server_access(args, socket_path),
        Command::LockServer => run_lock_server(socket_path),
        Command::LockSession(args) => run_lock_session(args, socket_path),
        Command::LockClient(args) => run_lock_client(args, socket_path),
        Command::NewWindow(args) => run_new_window(args, socket_path),
        Command::KillWindow(args) => run_kill_window(args, socket_path),
        Command::SelectWindow(args) => run_select_window(args, socket_path),
        Command::RenameWindow(args) => run_rename_window(args, socket_path),
        Command::NextWindow(args) => run_next_window(args, socket_path),
        Command::PreviousWindow(args) => run_previous_window(args, socket_path),
        Command::LastWindow(args) => run_last_window(args, socket_path),
        Command::ListSessions(args) => run_list_sessions(args, socket_path),
        Command::ListWindows(args) => run_list_windows(args, socket_path),
        Command::LinkWindow(args) => run_link_window(args, socket_path),
        Command::MoveWindow(args) => run_move_window(args, socket_path),
        Command::SwapWindow(args) => run_swap_window(args, socket_path),
        Command::RotateWindow(args) => run_rotate_window(args, socket_path),
        Command::ResizeWindow(args) => run_resize_window(args, socket_path),
        Command::RespawnWindow(args) => run_respawn_window(args, socket_path),
        Command::SplitWindow(args) => run_split_window(args, socket_path),
        Command::SwapPane(args) => run_swap_pane(args, socket_path),
        Command::LastPane(args) => run_last_pane(args, socket_path),
        Command::JoinPane(args) => run_join_pane(args, socket_path),
        Command::MovePane(args) => run_move_pane(args, socket_path),
        Command::BreakPane(args) => run_break_pane(args, socket_path),
        Command::PipePane(args) => run_pipe_pane(args, socket_path),
        Command::RespawnPane(args) => run_respawn_pane(args, socket_path),
        Command::KillPane(args) => {
            run_command_resolved(socket_path, "kill-pane", move |connection| {
                let target = match args.target.as_ref() {
                    Some(target) => resolve_pane_target_spec(connection, target)?,
                    None => resolve_current_pane_target(connection, "kill-pane")?,
                };
                connection
                    .kill_pane_with_options(target, args.kill_all_except)
                    .map_err(ExitFailure::from_client)
            })
        }
        Command::SelectLayout(args) => {
            if args.spread {
                return run_command_resolved(socket_path, "select-layout", move |connection| {
                    let target = match args.target.as_ref() {
                        Some(target) => resolve_select_layout_target_spec(connection, target)?,
                        None => rmux_proto::SelectLayoutTarget::Window(
                            resolve_window_target_or_current(connection, None, "select-layout")?,
                        ),
                    };
                    connection
                        .spread_layout(target)
                        .map_err(ExitFailure::from_client)
                });
            }
            if args.next {
                return run_command_resolved(socket_path, "select-layout", move |connection| {
                    let target = resolve_window_target_or_current(
                        connection,
                        args.target.as_ref(),
                        "select-layout",
                    )?;
                    connection
                        .next_layout(target)
                        .map_err(ExitFailure::from_client)
                });
            }
            if args.previous {
                return run_command_resolved(socket_path, "select-layout", move |connection| {
                    let target = resolve_window_target_or_current(
                        connection,
                        args.target.as_ref(),
                        "select-layout",
                    )?;
                    connection
                        .previous_layout(target)
                        .map_err(ExitFailure::from_client)
                });
            }
            if args.old {
                return run_command_resolved(socket_path, "select-layout", move |connection| {
                    let target = match args.target.as_ref() {
                        Some(target) => resolve_select_layout_target_spec(connection, target)?,
                        None => rmux_proto::SelectLayoutTarget::Window(
                            resolve_window_target_or_current(connection, None, "select-layout")?,
                        ),
                    };
                    connection
                        .select_old_layout(target)
                        .map_err(ExitFailure::from_client)
                });
            }
            if args.layout.is_none() {
                return run_select_layout_noop(args.target.as_ref(), socket_path);
            }
            run_command_resolved(socket_path, "select-layout", move |connection| {
                let target = match args.target.as_ref() {
                    Some(target) => resolve_select_layout_target_spec(connection, target)?,
                    None => rmux_proto::SelectLayoutTarget::Window(
                        resolve_window_target_or_current(connection, None, "select-layout")?,
                    ),
                };
                let layout = args.layout.as_ref().expect("handled no-op layout");
                match layout.parse::<LayoutName>() {
                    Ok(parsed) => connection
                        .select_layout(target, parsed)
                        .map_err(ExitFailure::from_client),
                    Err(_) if looks_like_custom_layout(layout) => connection
                        .select_custom_layout(target, layout.clone())
                        .map_err(ExitFailure::from_client),
                    Err(_) => Err(invalid_layout_failure(layout)),
                }
            })
        }
        Command::NextLayout(args) => {
            run_command_resolved(socket_path, "next-layout", move |connection| {
                let target = resolve_window_target_or_current(
                    connection,
                    args.target.as_ref(),
                    "next-layout",
                )?;
                connection
                    .next_layout(target)
                    .map_err(ExitFailure::from_client)
            })
        }
        Command::PreviousLayout(args) => {
            run_command_resolved(socket_path, "previous-layout", move |connection| {
                let target = resolve_window_target_or_current(
                    connection,
                    args.target.as_ref(),
                    "previous-layout",
                )?;
                connection
                    .previous_layout(target)
                    .map_err(ExitFailure::from_client)
            })
        }
        Command::ResizePane(args) => run_resize_pane(args, socket_path),
        Command::DisplayPanes(args) => {
            let template = args.template_command();
            let raw_target = args.target.as_ref().map(|target| target.raw().to_owned());
            run_command_resolved(socket_path, "display-panes", move |connection| {
                let target = match resolve_session_target_or_current(
                    connection,
                    args.target.as_ref(),
                    "display-panes",
                ) {
                    Ok(target) => target,
                    Err(_error) if raw_target.is_some() => {
                        return Err(display_panes_client_target_error(
                            raw_target.as_deref().expect("target checked above"),
                        ))
                    }
                    Err(error) => return Err(error),
                };
                let response = connection
                    .display_panes(
                        target,
                        args.duration_ms,
                        args.non_blocking,
                        args.no_command,
                        template,
                    )
                    .map_err(ExitFailure::from_client)?;
                if raw_target.is_some()
                    && matches!(
                        &response,
                        Response::Error(ErrorResponse { error })
                            if error.to_string() == "no current client"
                    )
                {
                    return Err(display_panes_client_target_error(
                        raw_target.as_deref().expect("target checked above"),
                    ));
                }
                Ok(response)
            })
        }
        Command::ListPanes(args) => run_list_panes(args, socket_path),
        Command::SelectPane(args) => run_select_pane(args, socket_path),
        Command::CopyMode(args) => {
            run_command_resolved(socket_path, "copy-mode", move |connection| {
                let target = args
                    .target
                    .as_ref()
                    .map(|target| resolve_pane_target_spec(connection, target))
                    .transpose()?;
                let source = args
                    .source
                    .as_ref()
                    .map(|target| resolve_pane_target_spec(connection, target))
                    .transpose()?;
                connection
                    .copy_mode(CopyModeRequest {
                        target,
                        page_down: args.page_down,
                        exit_on_scroll: args.exit_on_scroll,
                        hide_position: args.hide_position,
                        mouse_drag_start: args.mouse_drag_start,
                        cancel_mode: args.cancel_mode,
                        scrollbar_scroll: args.scrollbar_scroll,
                        source,
                        page_up: args.page_up,
                    })
                    .map_err(ExitFailure::from_client)
            })
        }
        Command::ClockMode(args) => {
            run_command_resolved(socket_path, "clock-mode", move |connection| {
                let target = args
                    .target
                    .as_ref()
                    .map(|target| resolve_pane_target_spec(connection, target))
                    .transpose()?;
                connection
                    .clock_mode(target)
                    .map_err(ExitFailure::from_client)
            })
        }
        Command::WaitPane(args) => run_wait_pane(args, socket_path),
        Command::PaneSnapshot(args) => run_pane_snapshot(args, socket_path),
        Command::StreamPane(args) => run_stream_pane(args, socket_path),
        Command::CollectPaneOutput(args) => run_collect_pane_output(args, socket_path),
        Command::Locator(args) => run_locator(args, socket_path),
        Command::ExpectPane(args) => run_expect_pane(args, socket_path),
        Command::FindPanes(args) => run_find_panes(args, socket_path),
        Command::FindSessions(args) => run_find_sessions(args, socket_path),
        Command::BroadcastKeys(args) => run_broadcast_keys(args, socket_path),
        Command::WithSession(args) => run_with_session(args, socket_path),
        Command::SendKeys(args) => run_send_keys(args, socket_path),
        Command::BindKey(args) => run_bind_key(args, socket_path),
        Command::UnbindKey(args) => run_unbind_key(args, socket_path),
        Command::ListCommands(args) => run_list_commands(args),
        Command::ListKeys(args) => run_list_keys(args, socket_path),
        Command::SendPrefix(args) => run_send_prefix(args, socket_path),
        Command::Prompt(args) => {
            run_queued_server_command(socket_path, "command-prompt", args.queue_command)
        }
        Command::ConfirmBefore(args) => {
            run_queued_server_command(socket_path, "confirm-before", args.queue_command)
        }
        Command::FindWindow(args) => {
            run_queued_server_command(socket_path, "find-window", args.queue_command)
        }
        Command::UnlinkWindow(args) => run_unlink_window(args, socket_path),
        Command::ChooseTree(args) => {
            run_queued_server_command(socket_path, "choose-tree", args.queue_command)
        }
        Command::ChooseBuffer(args) => {
            run_queued_server_command(socket_path, "choose-buffer", args.queue_command)
        }
        Command::ChooseClient(args) => {
            run_queued_server_command(socket_path, "choose-client", args.queue_command)
        }
        Command::CustomizeMode(args) => {
            run_queued_server_command(socket_path, "customize-mode", args.queue_command)
        }
        Command::AttachSession(args) => {
            if queue_attach_detach {
                match run_attach_session_queued(
                    args,
                    socket_path,
                    command_startup,
                    client_terminal,
                )? {
                    QueuedAttachSessionResult::Detached(guard) => {
                        *queued_attach_session = Some(*guard);
                        Ok(0)
                    }
                    QueuedAttachSessionResult::Completed(exit_code) => Ok(exit_code),
                }
            } else {
                run_attach_session(args, socket_path, command_startup, client_terminal)
            }
        }
        Command::RefreshClient(args) => super::run_refresh_client(args, socket_path),
        Command::ListClients(args) => super::run_list_clients(args, socket_path),
        Command::SwitchClient(args) => super::run_switch_client(args, socket_path),
        Command::DetachClient(args) => super::run_detach_client(args, socket_path),
        Command::SuspendClient(args) => super::run_suspend_client(args, socket_path),
        Command::SetOption(args) => {
            run_set_option(SetOptionCommandKind::SetOption, args, socket_path)
        }
        Command::SetWindowOption(args) => {
            run_set_option(SetOptionCommandKind::SetWindowOption, args, socket_path)
        }
        Command::SetEnvironment(args) => run_set_environment(args, socket_path),
        Command::ShowOptions(args) => {
            run_show_options(ShowOptionsCommandKind::ShowOptions, args, socket_path)
        }
        Command::ShowWindowOptions(args) => {
            run_show_options(ShowOptionsCommandKind::ShowWindowOptions, args, socket_path)
        }
        Command::ShowEnvironment(args) => run_show_environment(args, socket_path),
        Command::SetHook(args) => run_set_hook(args, socket_path),
        Command::ShowHooks(args) => run_show_hooks(args, socket_path),
        Command::SetBuffer(args) => run_command(socket_path, "set-buffer", move |connection| {
            connection.set_buffer(
                args.name,
                args.content.unwrap_or_default().into_bytes(),
                args.append,
                args.new_name,
                args.set_clipboard,
            )
        }),
        Command::ShowBuffer(args) => {
            run_payload_command(socket_path, "show-buffer", move |connection| {
                connection.show_buffer(args.name)
            })
        }
        Command::PasteBuffer(args) => {
            run_command_resolved(socket_path, "paste-buffer", move |connection| {
                let target = resolve_pane_target_or_current(
                    connection,
                    args.target.as_ref(),
                    "paste-buffer",
                )?;
                connection
                    .paste_buffer(
                        args.name,
                        target,
                        args.delete_after,
                        args.separator,
                        args.linefeed,
                        args.raw,
                        args.bracketed,
                    )
                    .map_err(ExitFailure::from_client)
            })
        }
        Command::ListBuffers(args) => {
            run_payload_command(socket_path, "list-buffers", move |connection| {
                connection.list_buffers(args.format, args.filter, args.sort_order, args.reversed)
            })
        }
        Command::DeleteBuffer(args) => {
            run_command(socket_path, "delete-buffer", move |connection| {
                connection.delete_buffer(args.name)
            })
        }
        Command::LoadBuffer(args) => run_load_buffer(args, socket_path),
        Command::SaveBuffer(args) => run_save_buffer(args, socket_path),
        Command::CapturePane(args) if args.print => {
            let args = capture_pane_request(args)?;
            run_payload_command_resolved(socket_path, "capture-pane", move |connection| {
                send_capture_pane_request(connection, socket_path, args)
            })
        }
        Command::CapturePane(args) => {
            let args = capture_pane_request(args)?;
            run_command_resolved(socket_path, "capture-pane", move |connection| {
                send_capture_pane_request(connection, socket_path, args)
            })
        }
        Command::ClearHistory(args) => {
            run_command_resolved(socket_path, "clear-history", move |connection| {
                let target = resolve_pane_target_or_current(
                    connection,
                    args.target.as_ref(),
                    "clear-history",
                )?;
                connection
                    .clear_history(target, args.reset_hyperlinks)
                    .map_err(ExitFailure::from_client)
            })
        }
        Command::DisplayMessage(args) => run_display_message(args, socket_path),
        Command::ShowMessages(args) => {
            run_payload_command(socket_path, "show-messages", move |connection| {
                connection.show_messages(args.jobs, args.terminals, args.target_client)
            })
        }
        Command::RunShell(args) if args.background => {
            let (command, arguments) =
                run_shell_command_and_arguments(args.command, args.as_commands)?;
            run_command_resolved(socket_path, "run-shell", move |connection| {
                let target =
                    resolve_run_shell_target(connection, socket_path, args.target.as_ref())?;
                connection
                    .run_shell(
                        command,
                        arguments,
                        true,
                        args.as_commands,
                        args.show_stderr,
                        args.delay_seconds,
                        args.start_directory,
                        target,
                    )
                    .map_err(ExitFailure::from_client)
            })
        }
        Command::RunShell(args) => run_shell_foreground(socket_path, args),
        Command::SourceFile(args) => run_source_file(args, socket_path, command_startup),
        Command::IfShell(args) => run_command(socket_path, "if-shell", move |connection| {
            connection.if_shell(
                args.condition,
                args.format_mode,
                args.then_command,
                args.else_command,
                args.target,
                args.background,
            )
        }),
        Command::WaitFor(args) => {
            let mode = args.mode();
            run_command(socket_path, "wait-for", move |connection| {
                connection.wait_for(args.channel, mode)
            })
        }
        Command::WebShare(args) => run_web_share(args, socket_path, command_startup),
        Command::DisplayMenu(args) => {
            run_queued_server_command(socket_path, "display-menu", args.queue_command)
        }
        Command::DisplayPopup(args) => {
            run_queued_server_command(socket_path, "display-popup", args.queue_command)
        }
        Command::ClearPromptHistory(args) => {
            run_queued_server_command(socket_path, "clear-prompt-history", args.queue_command)
        }
        Command::ShowPromptHistory(args) => {
            run_queued_server_command(socket_path, "show-prompt-history", args.queue_command)
        }
        Command::Unsupported(args) => Err(ExitFailure::new(
            1,
            format!(
                "command not implemented: {}{}",
                args.name,
                unsupported_argument_suffix(&args.arguments)
            ),
        )),
    }
}

fn run_shell_foreground(
    socket_path: &Path,
    args: crate::cli_args::RunShellArgs,
) -> Result<i32, ExitFailure> {
    let (command, arguments) = run_shell_command_and_arguments(args.command, args.as_commands)?;
    let mut connection = connect(socket_path)
        .map_err(|error| ExitFailure::from_client_connect(socket_path, error))?;
    let target = resolve_run_shell_target(&mut connection, socket_path, args.target.as_ref())?;
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

fn resolve_run_shell_target(
    connection: &mut rmux_client::Connection,
    socket_path: &Path,
    target: Option<&crate::cli_args::TargetSpec>,
) -> Result<Option<rmux_proto::PaneTarget>, ExitFailure> {
    match target {
        Some(target) => match resolve_pane_target_spec(connection, target) {
            Ok(target) => Ok(Some(target)),
            Err(_) => Ok(None),
        },
        None => inherited_pane_target(connection, socket_path),
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
        Command::Noop => false,
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

fn run_source_file(
    args: crate::cli_args::SourceFileArgs,
    socket_path: &Path,
    startup: StartupOptions,
) -> Result<i32, ExitFailure> {
    let stdin = if args.paths.iter().any(|path| path == "-") {
        let mut buffer = String::new();
        std::io::stdin()
            .read_to_string(&mut buffer)
            .map_err(|error| ExitFailure::new(1, format!("failed to read stdin: {error}")))?;
        Some(buffer)
    } else {
        None
    };

    let mut connection = connect_with_startserver(socket_path, startup)?;
    let target = match args.target.as_ref() {
        Some(target) => Some(resolve_pane_target_spec(&mut connection, target)?),
        None => inherited_pane_target(&mut connection, socket_path)?,
    };
    let response = connection
        .source_file(
            args.paths,
            args.quiet,
            args.parse_only,
            args.verbose,
            args.expand_paths,
            target,
            stdin,
        )
        .map_err(ExitFailure::from_client)?;
    if let Response::Error(ErrorResponse { error }) = &response {
        if source_file_error_uses_stdout(error) {
            return Err(ExitFailure::new_stdout(
                1,
                tmux_cli_error_message("source-file", error),
            ));
        }
    }
    if let Response::SourceFile(response) = &response {
        if let Some(output) = response.command_output() {
            write_command_output(output)?;
        }
        if !response.stderr().is_empty() {
            std::io::stderr()
                .write_all(response.stderr())
                .map_err(|error| {
                    ExitFailure::new(1, format!("failed to write source-file stderr: {error}"))
                })?;
        }
        return Ok(response.exit_status().unwrap_or(0));
    }
    finish_command_success(response, "source-file")
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
