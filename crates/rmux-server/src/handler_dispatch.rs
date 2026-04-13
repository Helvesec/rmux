use rmux_proto::request::Request;
use rmux_proto::{ControlModeResponse, ErrorResponse, Response, RmuxError};
#[cfg(test)]
use tokio::sync::broadcast;
#[cfg(test)]
use tracing::warn;

use crate::hook_runtime::{capture_inline_hooks, PendingInlineHook};
use crate::outer_terminal::OuterTerminalContext;
use crate::pane_io::HandleOutcome;

use super::{client_environment_snapshot, effective_client_terminal_context, RequestHandler};

impl RequestHandler {
    #[cfg(test)]
    pub(crate) async fn handle(&self, request: Request) -> Response {
        let mut lifecycle_events = self.subscribe_lifecycle_events();
        let outcome = self.dispatch(std::process::id(), request).await;

        loop {
            match lifecycle_events.try_recv() {
                Ok(event) => self.dispatch_lifecycle_hook(event).await,
                Err(
                    broadcast::error::TryRecvError::Empty | broadcast::error::TryRecvError::Closed,
                ) => {
                    break;
                }
                Err(broadcast::error::TryRecvError::Lagged(skipped)) => {
                    warn!(
                        skipped,
                        "test lifecycle hook receiver lagged; dropping events"
                    );
                }
            }
        }

        outcome.response
    }

    pub(crate) async fn dispatch(&self, requester_pid: u32, request: Request) -> HandleOutcome {
        let request_for_hooks = request.clone();
        let (outcome, inline_hooks) = self.dispatch_captured(requester_pid, request).await;
        self.run_inline_hooks(requester_pid, inline_hooks, None)
            .await;
        self.run_request_hooks(requester_pid, &request_for_hooks, &outcome.response, None)
            .await;
        outcome
    }

    pub(crate) async fn dispatch_captured(
        &self,
        requester_pid: u32,
        request: Request,
    ) -> (HandleOutcome, Vec<PendingInlineHook>) {
        capture_inline_hooks(self.dispatch_request(requester_pid, request)).await
    }

