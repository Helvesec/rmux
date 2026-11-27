use std::collections::{HashMap, HashSet};

use rmux_core::{LifecycleEvent, PaneId, WindowId};
use rmux_proto::{RmuxError, SessionId, SessionName, WindowTarget};

use super::{prepare_lifecycle_event, prepare_lifecycle_event_if_enabled, QueuedLifecycleEvent};
use crate::pane_terminals::{session_not_found, HandlerState};

#[derive(Debug, Clone, Copy)]
struct ActiveWindowIdentity {
    window_id: WindowId,
}

#[derive(Debug, Clone, Copy)]
struct ActivePaneIdentity {
    pane_id: PaneId,
}

/// Stable active-window and active-pane identities captured at a mutation
/// boundary.
///
/// Window indexes and pane indexes are mutable tmux addressing slots. Control
/// notifications describe selection changes, so comparing indexes would
/// publish false positives after reindexing and miss replacements in the same
/// slot. This snapshot compares only stable session, window, and pane IDs.
pub(in crate::handler) struct SelectionTransitionSnapshot {
    sessions: HashMap<SessionId, ActiveWindowIdentity>,
    windows: HashMap<WindowId, ActivePaneIdentity>,
}

/// The two stable selections a targeted `switch-client` commit can change.
///
/// Unlike [`SelectionTransitionSnapshot`], this snapshot is intentionally
/// O(1): the command can only select its target session's active window and
/// its target window's active pane. Capturing the whole server here would turn
/// a targeted commit into a catch-up scan.
pub(in crate::handler) struct SelectionTargetTransitionSnapshot {
    target: WindowTarget,
    session_id: SessionId,
    target_window_id: WindowId,
    previous_active_window_id: WindowId,
    previous_target_pane_id: PaneId,
}

impl SelectionTargetTransitionSnapshot {
    pub(in crate::handler) fn capture(
        state: &HandlerState,
        target: WindowTarget,
    ) -> Result<Self, RmuxError> {
        let session = state
            .sessions
            .session(target.session_name())
            .ok_or_else(|| session_not_found(target.session_name()))?;
        let active_window = session
            .window_at(session.active_window_index())
            .ok_or_else(|| {
                RmuxError::Server("switch target session has no active window".to_owned())
            })?;
        let target_window = session
            .window_at(target.window_index())
            .ok_or_else(|| RmuxError::invalid_target(target.to_string(), "window not found"))?;
        let target_pane = target_window.active_pane().ok_or_else(|| {
            RmuxError::invalid_target(target.to_string(), "window has no active pane")
        })?;
        Ok(Self {
            target,
            session_id: session.id(),
            target_window_id: target_window.id(),
            previous_active_window_id: active_window.id(),
            previous_target_pane_id: target_pane.id(),
        })
    }

    /// Prepares only the transitions committed since [`Self::capture`], in
    /// tmux 3.7b order: pane, then window. The caller publishes these before
    /// the client/session transition.
    pub(in crate::handler) fn prepare(self, state: &mut HandlerState) -> Vec<QueuedLifecycleEvent> {
        let (current_active_window_id, current_target_pane_id) = {
            let session = state
                .sessions
                .session(self.target.session_name())
                .filter(|session| session.id() == self.session_id)
                .expect("committed switch target session remains locked");
            let active_window = session
                .window_at(session.active_window_index())
                .expect("committed switch target session has an active window");
            let target_window = session
                .window_at(self.target.window_index())
                .filter(|window| window.id() == self.target_window_id)
                .expect("committed switch target window remains locked");
            let target_pane = target_window
                .active_pane()
                .expect("committed switch target window has an active pane");
            (active_window.id(), target_pane.id())
        };

        let mut events = Vec::with_capacity(2);
        if current_target_pane_id != self.previous_target_pane_id {
            if let Some(event) = prepare_lifecycle_event_if_enabled(
                state,
                &LifecycleEvent::WindowPaneChanged {
                    target: self.target.clone(),
                },
            ) {
                events.push(event);
            }
        }
        if current_active_window_id != self.previous_active_window_id {
            let event = LifecycleEvent::SessionWindowChanged {
                session_name: self.target.session_name().clone(),
            };
            if let Some(mut prepared) = prepare_lifecycle_event_if_enabled(state, &event) {
                prepared.control_session_identity = Some(self.session_id);
                events.push(prepared);
            }
        }
        events
    }
}

impl SelectionTransitionSnapshot {
    pub(in crate::handler) fn capture(state: &HandlerState) -> Self {
        let mut sessions = HashMap::new();
        let mut windows = HashMap::new();
        for (_session_name, session) in state
            .sessions
            .iter()
            .map(|session| (session.name(), session))
        {
            if let Some(window) = session.window_at(session.active_window_index()) {
                sessions.insert(
                    session.id(),
                    ActiveWindowIdentity {
                        window_id: window.id(),
                    },
                );
            }
            for window in session.windows().map(|(_, window)| window) {
                if let Some(pane) = window.active_pane() {
                    windows
                        .entry(window.id())
                        .or_insert(ActivePaneIdentity { pane_id: pane.id() });
                }
            }
        }
        Self { sessions, windows }
    }

