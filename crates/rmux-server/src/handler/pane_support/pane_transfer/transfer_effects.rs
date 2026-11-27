use std::collections::HashSet;

use rmux_core::{HookStore, LifecycleEvent};
use rmux_proto::{PaneTarget, Response, SessionId, SessionName, WindowTarget};

use super::super::super::{
    attach_support::{
        prepare_applied_window_resize_events, PreparedAttachedDestroySwitches,
        SessionDetachOnDestroy,
    },
    defer_lifecycle_event, prepare_deferred_lifecycle_event, prepare_lifecycle_event,
    DeferredLifecycleEvent, QueuedLifecycleEvent, RequestHandler, SelectionTransitionSnapshot,
};
use crate::pane_terminals::HandlerState;

pub(super) struct PaneTransferEffects {
    source_family_sessions: Vec<SessionName>,
    selection_before: SelectionTransitionSnapshot,
    refresh_sessions: Vec<SessionName>,
    hook_snapshot: HookStore,
    unlinked_windows: Vec<DeferredLifecycleEvent>,
    closed_sessions: Vec<(
        SessionName,
        SessionId,
        SessionDetachOnDestroy,
        DeferredLifecycleEvent,
    )>,
}

pub(super) struct PreparedPaneTransferEffects {
    refresh_sessions: Vec<SessionName>,
    unlinked_windows: Vec<QueuedLifecycleEvent>,
    pane_selection_events: Vec<QueuedLifecycleEvent>,
    layout_events: Vec<QueuedLifecycleEvent>,
    resize_events: Vec<QueuedLifecycleEvent>,
    linked_event: Option<QueuedLifecycleEvent>,
    session_selection_events: Vec<QueuedLifecycleEvent>,
    closed_sessions: Vec<(
        SessionName,
        SessionId,
        SessionDetachOnDestroy,
        QueuedLifecycleEvent,
    )>,
}

#[derive(Clone, Copy)]
pub(super) enum SourceWindowEffect {
    None,
    RemoveLinkedFamily,
    MoveGroupedSlots,
}

#[derive(Clone, Copy)]
pub(super) enum PaneTransferEmissionPolicy {
    JoinOrMove,
    Break,
}

impl PaneTransferEmissionPolicy {
    fn reserve_applied_resizes(self) -> bool {
        matches!(self, Self::JoinOrMove)
    }

    fn include_selection_changes(self) -> bool {
        matches!(self, Self::Break)
    }
}

impl PaneTransferEffects {
    pub(super) fn capture(
        state: &HandlerState,
        source: &rmux_proto::PaneTarget,
        target: Option<&WindowTarget>,
        source_window_effect: SourceWindowEffect,
    ) -> Self {
        let source_family_sessions =
            state.window_linked_session_family_list(source.session_name(), source.window_index());
        let source_window =
            WindowTarget::with_window(source.session_name().clone(), source.window_index());
        let refresh_sessions = pane_transfer_refresh_sessions(state, &source_window, target);
        let unlinked_windows = unlinked_window_events(state, source, source_window_effect)
            .into_iter()
            .map(|event| defer_lifecycle_event(state, &event))
            .collect();
        let closed_sessions = source_family_sessions
            .iter()
            .filter_map(|session_name| {
                let session_id = state.sessions.session(session_name)?.id().as_u32();
                let event = LifecycleEvent::SessionClosed {
                    session_name: session_name.clone(),
                    session_id: Some(session_id),
                };
                Some((
                    session_name.clone(),
                    SessionId::new(session_id),
                    SessionDetachOnDestroy::capture(state, session_name),
                    defer_lifecycle_event(state, &event),
                ))
            })
            .collect();
        Self {
            source_family_sessions,
            selection_before: SelectionTransitionSnapshot::capture(state),
            refresh_sessions,
            hook_snapshot: state.hooks.clone(),
            unlinked_windows,
            closed_sessions,
        }
    }

    pub(super) fn removed_sessions(
        &self,
        state: &HandlerState,
        response: &Response,
    ) -> Vec<SessionName> {
        if !matches!(
            response,
            Response::JoinPane(_) | Response::MovePane(_) | Response::BreakPane(_)
        ) {
            return Vec::new();
        }
        self.source_family_sessions
            .iter()
            .filter(|session_name| state.sessions.session(session_name).is_none())
            .cloned()
            .collect()
    }

