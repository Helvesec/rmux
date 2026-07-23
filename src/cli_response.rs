use rmux_proto::{CommandOutput, ErrorResponse, Response};

use crate::cli::ExitFailure;
pub(crate) use crate::tmux_error_surface::tmux_cli_error_message;

const QUEUED_SOURCE_FILE_SUCCESS_COMMANDS: &[&str] = &[
    "command-prompt",
    "confirm-before",
    "find-window",
    "choose-tree",
    "choose-buffer",
    "choose-client",
    "customize-mode",
    "display-message",
    "display-menu",
    "display-popup",
    "if-shell",
    "list-windows",
    "new-window",
    "clear-prompt-history",
    "show-prompt-history",
];

fn queued_source_file_success_command(command_name: &str) -> bool {
    QUEUED_SOURCE_FILE_SUCCESS_COMMANDS.contains(&command_name)
}

pub(crate) fn expect_command_success(
    response: Response,
    command_name: &'static str,
) -> Result<(), ExitFailure> {
    match response {
        Response::NewSession(_) if command_name == "new-session" => Ok(()),
        Response::KillServer(_) if command_name == "kill-server" => Ok(()),
        Response::KillSession(_) if command_name == "kill-session" => Ok(()),
        Response::RenameSession(_) if command_name == "rename-session" => Ok(()),
        Response::ServerAccess(_) if command_name == "server-access" => Ok(()),
        Response::LockServer(_) if command_name == "lock-server" => Ok(()),
        Response::LockSession(_) if command_name == "lock-session" => Ok(()),
        Response::LockClient(_) if command_name == "lock-client" => Ok(()),
        Response::NewWindow(_) if command_name == "new-window" => Ok(()),
        Response::KillWindow(_) if command_name == "kill-window" => Ok(()),
        Response::SelectWindow(_) if command_name == "select-window" => Ok(()),
        Response::NextWindow(_) if command_name == "select-window" => Ok(()),
        Response::PreviousWindow(_) if command_name == "select-window" => Ok(()),
        Response::LastWindow(_) if command_name == "select-window" => Ok(()),
        Response::RenameWindow(_) if command_name == "rename-window" => Ok(()),
        Response::NextWindow(_) if command_name == "next-window" => Ok(()),
        Response::PreviousWindow(_) if command_name == "previous-window" => Ok(()),
        Response::LastWindow(_) if command_name == "last-window" => Ok(()),
        Response::LinkWindow(_) if command_name == "link-window" => Ok(()),
        Response::MoveWindow(_) if command_name == "move-window" => Ok(()),
        Response::SwapWindow(_) if command_name == "swap-window" => Ok(()),
        Response::RotateWindow(_) if command_name == "rotate-window" => Ok(()),
        Response::ResizeWindow(_) if command_name == "resize-window" => Ok(()),
        Response::RespawnWindow(_) if command_name == "respawn-window" => Ok(()),
        Response::SplitWindow(_) if command_name == "split-window" => Ok(()),
        Response::SwapPane(_) if command_name == "swap-pane" => Ok(()),
        Response::LastPane(_) if command_name == "last-pane" => Ok(()),
        Response::JoinPane(_) if command_name == "join-pane" => Ok(()),
        Response::MovePane(_) if command_name == "move-pane" => Ok(()),
        Response::BreakPane(_) if command_name == "break-pane" => Ok(()),
        Response::PipePane(_) if command_name == "pipe-pane" => Ok(()),
        Response::RespawnPane(_) if command_name == "respawn-pane" => Ok(()),
        Response::KillPane(_) if command_name == "kill-pane" => Ok(()),
        Response::SelectLayout(_) if command_name == "select-layout" => Ok(()),
        Response::NextLayout(_) if command_name == "select-layout" => Ok(()),
        Response::PreviousLayout(_) if command_name == "select-layout" => Ok(()),
        Response::NextLayout(_) if command_name == "next-layout" => Ok(()),
        Response::PreviousLayout(_) if command_name == "previous-layout" => Ok(()),
        Response::ResizePane(_) if command_name == "resize-pane" => Ok(()),
        Response::DisplayPanes(_) if command_name == "display-panes" => Ok(()),
        Response::SelectPane(_) if command_name == "select-pane" => Ok(()),
        Response::CopyMode(_) if command_name == "copy-mode" => Ok(()),
        Response::ClockMode(_) if command_name == "clock-mode" => Ok(()),
        Response::SendKeys(_) if command_name == "send-keys" => Ok(()),
        Response::BindKey(_) if command_name == "bind-key" => Ok(()),
        Response::UnbindKey(_) if command_name == "unbind-key" => Ok(()),
        Response::SendPrefix(_) if command_name == "send-prefix" => Ok(()),
        Response::AttachSession(_) if command_name == "attach-session" => Ok(()),
        Response::RefreshClient(_) if command_name == "refresh-client" => Ok(()),
        Response::SwitchClient(_) if command_name == "switch-client" => Ok(()),
        Response::DetachClient(_) if command_name == "detach-client" => Ok(()),
        Response::SuspendClient(_) if command_name == "suspend-client" => Ok(()),
        Response::SetOption(_) | Response::SetOptionByName(_)
            if matches!(command_name, "set-option" | "set-window-option") =>
        {
            Ok(())
        }
        Response::SetEnvironment(_) if command_name == "set-environment" => Ok(()),
        Response::SetHook(_) if command_name == "set-hook" => Ok(()),
        Response::SetBuffer(_) if command_name == "set-buffer" => Ok(()),
        Response::PasteBuffer(_) if command_name == "paste-buffer" => Ok(()),
        Response::DeleteBuffer(_) if command_name == "delete-buffer" => Ok(()),
        Response::LoadBuffer(_) if command_name == "load-buffer" => Ok(()),
        Response::SaveBuffer(_) if command_name == "save-buffer" => Ok(()),
        Response::ClearHistory(_) if command_name == "clear-history" => Ok(()),
        Response::CapturePane(response)
            if command_name == "capture-pane" && response.command_output().is_none() =>
        {
            Ok(())
        }
        Response::DisplayMessage(response)
            if command_name == "display-message" && response.command_output().is_none() =>
        {
            Ok(())
        }
        Response::RunShell(_) if command_name == "run-shell" => Ok(()),
        Response::IfShell(_) if command_name == "if-shell" => Ok(()),
        Response::SourceFile(_) if command_name == "source-file" => Ok(()),
        Response::SourceFile(_) if queued_source_file_success_command(command_name) => Ok(()),
        Response::UnlinkWindow(_) if command_name == "unlink-window" => Ok(()),
        Response::WaitFor(_) if command_name == "wait-for" => Ok(()),
        Response::ControlMode(_) if command_name == "control-mode" => Ok(()),
        Response::WebShare(_) if command_name == "web-share" => Ok(()),
        Response::Error(ErrorResponse { error }) => Err(ExitFailure::new(
            1,
            tmux_cli_error_message(command_name, &error),
        )),
        other => Err(unexpected_response(command_name, &other)),
    }
}

