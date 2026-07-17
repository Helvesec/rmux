use std::collections::HashMap;

use rmux_core::{HookStore, LifecycleEvent, SessionStore, WindowId};
use rmux_proto::{PaneTarget, SessionName, Target, WindowTarget};

use super::super::{
    defer_lifecycle_event, prepare_deferred_lifecycle_event, DeferredLifecycleEvent,
    QueuedLifecycleEvent,
};
use crate::hook_runtime::ExactPaneHookTarget;
use crate::pane_terminals::HandlerState;
use crate::pane_terminals::KilledPaneHookContext;

pub(super) struct AfterKillPaneHookTarget {
    pub(super) target: PaneTarget,
    pub(super) identity: ExactPaneHookTarget,
}

pub(super) fn after_kill_pane_target(
    state: &HandlerState,
    context: &KilledPaneHookContext,
    affected_sessions: &[SessionName],
) -> Option<AfterKillPaneHookTarget> {
    let target = pane_id_target_after_kill(
        &state.sessions,
        context.pane_id,
        context.window_id,
        context.target.session_name(),
        context.target.window_index(),
    )
    .or_else(|| {
        active_window_id_target_after_kill(
            &state.sessions,
            context.window_id,
            context.target.session_name(),
            context.target.window_index(),
        )
    })
    .or_else(|| super::super::active_session_target(&state.sessions, context.target.session_name()))
    .or_else(|| {
        affected_sessions
            .iter()
            .filter(|session_name| *session_name != context.target.session_name())
            .find_map(|session_name| {
                super::super::active_session_target(&state.sessions, session_name)
            })
    })?;
    let Target::Pane(target) = target else {
        return None;
    };
    let session = state.sessions.session(target.session_name())?;
    let window = session.window_at(target.window_index())?;
    let pane = window.pane(target.pane_index())?;
    let preferred_window_index = target.window_index();
    let window_occurrence_id =
        state.window_link_occurrence_id(target.session_name(), preferred_window_index);
    Some(AfterKillPaneHookTarget {
        target,
        identity: ExactPaneHookTarget {
            session_id: session.id(),
            window_id: window.id(),
            pane_id: pane.id(),
            preferred_window_index,
            window_occurrence_id,
        },
    })
}

fn pane_id_target_after_kill(
    sessions: &SessionStore,
    pane_id: u32,
    preferred_window_id: u32,
    preferred_session: &SessionName,
    preferred_window_index: u32,
) -> Option<Target> {
    let mut candidates = sessions
        .iter()
        .flat_map(|(session_name, session)| {
            session
                .windows()
                .iter()
                .flat_map(move |(window_index, window)| {
                    window
                        .panes()
                        .iter()
                        .filter(move |pane| pane.id().as_u32() == pane_id)
                        .map(move |pane| {
                            PaneTarget::with_window(
                                session_name.clone(),
                                *window_index,
                                pane.index(),
                            )
                        })
                })
        })
        .collect::<Vec<_>>();
    sort_stable_pane_targets(
        &mut candidates,
        sessions,
        preferred_session,
        preferred_window_id,
        preferred_window_index,
    );
    candidates.into_iter().next().map(Target::Pane)
}

fn active_window_id_target_after_kill(
    sessions: &SessionStore,
    window_id: u32,
    preferred_session: &SessionName,
    preferred_window_index: u32,
) -> Option<Target> {
    let mut candidates = sessions
        .iter()
        .flat_map(|(session_name, session)| {
            session
                .windows()
                .iter()
                .filter(move |(_, window)| window.id().as_u32() == window_id)
                .filter_map(move |(window_index, window)| {
                    let pane = window.active_pane()?;
                    Some(PaneTarget::with_window(
                        session_name.clone(),
                        *window_index,
                        pane.index(),
                    ))
                })
        })
        .collect::<Vec<_>>();
    sort_stable_pane_targets(
        &mut candidates,
        sessions,
        preferred_session,
        window_id,
        preferred_window_index,
    );
    candidates.into_iter().next().map(Target::Pane)
}

fn sort_stable_pane_targets(
    targets: &mut [PaneTarget],
    sessions: &SessionStore,
    preferred_session: &SessionName,
    preferred_window_id: u32,
    preferred_window_index: u32,
) {
    targets.sort_by(|left, right| {
        (left.session_name() != preferred_session)
            .cmp(&(right.session_name() != preferred_session))
            .then_with(|| {
                pane_target_window_id(sessions, left)
                    .is_none_or(|window_id| window_id != preferred_window_id)
                    .cmp(
                        &pane_target_window_id(sessions, right)
                            .is_none_or(|window_id| window_id != preferred_window_id),
                    )
            })
            .then_with(|| {
                (left.window_index() != preferred_window_index)
                    .cmp(&(right.window_index() != preferred_window_index))
            })
            .then_with(|| {
                left.session_name()
                    .as_str()
                    .cmp(right.session_name().as_str())
            })
            .then_with(|| left.window_index().cmp(&right.window_index()))
            .then_with(|| left.pane_index().cmp(&right.pane_index()))
    });
}

