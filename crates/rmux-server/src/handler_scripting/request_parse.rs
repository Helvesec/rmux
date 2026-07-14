use std::path::Path;

use rmux_core::{
    command_parser::ParsedCommand, tmux_precedence, OptionStore, SessionStore, TargetFindContext,
};
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
use super::command_args::{command_arguments_as_strings, command_arguments_with_blocks_as_strings};
use super::config_parse::{
    default_set_option_target, parse_set_environment, parse_set_hook, parse_set_option,
    parse_set_option_invocation, parse_show_environment, parse_show_hooks, parse_show_options,
    ParsedSetOptionCommand,
};
use super::display_parse::{
    parse_capture_pane, parse_clear_history, parse_display_message, parse_queued_display_message,
    parse_show_messages,
};
use super::key_parse::{
    parse_bind_key, parse_list_keys, parse_send_keys, parse_send_prefix, parse_unbind_key,
};
use super::layout_parse::{
    parse_display_panes, parse_resize_pane, parse_resize_pane_mouse_target, parse_select_layout,
};
use super::list_commands_runtime::parse_queued_list_commands;
use super::list_parse::{
    parse_list_panes, parse_list_sessions, parse_list_windows, parse_queued_list_panes_all,
};
use super::mode_parse::{parse_clock_mode, parse_copy_mode};
use super::pane_parse::{
    parse_break_pane, parse_join_pane, parse_move_pane, parse_pane_request, parse_pipe_pane,
    parse_queued_split_window, parse_respawn_pane, parse_select_pane, parse_split_window,
    parse_swap_pane,
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
use rmux_proto::Target;

pub(super) fn parse_queue_invocation(
    command: ParsedCommand,
    caller_cwd: Option<&Path>,
    sessions: &SessionStore,
    options: &OptionStore,
    find_context: &TargetFindContext,
    queue_current_target: Option<&Target>,
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
    if command.name() == "list-commands" {
        return parse_queued_list_commands(command).map(QueueInvocation::ListCommands);
    }
    if command.name() == "list-panes" {
        let arguments = command_arguments_as_strings(command.name(), command.arguments())?;
        if let Some(command) = parse_queued_list_panes_all(CommandTokens::new(arguments))? {
            return Ok(QueueInvocation::ListPanesAll(command));
        }
    }
    if command.name() == "command-prompt" {
        return parse_queued_command_prompt(command).map(QueueInvocation::CommandPrompt);
    }
    if matches!(command.name(), "confirm-before" | "confirm") {
        return parse_queued_confirm_before(command).map(QueueInvocation::ConfirmBefore);
    }
    if command.name() == "display-message" {
        let arguments = command_arguments_as_strings(command.name(), command.arguments())?;
        return parse_queued_display_message(
            CommandTokens::new(arguments),
            sessions,
            find_context,
            queue_current_target,
        )
        .map(QueueInvocation::Request);
    }
    if let Some(command) = RequestHandler::parse_mode_tree_queue_command(command.clone())? {
        return Ok(QueueInvocation::ModeTree(command));
    }

    let command_name = command.name().to_owned();
    if matches!(command_name.as_str(), "bind-key" | "set-hook") {
        let arguments = command_arguments_with_blocks_as_strings(command.arguments());
        let arguments = if command_name == "set-hook" {
            resolve_queue_target_arguments(&command_name, arguments, sessions, find_context)?
        } else {
            arguments
        };
        return parse_request_from_parts(
            command_name,
            arguments,
            caller_cwd,
            sessions,
            options,
            find_context,
        )
        .map(QueueInvocation::Request);
    }
    if matches!(
        command_name.as_str(),
        "display-menu" | "menu" | "display-popup" | "popup"
    ) {
        let arguments = command_arguments_with_blocks_as_strings(command.arguments());
        let arguments =
            resolve_queue_target_arguments(&command_name, arguments, sessions, find_context)?;
        if let Some(command) =
            RequestHandler::parse_overlay_queue_command(&command_name, arguments)?
        {
            return Ok(QueueInvocation::Overlay(command));
        }
    }
    let arguments = resolve_queue_target_arguments(
        &command_name,
        command_arguments_as_strings(&command_name, command.arguments())?,
        sessions,
        find_context,
    )?;
    let arguments = tmux_precedence::normalize_tmux_precedence(&command_name, arguments);
    if matches!(command_name.as_str(), "set-option" | "set-window-option") {
        let force_window = command_name == "set-window-option";
        return match parse_set_option_invocation(
            CommandTokens::new(arguments),
            force_window,
            default_set_option_target(sessions, find_context),
        )? {
            ParsedSetOptionCommand::Request(request) => Ok(QueueInvocation::Request(*request)),
            ParsedSetOptionCommand::Ignored(_) => Ok(QueueInvocation::NoOp),
            ParsedSetOptionCommand::NoOp => Ok(QueueInvocation::NoOp),
        };
    }
    if command_name == "split-window" {
        return parse_queued_split_window(CommandTokens::new(arguments), sessions, find_context)
            .map(QueueInvocation::SplitWindow);
    }
    if command_name == "resize-pane" {
        if let Some(target) = parse_resize_pane_mouse_target(
            CommandTokens::new(arguments.clone()),
            sessions,
            find_context,
        )? {
            return Ok(QueueInvocation::MouseResizePane(target));
        }
    }
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
        args.no_extra("start-server")?;
        return Ok(QueueInvocation::StartServer);
    }
    parse_request_from_parts(
        command_name,
        arguments,
        caller_cwd,
        sessions,
        options,
        find_context,
    )
    .map(QueueInvocation::Request)
}

