//! Post-mutation lifecycle and attached-client effects for `move-window`.

use std::collections::{HashMap, HashSet};

use rmux_core::{HookStore, LifecycleEvent, WindowId};
use rmux_proto::{
    MoveWindowRequest, MoveWindowResponse, MoveWindowTarget, SessionId, SessionName, WindowTarget,
};

use super::super::attach_support::{surviving_attached_resize_targets, SessionDetachOnDestroy};
use super::super::{
    defer_lifecycle_event, prepare_deferred_lifecycle_event, prepare_lifecycle_event_if_enabled,
    DeferredLifecycleEvent, QueuedLifecycleEvent, RequestHandler,
};
use crate::pane_terminals::HandlerState;

pub(super) struct MoveWindowEffects {
    source_session_name: Option<SessionName>,
    source_family_sessions: Vec<SessionName>,
    destination_window_id_before: Option<WindowId>,
    resize_window_ids: Vec<WindowId>,
    hook_snapshot: HookStore,
    unlinked_candidates: Vec<WindowUnlinkedCandidate>,
    deferred_closed_sessions: Vec<(
        SessionName,
        SessionId,
        SessionDetachOnDestroy,
        DeferredLifecycleEvent,
    )>,
}

pub(in crate::handler) struct PreparedMoveWindowEffects {
    linked_event: Option<QueuedLifecycleEvent>,
    lifecycle_events: Vec<PreparedLifecycleEffect>,
    removed_sessions: Vec<SessionName>,
    refresh_sessions: Vec<SessionName>,
    resize_window_ids: Vec<WindowId>,
}

enum PreparedLifecycleEffect {
    Unlinked(QueuedLifecycleEvent),
    Closed {
        session_name: SessionName,
        session_id: SessionId,
        detach_on_destroy: SessionDetachOnDestroy,
        event: QueuedLifecycleEvent,
    },
}

enum PendingLifecycleEffect {
    Unlinked(DeferredLifecycleEvent),
    Closed {
        session_name: SessionName,
        session_id: SessionId,
        detach_on_destroy: SessionDetachOnDestroy,
        event: DeferredLifecycleEvent,
    },
}

struct WindowUnlinkedCandidate {
    target: WindowTarget,
    window_id: u32,
    window_name: String,
    deferred: DeferredLifecycleEvent,
}

impl MoveWindowEffects {
    pub(super) fn capture(state: &HandlerState, request: &MoveWindowRequest) -> Self {
        let source = if request.renumber {
            None
        } else {
            request.source.as_ref()
        };
        let mut source_family_sessions = Vec::new();
        let mut seen = HashSet::new();
        if let Some(source) = source {
            push_unique_session(
                &mut source_family_sessions,
                &mut seen,
                source.session_name().clone(),
            );
            for session_name in state
                .window_linked_session_family_list(source.session_name(), source.window_index())
            {
                push_unique_session(&mut source_family_sessions, &mut seen, session_name);
            }
        }
        let unlinked_candidates = source
            .map(|source| capture_unlinked_candidates(state, source))
            .unwrap_or_default();
        let destination_window_id_before = match &request.target {
            MoveWindowTarget::Session(_) => None,
            MoveWindowTarget::Window(target) => state
                .sessions
                .session(target.session_name())
                .and_then(|session| session.window_at(target.window_index()))
                .map(rmux_core::Window::id),
        };
        let mut resize_window_ids = source
            .and_then(|source| {
                state
                    .sessions
                    .session(source.session_name())
                    .and_then(|session| session.window_at(source.window_index()))
                    .map(rmux_core::Window::id)
            })
            .into_iter()
            .collect::<Vec<_>>();
        resize_window_ids.extend(destination_window_id_before);

        let deferred_closed_sessions = source_family_sessions
            .iter()
            .filter_map(|session_name| {
                let session_id = state.sessions.session(session_name)?.id();
                let event = LifecycleEvent::SessionClosed {
                    session_name: session_name.clone(),
                    session_id: Some(session_id.as_u32()),
                };
                Some((
                    session_name.clone(),
                    session_id,
                    SessionDetachOnDestroy::capture(state, session_name),
                    defer_lifecycle_event(state, &event),
                ))
            })
            .collect();

        Self {
            source_session_name: source.map(|source| source.session_name().clone()),
            source_family_sessions,
            destination_window_id_before,
            resize_window_ids,
            hook_snapshot: state.hooks.clone(),
            unlinked_candidates,
            deferred_closed_sessions,
        }
    }