fn pane_target_window_id(sessions: &SessionStore, target: &PaneTarget) -> Option<u32> {
    sessions
        .session(target.session_name())?
        .window_at(target.window_index())
        .map(|window| window.id().as_u32())
}

pub(super) struct KillPaneLifecycleBatch {
    hook_snapshot: HookStore,
    deferred_sessions: HashMap<SessionName, DeferredLifecycleEvent>,
    deferred_windows: Vec<DeferredWindowUnlinked>,
}

struct DeferredWindowUnlinked {
    target: WindowTarget,
    window_id: WindowId,
    event: DeferredLifecycleEvent,
}

impl KillPaneLifecycleBatch {
    pub(super) fn capture(
        state: &HandlerState,
        target: &PaneTarget,
        kill_all_except: bool,
    ) -> Self {
        let mut candidates =
            state.window_linked_session_family_list(target.session_name(), target.window_index());
        if !candidates.contains(target.session_name()) {
            candidates.push(target.session_name().clone());
        }
        candidates.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        candidates.dedup();

        let deferred_sessions = candidates
            .into_iter()
            .filter_map(|session_name| {
                let session_id = state.sessions.session(&session_name)?.id().as_u32();
                let event = LifecycleEvent::SessionClosed {
                    session_name: session_name.clone(),
                    session_id: Some(session_id),
                };
                Some((session_name, defer_lifecycle_event(state, &event)))
            })
            .collect();

        let deferred_windows = if !kill_all_except
            && state
                .sessions
                .session(target.session_name())
                .and_then(|session| session.window_at(target.window_index()))
                .is_some_and(|window| window.pane_count() == 1)
        {
            let source =
                WindowTarget::with_window(target.session_name().clone(), target.window_index());
            let mut targets =
                state.window_linked_window_targets(target.session_name(), target.window_index());
            if !targets.contains(&source) {
                targets.push(source);
            }
            targets.sort_by(|left, right| {
                left.session_name()
                    .as_str()
                    .cmp(right.session_name().as_str())
                    .then_with(|| left.window_index().cmp(&right.window_index()))
            });
            targets.dedup();
            targets
                .into_iter()
                .filter_map(|window_target| {
                    let window = state
                        .sessions
                        .session(window_target.session_name())?
                        .window_at(window_target.window_index())?;
                    let window_id = window.id();
                    let event = LifecycleEvent::WindowUnlinked {
                        session_name: window_target.session_name().clone(),
                        target: Some(window_target.clone()),
                        window_id: Some(window_id.as_u32()),
                        window_name: Some(window.name().unwrap_or_default().to_owned()),
                    };
                    Some(DeferredWindowUnlinked {
                        target: window_target,
                        window_id,
                        event: defer_lifecycle_event(state, &event),
                    })
                })
                .collect()
        } else {
            Vec::new()
        };

        Self {
            hook_snapshot: state.hooks.clone(),
            deferred_sessions,
            deferred_windows,
        }
    }

    pub(super) fn prepare_committed(
        mut self,
        state: &mut HandlerState,
        destroyed_sessions: &[(SessionName, u32)],
    ) -> Vec<QueuedLifecycleEvent> {
        self.deferred_windows.retain(|window| {
            state
                .sessions
                .session(window.target.session_name())
                .and_then(|session| session.window_at(window.target.window_index()))
                .is_none_or(|current| current.id() != window.window_id)
        });
        let mut ordered_sessions = destroyed_sessions
            .iter()
            .map(|(session_name, _)| session_name.clone())
            .collect::<Vec<_>>();
        ordered_sessions.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        ordered_sessions.dedup();

        let mut prepared = Vec::with_capacity(
            self.deferred_windows
                .len()
                .saturating_add(ordered_sessions.len()),
        );
        let remaining_windows = if self.deferred_windows.is_empty() {
            Vec::new()
        } else {
            let first = self.deferred_windows.remove(0);
            prepared.push(prepare_deferred_lifecycle_event(
                state,
                &mut self.hook_snapshot,
                first.event,
            ));
            self.deferred_windows
        };
        for session_name in ordered_sessions {
            let deferred = self.deferred_sessions.remove(&session_name);
            debug_assert!(
                deferred.is_some(),
                "destroyed pane-family session must have a deferred hook snapshot"
            );
            if let Some(deferred) = deferred {
                prepared.push(prepare_deferred_lifecycle_event(
                    state,
                    &mut self.hook_snapshot,
                    deferred,
                ));
            }
            let _ = state.hooks.remove_session(&session_name);
        }
        for window in remaining_windows {
            prepared.push(prepare_deferred_lifecycle_event(
                state,
                &mut self.hook_snapshot,
                window.event,
            ));
        }
        prepared
    }
}