pub(crate) fn parse_request_from_parts(
    command_name: String,
    arguments: Vec<String>,
    caller_cwd: Option<&Path>,
    sessions: &SessionStore,
    options: &OptionStore,
    find_context: &TargetFindContext,
) -> Result<Request, RmuxError> {
    let arguments = tmux_precedence::normalize_tmux_precedence(&command_name, arguments);
    let args = CommandTokens::new(arguments);
    match command_name.as_str() {
        "run-shell" => parse_run_shell(args),
        "if-shell" => parse_if_shell(args, caller_cwd),
        "wait-for" => parse_wait_for(args),
        "set-option" => parse_set_option(
            args,
            false,
            default_set_option_target(sessions, find_context),
        ),
        "set-window-option" => parse_set_option(
            args,
            true,
            default_set_option_target(sessions, find_context),
        ),
        "set-environment" => parse_set_environment(args, sessions, find_context),
        "set-hook" => parse_set_hook(args, sessions, find_context),
        "show-options" => parse_show_options(args, false, sessions, find_context),
        "show-window-options" => parse_show_options(args, true, sessions, find_context),
        "show-environment" => parse_show_environment(args, sessions, find_context),
        "show-hooks" => parse_show_hooks(args, sessions, find_context),
        "set-buffer" => parse_set_buffer(args),
        "show-buffer" => parse_show_buffer(args),
        "paste-buffer" => parse_paste_buffer(args, sessions, find_context),
        "list-buffers" => parse_list_buffers(args),
        "delete-buffer" => parse_delete_buffer(args),
        "load-buffer" => parse_load_buffer(args, caller_cwd),
        "save-buffer" => parse_save_buffer(args, caller_cwd),
        "capture-pane" => parse_capture_pane(args, sessions, find_context),
        "clear-history" => parse_clear_history(args, sessions, find_context),
        "display-message" => parse_display_message(args),
        "show-messages" => parse_show_messages(args),
        "new-session" => parse_new_session(args),
        "attach-session" => parse_attach_session(args),
        "refresh-client" => parse_refresh_client(args),
        "list-clients" => parse_list_clients(args),
        "has-session" => parse_session_request(args, "has-session", sessions, find_context),
        "kill-session" => parse_kill_session(args, sessions, find_context),
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
        "link-window" => parse_link_window(args, sessions, options, find_context),
        "move-window" => parse_move_window(args, sessions, options, find_context),
        "swap-window" => parse_swap_window(args, sessions, find_context),
        "rotate-window" => parse_rotate_window(args, sessions, find_context),
        "resize-window" => parse_resize_window(args, sessions, find_context),
        "respawn-window" => parse_respawn_window(args, sessions, find_context),
        "split-window" => parse_split_window(args, sessions, find_context),
        "display-panes" => parse_display_panes(args, sessions, find_context),
        "last-pane" => parse_window_request(args, "last-pane", sessions, find_context),
        "swap-pane" => parse_swap_pane(args, sessions, find_context),
        "join-pane" => parse_join_pane(args, sessions, find_context),
        "move-pane" => parse_move_pane(args, sessions, find_context),
        "break-pane" => parse_break_pane(args, sessions, find_context),
        "pipe-pane" => parse_pipe_pane(args, sessions, find_context),
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
        "list-windows" => parse_list_windows(args, sessions, find_context),
        "list-panes" => parse_list_panes(args, sessions, find_context),
        "send-keys" => parse_send_keys(args),
        "bind-key" => parse_bind_key(args),
        "unbind-key" => parse_unbind_key(args),
        "list-keys" => parse_list_keys(args),
        "send-prefix" => parse_send_prefix(args),
        "switch-client" => parse_switch_client(args),
        "detach-client" => parse_detach_client(args),
        "suspend-client" => parse_suspend_client(args),
        "unlink-window" => parse_unlink_window(args, sessions, find_context),
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

#[cfg(test)]
mod tests {
    use super::*;
    use rmux_proto::{PaneTarget, ScopeSelector, SetEnvironmentMode};

    fn parse_request(command: &str, args: &[&str]) -> Request {
        parse_request_from_parts(
            command.to_owned(),
            args.iter().map(|arg| (*arg).to_owned()).collect(),
            None,
            &SessionStore::default(),
            &OptionStore::default(),
            &TargetFindContext::new(None),
        )
        .expect("request parses")
    }

    fn assert_mouse_target(target: Option<&PaneTarget>) {
        let target = target.expect("target should parse from -t=");
        assert_eq!(target.session_name().as_str(), "=");
        assert_eq!(target.window_index(), 0);
        assert_eq!(target.pane_index(), 0);
    }

    #[test]
    fn default_mouse_binding_copy_mode_compact_target_parses() {
        let Request::CopyMode(request) = parse_request("copy-mode", &["-Ht="]) else {
            panic!("copy-mode -Ht= should parse as CopyMode");
        };

        assert!(request.hide_position);
        assert_mouse_target(request.target.as_ref());
    }

    #[test]
    fn default_mouse_binding_send_keys_compact_target_parses() {
        let Request::SendKeysExt(request) = parse_request("send-keys", &["-Xt=", "select-word"])
        else {
            panic!("send -Xt= should parse as extended send-keys");
        };

        assert!(request.copy_mode_command);
        assert_mouse_target(request.target.as_ref());
        assert_eq!(request.keys, vec!["select-word"]);
    }

    #[test]
    fn set_environment_accepts_compact_global_unset_flags() {
        let Request::SetEnvironment(request) =
            parse_request("set-environment", &["-gu", "AUDIT_VAR"])
        else {
            panic!("set-environment -gu should parse as SetEnvironment");
        };

        assert_eq!(request.scope, ScopeSelector::Global);
        assert_eq!(request.mode, Some(SetEnvironmentMode::Unset));
        assert_eq!(request.name, "AUDIT_VAR");
        assert!(request.value.is_empty());
    }

    #[test]
    fn set_environment_compact_target_consumes_only_its_value() {
        let Request::SetEnvironment(request) =
            parse_request("set-environment", &["-Fhtalpha", "AUDIT_VAR", "#{version}"])
        else {
            panic!("set-environment compact target should parse as SetEnvironment");
        };

        let ScopeSelector::Session(scope) = request.scope else {
            panic!("compact -t should select session scope");
        };
        assert_eq!(scope.as_str(), "alpha");
        assert!(request.format);
        assert!(request.hidden);
        assert_eq!(request.name, "AUDIT_VAR");
        assert_eq!(request.value, "#{version}");
    }

    #[test]
    fn set_environment_unset_takes_precedence_over_clear() {
        // Measured against the pinned tmux 3.7b oracle on 2026-07-14:
        // compact and separated `-r -u`/`-u -r` all use `-u` semantics.
        for arguments in [
            vec!["-gru".to_owned(), "AUDIT_VAR".to_owned()],
            vec!["-gur".to_owned(), "AUDIT_VAR".to_owned()],
            vec![
                "-g".to_owned(),
                "-r".to_owned(),
                "-u".to_owned(),
                "AUDIT_VAR".to_owned(),
            ],
            vec![
                "-g".to_owned(),
                "-u".to_owned(),
                "-r".to_owned(),
                "AUDIT_VAR".to_owned(),
            ],
        ] {
            let Request::SetEnvironment(request) = parse_request_from_parts(
                "set-environment".to_owned(),
                arguments,
                None,
                &SessionStore::default(),
                &OptionStore::default(),
                &TargetFindContext::new(None),
            )
            .expect("set-environment accepts combined mode flags") else {
                panic!("expected set-environment request");
            };

            assert_eq!(request.mode, Some(SetEnvironmentMode::Unset));
        }
    }
}