    pub(super) fn prepare_success(
        self,
        state: &mut HandlerState,
        response: &MoveWindowResponse,
    ) -> PreparedMoveWindowEffects {
        let Self {
            source_session_name,
            source_family_sessions,
            destination_window_id_before,
            resize_window_ids,
            mut hook_snapshot,
            unlinked_candidates,
            deferred_closed_sessions,
        } = self;
        let cross_session_move = source_session_name
            .as_ref()
            .is_some_and(|source_session| source_session != &response.session_name);
        let mut unlinked_events = Vec::new();
        for candidate in unlinked_candidates {
            if candidate.original_slot_still_links_window(state) {
                continue;
            }
            let session_name = candidate.target.session_name().clone();
            let event = if cross_session_move {
                candidate.deferred
            } else {
                let target = candidate
                    .resolved_target_in_original_session(state)
                    .unwrap_or_else(|| candidate.target.clone());
                defer_lifecycle_event(state, &candidate.event(target))
            };
            unlinked_events.push((session_name, event));
        }
        let destination_already_linked_final_window =
            destination_window_id_before.is_some_and(|window_id| {
                response
                    .target
                    .as_ref()
                    .and_then(|target| {
                        state
                            .sessions
                            .session(target.session_name())
                            .and_then(|session| session.window_at(target.window_index()))
                    })
                    .is_some_and(|window| window.id() == window_id)
            });
        let linked_event = (!unlinked_events.is_empty()
            && !destination_already_linked_final_window)
            .then(|| LifecycleEvent::WindowLinked {
                session_name: response.session_name.clone(),
                target: response.target.clone(),
            })
            .and_then(|event| prepare_lifecycle_event_if_enabled(state, &event));
        let removed_sessions = source_family_sessions
            .iter()
            .filter(|session_name| state.sessions.session(session_name).is_none())
            .cloned()
            .collect::<Vec<_>>();
        let mut deferred_closed_by_session = deferred_closed_sessions
            .into_iter()
            .map(|(session_name, session_id, detach_on_destroy, deferred)| {
                (session_name, (session_id, detach_on_destroy, deferred))
            })
            .collect::<HashMap<_, _>>();
        let mut closed_sessions = Vec::new();
        for session_name in ordered_closed_sessions(&removed_sessions, source_session_name.as_ref())
        {
            let Some((session_id, detach_on_destroy, deferred)) =
                deferred_closed_by_session.remove(&session_name)
            else {
                continue;
            };
            closed_sessions.push((session_name, session_id, detach_on_destroy, deferred));
        }
        let lifecycle_events = order_lifecycle_effects(
            unlinked_events,
            closed_sessions,
            source_session_name.as_ref(),
            &removed_sessions,
        )
        .into_iter()
        .map(|effect| effect.prepare(state, &mut hook_snapshot))
        .collect();

        let mut refresh_sessions = Vec::new();
        let mut seen = HashSet::new();
        for session_name in source_family_sessions {
            if state.sessions.session(&session_name).is_some() {
                push_unique_session(&mut refresh_sessions, &mut seen, session_name);
            }
        }
        push_unique_session(
            &mut refresh_sessions,
            &mut seen,
            response.session_name.clone(),
        );
        let destination_family = response.target.as_ref().map_or_else(
            || state.sessions.session_group_members(&response.session_name),
            |target| {
                state
                    .window_linked_session_family_list(target.session_name(), target.window_index())
            },
        );
        for session_name in destination_family {
            push_unique_session(&mut refresh_sessions, &mut seen, session_name);
        }
        PreparedMoveWindowEffects {
            linked_event,
            lifecycle_events,
            removed_sessions,
            refresh_sessions,
            resize_window_ids,
        }
    }
}

