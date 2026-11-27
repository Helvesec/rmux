use rmux_core::{
    formats::{is_truthy, render_list_sessions_line, FormatContext},
    PaneId,
};
#[cfg(all(test, feature = "web"))]
use rmux_proto::SessionId;
#[cfg(test)]
use rmux_proto::SessionName;
use rmux_proto::{ErrorResponse, ListSessionsResponse, Response, RmuxError};

use crate::format_runtime::{render_runtime_template, RuntimeFormatContext};

mod client_environment;
mod control_mode;
mod creation;
#[cfg(windows)]
mod deferred;
mod has;
mod kill;
mod list;
mod options;
mod output;
mod rename;
mod unattached;

// The deferred bracketed-paste final-sink proof lives next to the flush it
// exercises. It is Windows-only because the starting-pane input queue is.
#[cfg(all(test, windows))]
#[path = "../handler_session/bracketed_paste_final_sink_tests.rs"]
mod bracketed_paste_final_sink_tests;

/// Lets a test hold a deferred initial pane at the lifecycle boundary between
/// "the child is running and publishing output" and "the starting queue is
/// drained".
///
/// The deferred proof needs a real child to announce `ESC[?2004h` through
/// actual pane output *while its pane is still starting*. That window exists in
/// production but is not otherwise observable, and stamping the transcript
/// instead only proves that a synthetic mode selects a route.
///
/// The gate is keyed by runtime session name, so a gated test never delays
/// another one, and it is `#[cfg(test)]`, so no shipped code path can reach it.
#[cfg(all(test, windows))]
pub(crate) mod deferred_initial_gate {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, OnceLock};

    use rmux_proto::SessionName;
    use tokio::sync::Semaphore;

    fn gates() -> &'static Mutex<HashMap<SessionName, Arc<Semaphore>>> {
        static GATES: OnceLock<Mutex<HashMap<SessionName, Arc<Semaphore>>>> = OnceLock::new();
        GATES.get_or_init(|| Mutex::new(HashMap::new()))
    }

    /// Installed before the session is created; dropping it releases the pane.
    pub(crate) struct DeferredInitialGate {
        runtime_session_name: SessionName,
        permits: Arc<Semaphore>,
    }

    impl DeferredInitialGate {
        pub(crate) fn install(runtime_session_name: &SessionName) -> Self {
            let permits = Arc::new(Semaphore::new(0));
            gates()
                .lock()
                .expect("deferred initial gate registry")
                .insert(runtime_session_name.clone(), Arc::clone(&permits));
            Self {
                runtime_session_name: runtime_session_name.clone(),
                permits,
            }
        }
    }

    impl Drop for DeferredInitialGate {
        fn drop(&mut self) {
            // A stored permit releases the spawn task whether or not it has
            // reached the boundary yet, so a test can never deadlock by
            // finishing early.
            self.permits.add_permits(1);
            gates()
                .lock()
                .expect("deferred initial gate registry")
                .remove(&self.runtime_session_name);
        }
    }

    pub(super) async fn wait_if_gated(runtime_session_name: &SessionName) {
        let permits = gates()
            .lock()
            .expect("deferred initial gate registry")
            .get(runtime_session_name)
            .map(Arc::clone);
        if let Some(permits) = permits {
            let _ = permits.acquire().await;
        }
    }
}

#[cfg(test)]
#[derive(Debug, Default)]
pub(in crate::handler) struct RenameSessionIdentityPause {
    pub(in crate::handler) reached: tokio::sync::Notify,
    pub(in crate::handler) release: tokio::sync::Notify,
}

#[cfg(test)]
static RENAME_SESSION_IDENTITY_PAUSE: std::sync::Mutex<
    Option<(SessionName, std::sync::Arc<RenameSessionIdentityPause>)>,
> = std::sync::Mutex::new(None);

#[cfg(test)]
#[derive(Debug, Default)]
pub(in crate::handler) struct RenameSessionControlCommitPause {
    pub(in crate::handler) reached: tokio::sync::Notify,
    pub(in crate::handler) release: tokio::sync::Notify,
}

#[cfg(test)]
static RENAME_SESSION_CONTROL_COMMIT_PAUSE: std::sync::Mutex<
    Vec<(SessionName, std::sync::Arc<RenameSessionControlCommitPause>)>,
