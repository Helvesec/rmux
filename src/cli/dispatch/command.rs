use std::path::Path;

use rmux_proto::{ClientTerminalContext, CopyModeRequest, LayoutName};

use super::super::attach_transport::{QueuedAttachSession, QueuedAttachSessionResult};
use super::super::automation::{
    run_broadcast_keys, run_collect_pane_output, run_expect_pane, run_find_panes,
    run_find_sessions, run_locator, run_pane_snapshot, run_stream_pane,
    run_wait_pane_with_deadline, run_with_session,
};
use super::super::buffer_commands::{run_load_buffer, run_save_buffer};
use super::super::capture_pane::{capture_pane_request, send_capture_pane_request};
use super::super::client_commands::{run_attach_session, run_attach_session_queued};
use super::super::command_inventory::run_list_commands;
use super::super::command_runner::{
    run_command, run_command_resolved, run_payload_command, run_payload_command_resolved,
    run_queued_server_command,
};
use super::super::config_commands::{
    run_apply_parse_time_assignments, run_set_environment, run_set_hook, run_set_option,
    run_show_environment, run_show_hooks, run_show_options, run_source_file,
};
use super::super::display_panes::run_display_panes;
use super::super::key_commands::{
    run_bind_key, run_list_keys, run_send_keys, run_send_prefix, run_unbind_key,
};
use super::super::message_commands::run_display_message;
use super::super::pane_commands::{
    run_break_pane, run_join_pane, run_last_pane, run_list_panes, run_move_pane, run_pipe_pane,
    run_resize_pane, run_respawn_pane, run_select_pane, run_split_window, run_swap_pane,
};
use super::super::server_commands::{
    run_kill_server, run_lock_client, run_lock_server, run_lock_session, run_server_access,
    run_start_server,
};
use super::super::session_commands::{
    run_has_session, run_kill_session, run_list_sessions, run_new_session, run_rename_session,
};
use super::super::target_resolution::{
    resolve_current_pane_target, resolve_pane_target_or_current, resolve_pane_target_spec,
    resolve_select_layout_target_spec, resolve_window_target_or_current,
};
use super::super::web_commands::run_web_share;
use super::super::window_commands::{
    run_kill_window, run_last_window, run_link_window, run_list_windows, run_move_window,
    run_new_window, run_next_window, run_previous_window, run_rename_window, run_resize_window,
    run_respawn_window, run_rotate_window, run_select_window, run_swap_window, run_unlink_window,
};
use super::super::window_snapshot::{
    run_export_window_snapshot, run_export_window_snapshot_v2, run_restore_window_snapshot,
    run_restore_window_snapshot_v2,
};
use super::super::{ExitFailure, StartupOptions};
use super::{
    command_has_start_server_flag, command_requires_web_daemon, invalid_layout_failure,
    looks_like_custom_layout, resolve_canfail_pane_target, run_noop, run_select_layout_noop,
    run_shell_command_and_arguments, run_shell_foreground, unsupported_argument_suffix,
};
use crate::cli_args::{Command, SelectLayoutMode, SetOptionCommandKind, ShowOptionsCommandKind};

pub(super) fn dispatch(
    command: Command,
    socket_path: &Path,
    startup: StartupOptions,
    client_terminal: ClientTerminalContext,
    queue_attach_detach: bool,
    queued_attach_session: &mut Option<QueuedAttachSession>,
) -> Result<i32, ExitFailure> {
    let start_server_args = match &command {
        Command::StartServer(args) => Some(args),
        _ => None,
    };
    let command_startup = startup.for_command(
        command_has_start_server_flag(&command),
        command_requires_web_daemon(&command),
        start_server_args,
    );

    match command {
        Command::Noop => run_noop(socket_path),
        Command::ApplyParseTimeAssignments(assignments) => {
            run_apply_parse_time_assignments(socket_path, assignments)
        }
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
            let mode = args.mode();
            if mode == Some(SelectLayoutMode::Next) {
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
            if mode == Some(SelectLayoutMode::Previous) {
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
            if mode == Some(SelectLayoutMode::Spread) {
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
            if mode == Some(SelectLayoutMode::Old) && args.layout.is_none() {
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
                if mode == Some(SelectLayoutMode::Old) {
                    return connection
                        .select_custom_layout(target, layout.clone())
                        .map_err(ExitFailure::from_client);
                }
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
        Command::ExportWindowSnapshot(args) => run_export_window_snapshot(args, socket_path),
        Command::RestoreWindowSnapshot(args) => run_restore_window_snapshot(args, socket_path),
        Command::ExportWindowSnapshotV2(args) => run_export_window_snapshot_v2(args, socket_path),
        Command::RestoreWindowSnapshotV2(args) => run_restore_window_snapshot_v2(args, socket_path),
        Command::ResizePane(args) => run_resize_pane(args, socket_path),
        Command::DisplayPanes(args) => run_display_panes(args, socket_path),
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
        Command::WaitPane(args) => {
            run_wait_pane_with_deadline(args, socket_path, startup.automation_deadline)
        }
        Command::PaneSnapshot(args) => run_pane_snapshot(args, socket_path),
        Command::StreamPane(args) => run_stream_pane(args, socket_path),
        Command::CollectPaneOutput(args) => run_collect_pane_output(args, socket_path),
        Command::Locator(args) => run_locator(args, socket_path),
        Command::ExpectPane(args) => run_expect_pane(args, socket_path),
        Command::FindPanes(args) => run_find_panes(args, socket_path),
        Command::FindSessions(args) => run_find_sessions(args, socket_path),
        Command::BroadcastKeys(args) => run_broadcast_keys(args, socket_path),
        Command::WithSession(args) => run_with_session(args, socket_path),
        Command::SendKeys(args) => run_send_keys(args, socket_path, startup.automation_deadline),
        Command::BindKey(args) => run_bind_key(args, socket_path),
        Command::UnbindKey(args) => run_unbind_key(args, socket_path),
        Command::ListCommands(args) => run_list_commands(args, socket_path),
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
        Command::RefreshClient(args) => super::super::run_refresh_client(args, socket_path),
        Command::ListClients(args) => super::super::run_list_clients(args, socket_path),
        Command::SwitchClient(args) => super::super::run_switch_client(args, socket_path),
        Command::DetachClient(args) => super::super::run_detach_client(args, socket_path),
        Command::SuspendClient(args) => super::super::run_suspend_client(args, socket_path),
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
            connection.set_buffer_target_client(
                args.name,
                args.content.unwrap_or_default().into_bytes(),
                args.append,
                args.new_name,
                args.set_clipboard,
                args.target_client,
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
                let target = resolve_canfail_pane_target(connection, args.target.as_ref())?;
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
        Command::IfShell(args) => {
            run_queued_server_command(socket_path, "if-shell", args.queue_command)
        }
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
        Command::Unsupported(args) if args.name == "new-pane" => {
            super::super::alias_fallback::new_pane::run(socket_path, args.arguments)
        }
        Command::Unsupported(args) if args.name == "set-pane-placement" => {
            super::super::alias_fallback::set_pane_placement::run(socket_path, args.arguments)
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