impl RequestHandler {
    pub(super) async fn finish_move_window_effects(&self, effects: PreparedMoveWindowEffects) {
        let removed_attached_identities = effects
            .lifecycle_events
            .iter()
            .filter_map(|effect| match effect {
                PreparedLifecycleEffect::Closed {
                    session_name,
                    session_id,
                    detach_on_destroy,
                    ..
                } => Some((session_name.clone(), *session_id, *detach_on_destroy)),
                PreparedLifecycleEffect::Unlinked(_) => None,
            })
            .collect::<Vec<_>>();
        let removed_identities = removed_attached_identities
            .iter()
            .map(|(session_name, session_id, _)| (session_name.clone(), *session_id))
            .collect::<Vec<_>>();
        let resize_window_ids = effects.resize_window_ids;
        let mut refresh_sessions = effects.refresh_sessions;
        let mut prepared_rehomes = HashMap::new();
        for (session_name, session_id, detach_on_destroy) in &removed_attached_identities {
            let prepared = self
                .prepare_destroy_session_rehome(session_name, *session_id, *detach_on_destroy)
                .await;
            prepared_rehomes.insert(*session_id, prepared);
        }

        // The linked event and ordered teardown effects were reserved before
        // the control rehome events. Publish that exact order first.
        if let Some(event) = effects.linked_event {
            self.pause_before_window_lifecycle_emit().await;
            self.emit_prepared(event).await;
        }
        for effect in effects.lifecycle_events {
            match effect {
                PreparedLifecycleEffect::Unlinked(event) => {
                    self.emit_prepared(event).await;
                }
                PreparedLifecycleEffect::Closed {
                    session_name,
                    session_id,
                    detach_on_destroy: _,
                    event,
                } => {
                    self.prune_web_session(Some((session_name, session_id)));
                    self.emit_prepared(event).await;
                }
            }
        }
        for (_, session_id, _) in &removed_attached_identities {
            if let Some(prepared) = prepared_rehomes.get_mut(session_id) {
                for event in prepared.control_lifecycle_events.drain(..) {
                    self.emit_prepared(event).await;
                }
            }
        }

        self.remove_session_leases(&removed_identities);
        for (session_name, session_id, _detach_on_destroy) in removed_attached_identities {
            let prepared = prepared_rehomes
                .remove(&session_id)
                .expect("move-window destroy rehome must be prepared before publication");
            self.exit_prepared_attached_session_identity(prepared).await;
            self.cancel_session_silence_timers(&session_name).await;
            self.refresh_control_session(&session_name).await;
        }
        let resize_targets = {
            let state = self.state.lock().await;
            let resize_targets = surviving_attached_resize_targets(&state, resize_window_ids);
            for resize_target in &resize_targets {
                refresh_sessions.extend(state.window_linked_session_family_list(
                    resize_target.session_name(),
                    resize_target.window_index(),
                ));
            }
            resize_targets
        };
        for resize_target in resize_targets {
            let _ = self
                .reconcile_attached_window_size_and_emit(&resize_target)
                .await;
        }
        refresh_sessions.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        refresh_sessions.dedup();
        for session_name in refresh_sessions {
            self.refresh_attached_session(&session_name).await;
        }
        if !effects.removed_sessions.is_empty() {
            let _ = self.queue_shutdown_if_server_empty().await;
        }
    }
}

fn ordered_closed_sessions(
    removed_sessions: &[SessionName],
    source_session_name: Option<&SessionName>,
) -> Vec<SessionName> {
    let source_was_removed = source_session_name
        .is_some_and(|source| removed_sessions.iter().any(|removed| removed == source));
    if !source_was_removed {
        return removed_sessions.to_vec();
    }

    let source_session_name = source_session_name.expect("removed move source is present");
    let mut ordered = removed_sessions
        .iter()
        .filter(|session_name| *session_name != source_session_name)
        .cloned()
        .collect::<Vec<_>>();
    ordered.push(source_session_name.clone());
    ordered
}

fn order_lifecycle_effects(
    mut unlinked: Vec<(SessionName, DeferredLifecycleEvent)>,
    mut closed: Vec<(
        SessionName,
        SessionId,
        SessionDetachOnDestroy,
        DeferredLifecycleEvent,
    )>,
    source_session_name: Option<&SessionName>,
    removed_sessions: &[SessionName],
) -> Vec<PendingLifecycleEffect> {
    let mut ordered = Vec::with_capacity(unlinked.len() + closed.len());
    let source_was_removed = source_session_name
        .is_some_and(|source| removed_sessions.iter().any(|removed| removed == source));
    if !source_was_removed {
        ordered.extend(unlinked.drain(..).map(unlinked_effect));
        ordered.extend(closed.drain(..).map(closed_effect));
        return ordered;
    }

    let source_session_name = source_session_name.expect("removed move source is present");
    take_unlinked_effect(&mut unlinked, source_session_name, &mut ordered);
    let other_closed_sessions = closed
        .iter()
        .map(|(session_name, _, _, _)| session_name.clone())
        .filter(|session_name| session_name != source_session_name)
        .collect::<Vec<_>>();
    for session_name in other_closed_sessions {
        take_closed_effect(&mut closed, &session_name, &mut ordered);
        take_unlinked_effect(&mut unlinked, &session_name, &mut ordered);
    }
    take_closed_effect(&mut closed, source_session_name, &mut ordered);
    ordered.extend(unlinked.drain(..).map(unlinked_effect));
    ordered.extend(closed.drain(..).map(closed_effect));
    ordered
}

