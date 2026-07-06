use rmux_proto::request::Request;
#[cfg(all(any(unix, windows), feature = "web"))]
use rmux_proto::CAPABILITY_WEB_SHARE;
use rmux_proto::{
    ControlModeResponse, ErrorResponse, HandshakeResponse, Response, RmuxError,
    SUPPORTED_CAPABILITIES,
};
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

    #[cfg(test)]
    pub(crate) async fn dispatch(&self, requester_pid: u32, request: Request) -> HandleOutcome {
        self.dispatch_for_connection(requester_pid, u64::from(requester_pid), request)
            .await
    }

    pub(crate) async fn dispatch_for_connection(
        &self,
        requester_pid: u32,
        connection_id: u64,
        request: Request,
    ) -> HandleOutcome {
        let request_for_hooks = request.clone();
        let (outcome, inline_hooks) = self
            .dispatch_captured(requester_pid, connection_id, request)
            .await;
        let inline_hook_names = inline_hooks
            .iter()
            .map(|pending| pending.hook)
            .collect::<Vec<_>>();
        self.run_inline_hooks(requester_pid, inline_hooks, None)
            .await;
        self.run_request_hooks(
            requester_pid,
            &request_for_hooks,
            &outcome.response,
            None,
            &inline_hook_names,
        )
        .await;
        outcome
    }

    #[async_recursion::async_recursion]
    pub(crate) async fn dispatch_captured(
        &self,
        requester_pid: u32,
        connection_id: u64,
        request: Request,
    ) -> (HandleOutcome, Vec<PendingInlineHook>) {
        capture_inline_hooks(Box::pin(self.dispatch_request(
            requester_pid,
            connection_id,
            request,
        )))
        .await
    }

    #[async_recursion::async_recursion]
    pub(crate) async fn dispatch_captured_with_client_name(
        &self,
        requester_pid: u32,
        connection_id: u64,
        request: Request,
        client_name: Option<String>,
    ) -> (HandleOutcome, Vec<PendingInlineHook>) {
        capture_inline_hooks(Box::pin(self.dispatch_request_with_client_name(
            requester_pid,
            connection_id,
            request,
            client_name,
        )))
        .await
    }

    async fn dispatch_request_with_client_name(
        &self,
        requester_pid: u32,
        connection_id: u64,
        request: Request,
        client_name: Option<String>,
    ) -> HandleOutcome {
        match (request, client_name) {
            (Request::RunShell(request), Some(client_name)) => HandleOutcome::response(
                self.handle_run_shell_with_client_name(requester_pid, *request, Some(client_name))
                    .await,
            ),
            (Request::IfShell(request), Some(client_name)) => HandleOutcome::response(
                self.handle_if_shell_with_client_name(requester_pid, *request, Some(client_name))
                    .await,
            ),
            (Request::SetOptionByName(request), Some(client_name)) => HandleOutcome::response(
                self.handle_set_option_by_name_with_client_name(*request, Some(client_name))
                    .await,
            ),
            (request, _) => {
                self.dispatch_request(requester_pid, connection_id, request)
                    .await
            }
        }
    }

    #[async_recursion::async_recursion]
    async fn dispatch_request(
        &self,
        requester_pid: u32,
        connection_id: u64,
        request: Request,
    ) -> HandleOutcome {
        if let Request::Handshake(request) = request {
            let supported_capabilities = supported_capabilities();
            let response = if let Err(error) = request.validate_against(&supported_capabilities) {
                Response::Error(ErrorResponse { error })
            } else {
                Response::Handshake(HandshakeResponse {
                    wire_version: rmux_proto::RMUX_WIRE_VERSION,
                    capabilities: supported_capabilities
                        .iter()
                        .map(|value| value.to_string())
                        .collect(),
                })
            };
            return HandleOutcome::response(response);
        }
        if let Request::DaemonStatus(_request) = request {
            return HandleOutcome::response(self.handle_daemon_status(connection_id).await);
        }

        #[cfg(windows)]
        self.wait_for_windows_deferred_request(&request).await;

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
            Request::CreateSessionLease(request) => {
                HandleOutcome::response(self.handle_create_session_lease(request).await)
            }
            Request::RenewSessionLease(request) => {
                HandleOutcome::response(self.handle_renew_session_lease(request).await)
            }
            Request::ReleaseSessionLease(request) => {
                HandleOutcome::response(self.handle_release_session_lease(request).await)
            }
            Request::RenameSession(request) => {
                HandleOutcome::response(self.handle_rename_session(request).await)
            }
            Request::NewWindow(request) => {
                HandleOutcome::response(self.handle_new_window(requester_pid, *request).await)
            }
            Request::KillWindow(request) => {
                HandleOutcome::response(self.handle_kill_window(request).await)
            }
            Request::SelectWindow(request) => HandleOutcome::response(
                self.handle_select_window(Some(requester_pid), request)
                    .await,
            ),
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
                HandleOutcome::response(self.handle_list_windows(*request).await)
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
                HandleOutcome::response(self.handle_respawn_window(requester_pid, *request).await)
            }
            Request::SplitWindow(request) => {
                HandleOutcome::response(self.handle_split_window(requester_pid, request).await)
            }
            Request::SplitWindowExt(request) => {
                HandleOutcome::response(self.handle_split_window_ext(requester_pid, *request).await)
            }
            Request::SplitWindowTargetAction(request) => HandleOutcome::response(
                self.handle_split_window_target_action(requester_pid, *request)
                    .await,
            ),
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
                HandleOutcome::response(self.handle_break_pane(*request).await)
            }
            Request::PipePane(request) => {
                HandleOutcome::response(self.handle_pipe_pane(requester_pid, request).await)
            }
            Request::RespawnPane(request) => {
                HandleOutcome::response(self.handle_respawn_pane(requester_pid, *request).await)
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
            Request::ShutdownIfIdle(_request) => {
                HandleOutcome::response(self.handle_shutdown_if_idle(connection_id).await)
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
            Request::ResizePaneTargetAction(request) => HandleOutcome::response(
                self.handle_resize_pane_target_action(requester_pid, request)
                    .await,
            ),
            Request::DisplayPanes(request) => {
                HandleOutcome::response(self.handle_display_panes(request).await)
            }
            Request::ListPanes(request) => {
                HandleOutcome::response(self.handle_list_panes(*request).await)
            }
            Request::SelectPane(request) => {
                HandleOutcome::response(self.handle_select_pane(*request).await)
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
            Request::SendKeysExt2(request) => HandleOutcome::response(
                Box::pin(self.handle_send_keys_ext2(requester_pid, *request)).await,
            ),
            Request::PaneBroadcastInput(request) => {
                HandleOutcome::response(self.handle_pane_broadcast_input(request).await)
            }
            Request::PaneOptionSet(request) => {
                HandleOutcome::response(self.handle_pane_option_set(request).await)
            }
            Request::PaneOptionGet(request) => {
                HandleOutcome::response(self.handle_pane_option_get(request).await)
            }
            Request::SubscribePaneState(request) => HandleOutcome::response(
                self.handle_subscribe_pane_state(connection_id, request)
                    .await,
            ),
            Request::PaneStateCursor(request) => {
                HandleOutcome::response(self.handle_pane_state_cursor(connection_id, request).await)
            }
            Request::UnsubscribePaneState(request) => HandleOutcome::response(
                self.handle_unsubscribe_pane_state(connection_id, request)
                    .await,
            ),
            Request::PaneForegroundState(request) => {
                HandleOutcome::response(self.handle_pane_foreground_state(request).await)
            }
            Request::BindKey(request) => {
                HandleOutcome::response(self.handle_bind_key(*request).await)
            }
            Request::UnbindKey(request) => {
                HandleOutcome::response(self.handle_unbind_key(request).await)
            }
            Request::ListKeys(request) => {
                HandleOutcome::response(self.handle_list_keys(*request).await)
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
                HandleOutcome::response(self.handle_set_option_by_name(*request).await)
            }
            Request::SetEnvironment(request) => {
                HandleOutcome::response(self.handle_set_environment(*request).await)
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
                HandleOutcome::response(self.handle_paste_buffer(*request).await)
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
                HandleOutcome::response(self.handle_capture_pane(*request).await)
            }
            Request::CapturePaneTargetAction(request) => HandleOutcome::response(
                self.handle_capture_pane_target_action(requester_pid, *request)
                    .await,
            ),
            Request::PaneSnapshot(request) => {
                HandleOutcome::response(self.handle_pane_snapshot(request).await)
            }
            Request::SubscribePaneOutput(request) => HandleOutcome::response(
                self.handle_subscribe_pane_output(connection_id, request)
                    .await,
            ),
            Request::SubscribePaneOutputRef(request) => HandleOutcome::response(
                self.handle_subscribe_pane_output_ref(connection_id, request)
                    .await,
            ),
            Request::UnsubscribePaneOutput(request) => HandleOutcome::response(
                self.handle_unsubscribe_pane_output(connection_id, request)
                    .await,
            ),
            Request::PaneOutputCursor(request) => HandleOutcome::response(
                self.handle_pane_output_cursor(connection_id, request).await,
            ),
            Request::SdkWaitForOutput(request) => HandleOutcome::response(
                self.handle_sdk_wait_for_output(connection_id, request)
                    .await,
            ),
            Request::SdkWaitForOutputRef(request) => HandleOutcome::response(
                self.handle_sdk_wait_for_output_ref(connection_id, request)
                    .await,
            ),
            Request::CancelSdkWait(request) => {
                HandleOutcome::response(self.handle_cancel_sdk_wait(request).await)
            }
            Request::ClearHistory(request) => {
                HandleOutcome::response(self.handle_clear_history(request).await)
            }
            Request::DisplayMessage(request) => {
                HandleOutcome::response(self.handle_display_message(requester_pid, request).await)
            }
            Request::DisplayMessageExt(request) => HandleOutcome::response(
                self.handle_display_message_ext(requester_pid, *request)
                    .await,
            ),
            Request::ResolveTarget(request) => {
                HandleOutcome::response(self.handle_resolve_target(requester_pid, request).await)
            }
            Request::ShowMessages(request) => {
                HandleOutcome::response(self.handle_show_messages(requester_pid, request).await)
            }
            Request::NewSessionExt(request) => {
                HandleOutcome::response(self.handle_new_session_ext(requester_pid, *request).await)
            }
            Request::AttachSessionExt(request) => {
                self.handle_attach_session_ext(requester_pid, request).await
            }
            Request::AttachSessionExt2(request) => {
                self.handle_attach_session_ext2(requester_pid, *request)
                    .await
            }
            Request::AttachSessionExt3(request) => {
                self.handle_attach_session_ext3(requester_pid, *request)
                    .await
            }
            Request::RefreshClient(request) => {
                HandleOutcome::response(self.handle_refresh_client(requester_pid, *request).await)
            }
            Request::ListClients(request) => {
                HandleOutcome::response(self.handle_list_clients(requester_pid, *request).await)
            }
            Request::SuspendClient(request) => {
                HandleOutcome::response(self.handle_suspend_client(requester_pid, request).await)
            }
            Request::DetachClientExt(request) => {
                HandleOutcome::response(self.handle_detach_client_ext(requester_pid, request).await)
            }
            Request::SwitchClientExt2(request) => HandleOutcome::response(
                self.handle_switch_client_ext2(requester_pid, *request)
                    .await,
            ),
            Request::SwitchClientExt3(request) => HandleOutcome::response(
                self.handle_switch_client_ext3(requester_pid, *request)
                    .await,
            ),
            Request::RunShell(request) => {
                HandleOutcome::response(self.handle_run_shell(requester_pid, *request).await)
            }
            Request::IfShell(request) => {
                HandleOutcome::response(self.handle_if_shell(requester_pid, *request).await)
            }
            Request::WaitFor(request) => {
                HandleOutcome::response(self.handle_wait_for(true, request).await)
            }
            Request::SourceFile(request) => {
                HandleOutcome::response(self.handle_source_file(requester_pid, *request).await)
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
            Request::PaneInput(request) => {
                HandleOutcome::response(self.handle_pane_input_ref(request).await)
            }
            Request::PaneResize(request) => {
                HandleOutcome::response(self.handle_pane_resize_ref(request).await)
            }
            Request::PaneKill(request) => {
                HandleOutcome::response(self.handle_pane_kill_ref(request).await)
            }
            Request::PaneRespawn(request) => {
                HandleOutcome::response(self.handle_pane_respawn_ref(*request).await)
            }
            Request::PaneSnapshotRef(request) => {
                HandleOutcome::response(self.handle_pane_snapshot_ref(request).await)
            }
            Request::PaneSelect(request) => {
                HandleOutcome::response(self.handle_pane_select_ref(request).await)
            }
            Request::WebShare(request) => {
                HandleOutcome::response(self.handle_web_share(*request).await)
            }
            _ => HandleOutcome::response(Response::Error(ErrorResponse {
                error: RmuxError::Server(format!(
                    "{command_name} is only available through queued command dispatch"
                )),
            })),
        }
    }
}

#[cfg(windows)]
impl RequestHandler {
    async fn wait_for_windows_deferred_request(&self, request: &Request) {
        if request_waits_for_windows_deferred_panes(request) {
            self.wait_for_windows_deferred_all_pane_pids().await;
        }
    }
}

#[cfg(windows)]
fn request_waits_for_windows_deferred_panes(request: &Request) -> bool {
    match request {
        Request::NewWindow(request) if request.detached => return false,
        _ => {}
    }

    !matches!(
        request,
        Request::NewSession(_)
            | Request::NewSessionExt(_)
            | Request::HasSession(_)
            | Request::CreateSessionLease(_)
            | Request::RenewSessionLease(_)
            | Request::ReleaseSessionLease(_)
            | Request::KillServer(_)
            | Request::ShutdownIfIdle(_)
            | Request::KillPane(_)
            | Request::RespawnPane(_)
            | Request::LockServer(_)
            | Request::LockSession(_)
            | Request::LockClient(_)
            | Request::ServerAccess(_)
            | Request::ListWindows(_)
            | Request::SendKeys(_)
            | Request::SendKeysExt(_)
            | Request::SendKeysExt2(_)
            | Request::PaneBroadcastInput(_)
            | Request::BindKey(_)
            | Request::UnbindKey(_)
            | Request::ListKeys(_)
            | Request::SetEnvironment(_)
            | Request::SetHook(_)
            | Request::SetHookMutation(_)
            | Request::ShowOptions(_)
            | Request::ShowEnvironment(_)
            | Request::ShowHooks(_)
            | Request::ListSessions(_)
            | Request::ListPanes(_)
            | Request::SetBuffer(_)
            | Request::ShowBuffer(_)
            | Request::ListBuffers(_)
            | Request::DeleteBuffer(_)
            | Request::LoadBuffer(_)
            | Request::SaveBuffer(_)
            | Request::CapturePane(_)
            | Request::CapturePaneTargetAction(_)
            | Request::PaneSnapshot(_)
            | Request::SubscribePaneOutput(_)
            | Request::SubscribePaneOutputRef(_)
            | Request::UnsubscribePaneOutput(_)
            | Request::PaneOutputCursor(_)
            | Request::SubscribePaneState(_)
            | Request::PaneStateCursor(_)
            | Request::UnsubscribePaneState(_)
            | Request::PaneForegroundState(_)
            | Request::SdkWaitForOutput(_)
            | Request::SdkWaitForOutputRef(_)
            | Request::CancelSdkWait(_)
            | Request::ClearHistory(_)
            | Request::DisplayMessage(_)
            | Request::DisplayMessageExt(_)
            | Request::ResolveTarget(_)
            | Request::ShowMessages(_)
            | Request::RefreshClient(_)
            | Request::ListClients(_)
            | Request::SuspendClient(_)
            | Request::DetachClient(_)
            | Request::DetachClientExt(_)
            | Request::SwitchClient(_)
            | Request::SwitchClientExt(_)
            | Request::SwitchClientExt2(_)
            | Request::SwitchClientExt3(_)
            | Request::RunShell(_)
            | Request::IfShell(_)
            | Request::WaitFor(_)
            | Request::ControlMode(_)
            | Request::PaneInput(_)
            | Request::PaneSnapshotRef(_)
            | Request::WebShare(_)
    )
}

fn supported_capabilities() -> Vec<&'static str> {
    #[cfg(all(any(unix, windows), feature = "web"))]
    {
        let mut capabilities = SUPPORTED_CAPABILITIES.to_vec();
        capabilities.push(CAPABILITY_WEB_SHARE);
        capabilities
    }
    #[cfg(not(all(any(unix, windows), feature = "web")))]
    {
        SUPPORTED_CAPABILITIES.to_vec()
    }
}

