use super::{request_changes_hook_identity_aliases, supported_capabilities};
use crate::future::boxed;
use crate::handler::{
    client_environment_snapshot, effective_client_terminal_context, RequestHandler,
};
use crate::outer_terminal::OuterTerminalContext;
use crate::pane_io::HandleOutcome;
use rmux_proto::{
    ControlModeResponse, ErrorResponse, HandshakeResponse, Request, Response, RmuxError,
};

impl RequestHandler {
    pub(super) async fn dispatch_request_inner(
        &self,
        requester_pid: u32,
        connection_id: u64,
        request: Request,
    ) -> HandleOutcome {
        if let Request::RuntimeCommandExecution(request) = request {
            return HandleOutcome::response(
                boxed(|| self.handle_runtime_command_execution(requester_pid, *request)).await,
            );
        }
        if let Request::RuntimeCommandExecutionV2(request) = request {
            return HandleOutcome::response(
                boxed(|| self.handle_runtime_command_execution_v2(requester_pid, *request)).await,
            );
        }
        if let Request::SplitWindowCommandV1(request) = request {
            return HandleOutcome::response(
                boxed(|| self.handle_split_window_command_v1(requester_pid, *request)).await,
            );
        }
        if let Request::NewPaneInputV1(request) = request {
            return HandleOutcome::response(
                boxed(|| self.handle_new_pane_input_v1(requester_pid, *request)).await,
            );
        }
        if let Request::SourceFileInputV1(request) = request {
            return HandleOutcome::response(
                boxed(|| self.handle_source_file_input_v1(requester_pid, *request)).await,
            );
        }
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
            return HandleOutcome::response(
                boxed(|| self.handle_daemon_status(requester_pid, connection_id)).await,
            );
        }

        #[cfg(windows)]
        self.wait_for_windows_deferred_request(&request).await;