    pub(in crate::handler) fn prepare_session_window_changes(
        &self,
        state: &mut HandlerState,
        preferred_sessions: &[SessionName],
    ) -> Vec<QueuedLifecycleEvent> {
        let mut changed = state
            .sessions
            .iter()
            .map(|session| (session.name(), session))
            .filter_map(|(session_name, session)| {
                let previous = self.sessions.get(&session.id())?;
                let current_window_id = session
                    .window_at(session.active_window_index())
                    .map(rmux_core::Window::id)?;
                (previous.window_id != current_window_id)
                    .then(|| (session.id(), session_name.clone()))
            })
            .collect::<Vec<_>>();
        order_sessions(&mut changed, preferred_sessions);

        changed
            .into_iter()
            .filter_map(|(session_id, session_name)| {
                let event = LifecycleEvent::SessionWindowChanged {
                    session_name: session_name.clone(),
                };
                let mut prepared = prepare_lifecycle_event_if_enabled(state, &event)?;
                prepared.control_session_identity = Some(session_id);
                Some(prepared)
            })
            .collect()
    }

    pub(in crate::handler) fn prepare_session_window_change(
        &self,
        state: &mut HandlerState,
        session_name: &SessionName,
    ) -> Option<QueuedLifecycleEvent> {
        let session = state.sessions.session(session_name)?;
        let session_id = session.id();
        let previous = self.sessions.get(&session_id)?;
        let current_window_id = session
            .window_at(session.active_window_index())
            .map(rmux_core::Window::id)?;
        if previous.window_id == current_window_id {
            return None;
        }

        let event = LifecycleEvent::SessionWindowChanged {
            session_name: session_name.clone(),
        };
        let mut prepared = prepare_lifecycle_event_if_enabled(state, &event)?;
        prepared.control_session_identity = Some(session_id);
        Some(prepared)
    }

    pub(in crate::handler) fn prepare_window_pane_changes(
        &self,
        state: &mut HandlerState,
        preferred_windows: &[WindowTarget],
    ) -> Vec<QueuedLifecycleEvent> {
        let mut current = HashMap::new();
        for target in preferred_windows {
            let Some(window) = state
                .sessions
                .session(target.session_name())
                .and_then(|session| session.window_at(target.window_index()))
            else {
                continue;
            };
            if let Some(pane) = window.active_pane() {
                current
                    .entry(window.id())
                    .or_insert((pane.id(), target.clone()));
            }
        }
        for (session_name, session) in state
            .sessions
            .iter()
            .map(|session| (session.name(), session))
        {
            for (window_index, window) in session.windows() {
                let Some(pane) = window.active_pane() else {
                    continue;
                };
                current.entry(window.id()).or_insert_with(|| {
                    (
                        pane.id(),
                        WindowTarget::with_window(session_name.clone(), window_index),
                    )
                });
            }
        }

        let mut changed = current
            .into_iter()
            .filter_map(|(window_id, (pane_id, target))| {
                let previous = self.windows.get(&window_id)?;
                (previous.pane_id != pane_id).then_some((window_id, target))
            })
            .collect::<Vec<_>>();
        order_windows(state, &mut changed, preferred_windows);

        changed
            .into_iter()
            .filter_map(|(_window_id, target)| {
                prepare_lifecycle_event_if_enabled(
                    state,
                    &LifecycleEvent::WindowPaneChanged { target },
                )
            })
            .collect()
    }

    pub(in crate::handler) fn prepare_surviving_kill_pane_changes(
        &self,
        state: &mut HandlerState,
        layout_target: WindowTarget,
    ) -> Vec<QueuedLifecycleEvent> {
        let mut events = vec![prepare_lifecycle_event(
            state,
            &LifecycleEvent::WindowLayoutChanged {
                target: layout_target.clone(),
            },
        )];
        events
            .extend(self.prepare_window_pane_changes(state, std::slice::from_ref(&layout_target)));
        events
    }
}

fn order_sessions(changed: &mut [(SessionId, SessionName)], preferred_sessions: &[SessionName]) {
    let mut preferred_positions = HashMap::new();
    for (position, session_name) in preferred_sessions.iter().enumerate() {
        preferred_positions
            .entry(session_name.clone())
            .or_insert(position);
    }
    changed.sort_by(|(left_id, left_name), (right_id, right_name)| {
        let left_position = preferred_positions.get(left_name).copied();
        let right_position = preferred_positions.get(right_name).copied();
        left_position
            .is_none()
            .cmp(&right_position.is_none())
            .then_with(|| left_position.cmp(&right_position))
            .then_with(|| left_id.as_u32().cmp(&right_id.as_u32()))
    });
}

fn order_windows(
    state: &HandlerState,
    changed: &mut [(WindowId, WindowTarget)],
    preferred_windows: &[WindowTarget],
) {
    let preferred_ids = preferred_windows
        .iter()
        .filter_map(|target| {
            state
                .sessions
                .session(target.session_name())
                .and_then(|session| session.window_at(target.window_index()))
                .map(rmux_core::Window::id)
        })
        .collect::<Vec<_>>();
    let mut seen = HashSet::new();
    let preferred_positions = preferred_ids
        .into_iter()
        .filter(|window_id| seen.insert(*window_id))
        .enumerate()
        .map(|(position, window_id)| (window_id, position))
        .collect::<HashMap<_, _>>();
    changed.sort_by(|(left_id, _), (right_id, _)| {
        let left_position = preferred_positions.get(left_id).copied();
        let right_position = preferred_positions.get(right_id).copied();
        left_position
            .is_none()
            .cmp(&right_position.is_none())
            .then_with(|| left_position.cmp(&right_position))
            .then_with(|| left_id.as_u32().cmp(&right_id.as_u32()))
    });
}