> = std::sync::Mutex::new(Vec::new());

#[cfg(all(test, feature = "web"))]
#[derive(Debug, Default)]
pub(in crate::handler) struct KillSessionWebPrunePause {
    pub(in crate::handler) reached: tokio::sync::Notify,
    pub(in crate::handler) release: tokio::sync::Notify,
}

#[cfg(all(test, feature = "web"))]
static KILL_SESSION_WEB_PRUNE_PAUSE: std::sync::Mutex<
    Option<(SessionName, std::sync::Arc<KillSessionWebPrunePause>)>,
> = std::sync::Mutex::new(None);

#[cfg(test)]
#[derive(Debug, Default)]
pub(in crate::handler) struct KillSessionSubscriptionRekeyPause {
    pub(in crate::handler) reached: tokio::sync::Notify,
    pub(in crate::handler) release: tokio::sync::Notify,
}

#[cfg(test)]
static KILL_SESSION_SUBSCRIPTION_REKEY_PAUSE: std::sync::Mutex<
    Option<(
        SessionName,
        std::sync::Arc<KillSessionSubscriptionRekeyPause>,
    )>,
> = std::sync::Mutex::new(None);

#[cfg(test)]
#[derive(Debug, Default)]
pub(in crate::handler) struct KillSessionSelectionIdentityPause {
    pub(in crate::handler) reached: tokio::sync::Notify,
    pub(in crate::handler) release: tokio::sync::Notify,
}

#[cfg(test)]
static KILL_SESSION_SELECTION_IDENTITY_PAUSE: std::sync::Mutex<
    Vec<(
        SessionName,
        std::sync::Arc<KillSessionSelectionIdentityPause>,
    )>,
> = std::sync::Mutex::new(Vec::new());

use list::{sort_list_sessions, ListSessionSnapshot};

#[cfg(windows)]
use super::pane_support::format_references_pane_pid;
use super::{
    command_output_from_lines, parse_session_sort_order, RequestHandler, SessionSortOrder,
};

impl RequestHandler {
    #[cfg(test)]
    pub(in crate::handler) fn install_rename_session_control_commit_pause(
        &self,
        session_name: SessionName,
    ) -> std::sync::Arc<RenameSessionControlCommitPause> {
        let pause = std::sync::Arc::new(RenameSessionControlCommitPause::default());
        let mut pauses = RENAME_SESSION_CONTROL_COMMIT_PAUSE
            .lock()
            .expect("rename-session control commit pause lock");
        pauses.retain(|(paused_session, _)| paused_session != &session_name);
        pauses.push((session_name, pause.clone()));
        pause
    }

    #[cfg(test)]
    async fn pause_before_rename_session_control_commit(&self, session_name: &SessionName) {
        let pause = RENAME_SESSION_CONTROL_COMMIT_PAUSE
            .lock()
            .expect("rename-session control commit pause lock")
            .iter()
            .find(|(paused_session, _)| paused_session == session_name)
            .map(|(_, pause)| pause.clone());
        let Some(pause) = pause else {
            return;
        };
        pause.reached.notify_one();
        pause.release.notified().await;
        RENAME_SESSION_CONTROL_COMMIT_PAUSE
            .lock()
            .expect("rename-session control commit pause lock")
            .retain(|(paused_session, current)| {
                paused_session != session_name || !std::sync::Arc::ptr_eq(current, &pause)
            });
    }

    #[cfg(test)]
    pub(in crate::handler) fn install_kill_session_selection_identity_pause(
        &self,
        session_name: SessionName,
    ) -> std::sync::Arc<KillSessionSelectionIdentityPause> {
        let pause = std::sync::Arc::new(KillSessionSelectionIdentityPause::default());
        let mut pauses = KILL_SESSION_SELECTION_IDENTITY_PAUSE
            .lock()
            .expect("kill-session selection identity pause lock");
        pauses.retain(|(paused_session, _)| paused_session != &session_name);
        pauses.push((session_name, pause.clone()));
        pause
    }