#[cfg(all(test, windows))]
mod windows_deferred_wait_tests {
    use rmux_proto::request::{
        KillPaneRequest, NewWindowRequest, Request, RespawnPaneRequest, SplitWindowExtRequest,
        SplitWindowTargetActionRequest,
    };
    use rmux_proto::{PaneTarget, ProcessCommand, SessionName, SplitDirection, SplitWindowTarget};

    use super::request_waits_for_windows_deferred_panes;

    fn session_name(value: &str) -> SessionName {
        SessionName::new(value).expect("valid session name")
    }

    fn new_window(detached: bool) -> Request {
        Request::NewWindow(Box::new(NewWindowRequest {
            target: session_name("bench"),
            name: None,
            detached,
            environment: None,
            command: None,
            start_directory: None,
            target_window_index: None,
            insert_at_target: false,
            process_command: None,
        }))
    }

    fn split_window_target_action(detached: bool) -> Request {
        Request::SplitWindowTargetAction(Box::new(SplitWindowTargetActionRequest {
            target: Some("bench".to_owned()),
            direction: SplitDirection::Horizontal,
            before: false,
            environment: None,
            command: None,
            process_command: None,
            start_directory: None,
            keep_alive_on_exit: None,
            detached,
            size: None,
            preserve_zoom: false,
            full_size: false,
            stdin_payload: None,
        }))
    }