fn take_unlinked_effect(
    events: &mut Vec<(SessionName, DeferredLifecycleEvent)>,
    session_name: &SessionName,
    ordered: &mut Vec<PendingLifecycleEffect>,
) {
    let Some(index) = events
        .iter()
        .position(|(candidate, _)| candidate == session_name)
    else {
        return;
    };
    ordered.push(unlinked_effect(events.remove(index)));
}

fn take_closed_effect(
    events: &mut Vec<(
        SessionName,
        SessionId,
        SessionDetachOnDestroy,
        DeferredLifecycleEvent,
    )>,
    session_name: &SessionName,
    ordered: &mut Vec<PendingLifecycleEffect>,
) {
    let Some(index) = events
        .iter()
        .position(|(candidate, _, _, _)| candidate == session_name)
    else {
        return;
    };
    ordered.push(closed_effect(events.remove(index)));
}

fn unlinked_effect(
    (_session_name, event): (SessionName, DeferredLifecycleEvent),
) -> PendingLifecycleEffect {
    PendingLifecycleEffect::Unlinked(event)
}

fn closed_effect(
    (session_name, session_id, detach_on_destroy, event): (
        SessionName,
        SessionId,
        SessionDetachOnDestroy,
        DeferredLifecycleEvent,
    ),
) -> PendingLifecycleEffect {
    PendingLifecycleEffect::Closed {
        session_name,
        session_id,
        detach_on_destroy,
        event,
    }
}

impl PendingLifecycleEffect {
    fn prepare(
        self,
        state: &mut HandlerState,
        hook_snapshot: &mut HookStore,
    ) -> PreparedLifecycleEffect {
        match self {
            Self::Unlinked(event) => PreparedLifecycleEffect::Unlinked(
                prepare_deferred_lifecycle_event(state, hook_snapshot, event),
            ),
            Self::Closed {
                session_name,
                session_id,
                detach_on_destroy,
                event,
            } => PreparedLifecycleEffect::Closed {
                session_name,
                session_id,
                detach_on_destroy,
                event: prepare_deferred_lifecycle_event(state, hook_snapshot, event),
            },
        }
    }
}

impl WindowUnlinkedCandidate {
    fn capture(state: &HandlerState, target: WindowTarget) -> Option<Self> {
        let window = state
            .sessions
            .session(target.session_name())?
            .window_at(target.window_index())?;
        let window_id = window.id().as_u32();
        let window_name = window.name().unwrap_or_default().to_owned();
        let event = LifecycleEvent::WindowUnlinked {
            session_name: target.session_name().clone(),
            target: Some(target.clone()),
            window_id: Some(window_id),
            window_name: Some(window_name.clone()),
        };
        Some(Self {
            target,
            window_id,
            window_name,
            deferred: defer_lifecycle_event(state, &event),
        })
    }

    fn original_slot_still_links_window(&self, state: &HandlerState) -> bool {
        state
            .sessions
            .session(self.target.session_name())
            .and_then(|session| session.window_at(self.target.window_index()))
            .is_some_and(|window| window.id().as_u32() == self.window_id)
    }

    fn resolved_target_in_original_session(&self, state: &HandlerState) -> Option<WindowTarget> {
        let session = state.sessions.session(self.target.session_name())?;
        session.windows().iter().find_map(|(window_index, window)| {
            (window.id().as_u32() == self.window_id).then(|| {
                WindowTarget::with_window(self.target.session_name().clone(), *window_index)
            })
        })
    }

    fn event(&self, target: WindowTarget) -> LifecycleEvent {
        LifecycleEvent::WindowUnlinked {
            session_name: self.target.session_name().clone(),
            target: Some(target),
            window_id: Some(self.window_id),
            window_name: Some(self.window_name.clone()),
        }
    }
}

fn capture_unlinked_candidates(
    state: &HandlerState,
    source: &WindowTarget,
) -> Vec<WindowUnlinkedCandidate> {
    let mut targets = vec![source.clone()];
    targets.extend(
        state
            .window_linked_window_targets(source.session_name(), source.window_index())
            .into_iter()
            .filter(|target| target != source),
    );
    targets
        .into_iter()
        .filter_map(|target| WindowUnlinkedCandidate::capture(state, target))
        .collect()
}

fn push_unique_session(
    sessions: &mut Vec<SessionName>,
    seen: &mut HashSet<SessionName>,
    session_name: SessionName,
) {
    if seen.insert(session_name.clone()) {
        sessions.push(session_name);
    }
}
