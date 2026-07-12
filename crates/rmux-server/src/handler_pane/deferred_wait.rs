use rmux_proto::Target;

use super::super::RequestHandler;
use crate::pane_terminals::HandlerState;

const DEFERRED_PANE_WAIT: std::time::Duration = std::time::Duration::from_secs(10);
const DEFERRED_PANE_POLL: std::time::Duration = std::time::Duration::from_millis(5);

impl RequestHandler {
    pub(in crate::handler) async fn wait_for_windows_deferred_list_pane_pids(
        &self,
        session_name: &rmux_proto::SessionName,
        target_window_index: Option<u32>,
    ) {
        self.wait_for_windows_deferred_panes_until(|| async {
            let state = self.state.lock().await;
            list_pane_scope_has_pending_pid(&state, session_name, target_window_index)
        })
        .await;
    }

    pub(in crate::handler) async fn wait_for_windows_deferred_session_pane_pids(
        &self,
        session_name: &rmux_proto::SessionName,
    ) {
        self.wait_for_windows_deferred_panes_until(|| async {
            let state = self.state.lock().await;
            list_pane_scope_has_pending_pid(&state, session_name, None)
        })
        .await;
    }

    pub(in crate::handler) async fn wait_for_windows_deferred_list_session_pane_pids(&self) {
        self.wait_for_windows_deferred_panes_until(|| async {
            let state = self.state.lock().await;
            list_session_scope_has_pending_active_pane_pid(&state)
        })
        .await;
    }

    pub(in crate::handler) async fn wait_for_windows_deferred_all_pane_pids(&self) {
        self.wait_for_windows_deferred_panes_until(|| async {
            let state = self.state.lock().await;
            state_has_pending_pane_pid(&state)
        })
        .await;
    }

    pub(in crate::handler) async fn wait_for_windows_deferred_all_panes_ready(&self) {
        self.wait_for_windows_deferred_panes_until(|| async {
            let state = self.state.lock().await;
            state_has_deferred_pane(&state)
        })
        .await;
    }

    pub(in crate::handler) async fn wait_for_windows_deferred_session_panes_ready(
        &self,
        session_name: &rmux_proto::SessionName,
    ) {
        self.wait_for_windows_deferred_panes_until(|| async {
            let state = self.state.lock().await;
            session_has_deferred_pane(&state, session_name)
        })
        .await;
    }

    pub(in crate::handler) async fn wait_for_windows_deferred_target_pane_pids(
        &self,
        target: &Target,
    ) {
        self.wait_for_windows_deferred_panes_until(|| async {
            let state = self.state.lock().await;
            target_has_pending_pane_pid(&state, target)
        })
        .await;
    }

    async fn wait_for_windows_deferred_panes_until<F, Fut>(&self, mut is_pending: F)
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = bool>,
    {
        let deadline = tokio::time::Instant::now() + DEFERRED_PANE_WAIT;
        while is_pending().await {
            let now = tokio::time::Instant::now();
            if now >= deadline {
                break;
            }
            tokio::time::sleep(DEFERRED_PANE_POLL.min(deadline - now)).await;
        }
    }
}

pub(in crate::handler) fn format_references_pane_pid(format: Option<&str>) -> bool {
    format.is_some_and(|format| format.contains("pane_pid"))
}

fn target_has_pending_pane_pid(state: &HandlerState, target: &Target) -> bool {
    match target {
        Target::Session(session_name) => list_pane_scope_has_pending_pid(state, session_name, None),
        Target::Window(target) => list_pane_scope_has_pending_pid(
            state,
            target.session_name(),
            Some(target.window_index()),
        ),
        Target::Pane(target) => pane_pid_is_pending(
            state,
            target.session_name(),
            target.window_index(),
            target.pane_index(),
        ),
    }
}

fn list_session_scope_has_pending_active_pane_pid(state: &HandlerState) -> bool {
    state.sessions.iter().any(|(session_name, session)| {
        let window_index = session.active_window_index();
        session.window().active_pane().is_some_and(|pane| {
            pane_pid_is_pending(state, session_name, window_index, pane.index())
        })
    })
}

fn state_has_pending_pane_pid(state: &HandlerState) -> bool {
    state.sessions.iter().any(|(session_name, session)| {
        session.windows().iter().any(|(window_index, window)| {
            window
                .panes()
                .iter()
                .any(|pane| pane_pid_is_pending(state, session_name, *window_index, pane.index()))
        })
    })
}

fn state_has_deferred_pane(state: &HandlerState) -> bool {
    state.sessions.iter().any(|(session_name, session)| {
        session.windows().iter().any(|(window_index, window)| {
            window.panes().iter().any(|pane| {
                state.pane_is_starting_in_window(session_name, *window_index, pane.index())
            })
        })
    })
}

fn session_has_deferred_pane(state: &HandlerState, session_name: &rmux_proto::SessionName) -> bool {
    let Some(session) = state.sessions.session(session_name) else {
        return false;
    };
    session.windows().iter().any(|(window_index, window)| {
        window
            .panes()
            .iter()
            .any(|pane| state.pane_is_starting_in_window(session_name, *window_index, pane.index()))
    })
}

fn list_pane_scope_has_pending_pid(
    state: &HandlerState,
    session_name: &rmux_proto::SessionName,
    target_window_index: Option<u32>,
) -> bool {
    let Some(session) = state.sessions.session(session_name) else {
        return false;
    };
    session
        .windows()
        .iter()
        .filter(|(window_index, _)| {
            target_window_index.is_none_or(|target| target == **window_index)
        })
        .any(|(window_index, window)| {
            window
                .panes()
                .iter()
                .any(|pane| pane_pid_is_pending(state, session_name, *window_index, pane.index()))
        })
}

fn pane_pid_is_pending(
    state: &HandlerState,
    session_name: &rmux_proto::SessionName,
    window_index: u32,
    pane_index: u32,
) -> bool {
    state.pane_is_starting_in_window(session_name, window_index, pane_index)
        && state
            .pane_pid_in_window(session_name, window_index, pane_index)
            .is_err()
}