    pub(super) fn prepare_emitted(
        self,
        state: &mut HandlerState,
        removed_sessions: &[SessionName],
        layout_targets: &[WindowTarget],
        linked_event: Option<LifecycleEvent>,
        policy: PaneTransferEmissionPolicy,
    ) -> PreparedPaneTransferEffects {
        let Self {
            refresh_sessions,
            source_family_sessions,
            selection_before,
            mut hook_snapshot,
            unlinked_windows,
            closed_sessions,
            ..
        } = self;
        let unlinked_windows = unlinked_windows
            .into_iter()
            .map(|event| prepare_deferred_lifecycle_event(state, &mut hook_snapshot, event))
            .collect();
        let pane_selection_events = if policy.include_selection_changes() {
            selection_before.prepare_window_pane_changes(state, layout_targets)
        } else {
            Vec::new()
        };
        let mut control_notified_windows = HashSet::new();
        let layout_events = layout_targets
            .iter()
            .cloned()
            .map(|target| {
                let window_id = state
                    .sessions
                    .session(target.session_name())
                    .and_then(|session| session.window_at(target.window_index()))
                    .map(rmux_core::Window::id);
                let mut event =
                    prepare_lifecycle_event(state, &LifecycleEvent::WindowLayoutChanged { target });
                // tmux 3.7b still dispatches both pane-transfer layout hooks
                // when the source window disappears, but sends only one
                // `%layout-change` for the surviving target window.
                if window_id.is_some_and(|window_id| !control_notified_windows.insert(window_id)) {
                    event.suppress_control_effects();
                }
                event
            })
            .collect();
        // Reserve the resize immediately after its layout events. This both
        // claims the layout half of the pending pair and prevents an executing
        // layout hook from draining the shared resize queue through a nested
        // command. Reserving it before session-close tickets also keeps ticket
        // order identical to the emission order below.
        let resize_events = if policy.reserve_applied_resizes() {
            prepare_applied_window_resize_events(state)
        } else {
            Vec::new()
        };
        let linked_event = linked_event
            .as_ref()
            .map(|event| prepare_lifecycle_event(state, event));
        let session_selection_events = if policy.include_selection_changes() {
            let mut preferred_sessions = linked_event
                .as_ref()
                .and_then(|event| event.event.session_name().cloned())
                .into_iter()
                .collect::<Vec<_>>();
            preferred_sessions.extend(source_family_sessions);
            selection_before.prepare_session_window_changes(state, &preferred_sessions)
        } else {
            Vec::new()
        };
        let closed_sessions = closed_sessions
            .into_iter()
            .filter(|(session_name, _, _, _)| removed_sessions.contains(session_name))
            .map(|(session_name, session_id, detach_on_destroy, event)| {
                (
                    session_name,
                    session_id,
                    detach_on_destroy,
                    prepare_deferred_lifecycle_event(state, &mut hook_snapshot, event),
                )
            })
            .collect();
        PreparedPaneTransferEffects {
            refresh_sessions,
            unlinked_windows,
            pane_selection_events,
            layout_events,
            resize_events,
            linked_event,
            session_selection_events,
            closed_sessions,
        }
    }
}

impl RequestHandler {
    pub(super) async fn emit_prepared_pane_transfer_lifecycle(
        &self,
        effects: &PreparedPaneTransferEffects,
    ) -> std::collections::HashMap<SessionId, PreparedAttachedDestroySwitches> {
        let mut prepared_rehomes = std::collections::HashMap::new();
        for (session_name, session_id, detach_on_destroy, _) in &effects.closed_sessions {
            let prepared = self
                .prepare_destroy_session_rehome(session_name, *session_id, *detach_on_destroy)
                .await;
            prepared_rehomes.insert(*session_id, prepared);
        }
        for event in &effects.unlinked_windows {
            self.emit_prepared(event.clone()).await;
        }
        for event in &effects.pane_selection_events {
            self.emit_prepared(event.clone()).await;
        }
        for event in &effects.layout_events {
            self.emit_prepared(event.clone()).await;
        }
        for event in &effects.resize_events {
            self.emit_prepared(event.clone()).await;
        }
        if let Some(event) = &effects.linked_event {
            self.pause_before_window_lifecycle_emit().await;
            self.emit_prepared(event.clone()).await;
        }
        for event in &effects.session_selection_events {
            self.emit_prepared(event.clone()).await;
        }
        for (_, _, _, event) in &effects.closed_sessions {
            self.emit_prepared(event.clone()).await;
        }
        for (_, session_id, _, _) in &effects.closed_sessions {
            if let Some(prepared) = prepared_rehomes.get_mut(session_id) {
                for event in prepared.control_lifecycle_events.drain(..) {
                    self.emit_prepared(event).await;
                }
            }
        }
        prepared_rehomes
    }