        let refresh_hook_identities = request_changes_hook_identity_aliases(&request);
        let command_name = request.command_name().to_owned();
        #[allow(unreachable_patterns)]
        let outcome = match request {
            Request::NewSession(request) => HandleOutcome::response(
                boxed(|| self.handle_new_session(requester_pid, request)).await,
            ),
            Request::HasSession(request) => {
                HandleOutcome::response(boxed(|| self.handle_has_session(request)).await)
            }
            Request::KillSession(request) => {
                HandleOutcome::response(boxed(|| self.handle_kill_session(request)).await)
            }
            Request::CreateSessionLease(request) => {
                HandleOutcome::response(boxed(|| self.handle_create_session_lease(request)).await)
            }
            Request::RenewSessionLease(request) => {
                HandleOutcome::response(boxed(|| self.handle_renew_session_lease(request)).await)
            }
            Request::ReleaseSessionLease(request) => {
                HandleOutcome::response(boxed(|| self.handle_release_session_lease(request)).await)
            }
            Request::RenameSession(request) => {
                HandleOutcome::response(boxed(|| self.handle_rename_session(request)).await)
            }
            Request::NewWindow(request) => HandleOutcome::response(
                boxed(|| self.handle_new_window(requester_pid, *request)).await,
            ),
            Request::KillWindow(request) => {
                HandleOutcome::response(boxed(|| self.handle_kill_window(request)).await)
            }
            Request::SelectWindow(request) => HandleOutcome::response(
                boxed(|| self.handle_select_window(Some(requester_pid), request)).await,
            ),
            Request::RenameWindow(request) => {
                HandleOutcome::response(boxed(|| self.handle_rename_window(request)).await)
            }
            Request::NextWindow(request) => {
                HandleOutcome::response(boxed(|| self.handle_next_window(request)).await)
            }
            Request::PreviousWindow(request) => {
                HandleOutcome::response(boxed(|| self.handle_previous_window(request)).await)
            }
            Request::LastWindow(request) => {
                HandleOutcome::response(boxed(|| self.handle_last_window(request)).await)
            }
            Request::ListWindows(request) => {
                HandleOutcome::response(boxed(|| self.handle_list_windows(*request)).await)
            }
            Request::LinkWindow(request) => {
                HandleOutcome::response(boxed(|| self.handle_link_window(request)).await)
            }
            Request::MoveWindow(request) => {
                HandleOutcome::response(boxed(|| self.handle_move_window(request)).await)
            }
            Request::SwapWindow(request) => {
                HandleOutcome::response(boxed(|| self.handle_swap_window(request)).await)
            }
            Request::RotateWindow(request) => {
                HandleOutcome::response(boxed(|| self.handle_rotate_window(request)).await)
            }
            Request::ResizeWindow(request) => {
                HandleOutcome::response(boxed(|| self.handle_resize_window(request)).await)
            }
            Request::RespawnWindow(request) => HandleOutcome::response(
                boxed(|| self.handle_respawn_window(requester_pid, *request)).await,
            ),
            Request::SplitWindow(request) => HandleOutcome::response(
                boxed(|| self.handle_split_window(requester_pid, request)).await,
            ),
            Request::SplitWindowExt(request) => HandleOutcome::response(
                boxed(|| self.handle_split_window_ext(requester_pid, *request)).await,
            ),
            Request::SplitWindowTargetAction(request) => HandleOutcome::response(
                boxed(|| self.handle_split_window_target_action(requester_pid, *request)).await,
            ),
            Request::SplitWindowIdentity(request) => HandleOutcome::response(
                boxed(|| self.handle_split_window_identity(requester_pid, *request)).await,
            ),
            Request::SwapPane(request) => {
                HandleOutcome::response(boxed(|| self.handle_swap_pane(request)).await)
            }
            Request::LastPane(request) => {
                HandleOutcome::response(boxed(|| self.handle_last_pane(request)).await)
            }
            Request::JoinPane(request) => {
                HandleOutcome::response(boxed(|| self.handle_join_pane(request)).await)
            }
            Request::MovePane(request) => {
                HandleOutcome::response(boxed(|| self.handle_move_pane(request)).await)
            }
            Request::BreakPane(request) => HandleOutcome::response(
                boxed(|| self.handle_break_pane(requester_pid, *request)).await,
            ),
            Request::PipePane(request) => HandleOutcome::response(
                boxed(|| self.handle_pipe_pane(requester_pid, request)).await,
            ),
            Request::RespawnPane(request) => HandleOutcome::response(
                boxed(|| self.handle_respawn_pane(requester_pid, *request)).await,
            ),
            Request::KillPane(request) => {
                HandleOutcome::response(boxed(|| self.handle_kill_pane(request)).await)
            }
            Request::SelectLayout(request) => {
                HandleOutcome::response(boxed(|| self.handle_select_layout(request)).await)
            }
            Request::ExportWindowSnapshot(request) => {
                HandleOutcome::response(boxed(|| self.handle_export_window_snapshot(request)).await)
            }
            Request::ResizePaneAxes(request) => {
                HandleOutcome::response(boxed(|| self.handle_resize_pane_axes(*request)).await)
            }
            Request::ExportWindowSnapshotV2(request) => HandleOutcome::response(
                boxed(|| self.handle_export_window_snapshot_v2(request)).await,
            ),
            Request::RestoreWindowSnapshot(request) => HandleOutcome::response(
                boxed(|| self.handle_restore_window_snapshot(*request)).await,
            ),
            Request::RestoreWindowSnapshotV2(request) => HandleOutcome::response(
                boxed(|| self.handle_restore_window_snapshot_v2(*request)).await,
            ),
            Request::SelectCustomLayout(request) => {
                HandleOutcome::response(boxed(|| self.handle_select_custom_layout(request)).await)
            }
            Request::SelectOldLayout(request) => {
                HandleOutcome::response(boxed(|| self.handle_select_old_layout(request)).await)
            }
            Request::SpreadLayout(request) => {
                HandleOutcome::response(boxed(|| self.handle_spread_layout(request)).await)
            }
            Request::KillServer(_request) => {
                HandleOutcome::response(boxed(|| self.handle_kill_server()).await)
            }
            Request::ShutdownIfIdle(_request) => {
                HandleOutcome::response(boxed(|| self.handle_shutdown_if_idle(connection_id)).await)
            }
            Request::LockServer(_request) => {
                HandleOutcome::response(boxed(|| self.handle_lock_server()).await)
            }
            Request::LockSession(request) => {
                HandleOutcome::response(boxed(|| self.handle_lock_session(request)).await)
            }
            Request::LockClient(request) => HandleOutcome::response(
                boxed(|| self.handle_lock_client(requester_pid, request)).await,
            ),
            Request::ServerAccess(request) => {
                HandleOutcome::response(boxed(|| self.handle_server_access(request)).await)
            }
            Request::NextLayout(request) => {
                HandleOutcome::response(boxed(|| self.handle_next_layout(request)).await)
            }
            Request::PreviousLayout(request) => {
                HandleOutcome::response(boxed(|| self.handle_previous_layout(request)).await)
            }
            Request::ResizePane(request) => {
                HandleOutcome::response(boxed(|| self.handle_resize_pane(request)).await)
            }
            Request::ResizePaneTargetAction(request) => HandleOutcome::response(
                boxed(|| self.handle_resize_pane_target_action(requester_pid, request)).await,
            ),
            Request::DisplayPanes(request) => HandleOutcome::response(
                boxed(|| self.handle_display_panes(requester_pid, *request)).await,
            ),
            Request::ListPanes(request) => {
                HandleOutcome::response(boxed(|| self.handle_list_panes(*request)).await)
            }
            Request::SelectPane(request) => {
                HandleOutcome::response(boxed(|| self.handle_select_pane(*request)).await)
            }
            Request::SelectPaneAdjacent(request) => {
                HandleOutcome::response(boxed(|| self.handle_select_pane_adjacent(request)).await)
            }
            Request::SelectPaneMark(request) => {
                HandleOutcome::response(boxed(|| self.handle_select_pane_mark(request)).await)
            }
            Request::CopyMode(request) => HandleOutcome::response(
                boxed(|| self.handle_copy_mode(requester_pid, request)).await,
            ),
            Request::ClockMode(request) => HandleOutcome::response(
                boxed(|| self.handle_clock_mode(requester_pid, request)).await,
            ),
            Request::SendKeys(request) => HandleOutcome::response(
                boxed(|| self.handle_send_keys_for_requester(requester_pid, request)).await,
            ),
            Request::SendKeysExt(request) => HandleOutcome::response(
                Box::pin(self.handle_send_keys_ext(requester_pid, request)).await,
            ),
            Request::SendKeysExt2(request) => HandleOutcome::response(
                Box::pin(self.handle_send_keys_ext2(requester_pid, *request)).await,
            ),
            Request::PaneBroadcastInput(request) => {
                HandleOutcome::response(boxed(|| self.handle_pane_broadcast_input(request)).await)
            }
            Request::PaneOptionSet(request) => {
                HandleOutcome::response(boxed(|| self.handle_pane_option_set(request)).await)
            }
            Request::PaneOptionGet(request) => {
                HandleOutcome::response(boxed(|| self.handle_pane_option_get(request)).await)
            }
            Request::SubscribePaneState(request) => HandleOutcome::response(
                boxed(|| self.handle_subscribe_pane_state(connection_id, request)).await,
            ),
            Request::PaneStateCursor(request) => HandleOutcome::response(
                boxed(|| self.handle_pane_state_cursor(connection_id, request)).await,
            ),
            Request::UnsubscribePaneState(request) => HandleOutcome::response(
                boxed(|| self.handle_unsubscribe_pane_state(connection_id, request)).await,
            ),
            Request::PaneForegroundState(request) => {
                HandleOutcome::response(boxed(|| self.handle_pane_foreground_state(request)).await)
            }
            Request::BindKey(request) => {
                HandleOutcome::response(boxed(|| self.handle_bind_key(*request)).await)
            }
            Request::UnbindKey(request) => {
                HandleOutcome::response(boxed(|| self.handle_unbind_key(request)).await)
            }
            Request::ListKeys(request) => {
                HandleOutcome::response(boxed(|| self.handle_list_keys(*request)).await)
            }
            Request::SendPrefix(request) => HandleOutcome::response(
                boxed(|| self.handle_send_prefix(requester_pid, request)).await,
            ),
            Request::AttachSession(request) => {
                boxed(|| self.handle_attach_session(requester_pid, request)).await
            }
            Request::SwitchClient(request) => HandleOutcome::response(
                boxed(|| self.handle_switch_client(requester_pid, request)).await,
            ),
            Request::SwitchClientExt(request) => HandleOutcome::response(
                boxed(|| self.handle_switch_client_ext(requester_pid, request)).await,
            ),
            Request::DetachClient(_request) => {
                HandleOutcome::response(boxed(|| self.handle_detach_client(requester_pid)).await)
            }
            Request::SetOption(request) => {
                HandleOutcome::response(boxed(|| self.handle_set_option(request)).await)
            }
            Request::SetOptionByName(request) => {
                HandleOutcome::response(boxed(|| self.handle_set_option_by_name(*request)).await)
            }
            Request::SetEnvironment(request) => {
                HandleOutcome::response(boxed(|| self.handle_set_environment(*request)).await)
            }
            Request::SetHook(request) => {
                HandleOutcome::response(boxed(|| self.handle_set_hook(request)).await)
            }
            Request::SetHookMutation(request) => {
                HandleOutcome::response(boxed(|| self.handle_set_hook_mutation(request)).await)
            }
            Request::ShowOptions(request) => {
                HandleOutcome::response(boxed(|| self.handle_show_options(request)).await)
            }
            Request::ShowEnvironment(request) => {
                HandleOutcome::response(boxed(|| self.handle_show_environment(request)).await)
            }
            Request::ShowHooks(request) => {
                HandleOutcome::response(boxed(|| self.handle_show_hooks(request)).await)
            }
            Request::ListSessions(request) => {
                HandleOutcome::response(boxed(|| self.handle_list_sessions(request)).await)
            }
            Request::SetBuffer(request) => HandleOutcome::response(
                boxed(|| self.handle_set_buffer(requester_pid, *request)).await,
            ),
            Request::ShowBuffer(request) => {
                HandleOutcome::response(boxed(|| self.handle_show_buffer(request)).await)
            }
            Request::PasteBuffer(request) => {
                HandleOutcome::response(boxed(|| self.handle_paste_buffer(*request)).await)
            }
            Request::ListBuffers(request) => {
                HandleOutcome::response(boxed(|| self.handle_list_buffers(request)).await)
            }
            Request::DeleteBuffer(request) => {
                HandleOutcome::response(boxed(|| self.handle_delete_buffer(request)).await)
            }
            Request::LoadBuffer(request) => HandleOutcome::response(
                boxed(|| self.handle_load_buffer(requester_pid, *request)).await,
            ),
            Request::SaveBuffer(request) => {
                HandleOutcome::response(boxed(|| self.handle_save_buffer(request)).await)
            }
            Request::CapturePane(request) => {
                HandleOutcome::response(boxed(|| self.handle_capture_pane(*request)).await)
            }
            Request::CapturePaneTargetAction(request) => HandleOutcome::response(
                boxed(|| self.handle_capture_pane_target_action(requester_pid, *request)).await,
            ),
            Request::PaneSnapshot(request) => {
                HandleOutcome::response(boxed(|| self.handle_pane_snapshot(request)).await)
            }
            Request::SubscribePaneOutput(request) => HandleOutcome::response(
                boxed(|| self.handle_subscribe_pane_output(connection_id, request)).await,
            ),
            Request::SubscribePaneOutputRef(request) => HandleOutcome::response(
                boxed(|| self.handle_subscribe_pane_output_ref(connection_id, request)).await,
            ),
            Request::UnsubscribePaneOutput(request) => HandleOutcome::response(
                boxed(|| self.handle_unsubscribe_pane_output(connection_id, request)).await,
            ),
            Request::PaneOutputCursor(request) => HandleOutcome::response(
                boxed(|| self.handle_pane_output_cursor(connection_id, request)).await,
            ),
            Request::SubscribePaneStream(request) => HandleOutcome::response(
                boxed(|| self.handle_subscribe_pane_stream(connection_id, request)).await,
            ),
            Request::PaneStreamCursor(request) => HandleOutcome::response(
                boxed(|| self.handle_pane_stream_cursor(connection_id, request)).await,
            ),
            Request::UnsubscribePaneStream(request) => HandleOutcome::response(
                boxed(|| self.handle_unsubscribe_pane_stream(connection_id, request)).await,
            ),
            Request::SdkWaitForOutput(request) => HandleOutcome::response(
                boxed(|| self.handle_sdk_wait_for_output(connection_id, request)).await,
            ),
            Request::SdkWaitForOutputRef(request) => HandleOutcome::response(
                boxed(|| self.handle_sdk_wait_for_output_ref(connection_id, request)).await,
            ),
            Request::CancelSdkWait(request) => {
                HandleOutcome::response(boxed(|| self.handle_cancel_sdk_wait(request)).await)
            }
            Request::ClearHistory(request) => {
                HandleOutcome::response(boxed(|| self.handle_clear_history(request)).await)
            }
            Request::DisplayMessage(request) => HandleOutcome::response(
                boxed(|| self.handle_display_message(requester_pid, request)).await,
            ),
            Request::DisplayMessageExt(request) => HandleOutcome::response(
                boxed(|| self.handle_display_message_ext(requester_pid, *request)).await,
            ),
            Request::ResolveTarget(request) => HandleOutcome::response(
                boxed(|| self.handle_resolve_target(requester_pid, request)).await,
            ),
            Request::ShowMessages(request) => HandleOutcome::response(
                boxed(|| self.handle_show_messages(requester_pid, request)).await,
            ),
            Request::NewSessionExt(request) => HandleOutcome::response(
                boxed(|| self.handle_new_session_ext(requester_pid, *request)).await,
            ),
            Request::AttachSessionExt(request) => {
                boxed(|| self.handle_attach_session_ext(requester_pid, request)).await
            }
            Request::AttachSessionExt2(request) => {
                boxed(|| self.handle_attach_session_ext2(requester_pid, *request)).await
            }
            Request::AttachSessionExt3(request) => {
                boxed(|| self.handle_attach_session_ext3(requester_pid, *request)).await
            }
            Request::RefreshClient(request) => HandleOutcome::response(
                boxed(|| self.handle_refresh_client(requester_pid, *request)).await,
            ),
            Request::ListClients(request) => HandleOutcome::response(
                boxed(|| self.handle_list_clients(requester_pid, *request)).await,
            ),
            Request::SuspendClient(request) => HandleOutcome::response(
                boxed(|| self.handle_suspend_client(requester_pid, request)).await,
            ),
            Request::DetachClientExt(request) => HandleOutcome::response(
                boxed(|| self.handle_detach_client_ext(requester_pid, request)).await,
            ),
            Request::SwitchClientExt2(request) => HandleOutcome::response(
                boxed(|| self.handle_switch_client_ext2(requester_pid, *request)).await,
            ),
            Request::SwitchClientExt3(request) => HandleOutcome::response(
                boxed(|| self.handle_switch_client_ext3(requester_pid, *request)).await,
            ),
            Request::RunShell(request) => HandleOutcome::response(
                boxed(|| self.handle_run_shell(requester_pid, *request)).await,
            ),
            Request::IfShell(request) => HandleOutcome::response(
                boxed(|| self.handle_if_shell(requester_pid, *request)).await,
            ),
            Request::WaitFor(request) => {
                HandleOutcome::response(boxed(|| self.handle_wait_for(true, request)).await)
            }
            Request::SourceFile(request) => HandleOutcome::response(
                boxed(|| self.handle_source_file(requester_pid, *request)).await,
            ),
            Request::UnlinkWindow(request) => {
                HandleOutcome::response(boxed(|| self.handle_unlink_window(request)).await)
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
                        initial_command_count: request.initial_command_count,
                    },
                )
            }
            Request::PaneInput(request) => {
                HandleOutcome::response(boxed(|| self.handle_pane_input_ref(request)).await)
            }
            Request::PaneResize(request) => {
                HandleOutcome::response(boxed(|| self.handle_pane_resize_ref(request)).await)
            }
            Request::PaneKill(request) => {
                HandleOutcome::response(boxed(|| self.handle_pane_kill_ref(request)).await)
            }
            Request::PaneRespawn(request) => {
                HandleOutcome::response(boxed(|| self.handle_pane_respawn_ref(*request)).await)
            }
            Request::PaneSnapshotRef(request) => {
                HandleOutcome::response(boxed(|| self.handle_pane_snapshot_ref(request)).await)
            }
            Request::PaneSelect(request) => {
                HandleOutcome::response(boxed(|| self.handle_pane_select_ref(request)).await)
            }
            Request::WebShare(request) => {
                HandleOutcome::response(boxed(|| self.handle_web_share(*request)).await)
            }
            _ => HandleOutcome::response(Response::Error(ErrorResponse {
                error: RmuxError::Server(format!(
                    "{command_name} is only available through queued command dispatch"
                )),
            })),
        };
        if refresh_hook_identities {
            let mut state = self.state.lock().await;
            super::super::prune_dead_hook_identities(&mut state);
        }
        outcome
    }
}