    async fn dispatch_request(&self, requester_pid: u32, request: Request) -> HandleOutcome {
        if let Some(error) = self.take_startup_config_error().await {
            return HandleOutcome::response(Response::Error(ErrorResponse { error }));
        }

        let command_name = request.command_name().to_owned();
        #[allow(unreachable_patterns)]
        match request {
            Request::NewSession(request) => {
                HandleOutcome::response(self.handle_new_session(requester_pid, request).await)
            }
            Request::HasSession(request) => {
                HandleOutcome::response(self.handle_has_session(request).await)
            }
            Request::KillSession(request) => {
                HandleOutcome::response(self.handle_kill_session(request).await)
            }
            Request::RenameSession(request) => {
                HandleOutcome::response(self.handle_rename_session(request).await)
            }
            Request::NewWindow(request) => {
                HandleOutcome::response(self.handle_new_window(request).await)
            }
            Request::KillWindow(request) => {
                HandleOutcome::response(self.handle_kill_window(request).await)
            }
            Request::SelectWindow(request) => {
                HandleOutcome::response(self.handle_select_window(request).await)
            }
            Request::RenameWindow(request) => {
                HandleOutcome::response(self.handle_rename_window(request).await)
            }
            Request::NextWindow(request) => {
                HandleOutcome::response(self.handle_next_window(request).await)
            }
            Request::PreviousWindow(request) => {
                HandleOutcome::response(self.handle_previous_window(request).await)
            }
            Request::LastWindow(request) => {
                HandleOutcome::response(self.handle_last_window(request).await)
            }
            Request::ListWindows(request) => {
                HandleOutcome::response(self.handle_list_windows(request).await)
            }
            Request::LinkWindow(request) => {
                HandleOutcome::response(self.handle_link_window(request).await)
            }
            Request::MoveWindow(request) => {
                HandleOutcome::response(self.handle_move_window(request).await)
            }
            Request::SwapWindow(request) => {
                HandleOutcome::response(self.handle_swap_window(request).await)
            }
            Request::RotateWindow(request) => {
                HandleOutcome::response(self.handle_rotate_window(request).await)
            }
            Request::ResizeWindow(request) => {
                HandleOutcome::response(self.handle_resize_window(request).await)
            }
            Request::RespawnWindow(request) => {
                HandleOutcome::response(self.handle_respawn_window(request).await)
            }
            Request::SplitWindow(request) => {
                HandleOutcome::response(self.handle_split_window(request).await)
            }
            Request::SplitWindowExt(request) => {
                HandleOutcome::response(self.handle_split_window_ext(request).await)
            }
            Request::SwapPane(request) => {
                HandleOutcome::response(self.handle_swap_pane(request).await)
            }
            Request::LastPane(request) => {
                HandleOutcome::response(self.handle_last_pane(request).await)
            }
            Request::JoinPane(request) => {
                HandleOutcome::response(self.handle_join_pane(request).await)
            }
            Request::MovePane(request) => {
                HandleOutcome::response(self.handle_move_pane(request).await)
            }
            Request::BreakPane(request) => {
                HandleOutcome::response(self.handle_break_pane(request).await)
            }
            Request::PipePane(request) => {
                HandleOutcome::response(self.handle_pipe_pane(requester_pid, request).await)
            }
            Request::RespawnPane(request) => {
                HandleOutcome::response(self.handle_respawn_pane(request).await)
            }
            Request::KillPane(request) => {
                HandleOutcome::response(self.handle_kill_pane(request).await)
            }
            Request::SelectLayout(request) => {
                HandleOutcome::response(self.handle_select_layout(request).await)
            }
            Request::SelectCustomLayout(request) => {
                HandleOutcome::response(self.handle_select_custom_layout(request).await)
            }
            Request::SelectOldLayout(request) => {
                HandleOutcome::response(self.handle_select_old_layout(request).await)
            }
            Request::SpreadLayout(request) => {
                HandleOutcome::response(self.handle_spread_layout(request).await)
            }
            Request::KillServer(_request) => {
                HandleOutcome::response(self.handle_kill_server().await)
            }
            Request::LockServer(_request) => {
                HandleOutcome::response(self.handle_lock_server().await)
            }
            Request::LockSession(request) => {
                HandleOutcome::response(self.handle_lock_session(request).await)
            }
            Request::LockClient(request) => {
                HandleOutcome::response(self.handle_lock_client(requester_pid, request).await)
            }
            Request::ServerAccess(request) => {
                HandleOutcome::response(self.handle_server_access(request).await)
            }
            Request::NextLayout(request) => {
                HandleOutcome::response(self.handle_next_layout(request).await)
            }
            Request::PreviousLayout(request) => {
                HandleOutcome::response(self.handle_previous_layout(request).await)
            }
            Request::ResizePane(request) => {
                HandleOutcome::response(self.handle_resize_pane(request).await)
            }
            Request::DisplayPanes(request) => {
                HandleOutcome::response(self.handle_display_panes(request).await)
            }
            Request::ListPanes(request) => {
                HandleOutcome::response(self.handle_list_panes(request).await)
            }
            Request::SelectPane(request) => {
                HandleOutcome::response(self.handle_select_pane(request).await)
            }
            Request::SelectPaneAdjacent(request) => {
                HandleOutcome::response(self.handle_select_pane_adjacent(request).await)
            }
            Request::SelectPaneMark(request) => {
                HandleOutcome::response(self.handle_select_pane_mark(request).await)
            }
            Request::CopyMode(request) => {
                HandleOutcome::response(self.handle_copy_mode(requester_pid, request).await)
            }
            Request::ClockMode(request) => {
                HandleOutcome::response(self.handle_clock_mode(requester_pid, request).await)
            }
            Request::SendKeys(request) => {
                HandleOutcome::response(self.handle_send_keys(request).await)
            }
            Request::SendKeysExt(request) => HandleOutcome::response(
                Box::pin(self.handle_send_keys_ext(requester_pid, request)).await,
            ),
            Request::BindKey(request) => {
                HandleOutcome::response(self.handle_bind_key(request).await)
            }
            Request::UnbindKey(request) => {
                HandleOutcome::response(self.handle_unbind_key(request).await)
            }
            Request::ListKeys(request) => {
                HandleOutcome::response(self.handle_list_keys(request).await)
            }
            Request::SendPrefix(request) => {
                HandleOutcome::response(self.handle_send_prefix(requester_pid, request).await)
            }
            Request::AttachSession(request) => {
                self.handle_attach_session(requester_pid, request).await
            }
            Request::SwitchClient(request) => {
                HandleOutcome::response(self.handle_switch_client(requester_pid, request).await)
            }
            Request::SwitchClientExt(request) => {
                HandleOutcome::response(self.handle_switch_client_ext(requester_pid, request).await)
            }
            Request::DetachClient(_request) => {
                HandleOutcome::response(self.handle_detach_client(requester_pid).await)
            }
            Request::SetOption(request) => {
                HandleOutcome::response(self.handle_set_option(request).await)
            }
            Request::SetOptionByName(request) => {
                HandleOutcome::response(self.handle_set_option_by_name(request).await)
            }
            Request::SetEnvironment(request) => {
                HandleOutcome::response(self.handle_set_environment(request).await)
            }
            Request::SetHook(request) => {
                HandleOutcome::response(self.handle_set_hook(request).await)
            }
            Request::SetHookMutation(request) => {
                HandleOutcome::response(self.handle_set_hook_mutation(request).await)
            }
            Request::ShowOptions(request) => {
                HandleOutcome::response(self.handle_show_options(request).await)
            }
            Request::ShowEnvironment(request) => {
                HandleOutcome::response(self.handle_show_environment(request).await)
            }
            Request::ShowHooks(request) => {
                HandleOutcome::response(self.handle_show_hooks(request).await)
            }
            Request::ListSessions(request) => {
                HandleOutcome::response(self.handle_list_sessions(request).await)
            }
            Request::SetBuffer(request) => {
                HandleOutcome::response(self.handle_set_buffer(requester_pid, request).await)
            }
            Request::ShowBuffer(request) => {
                HandleOutcome::response(self.handle_show_buffer(request).await)
            }
            Request::PasteBuffer(request) => {
                HandleOutcome::response(self.handle_paste_buffer(request).await)
            }
            Request::ListBuffers(request) => {
                HandleOutcome::response(self.handle_list_buffers(request).await)
            }
            Request::DeleteBuffer(request) => {
                HandleOutcome::response(self.handle_delete_buffer(request).await)
            }
            Request::LoadBuffer(request) => {
                HandleOutcome::response(self.handle_load_buffer(requester_pid, request).await)
            }
            Request::SaveBuffer(request) => {
                HandleOutcome::response(self.handle_save_buffer(request).await)
            }
            Request::CapturePane(request) => {
                HandleOutcome::response(self.handle_capture_pane(request).await)
            }
            Request::ClearHistory(request) => {
                HandleOutcome::response(self.handle_clear_history(request).await)
            }
            Request::DisplayMessage(request) => {
                HandleOutcome::response(self.handle_display_message(requester_pid, request).await)
            }
            Request::ResolveTarget(request) => {
                HandleOutcome::response(self.handle_resolve_target(request).await)
            }
            Request::ShowMessages(request) => {
                HandleOutcome::response(self.handle_show_messages(requester_pid, request).await)
            }
            Request::NewSessionExt(request) => {
                HandleOutcome::response(self.handle_new_session_ext(requester_pid, request).await)
            }
            Request::AttachSessionExt(request) => {
                self.handle_attach_session_ext(requester_pid, request).await
            }
            Request::AttachSessionExt2(request) => {
                self.handle_attach_session_ext2(requester_pid, request)
                    .await
            }
            Request::RefreshClient(request) => {
                HandleOutcome::response(self.handle_refresh_client(requester_pid, request).await)
            }
            Request::ListClients(request) => {
                HandleOutcome::response(self.handle_list_clients(requester_pid, request).await)
            }
            Request::SuspendClient(request) => {
                HandleOutcome::response(self.handle_suspend_client(requester_pid, request).await)
            }
            Request::DetachClientExt(request) => {
                HandleOutcome::response(self.handle_detach_client_ext(requester_pid, request).await)
            }
            Request::SwitchClientExt2(request) => HandleOutcome::response(
                self.handle_switch_client_ext2(requester_pid, request).await,
            ),
            Request::SwitchClientExt3(request) => HandleOutcome::response(
                self.handle_switch_client_ext3(requester_pid, request).await,
            ),
            Request::RunShell(request) => {
                HandleOutcome::response(self.handle_run_shell(request).await)
            }
            Request::IfShell(request) => {
                HandleOutcome::response(self.handle_if_shell(requester_pid, request).await)
            }
            Request::WaitFor(request) => {
                HandleOutcome::response(self.handle_wait_for(true, request).await)
            }
            Request::SourceFile(request) => {
                HandleOutcome::response(self.handle_source_file(requester_pid, request).await)
            }
            Request::UnlinkWindow(request) => {
                HandleOutcome::response(self.handle_unlink_window(request).await)
            }
            Request::ControlMode(request) => {
                let client_environment = client_environment_snapshot(requester_pid);
                let client_terminal = effective_client_terminal_context(
                    client_environment.as_ref(),
                    &request.client_terminal,
                );
                let terminal_context =
                    OuterTerminalContext::from_environment(client_environment.as_ref())
                        .with_client_terminal(&client_terminal);
                HandleOutcome::control(
                    Response::ControlMode(ControlModeResponse { mode: request.mode }),
                    crate::control::ControlModeUpgrade {
                        mode: request.mode,
                        terminal_context,
                    },
                )
            }
            _ => HandleOutcome::response(Response::Error(ErrorResponse {
                error: RmuxError::Server(format!(
                    "{command_name} is only available through queued command dispatch"
                )),
            })),
        }
    }
}