    #[cfg(test)]
    async fn pause_after_kill_session_selection_identity_capture(
        &self,
        session_name: &SessionName,
    ) {
        // Consume the pause before waiting so a nested kill of the same
        // session can make progress while this request is suspended. The
        // deterministic race hook is intentionally one-shot.
        let pause = {
            let mut pauses = KILL_SESSION_SELECTION_IDENTITY_PAUSE
                .lock()
                .expect("kill-session selection identity pause lock");
            pauses
                .iter()
                .position(|(paused_session, _)| paused_session == session_name)
                .map(|index| pauses.remove(index).1)
        };
        let Some(pause) = pause else {
            return;
        };
        pause.reached.notify_one();
        pause.release.notified().await;
    }

    #[cfg(test)]
    pub(in crate::handler) fn install_kill_session_subscription_rekey_pause(
        &self,
        session_name: SessionName,
    ) -> std::sync::Arc<KillSessionSubscriptionRekeyPause> {
        let pause = std::sync::Arc::new(KillSessionSubscriptionRekeyPause::default());
        *KILL_SESSION_SUBSCRIPTION_REKEY_PAUSE
            .lock()
            .expect("kill-session subscription rekey pause lock") =
            Some((session_name, pause.clone()));
        pause
    }

    #[cfg(test)]
    async fn pause_before_kill_session_subscription_rekey(&self, session_name: &SessionName) {
        let pause = KILL_SESSION_SUBSCRIPTION_REKEY_PAUSE
            .lock()
            .expect("kill-session subscription rekey pause lock")
            .as_ref()
            .filter(|(paused_session, _)| paused_session == session_name)
            .map(|(_, pause)| pause.clone());
        let Some(pause) = pause else {
            return;
        };
        pause.reached.notify_one();
        pause.release.notified().await;
        let mut installed = KILL_SESSION_SUBSCRIPTION_REKEY_PAUSE
            .lock()
            .expect("kill-session subscription rekey pause lock");
        if installed
            .as_ref()
            .is_some_and(|(_, current)| std::sync::Arc::ptr_eq(current, &pause))
        {
            installed.take();
        }
    }

    #[cfg(all(test, feature = "web"))]
    pub(in crate::handler) fn install_kill_session_web_prune_pause(
        &self,
        session_name: SessionName,
    ) -> std::sync::Arc<KillSessionWebPrunePause> {
        let pause = std::sync::Arc::new(KillSessionWebPrunePause::default());
        *KILL_SESSION_WEB_PRUNE_PAUSE
            .lock()
            .expect("kill-session Web prune pause lock") = Some((session_name, pause.clone()));
        pause
    }

    #[cfg(all(test, feature = "web"))]
    async fn pause_before_kill_session_web_prune(
        &self,
        removed_sessions: &[(SessionName, SessionId)],
    ) {
        let pause = KILL_SESSION_WEB_PRUNE_PAUSE
            .lock()
            .expect("kill-session Web prune pause lock")
            .as_ref()
            .filter(|(paused_session, _)| {
                removed_sessions
                    .iter()
                    .any(|(session_name, _)| session_name == paused_session)
            })
            .map(|(_, pause)| pause.clone());
        let Some(pause) = pause else {
            return;
        };
        pause.reached.notify_one();
        pause.release.notified().await;
        let mut installed = KILL_SESSION_WEB_PRUNE_PAUSE
            .lock()
            .expect("kill-session Web prune pause lock");
        if installed
            .as_ref()
            .is_some_and(|(_, current)| std::sync::Arc::ptr_eq(current, &pause))
        {
            *installed = None;
        }
    }

    #[cfg(test)]
    pub(in crate::handler) fn install_rename_session_identity_pause(
        &self,
        session_name: SessionName,
    ) -> std::sync::Arc<RenameSessionIdentityPause> {
        let pause = std::sync::Arc::new(RenameSessionIdentityPause::default());
        *RENAME_SESSION_IDENTITY_PAUSE
            .lock()
            .expect("rename-session identity pause lock") = Some((session_name, pause.clone()));
        pause
    }

