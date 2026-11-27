mod selection;

use std::collections::{HashMap, HashSet};

#[cfg(windows)]
use super::pane_support::format_references_pane_pid;
use super::{
    attach_support::{
        linked_window_client_content_size, surviving_attached_resize_targets, ActiveAttachState,
        AttachedWindowSizePolicy, SessionDetachOnDestroy,
    },
    client_environment_snapshot, client_spawn_environment,
    focus_support::{prepare_server_focus_effects, FocusEffects},
    prepare_lifecycle_event,
    scripting_support::{client_name_for_pid, render_start_directory_template},
    PaneOutputSubscriptionKeySnapshot, RequestHandler, SelectionTransitionSnapshot,
};
use crate::hook_runtime::{lifecycle_hooks_disabled, PendingInlineHookFormat};
use crate::pane_terminals::{
    resolve_new_pane_process_command, HandlerState, ListWindowsSelection, NewWindowOptions,
    RespawnWindowOptions, WindowSpawnOptions,
};
use rmux_core::{LifecycleEvent, PaneId};
use rmux_proto::{
    ErrorResponse, HookName, KillSessionRequest, MoveWindowRequest, MoveWindowResponse, PaneTarget,
    Response, RmuxError, ScopeSelector, SessionName, Target, UnlinkWindowRequest,
    UnlinkWindowResponse, WindowTarget,
};

mod move_window_effects;
mod request_identity;
mod timer_mutations;

mod arrangement;
mod kill;
mod linking;
mod navigation;
mod spawn;

use move_window_effects::{MoveWindowEffects, PreparedMoveWindowEffects};
use request_identity::{
    require_expected_link_window_identity, require_expected_move_window_identity,
    require_expected_swap_window_identity,
};
use timer_mutations::{move_window_timer_target_overrides, swap_window_timer_target_overrides};

pub(in crate::handler) struct CommittedMoveWindow {
    response: MoveWindowResponse,
    removed_destination_pane_ids: Vec<PaneId>,
    effects: PreparedMoveWindowEffects,
    pub(in crate::handler) focus_effects: Option<FocusEffects>,
    subscriptions_removed: bool,
}

#[cfg(test)]
#[derive(Debug, Default)]
pub(in crate::handler) struct KillWindowCommitPause {
    pub(in crate::handler) reached: tokio::sync::Notify,
    pub(in crate::handler) release: tokio::sync::Notify,
}

#[cfg(test)]
static KILL_WINDOW_COMMIT_PAUSES: std::sync::Mutex<
    Vec<(WindowTarget, std::sync::Arc<KillWindowCommitPause>)>,
> = std::sync::Mutex::new(Vec::new());

fn linked_resize_sessions_for_window_change(
    state: &HandlerState,
    session_name: &SessionName,
    previous_window_index: u32,
    next_window_index: u32,
) -> Vec<SessionName> {
    let mut seen = HashSet::new();
    let mut sessions = Vec::new();
    for window_index in [previous_window_index, next_window_index] {
        for linked_session in state.window_linked_sessions_list(session_name, window_index) {
            if seen.insert(linked_session.clone()) {
                sessions.push(linked_session);
            }
        }
    }
    if sessions.is_empty() && seen.insert(session_name.clone()) {
        sessions.push(session_name.clone());
    }
    sessions
}

fn active_window_ids_by_session(
    state: &HandlerState,
) -> HashMap<SessionName, rmux_proto::WindowId> {
    state
        .sessions
        .iter()
        .map(|session| (session.name(), session))
        .map(|(session_name, session)| (session_name.clone(), session.window().id()))
        .collect()
}

fn changed_active_window_ids(
    previous: &HashMap<SessionName, rmux_proto::WindowId>,
    state: &HandlerState,
) -> Vec<rmux_proto::WindowId> {
    let mut changed = Vec::new();
    for (session_name, session) in state
        .sessions
        .iter()
        .map(|session| (session.name(), session))
    {
        let active_window_id = session.window().id();
        let previous_window_id = previous.get(session_name).copied();
        if previous_window_id != Some(active_window_id) {
            changed.push(active_window_id);
            if let Some(previous_window_id) = previous_window_id {
                changed.push(previous_window_id);
            }
        }
    }
    changed.sort_by_key(|window_id| window_id.as_u32());
    changed.dedup();
    changed
}

fn swap_selection_session_order(
    state: &HandlerState,
    request: &rmux_proto::SwapWindowRequest,
) -> Vec<SessionName> {
    let mut seen_session_ids = HashSet::new();
    let mut ordered_sessions = Vec::new();
    for target in [&request.target, &request.source] {
        for session_name in
            state.window_linked_session_family_list(target.session_name(), target.window_index())
        {
            let Some(session_id) = state
                .sessions
                .session(&session_name)
                .map(|session| session.id())
            else {
                continue;
            };
            if seen_session_ids.insert(session_id) {
                ordered_sessions.push(session_name);
            }
        }
    }
    ordered_sessions
}

impl RequestHandler {
    #[cfg(test)]
    pub(in crate::handler) fn install_kill_window_commit_pause(
        &self,
        target: WindowTarget,
    ) -> std::sync::Arc<KillWindowCommitPause> {
        let pause = std::sync::Arc::new(KillWindowCommitPause::default());
        let mut pauses = KILL_WINDOW_COMMIT_PAUSES
            .lock()
            .expect("kill-window commit pause lock");
        pauses.retain(|(paused_target, _)| paused_target != &target);
        pauses.push((target, pause.clone()));
        pause
    }

    #[cfg(test)]
    async fn pause_before_kill_window_commit(&self, target: &WindowTarget) {
        let pause = KILL_WINDOW_COMMIT_PAUSES
            .lock()
            .expect("kill-window commit pause lock")
            .iter()
            .find(|(paused_target, _)| paused_target == target)
            .map(|(_, pause)| pause.clone());
        let Some(pause) = pause else {
            return;
        };
        pause.reached.notify_one();
        pause.release.notified().await;
        KILL_WINDOW_COMMIT_PAUSES
            .lock()
            .expect("kill-window commit pause lock")
            .retain(|(paused_target, current)| {
                paused_target != target || !std::sync::Arc::ptr_eq(current, &pause)
            });
    }

    async fn reconcile_and_refresh_attached_sessions(&self, sessions: Vec<SessionName>) {
        for session_name in sessions {
            let _ = self
                .reconcile_attached_session_size_and_emit(&session_name)
                .await;
            self.refresh_attached_session(&session_name).await;
        }
    }
}