pub(crate) fn expect_command_output<'a>(
    response: &'a Response,
    command_name: &'static str,
) -> Result<&'a CommandOutput, ExitFailure> {
    match response {
        Response::Error(ErrorResponse { error }) => Err(ExitFailure::new(
            1,
            tmux_cli_error_message(command_name, error),
        )),
        other
            if matches!(
                command_name,
                "list-windows"
                    | "list-sessions"
                    | "list-clients"
                    | "list-panes"
                    | "show-options"
                    | "show-window-options"
                    | "show-environment"
                    | "show-hooks"
                    | "list-keys"
                    | "show-buffer"
                    | "list-buffers"
                    | "capture-pane"
                    | "break-pane"
                    | "display-message"
                    | "show-messages"
                    | "server-access"
                    | "run-shell"
                    | "source-file"
            ) =>
        {
            other
                .command_output()
                .ok_or_else(|| unexpected_response(command_name, other))
        }
        other => Err(unexpected_response(command_name, other)),
    }
}

fn unexpected_response(command_name: &str, response: &Response) -> ExitFailure {
    ExitFailure::new(
        1,
        format!(
            "protocol error: unexpected '{}' response for {command_name}",
            response_name(response)
        ),
    )
}

pub(crate) fn response_name(response: &Response) -> &'static str {
    #[allow(unreachable_patterns)]
    match response {
        Response::NewSession(_) => "new-session",
        Response::KillServer(_) => "kill-server",
        Response::HasSession(_) => "has-session",
        Response::KillSession(_) => "kill-session",
        Response::RenameSession(_) => "rename-session",
        Response::ServerAccess(_) => "server-access",
        Response::LockServer(_) => "lock-server",
        Response::LockSession(_) => "lock-session",
        Response::LockClient(_) => "lock-client",
        Response::NewWindow(_) => "new-window",
        Response::KillWindow(_) => "kill-window",
        Response::SelectWindow(_) => "select-window",
        Response::RenameWindow(_) => "rename-window",
        Response::NextWindow(_) => "next-window",
        Response::PreviousWindow(_) => "previous-window",
        Response::LastWindow(_) => "last-window",
        Response::ListWindows(_) => "list-windows",
        Response::ListSessions(_) => "list-sessions",
        Response::LinkWindow(_) => "link-window",
        Response::MoveWindow(_) => "move-window",
        Response::SwapWindow(_) => "swap-window",
        Response::RotateWindow(_) => "rotate-window",
        Response::ResizeWindow(_) => "resize-window",
        Response::RespawnWindow(_) => "respawn-window",
        Response::SplitWindow(_) => "split-window",
        Response::SwapPane(_) => "swap-pane",
        Response::LastPane(_) => "last-pane",
        Response::JoinPane(_) => "join-pane",
        Response::MovePane(_) => "move-pane",
        Response::BreakPane(_) => "break-pane",
        Response::PipePane(_) => "pipe-pane",
        Response::RespawnPane(_) => "respawn-pane",
        Response::KillPane(_) => "kill-pane",
        Response::SelectLayout(_) => "select-layout",
        Response::NextLayout(_) => "next-layout",
        Response::PreviousLayout(_) => "previous-layout",
        Response::ResizePane(_) => "resize-pane",
        Response::DisplayPanes(_) => "display-panes",
        Response::ListPanes(_) => "list-panes",
        Response::SelectPane(_) => "select-pane",
        Response::CopyMode(_) => "copy-mode",
        Response::ClockMode(_) => "clock-mode",
        Response::SendKeys(_) => "send-keys",
        Response::BindKey(_) => "bind-key",
        Response::UnbindKey(_) => "unbind-key",
        Response::ListKeys(_) => "list-keys",
        Response::SendPrefix(_) => "send-prefix",
        Response::AttachSession(_) => "attach-session",
        Response::RefreshClient(_) => "refresh-client",
        Response::ListClients(_) => "list-clients",
        Response::SwitchClient(_) => "switch-client",
        Response::DetachClient(_) => "detach-client",
        Response::SuspendClient(_) => "suspend-client",
        Response::SetOption(_) | Response::SetOptionByName(_) => "set-option",
        Response::SetEnvironment(_) => "set-environment",
        Response::SetHook(_) => "set-hook",
        Response::ShowOptions(_) => "show-options",
        Response::ShowEnvironment(_) => "show-environment",
        Response::ShowHooks(_) => "show-hooks",
        Response::SetBuffer(_) => "set-buffer",
        Response::ShowBuffer(_) => "show-buffer",
        Response::PasteBuffer(_) => "paste-buffer",
        Response::ListBuffers(_) => "list-buffers",
        Response::DeleteBuffer(_) => "delete-buffer",
        Response::LoadBuffer(_) => "load-buffer",
        Response::SaveBuffer(_) => "save-buffer",
        Response::CapturePane(_) => "capture-pane",
        Response::ClearHistory(_) => "clear-history",
        Response::DisplayMessage(_) => "display-message",
        Response::ShowMessages(_) => "show-messages",
        Response::RunShell(_) => "run-shell",
        Response::IfShell(_) => "if-shell",
        Response::WaitFor(_) => "wait-for",
        Response::SourceFile(_) => "source-file",
        Response::UnlinkWindow(_) => "unlink-window",
        Response::ControlMode(_) => "control-mode",
        Response::WebShare(_) => "web-share",
        Response::Error(_) => "error",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::{
        expect_command_success, queued_source_file_success_command, tmux_cli_error_message,
    };
    use rmux_proto::{ErrorResponse, KillSessionResponse, Response, RmuxError, SourceFileResponse};

    #[test]
    fn cluster_a_queued_commands_accept_source_file_success() {
        for command_name in super::QUEUED_SOURCE_FILE_SUCCESS_COMMANDS {
            assert!(queued_source_file_success_command(command_name));
            expect_command_success(
                Response::SourceFile(SourceFileResponse::no_output()),
                command_name,
            )
            .unwrap_or_else(|error| {
                panic!("expected source-file success for {command_name}, got {error:?}")
            });
        }
    }

    #[test]
    fn source_file_success_gate_rejects_unknown_command_names() {
        assert!(!queued_source_file_success_command("choose-window"));
        assert!(!queued_source_file_success_command("confirm"));
        assert!(!queued_source_file_success_command("source-file"));
        assert!(!queued_source_file_success_command("totally-unknown"));
    }

    #[test]
    fn queued_source_file_success_gate_rejects_other_success_variants() {
        let error = expect_command_success(
            Response::KillSession(KillSessionResponse { existed: false }),
            "show-prompt-history",
        )
        .expect_err("queued commands must only accept source-file success responses");

        assert_eq!(error.exit_code(), 1);
        assert_eq!(
            error.message(),
            "protocol error: unexpected 'kill-session' response for show-prompt-history"
        );
    }

    #[test]
    fn queued_source_file_success_gate_does_not_mask_error_responses() {
        let error = expect_command_success(
            Response::Error(ErrorResponse {
                error: RmuxError::Message("queued failure".to_owned()),
            }),
            "show-prompt-history",
        )
        .expect_err("queued commands must still surface server errors");

        assert_eq!(error.exit_code(), 1);
        assert_eq!(error.message(), "queued failure");
    }

    #[test]
    fn window_index_collision_uses_tmux_cli_error_shape() {
        for command_name in ["link-window", "move-window"] {
            let error = expect_command_success(
                Response::Error(ErrorResponse {
                    error: RmuxError::InvalidTarget {
                        value: "s:1".to_owned(),
                        reason: "window index already exists in session".to_owned(),
                    },
                }),
                command_name,
            )
            .expect_err("index collision should remain an error");

            assert_eq!(error.exit_code(), 1);
            assert_eq!(error.message(), "index in use: 1");
        }
    }

    #[test]
    fn set_option_bad_value_errors_use_tmux_surface() {
        let message = tmux_cli_error_message(
            "set-window-option",
            &RmuxError::InvalidSetOption("bad value: false".to_owned()),
        );

        assert_eq!(message, "bad value: false");
    }

    #[test]
    fn set_option_number_errors_strip_internal_prefix() {
        for error in [
            RmuxError::InvalidSetOption(
                "invalid set-option request: value is too small: -5".to_owned(),
            ),
            RmuxError::Server("invalid set-option request: value is invalid: abc".to_owned()),
        ] {
            let message = tmux_cli_error_message("set-option", &error);
            assert!(!message.starts_with("invalid set-option request:"));
            assert!(message.starts_with("value is "), "{message}");
        }
    }

    #[test]
    fn detach_client_without_attached_client_uses_tmux_error_text() {
        let message = tmux_cli_error_message(
            "detach-client",
            &RmuxError::Server("detach-client requires an attached client".to_owned()),
        );

        assert_eq!(message, "no current client");
    }
}