    #[cfg(test)]
    async fn pause_after_rename_session_identity_capture(&self, session_name: &SessionName) {
        let pause = RENAME_SESSION_IDENTITY_PAUSE
            .lock()
            .expect("rename-session identity pause lock")
            .as_ref()
            .filter(|(paused_session, _)| paused_session == session_name)
            .map(|(_, pause)| pause.clone());
        let Some(pause) = pause else {
            return;
        };
        pause.reached.notify_one();
        pause.release.notified().await;
        let mut installed = RENAME_SESSION_IDENTITY_PAUSE
            .lock()
            .expect("rename-session identity pause lock");
        if installed.as_ref().is_some_and(|(paused_session, current)| {
            paused_session == session_name && std::sync::Arc::ptr_eq(current, &pause)
        }) {
            *installed = None;
        }
    }

    pub(in crate::handler) async fn handle_list_sessions(
        &self,
        request: rmux_proto::ListSessionsRequest,
    ) -> Response {
        let socket_path = self.socket_path();
        let has_explicit_sort = request.sort_order.is_some();
        let sort_order = match parse_session_sort_order(request.sort_order.as_deref()) {
            Some(sort_order) => sort_order,
            None if request.sort_order.is_some() => {
                return Response::Error(ErrorResponse {
                    error: RmuxError::Server(rmux_core::INVALID_SORT_ORDER.to_owned()),
                });
            }
            None => SessionSortOrder::Name,
        };
        #[cfg(windows)]
        if format_references_pane_pid(request.format.as_deref())
            || format_references_pane_pid(request.filter.as_deref())
        {
            self.wait_for_windows_deferred_list_session_pane_pids()
                .await;
        }
        let client_loop_snapshots = self.client_loop_snapshots().await;
        let mut state = self.state.lock().await;
        let mut sessions = state
            .sessions
            .iter()
            .map(|session| (session.name(), session))
            .map(|(session_name, session)| ListSessionSnapshot {
                name: session_name.clone(),
                id: session.id().as_u32(),
                created_at: session.created_at(),
                activity_at: session.activity_at(),
            })
            .collect::<Vec<_>>();
        sort_list_sessions(
            &mut sessions,
            sort_order,
            request.reversed && has_explicit_sort,
        );
        // Native status queries must not expose active reports from dead panes
        // while an asynchronous exit callback is still queued.
        if [request.format.as_deref(), request.filter.as_deref()]
            .into_iter()
            .flatten()
            .any(|format| format.contains("program_status"))
        {
            for session in &sessions {
                let _ = state.refresh_list_panes_exit_statuses(&session.name, None);
            }
        }

        let active_attach = self.active_attach.lock().await;
        let active_control = self.active_control.lock().await;
        let lines = sessions
            .iter()
            .filter_map(|session| state.sessions.session(&session.name))
            .filter_map(|session| {
                let attached_count = active_attach.attached_count(session.name())
                    + active_control.attached_count(session.name());
                let active_window_index = session.active_window_index();
                let active_window = session.window();
                let mut context = FormatContext::new()
                    .with_managed_session(session)
                    .with_session_attached(attached_count)
                    .with_window(active_window_index, active_window, true, false);
                if let Some(pane) = active_window.active_pane() {
                    context = context.with_window_pane(active_window, pane);
                }
                let runtime = RuntimeFormatContext::new(context)
                    .with_state(&state)
                    .with_socket_path(&socket_path)
                    .with_session(session)
                    .with_window(active_window_index, active_window)
                    .with_client_loop_snapshots(&client_loop_snapshots);
                let runtime = match active_window.active_pane() {
                    Some(pane) => runtime.with_pane(pane),
                    None => runtime,
                };
                if let Some(filter) = request.filter.as_deref() {
                    let expanded = render_runtime_template(filter, &runtime, false);
                    if !is_truthy(&expanded) {
                        return None;
                    }
                }

                Some(render_list_sessions_line(
                    &runtime,
                    request.format.as_deref(),
                ))
            })
            .collect::<Vec<_>>();

        Response::ListSessions(ListSessionsResponse {
            output: command_output_from_lines(&lines),
        })
    }
}

fn session_pane_ids(session: rmux_core::ManagedSessionView<'_>) -> Vec<PaneId> {
    session
        .windows()
        .map(|(_, window)| window)
        .flat_map(|window| window.panes().iter().map(|pane| pane.id()))
        .collect()
}