    fn split_window_ext(detached: bool) -> Request {
        Request::SplitWindowExt(Box::new(SplitWindowExtRequest {
            target: SplitWindowTarget::Pane(PaneTarget::with_window(session_name("bench"), 0, 0)),
            direction: SplitDirection::Horizontal,
            before: false,
            environment: None,
            command: None,
            process_command: None,
            start_directory: None,
            keep_alive_on_exit: None,
            detached,
            size: None,
            preserve_zoom: false,
            full_size: false,
            stdin_payload: None,
        }))
    }

    fn kill_pane() -> Request {
        Request::KillPane(KillPaneRequest {
            target: PaneTarget::with_window(session_name("bench"), 0, 0),
            kill_all_except: false,
        })
    }

    fn respawn_pane() -> Request {
        Request::RespawnPane(Box::new(RespawnPaneRequest {
            target: PaneTarget::with_window(session_name("bench"), 0, 0),
            kill: true,
            start_directory: None,
            environment: None,
            command: None,
            process_command: Some(ProcessCommand::Shell("exit 0".to_owned())),
        }))
    }

    #[test]
    fn detached_new_window_does_not_wait_for_windows_deferred_panes() {
        assert!(!request_waits_for_windows_deferred_panes(&new_window(true)));
    }

    #[test]
    fn final_pane_lifecycle_mutations_do_not_wait_for_deferred_initial_spawn() {
        assert!(!request_waits_for_windows_deferred_panes(&kill_pane()));
        assert!(!request_waits_for_windows_deferred_panes(&respawn_pane()));
    }

    #[test]
    fn split_window_mutations_wait_for_windows_deferred_panes_even_when_detached() {
        assert!(request_waits_for_windows_deferred_panes(&split_window_ext(
            true
        )));
        assert!(request_waits_for_windows_deferred_panes(
            &split_window_target_action(true)
        ));
    }

    #[test]
    fn attached_window_mutations_wait_for_windows_deferred_panes() {
        assert!(request_waits_for_windows_deferred_panes(&new_window(false)));
        assert!(request_waits_for_windows_deferred_panes(&split_window_ext(
            false
        )));
        assert!(request_waits_for_windows_deferred_panes(
            &split_window_target_action(false)
        ));
    }
}
