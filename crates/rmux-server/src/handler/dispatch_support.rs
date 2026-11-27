mod commands;
mod unavailable_pane;

use rmux_core::command_parser::ParsedCommand;
use rmux_proto::request::Request;
use rmux_proto::{capabilities_for_features, Response};
#[cfg(all(test, any(unix, windows), feature = "web"))]
use std::sync::{Arc, Mutex};
#[cfg(test)]
use tokio::sync::broadcast;
#[cfg(all(test, any(unix, windows), feature = "web"))]
use tokio::sync::Notify;
#[cfg(test)]
use tracing::warn;

use crate::hook_runtime::{capture_inline_hooks, PendingInlineHook};
use crate::pane_io::HandleOutcome;
use unavailable_pane::answer_unavailable_pane;

use super::RequestHandler;

#[cfg(all(test, any(unix, windows), feature = "web"))]
#[derive(Debug, Default)]
pub(crate) struct WebShareDeliveryPause {
    reached: Notify,
    release: Notify,
}

#[cfg(all(test, any(unix, windows), feature = "web"))]
impl WebShareDeliveryPause {
    pub(crate) async fn wait_until_reached(&self) {
        self.reached.notified().await;
    }

    pub(crate) fn release(&self) {
        self.release.notify_one();
    }
}

#[cfg(all(test, any(unix, windows), feature = "web"))]
static WEB_SHARE_DELIVERY_PAUSES: Mutex<Vec<(usize, Arc<WebShareDeliveryPause>)>> =
    Mutex::new(Vec::new());

#[cfg(all(test, any(unix, windows), feature = "web"))]
async fn pause_after_web_share_rollback_is_armed(handler: &RequestHandler) {
    let handler_key = std::ptr::from_ref(handler).addr();
    let pause = {
        let mut pauses = WEB_SHARE_DELIVERY_PAUSES
            .lock()
            .expect("web-share delivery pause lock");
        let position = pauses.iter().position(|(key, _)| *key == handler_key);
        position.map(|position| pauses.swap_remove(position).1)
    };
    if let Some(pause) = pause {
        pause.reached.notify_one();
        pause.release.notified().await;
    }
}

