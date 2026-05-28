use std::path::Path;

use rmux_core::{command_parser::ParsedCommand, SessionStore, TargetFindContext};
use rmux_proto::{Request, RmuxError};

use super::super::RequestHandler;
use super::buffer_parse::{
    parse_delete_buffer, parse_list_buffers, parse_load_buffer, parse_paste_buffer,
    parse_save_buffer, parse_set_buffer, parse_show_buffer,
};
use super::client_parse::{
    parse_detach_client, parse_list_clients, parse_lock_client, parse_refresh_client,
    parse_server_access, parse_suspend_client, parse_switch_client,
};
use super::command_args::command_arguments_as_strings;
use super::config_parse::{
    parse_set_environment, parse_set_hook, parse_set_option, parse_show_environment,
    parse_show_hooks, parse_show_options,
};
use super::display_parse::{
    parse_capture_pane, parse_clear_history, parse_display_message, parse_show_messages,
};
use super::key_parse::{
    parse_bind_key, parse_list_keys, parse_send_keys, parse_send_prefix, parse_unbind_key,
};
use super::layout_parse::{parse_display_panes, parse_resize_pane, parse_select_layout};
use super::list_parse::{parse_list_panes, parse_list_sessions, parse_list_windows};
use super::mode_parse::{parse_clock_mode, parse_copy_mode};
use super::pane_parse::{
    parse_break_pane, parse_join_pane, parse_move_pane, parse_pane_request, parse_pipe_pane,
    parse_respawn_pane, parse_select_pane, parse_split_window, parse_swap_pane,
};
use super::prompt_parse::{
    parse_prompt_history_queue_command, parse_queued_command_prompt, parse_queued_confirm_before,
};
use super::queue::QueueInvocation;
use super::queue_parse::{
    parse_queued_if_shell, parse_queued_new_window, parse_queued_source_file,
};
use super::session_parse::{
    parse_attach_session, parse_kill_session, parse_new_session, parse_rename_session,
    parse_session_request,
};
use super::shell_parse::{parse_if_shell, parse_run_shell, parse_wait_for};
use super::targets::resolve_queue_target_arguments;
use super::tokens::CommandTokens;
use super::window_parse::{
    parse_kill_window, parse_link_window, parse_move_window, parse_new_window, parse_rename_window,
    parse_resize_window, parse_respawn_window, parse_rotate_window, parse_swap_window,
    parse_unlink_window, parse_window_request,
};

pub(super) fn parse_queue_invocation(
    command: ParsedCommand,
    caller_cwd: Option<&Path>,
    sessions: &SessionStore,
    find_context: &TargetFindContext,
) -> Result<QueueInvocation, RmuxError> {
    if command.name() == "new-window" {
        return parse_queued_new_window(command, sessions, find_context)
            .map(QueueInvocation::NewWindow);
    }
    if command.name() == "if-shell" {
        return parse_queued_if_shell(command, caller_cwd, sessions, find_context)
            .map(QueueInvocation::IfShell);
    }
    if command.name() == "source-file" {
        return parse_queued_source_file(command, caller_cwd, sessions, find_context)
            .map(QueueInvocation::SourceFile);
    }
    if command.name() == "command-prompt" {
        return parse_queued_command_prompt(command).map(QueueInvocation::CommandPrompt);
    }
    if matches!(command.name(), "confirm-before" | "confirm") {
        return parse_queued_confirm_before(command).map(QueueInvocation::ConfirmBefore);
    }
    if let Some(command) = RequestHandler::parse_mode_tree_queue_command(command.clone())? {
        return Ok(QueueInvocation::ModeTree(command));
    }

    let command_name = command.name().to_owned();
    let arguments = resolve_queue_target_arguments(
        &command_name,
        command_arguments_as_strings(&command_name, command.arguments())?,
        sessions,
        find_context,
    )?;
    if let Some(command) =
        RequestHandler::parse_overlay_queue_command(&command_name, arguments.clone())?
    {
        return Ok(QueueInvocation::Overlay(command));
    }
    if let Some(command) = parse_prompt_history_queue_command(&command_name, arguments.clone())? {
        return Ok(QueueInvocation::PromptHistory(command));
    }
    if command_name == "start-server" {
        let args = CommandTokens::new(arguments);
        parse_no_argument_request(args, "start-server")?;
        return Ok(QueueInvocation::StartServer);
    }
    parse_request_from_parts(command_name, arguments, caller_cwd, sessions, find_context)
        .map(QueueInvocation::Request)
}