    pub(super) async fn finish_pane_transfer_effects(
        &self,
        effects: PreparedPaneTransferEffects,
        removed_sessions: &[SessionName],
        target_session_name: &SessionName,
        mut prepared_attached_switches: std::collections::HashMap<
            SessionId,
            PreparedAttachedDestroySwitches,
        >,
    ) {
        let removed_attached_identities = effects
            .closed_sessions
            .iter()
            .map(|(session_name, session_id, detach_on_destroy, _)| {
                (session_name.clone(), *session_id, *detach_on_destroy)
            })
            .collect::<Vec<_>>();
        let removed_identities = removed_attached_identities
            .iter()
            .map(|(session_name, session_id, _)| (session_name.clone(), *session_id))
            .collect::<Vec<_>>();
        for (session_name, session_id, _, _) in effects.closed_sessions {
            self.prune_web_session(Some((session_name, session_id)));
        }
        self.remove_session_leases(&removed_identities);
        for (session_name, session_id, _detach_on_destroy) in removed_attached_identities {
            let prepared = prepared_attached_switches
                .remove(&session_id)
                .expect("pane transfer destroy rehome must be prepared before publication");
            self.exit_prepared_attached_session_identity(prepared).await;
            self.cancel_session_silence_timers(&session_name).await;
            self.refresh_control_session(&session_name).await;
        }
        for session_name in &effects.refresh_sessions {
            if !removed_sessions.contains(session_name) {
                self.refresh_attached_session(session_name).await;
            }
        }
        if !effects.refresh_sessions.contains(target_session_name) {
            self.refresh_attached_session(target_session_name).await;
        }
        if !removed_sessions.is_empty() {
            let _ = self.queue_shutdown_if_server_empty().await;
        }
    }
}

pub(super) fn pane_transfer_refresh_sessions(
    state: &HandlerState,
    source: &WindowTarget,
    target: Option<&WindowTarget>,
) -> Vec<SessionName> {
    let mut sessions =
        state.window_linked_session_family_list(source.session_name(), source.window_index());
    if let Some(target) = target {
        for session_name in
            state.window_linked_session_family_list(target.session_name(), target.window_index())
        {
            if !sessions.contains(&session_name) {
                sessions.push(session_name);
            }
        }
    }
    sessions
}

fn unlinked_window_events(
    state: &HandlerState,
    source: &PaneTarget,
    effect: SourceWindowEffect,
) -> Vec<LifecycleEvent> {
    let source_is_last_pane = state
        .sessions
        .session(source.session_name())
        .and_then(|session| session.window_at(source.window_index()))
        .is_some_and(|window| window.pane_count() == 1);
    if !source_is_last_pane {
        return Vec::new();
    }

    let targets = match effect {
        SourceWindowEffect::None => Vec::new(),
        SourceWindowEffect::RemoveLinkedFamily => {
            state.window_linked_window_targets(source.session_name(), source.window_index())
        }
        SourceWindowEffect::MoveGroupedSlots => state
            .sessions
            .session_group_members(source.session_name())
            .into_iter()
            .map(|session_name| WindowTarget::with_window(session_name, source.window_index()))
            .collect(),
    };
    targets
        .into_iter()
        .filter_map(|target| {
            let window = state
                .sessions
                .session(target.session_name())?
                .window_at(target.window_index())?;
            Some(LifecycleEvent::WindowUnlinked {
                session_name: target.session_name().clone(),
                target: Some(target),
                window_id: Some(window.id().as_u32()),
                window_name: Some(window.name().unwrap_or_default().to_owned()),
            })
        })
        .collect()
}