impl RequestHandler {
    pub(crate) async fn handle_revoked_cleanup_request(
        &self,
        connection_id: u64,
        request: Request,
    ) -> Option<Response> {
        match request {
            Request::UnsubscribePaneOutput(request) => Some(
                self.handle_unsubscribe_pane_output(connection_id, request)
                    .await,
            ),
            Request::UnsubscribePaneStream(request) => Some(
                self.handle_unsubscribe_pane_stream(connection_id, request)
                    .await,
            ),
            Request::UnsubscribePaneState(request) => Some(
                self.handle_unsubscribe_pane_state(connection_id, request)
                    .await,
            ),
            _ => None,
        }
    }

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
        self.run_dispatched_hooks(
            requester_pid,
            &request_for_hooks,
            &outcome.response,
            inline_hooks,
            None,
        )
        .await;
        outcome
    }

    #[cfg(all(any(unix, windows), feature = "web"))]
    pub(crate) async fn dispatch_for_connection_with_web_share_guard(
        &self,
        requester_pid: u32,
        connection_id: u64,
        request: Request,
    ) -> (HandleOutcome, Option<super::UndeliveredWebShareGuard>) {
        let request_for_hooks = request.clone();
        let (outcome, inline_hooks) = self
            .dispatch_captured(requester_pid, connection_id, request)
            .await;
        // The connection loop may cancel this future while a hook is waiting.
        // Arm rollback before those awaits and transfer it to the response writer.
        let undelivered_web_share =
            super::UndeliveredWebShareGuard::for_response(self, &outcome.response);
        #[cfg(test)]
        if undelivered_web_share.is_some() {
            pause_after_web_share_rollback_is_armed(self).await;
        }
        self.run_dispatched_hooks(
            requester_pid,
            &request_for_hooks,
            &outcome.response,
            inline_hooks,
            None,
        )
        .await;
        (outcome, undelivered_web_share)
    }

    /// Runs everything a dispatched request still owes once its handler
    /// returned: the applied-window-resize backstop, then its inline and
    /// request hooks.
    ///
    /// Every path that dispatches a [`Request`] must finish through here.
    /// `queued_command` carries the parsed command when the request came from a
    /// command queue (control mode, a multi-command CLI invocation, a hook, a
    /// key binding) and is `None` for a plain single-request connection.
    pub(in crate::handler) async fn run_dispatched_hooks(
        &self,
        requester_pid: u32,
        request: &Request,
        response: &Response,
        inline_hooks: Vec<PendingInlineHook>,
        queued_command: Option<&ParsedCommand>,
    ) {
        // Backstop for the applied-window-resize invariant: in tmux 3.7b every
        // window whose size changed has already published
        // `window-layout-changed` / `window-resized` by the time the command
        // returns. Commands that publish at their own ordering point leave an
        // empty queue here; a command that forgets is late, never silent.
        self.publish_applied_window_resizes().await;
        self.refresh_theme_sources().await;
        let inline_hook_names = inline_hooks
            .iter()
            .map(|pending| pending.hook)
            .collect::<Vec<_>>();
        self.run_inline_hooks(requester_pid, inline_hooks, queued_command)
            .await;
        self.run_request_hooks(
            requester_pid,
            request,
            response,
            queued_command,
            &inline_hook_names,
        )
        .await;
    }

    #[cfg(all(test, any(unix, windows), feature = "web"))]
    pub(crate) fn install_web_share_delivery_pause(&self) -> Arc<WebShareDeliveryPause> {
        let handler_key = std::ptr::from_ref(self).addr();
        let pause = Arc::new(WebShareDeliveryPause::default());
        let mut pauses = WEB_SHARE_DELIVERY_PAUSES
            .lock()
            .expect("web-share delivery pause lock");
        let previous = pauses.iter().any(|(key, _)| *key == handler_key);
        assert!(!previous, "web-share delivery pause already installed");
        pauses.push((handler_key, Arc::clone(&pause)));
        pause
    }

    #[async_recursion::async_recursion]
    pub(crate) async fn dispatch_captured(
        &self,
        requester_pid: u32,
        connection_id: u64,
        request: Request,
    ) -> (HandleOutcome, Vec<PendingInlineHook>) {
        capture_inline_hooks(answer_unavailable_pane(Box::pin(self.dispatch_request(
            requester_pid,
            connection_id,
            request,
        ))))
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
        capture_inline_hooks(answer_unavailable_pane(Box::pin(
            self.dispatch_request_with_client_name(
                requester_pid,
                connection_id,
                request,
                client_name,
            ),
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
            (Request::RunShell(request), client_name) => HandleOutcome::response(
                Box::pin(self.handle_run_shell_with_client_name(
                    requester_pid,
                    *request,
                    client_name,
                ))
                .await,
            ),
            (Request::IfShell(request), Some(client_name)) => HandleOutcome::response(
                Box::pin(self.handle_if_shell_with_client_name(
                    requester_pid,
                    *request,
                    Some(client_name),
                ))
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
        match request {
            Request::RunShell(request) => HandleOutcome::response(
                Box::pin(self.handle_run_shell(requester_pid, *request)).await,
            ),
            Request::IfShell(request) => HandleOutcome::response(
                Box::pin(self.handle_if_shell(requester_pid, *request)).await,
            ),
            request => {
                Box::pin(self.dispatch_request_inner(requester_pid, connection_id, request)).await
            }
        }
    }
}

fn request_changes_hook_identity_aliases(request: &Request) -> bool {
    matches!(
        request,
        Request::NewSession(_)
            | Request::NewSessionExt(_)
            | Request::KillSession(_)
            | Request::RenameSession(_)
            | Request::NewWindow(_)
            | Request::KillWindow(_)
            | Request::LinkWindow(_)
            | Request::MoveWindow(_)
            | Request::SwapWindow(_)
            | Request::RotateWindow(_)
            | Request::RespawnWindow(_)
            | Request::SplitWindow(_)
            | Request::SplitWindowExt(_)
            | Request::SplitWindowTargetAction(_)
            | Request::SplitWindowIdentity(_)
            | Request::SwapPane(_)
            | Request::JoinPane(_)
            | Request::MovePane(_)
            | Request::BreakPane(_)
            | Request::RespawnPane(_)
            | Request::KillPane(_)
            | Request::PaneKill(_)
            | Request::PaneRespawn(_)
            | Request::UnlinkWindow(_)
    )
}

#[cfg(windows)]
impl RequestHandler {
    async fn wait_for_windows_deferred_request(&self, request: &Request) {
        if !request_waits_for_windows_deferred_panes(request) {
            return;
        }
        // Scope the wait to the request's own target so a still-starting pane in
        // an unrelated session cannot block a targeted command — e.g. a
        // select-pane on session alpha must not wait on session beta's deferred
        // pane. Commands without a single determinable target keep the
        // conservative wait across every session.
        match request_deferred_wait_target(request) {
            Some(target) => {
                self.wait_for_windows_deferred_target_pane_pids(&target)
                    .await
            }
            None => self.wait_for_windows_deferred_all_pane_pids().await,
        }
    }
}

// Single-pane commands whose deferred-pane wait can be scoped to just their
// target pane's session. Commands that reference a second location (move-pane,
// swap-pane) keep the conservative all-session wait so a still-starting
// destination is not skipped.
#[cfg(any(windows, test))]
fn request_deferred_wait_target(request: &Request) -> Option<rmux_proto::Target> {
    use rmux_proto::{PaneTargetRef, Target};
    fn scope_from_ref(target: &PaneTargetRef) -> Target {
        match target {
            PaneTargetRef::Slot(pane) => Target::Pane(pane.clone()),
            // The id-typed API cannot resolve `pane_id -> (window_index,
            // pane_index)` without taking the state lock, so scope the wait
            // to the pane's session — still much better than the all-session
            // fallback the SDK path used before.
            PaneTargetRef::Id { session_name, .. } => Target::Session(session_name.clone()),
        }
    }
    match request {
        Request::SelectPane(request) => Some(Target::Pane(request.target.clone())),
        Request::SelectPaneAdjacent(request) => Some(Target::Pane(request.target.clone())),
        Request::SelectPaneMark(request) => Some(Target::Pane(request.target.clone())),
        Request::LastPane(request) => Some(Target::Window(request.target.clone())),
        Request::ResizePane(request) => Some(Target::Pane(request.target.clone())),
        // Request::ResizePaneTargetAction accepts an optional raw -t string;
        // the parser hasn't resolved the requester's current pane at this
        // layer, so keep the conservative wait to stay safe against a
        // still-starting sibling.
        Request::ResizePaneAxes(request) => Some(Target::Pane(request.target.clone())),
        Request::PaneResize(request) => Some(scope_from_ref(&request.target)),
        Request::SubscribePaneStream(request) => Some(scope_from_ref(&request.target)),
        Request::PipePane(request) => Some(Target::Pane(request.target.clone())),
        Request::PasteBuffer(request) => Some(Target::Pane(request.target.clone())),
        Request::RestoreWindowSnapshot(request) => Some(Target::Window(request.target.clone())),
        Request::RestoreWindowSnapshotV2(request) => Some(Target::Window(request.target.clone())),
        _ => None,
    }
}

#[cfg(any(windows, test))]
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
            | Request::PaneKill(_)
            | Request::RespawnPane(_)
            | Request::PaneRespawn(_)
            | Request::PaneSelect(_)
            | Request::SelectPane(_)
            | Request::SelectPaneAdjacent(_)
            | Request::SelectPaneMark(_)
            | Request::LastPane(_)
            | Request::SelectWindow(_)
            | Request::NextWindow(_)
            | Request::PreviousWindow(_)
            | Request::LastWindow(_)
            | Request::LockServer(_)
            | Request::LockSession(_)
            | Request::LockClient(_)
            | Request::ServerAccess(_)
            | Request::ListWindows(_)
            | Request::ExportWindowSnapshotV2(_)
            | Request::ExportWindowSnapshot(_)
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
            | Request::PaneOptionGet(_)
            | Request::PaneSnapshot(_)
            | Request::SubscribePaneOutput(_)
            | Request::SubscribePaneOutputRef(_)
            | Request::UnsubscribePaneOutput(_)
            | Request::PaneOutputCursor(_)
            | Request::PaneStreamCursor(_)
            | Request::UnsubscribePaneStream(_)
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

pub(super) fn supported_capabilities() -> Vec<&'static str> {
    capabilities_for_features(cfg!(all(any(unix, windows), feature = "web")))
}

#[cfg(test)]
mod windows_deferred_wait_tests {
    use rmux_core::PaneId;
    use rmux_proto::request::{
        KillPaneRequest, LastPaneRequest, LastWindowRequest, NewWindowRequest, NextWindowRequest,
        PaneKillRequest, PaneRespawnRequest, PreviousWindowRequest, Request, RespawnPaneRequest,
        SelectPaneAdjacentRequest, SelectPaneDirection, SelectPaneMarkRequest, SelectPaneRequest,
        SelectWindowRequest, SplitWindowExtRequest, SplitWindowTargetActionRequest,
        SubscribePaneStreamRequest, UnsubscribePaneStreamRequest,
    };
    use rmux_proto::{
        PaneOutputSubscriptionId, PaneStreamCursorRequest, PaneStreamMode, PaneTarget,
        PaneTargetRef, ProcessCommand, SessionName, SplitDirection, SplitWindowTarget, Target,
        WindowTarget,
    };

    use super::{request_deferred_wait_target, request_waits_for_windows_deferred_panes};

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

    fn pane_kill_by_id() -> Request {
        Request::PaneKill(PaneKillRequest {
            target: PaneTargetRef::by_id(session_name("bench"), PaneId::new(7)),
            kill_all_except: false,
        })
    }

    fn pane_respawn_by_id() -> Request {
        Request::PaneRespawn(Box::new(PaneRespawnRequest {
            target: PaneTargetRef::by_id(session_name("bench"), PaneId::new(7)),
            kill: true,
            start_directory: None,
            environment: None,
            command: None,
            process_command: Some(ProcessCommand::Shell("exit 0".to_owned())),
            keep_alive_on_exit: None,
        }))
    }

    fn pane_target() -> PaneTarget {
        PaneTarget::with_window(session_name("bench"), 0, 0)
    }

    fn window_target() -> WindowTarget {
        WindowTarget::with_window(session_name("bench"), 0)
    }

    #[test]
    fn detached_new_window_does_not_wait_for_windows_deferred_panes() {
        assert!(!request_waits_for_windows_deferred_panes(&new_window(true)));
    }

    #[test]
    fn final_pane_lifecycle_mutations_do_not_wait_for_deferred_initial_spawn() {
        assert!(!request_waits_for_windows_deferred_panes(&kill_pane()));
        assert!(!request_waits_for_windows_deferred_panes(&respawn_pane()));
        assert!(!request_waits_for_windows_deferred_panes(&pane_kill_by_id()));
        assert!(!request_waits_for_windows_deferred_panes(
            &pane_respawn_by_id()
        ));
    }

    #[test]
    fn pane_and_window_selection_never_wait_for_unrelated_deferred_panes() {
        let selection_requests = [
            Request::SelectPane(Box::new(SelectPaneRequest {
                target: pane_target(),
                title: None,
                input_disabled: None,
                preserve_zoom: false,
                style: None,
            })),
            Request::SelectPaneAdjacent(SelectPaneAdjacentRequest {
                target: pane_target(),
                direction: SelectPaneDirection::Right,
                preserve_zoom: false,
            }),
            Request::SelectPaneMark(SelectPaneMarkRequest {
                target: pane_target(),
                clear: false,
                title: None,
            }),
            Request::LastPane(LastPaneRequest {
                target: window_target(),
                preserve_zoom: false,
                input_disabled: None,
            }),
            Request::SelectWindow(SelectWindowRequest {
                target: window_target(),
            }),
            Request::NextWindow(NextWindowRequest {
                target: session_name("bench"),
                alerts_only: false,
            }),
            Request::PreviousWindow(PreviousWindowRequest {
                target: session_name("bench"),
                alerts_only: false,
            }),
            Request::LastWindow(LastWindowRequest {
                target: session_name("bench"),
            }),
        ];

        for request in selection_requests {
            assert!(
                !request_waits_for_windows_deferred_panes(&request),
                "{} must not wait for unrelated deferred panes",
                request.command_name()
            );
        }
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

    #[test]
    fn initial_stream_subscribe_waits_for_its_conpty_but_cursor_cleanup_never_waits() {
        let subscribe = Request::SubscribePaneStream(SubscribePaneStreamRequest {
            target: PaneTargetRef::slot(pane_target()),
            mode: PaneStreamMode::Raw,
            include_snapshot: false,
        });
        assert!(request_waits_for_windows_deferred_panes(&subscribe));
        assert_eq!(
            request_deferred_wait_target(&subscribe),
            Some(Target::Pane(pane_target())),
            "the Windows wait must be scoped to the requested starting pane"
        );

        let subscription_id = PaneOutputSubscriptionId::new(9);
        let cursor = Request::PaneStreamCursor(PaneStreamCursorRequest {
            subscription_id,
            max_events: Some(1),
        });
        let unsubscribe =
            Request::UnsubscribePaneStream(UnsubscribePaneStreamRequest { subscription_id });
        assert!(!request_waits_for_windows_deferred_panes(&cursor));
        assert!(!request_waits_for_windows_deferred_panes(&unsubscribe));
    }
}