pub(crate) fn parse_request_from_parts(
    command_name: String,
    arguments: Vec<String>,
    caller_cwd: Option<&Path>,
    sessions: &SessionStore,
    find_context: &TargetFindContext,
) -> Result<Request, RmuxError> {
    let args = CommandTokens::new(arguments);
    match command_name.as_str() {
        "run-shell" => parse_run_shell(args),
        "if-shell" => parse_if_shell(args, caller_cwd),
        "wait-for" => parse_wait_for(args),
        "set-option" => parse_set_option(args, false),
        "set-window-option" => parse_set_option(args, true),
        "set-environment" => parse_set_environment(args),
        "set-hook" => parse_set_hook(args),
        "show-options" => parse_show_options(args, false),
        "show-window-options" => parse_show_options(args, true),
        "show-environment" => parse_show_environment(args),
        "show-hooks" => parse_show_hooks(args),
        "set-buffer" => parse_set_buffer(args),
        "show-buffer" => parse_show_buffer(args),
        "paste-buffer" => parse_paste_buffer(args, sessions, find_context),
        "list-buffers" => parse_list_buffers(args),
        "delete-buffer" => parse_delete_buffer(args),
        "load-buffer" => parse_load_buffer(args, caller_cwd),
        "save-buffer" => parse_save_buffer(args, caller_cwd),
        "capture-pane" => parse_capture_pane(args),
        "clear-history" => parse_clear_history(args),
        "display-message" => parse_display_message(args),
        "show-messages" => parse_show_messages(args),
        "new-session" => parse_new_session(args),
        "attach-session" => parse_attach_session(args),
        "refresh-client" => parse_refresh_client(args),
        "list-clients" => parse_list_clients(args),
        "has-session" => parse_session_request(args, "has-session", sessions, find_context),
        "kill-session" => parse_kill_session(args),
        "kill-server" => parse_no_argument_request(args, "kill-server"),
        "lock-server" => parse_no_argument_request(args, "lock-server"),
        "lock-session" => parse_session_request(args, "lock-session", sessions, find_context),
        "lock-client" => parse_lock_client(args),
        "server-access" => parse_server_access(args),
        "rename-session" | "rename" => parse_rename_session(args, sessions, find_context),
        "list-sessions" => parse_list_sessions(args),
        "select-window" => parse_window_request(args, "select-window", sessions, find_context),
        "rename-window" => parse_rename_window(args, sessions, find_context),
        "next-window" => parse_session_request(args, "next-window", sessions, find_context),
        "previous-window" => parse_session_request(args, "previous-window", sessions, find_context),
        "last-window" => parse_session_request(args, "last-window", sessions, find_context),
        "link-window" => parse_link_window(args),
        "move-window" => parse_move_window(args, sessions, find_context),
        "swap-window" => parse_swap_window(args),
        "rotate-window" => parse_rotate_window(args, sessions, find_context),
        "resize-window" => parse_resize_window(args),
        "respawn-window" => parse_respawn_window(args, sessions, find_context),
        "split-window" => parse_split_window(args, sessions, find_context),
        "display-panes" => parse_display_panes(args, sessions, find_context),
        "last-pane" => parse_window_request(args, "last-pane", sessions, find_context),
        "swap-pane" => parse_swap_pane(args, sessions, find_context),
        "join-pane" => parse_join_pane(args),
        "move-pane" => parse_move_pane(args),
        "break-pane" => parse_break_pane(args, sessions, find_context),
        "pipe-pane" => parse_pipe_pane(args),
        "kill-pane" => parse_pane_request(args, "kill-pane", sessions, find_context),
        "respawn-pane" => parse_respawn_pane(args, sessions, find_context),
        "select-layout" => parse_select_layout(args, sessions, find_context),
        "next-layout" => parse_window_request(args, "next-layout", sessions, find_context),
        "previous-layout" => parse_window_request(args, "previous-layout", sessions, find_context),
        "resize-pane" => parse_resize_pane(args, sessions, find_context),
        "copy-mode" => parse_copy_mode(args),
        "clock-mode" => parse_clock_mode(args),
        "select-pane" => parse_select_pane(args, sessions, find_context),
        "new-window" => parse_new_window(args, sessions, find_context),
        "kill-window" => parse_kill_window(args, sessions, find_context),
        "list-windows" => parse_list_windows(args),
        "list-panes" => parse_list_panes(args, sessions, find_context),
        "send-keys" => parse_send_keys(args),
        "bind-key" => parse_bind_key(args),
        "unbind-key" => parse_unbind_key(args),
        "list-keys" => parse_list_keys(args),
        "send-prefix" => parse_send_prefix(args),
        "switch-client" => parse_switch_client(args),
        "detach-client" => parse_detach_client(args),
        "suspend-client" => parse_suspend_client(args),
        "unlink-window" => parse_unlink_window(args),
        other => Err(RmuxError::Server(format!(
            "unsupported command in queue: {other}"
        ))),
    }
}

fn parse_no_argument_request(args: CommandTokens, command: &str) -> Result<Request, RmuxError> {
    args.no_extra(command)?;
    match command {
        "kill-server" => Ok(Request::KillServer(rmux_proto::KillServerRequest)),
        "lock-server" => Ok(Request::LockServer(rmux_proto::LockServerRequest)),
        other => Err(RmuxError::Server(format!(
            "unsupported command in queue: {other}"
        ))),
    }
}
