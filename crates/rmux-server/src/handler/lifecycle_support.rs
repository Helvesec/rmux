use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use rmux_core::{
    command_parser::{CommandArgument, ParsedCommand},
    HookDispatch, HookScopeIdentity, HookStore, LifecycleEvent, PaneId, WindowId,
};
use rmux_proto::{
    HookName, PaneTarget, Request, Response, ScopeSelector, SessionId, Target, WindowTarget,
};
#[cfg(test)]
use tokio::sync::broadcast;
use tokio::sync::{mpsc, oneshot};
use tracing::warn;

use crate::hook_runtime::{
    hooks_disabled, lifecycle_hooks_disabled, queue_inline_hook, with_hook_execution,
    ExactPaneHookTarget, HookExecutionContext, NotificationState, PendingInlineHook,
    PendingInlineHookFormat,
};

use super::{
    active_session_target, active_window_target, fallback_current_target,
    target_for_request_response, target_to_scope, RequestHandler,
};

mod command_hooks;
mod current_target;
mod formats;
mod hook_dispatch;
mod publication;
mod stable_target;

use current_target::*;
pub(in crate::handler) use formats::after_hook_format_values;
use formats::{hook_only_format_values, lifecycle_hook_formats};
use stable_target::*;

mod ordered_wait;
mod retained_target;

pub(crate) use retained_target::RetainedLifecycleTargetRegistry;
pub(in crate::handler) use retained_target::{LeaseResolution, LifecycleTargetLease};

const LIFECYCLE_DISPATCH_ADMISSION_WAIT: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct QueuedLifecycleEvent {
    pub(in crate::handler) event: LifecycleEvent,
    pub(in crate::handler) control_session_identity: Option<SessionId>,
    pub(in crate::handler) control_window_identity: Option<(SessionId, WindowId)>,
    pub(in crate::handler) control_layout_notifications:
        Option<Vec<super::control_support::ControlLayoutNotification>>,
    pub(in crate::handler) hook_name: HookName,
    pub(in crate::handler) hooks: Vec<HookDispatch>,
    pub(in crate::handler) formats: Vec<(String, String)>,
    pub(in crate::handler) current_target: Option<Target>,
    stable_current_target_identity: Option<StableLifecycleTargetIdentity>,
    /// The exact pane state tmux captures when the notification fires
    /// (`ne->fs`), or `None` when `current_target` substitutes for it.
    notification_state: Option<StableLifecycleTargetIdentity>,
    retained_current_target: Option<Arc<LifecycleTargetLease>>,
    control_effects_dispatched: bool,
    server_exit: bool,
    commit_order: Option<crate::lifecycle_commit_order::LifecycleCommitTicket>,
    _unordered_publication: Option<crate::lifecycle_commit_order::LifecyclePublicationGuard>,
    publication_discarded: bool,
}

impl QueuedLifecycleEvent {
    pub(in crate::handler) fn suppress_control_effects(&mut self) {
        self.control_effects_dispatched = true;
    }

    /// Marks a session close published by the server exit, which ends every
    /// control client itself rather than one session at a time.
    pub(in crate::handler) fn for_server_exit(mut self) -> Self {
        self.server_exit = true;
        self
    }

    #[cfg(test)]
    pub(crate) fn event_for_test(&self) -> &LifecycleEvent {
        &self.event
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum StableLifecycleTargetIdentity {
    Session {
        session_id: SessionId,
    },
    Window {
        session_id: SessionId,
        window: StableWindowIdentity,
        window_loss: StableWindowLoss,
    },
    Pane {
        session_id: SessionId,
        window: StableWindowIdentity,
        pane_id: PaneId,
        window_loss: StableWindowLoss,
    },
    GlobalWindow {
        window_id: WindowId,
    },
    GlobalPane {
        pane_id: PaneId,
        window_id: Option<WindowId>,
    },
    Removed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StableWindowLoss {
    FallbackToSession,
    RequireWindowIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum HookDispatchCurrentTarget {
    Dynamic(Option<Target>),
    Stable(StableLifecycleTargetIdentity),
    Retainable(Arc<LifecycleTargetLease>),
    HistoricalPane(Arc<LifecycleTargetLease>),
    ExactPane(ExactPaneHookTarget),
    ExactSession(SessionId),
    MissingStable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StableWindowIdentity {
    window_id: WindowId,
    preferred_index: u32,
    occurrence_id: Option<crate::pane_terminals::WindowLinkOccurrenceId>,
}

#[derive(Debug)]
pub(crate) struct LifecycleDispatchItem {
    event: QueuedLifecycleEvent,
    completion: Option<oneshot::Sender<()>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeferredLifecycleEvent {
    queued: QueuedLifecycleEvent,
    dispatch_scope: ScopeSelector,
    dispatch_identity: Option<HookScopeIdentity>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::handler) struct UnsequencedLifecycleEvent {
    queued: QueuedLifecycleEvent,
    ordered: bool,
}

impl RequestHandler {
    async fn current_target_for_request_response(
        &self,
        requester_pid: u32,
        request: &Request,
        response: &Response,
    ) -> Option<Target> {
        let attached_session = self.current_session_candidate(requester_pid).await;
        let state = self.state.lock().await;
        target_for_request_response(&state, request, response, attached_session.as_ref())
    }

    async fn fallback_target_from_current_hook_formats(&self) -> Option<Target> {
        let formats = crate::hook_runtime::current_hook_formats();
        let state = self.state.lock().await;
        hook_format_session_target(&state, &formats)
    }

    async fn valid_hook_command_target(&self, target: Option<Target>) -> Option<Target> {
        if let Some(target) = target {
            let state = self.state.lock().await;
            if target_exists(&state, &target) {
                return pane_target_for_existing_target(&state, &target).or(Some(target));
            }
        }
        self.fallback_target_from_current_hook_formats().await
    }

    async fn stable_hook_command_target(
        &self,
        identity: &StableLifecycleTargetIdentity,
    ) -> Option<Target> {
        let state = self.state.lock().await;
        let target = identity.resolve(&state)?;
        pane_target_for_existing_target(&state, &target).or(Some(target))
    }

    async fn lifecycle_dispatch_current_target(
        &self,
        event: &QueuedLifecycleEvent,
    ) -> Option<Target> {
        let state = self.state.lock().await;
        if let Some(pane_target) = event
            .event
            .pane_target()
            .filter(|_| matches!(event.event, LifecycleEvent::PaneExited { .. }))
        {
            if let Some(Target::Pane(target)) = event
                .current_target
                .as_ref()
                .filter(|target| target_exists(&state, target))
            {
                if target != pane_target {
                    return Some(Target::Pane(target.clone()));
                }
            }
            return fallback_session_pane_target_after_exit(&state, pane_target).or_else(|| {
                event
                    .event
                    .session_name()
                    .and_then(|session_name| first_session_pane_target(&state, session_name))
                    .or_else(|| hook_format_session_target(&state, &event.formats))
            });
        }
        if let Some(target) = event
            .current_target
            .as_ref()
            .filter(|target| target_exists(&state, target))
        {
            return Some(target.clone());
        }
        if let Some(pane_target) = event.event.pane_target() {
            return fallback_session_pane_target_after_exit(&state, pane_target);
        }
        event
            .event
            .session_name()
            .and_then(|session_name| {
                active_session_target(&state.sessions, session_name)
                    .or_else(|| first_session_pane_target(&state, session_name))
            })
            .or_else(|| hook_format_session_target(&state, &event.formats))
    }
}

fn lifecycle_event_allows_missing_stable_target(
    event: &LifecycleEvent,
    identity: &StableLifecycleTargetIdentity,
) -> bool {
    match event {
        LifecycleEvent::WindowUnlinked { .. } => matches!(
            identity,
            StableLifecycleTargetIdentity::GlobalWindow { .. }
                | StableLifecycleTargetIdentity::Removed
        ),
        LifecycleEvent::SessionClosed { .. }
        | LifecycleEvent::PaneExited { .. }
        | LifecycleEvent::PaneDied { .. } => true,
        _ => false,
    }
}

pub(in crate::handler) fn prepare_lifecycle_event(
    state: &mut crate::pane_terminals::HandlerState,
    event: &LifecycleEvent,
) -> QueuedLifecycleEvent {
    if let LifecycleEvent::WindowLayoutChanged { target } = event {
        state.claim_applied_resize_layout_change(target);
    }
    let event = prepare_unsequenced_lifecycle_event(state, event);
    sequence_prepared_lifecycle_event(state, event)
}

pub(in crate::handler) fn prepare_unsequenced_lifecycle_event(
    state: &mut crate::pane_terminals::HandlerState,
    event: &LifecycleEvent,
) -> UnsequencedLifecycleEvent {
    let deferred = defer_lifecycle_event(state, event);
    if lifecycle_hooks_disabled() {
        return UnsequencedLifecycleEvent {
            queued: deferred.queued,
            ordered: false,
        };
    }
    let hooks = match &deferred.dispatch_identity {
        Some(identity) => state.hooks.dispatch_with_identity_or_scope(
            identity,
            &deferred.dispatch_scope,
            deferred.queued.hook_name,
        ),
        None => state
            .hooks
            .dispatch(&deferred.dispatch_scope, deferred.queued.hook_name),
    };
    UnsequencedLifecycleEvent {
        queued: deferred.with_hooks(hooks),
        ordered: true,
    }
}

pub(in crate::handler) fn sequence_prepared_lifecycle_event(
    state: &mut crate::pane_terminals::HandlerState,
    mut event: UnsequencedLifecycleEvent,
) -> QueuedLifecycleEvent {
    if event.ordered {
        if let Some(commit_order) = state.reserve_lifecycle_commit_order() {
            event.queued.commit_order = Some(commit_order);
            return event.queued;
        }
    }
    track_unordered_lifecycle_publication(state, event.queued)
}

pub(in crate::handler) fn prepare_lifecycle_event_if_enabled(
    state: &mut crate::pane_terminals::HandlerState,
    event: &LifecycleEvent,
) -> Option<QueuedLifecycleEvent> {
    Some(prepare_lifecycle_event(state, event))
}

pub(in crate::handler) fn defer_lifecycle_event(
    state: &crate::pane_terminals::HandlerState,
    event: &LifecycleEvent,
) -> DeferredLifecycleEvent {
    let hook_name = event.hook_name();
    let (current_target, stable_current_target_identity) =
        lifecycle_hook_current_target_snapshot(state, event);
    let notification_state = lifecycle_notification_state(state, event, current_target.as_ref());
    let dispatch_scope = lifecycle_hook_dispatch_scope(event, current_target.as_ref());
    let dispatch_identity = super::lifecycle_hook_scope_identity(state, &dispatch_scope, event);
    DeferredLifecycleEvent {
        queued: QueuedLifecycleEvent {
            event: event.clone(),
            control_session_identity: None,
            control_window_identity: None,
            control_layout_notifications: None,
            hook_name,
            hooks: Vec::new(),
            formats: lifecycle_hook_formats(state, event),
            current_target,
            stable_current_target_identity,
            notification_state,
            retained_current_target: retained_target::capture_for_event(state, event),
            control_effects_dispatched: false,
            server_exit: false,
            commit_order: None,
            _unordered_publication: None,
            publication_discarded: false,
        },
        dispatch_scope,
        dispatch_identity,
    }
}

pub(in crate::handler) fn prepare_deferred_lifecycle_event(
    state: &mut crate::pane_terminals::HandlerState,
    hook_snapshot: &mut HookStore,
    mut deferred: DeferredLifecycleEvent,
) -> QueuedLifecycleEvent {
    deferred.refresh_window_unlinked_current_target(state);
    if lifecycle_hooks_disabled() {
        return track_unordered_lifecycle_publication(state, deferred.queued);
    }
    let Some(commit_order) = state.reserve_lifecycle_commit_order() else {
        return track_unordered_lifecycle_publication(state, deferred.queued);
    };
    let hooks = match &deferred.dispatch_identity {
        Some(identity) => state.hooks.dispatch_deferred_with_identity_or_scope(
            hook_snapshot,
            identity,
            &deferred.dispatch_scope,
            deferred.queued.hook_name,
        ),
        None => state.hooks.dispatch_deferred_from(
            hook_snapshot,
            &deferred.dispatch_scope,
            deferred.queued.hook_name,
        ),
    };
    let mut queued = deferred.with_hooks(hooks);
    queued.commit_order = Some(commit_order);
    queued
}

fn track_unordered_lifecycle_publication(
    state: &crate::pane_terminals::HandlerState,
    mut queued: QueuedLifecycleEvent,
) -> QueuedLifecycleEvent {
    match state.track_unordered_lifecycle_publication() {
        Some(publication) => queued._unordered_publication = Some(publication),
        None => queued.publication_discarded = true,
    }
    queued
}

impl DeferredLifecycleEvent {
    pub(in crate::handler) fn retire_historical_pane_target(&self) {
        if matches!(self.queued.event, LifecycleEvent::PaneFocusOut { .. }) {
            if let Some(target) = self.queued.retained_current_target.as_ref() {
                target.retire_historical();
            }
        }
    }

    pub(in crate::handler) fn rename_pane_event_session(
        &mut self,
        old_name: &rmux_proto::SessionName,
        new_name: &rmux_proto::SessionName,
    ) {
        let rename_target = |target: &mut PaneTarget| {
            if target.session_name() == old_name {
                *target = PaneTarget::with_window(
                    new_name.clone(),
                    target.window_index(),
                    target.pane_index(),
                );
            }
        };
        match &mut self.queued.event {
            LifecycleEvent::PaneFocusOut { target } => rename_target(target),
            _ => return,
        }
        if let Some(Target::Pane(target)) = self.queued.current_target.as_mut() {
            rename_target(target);
        }
        if let ScopeSelector::Pane(target) = &mut self.dispatch_scope {
            rename_target(target);
        }
        for (name, value) in &mut self.queued.formats {
            if name == "hook_session_name" && value == old_name.as_str() {
                *value = new_name.to_string();
            }
        }
    }

    fn refresh_window_unlinked_current_target(
        &mut self,
        state: &crate::pane_terminals::HandlerState,
    ) {
        if !matches!(self.queued.event, LifecycleEvent::WindowUnlinked { .. }) {
            return;
        }
        let (current_target, stable_current_target_identity) =
            lifecycle_hook_current_target_snapshot(state, &self.queued.event);
        self.queued.current_target = current_target;
        self.queued.stable_current_target_identity = stable_current_target_identity;
    }

    fn with_hooks(mut self, hooks: Vec<HookDispatch>) -> QueuedLifecycleEvent {
        self.queued.hooks = hooks;
        self.queued
    }
}

fn canonicalize_exact_session_event(
    event: LifecycleEvent,
    session_name: rmux_proto::SessionName,
) -> Option<LifecycleEvent> {
    match event {
        LifecycleEvent::ClientAttached { client_name, .. } => {
            Some(LifecycleEvent::ClientAttached {
                session_name,
                client_name,
            })
        }
        LifecycleEvent::ClientSessionChanged { client_name, .. } => {
            Some(LifecycleEvent::ClientSessionChanged {
                session_name,
                client_name,
            })
        }
        LifecycleEvent::ClientDetached { client_name, .. } => {
            Some(LifecycleEvent::ClientDetached {
                session_name,
                client_name,
            })
        }
        LifecycleEvent::SessionCreated { .. } => {
            Some(LifecycleEvent::SessionCreated { session_name })
        }
        LifecycleEvent::SessionRenamed { .. } => {
            Some(LifecycleEvent::SessionRenamed { session_name })
        }
        LifecycleEvent::SessionWindowChanged { .. } => {
            Some(LifecycleEvent::SessionWindowChanged { session_name })
        }
        _ => None,
    }
}

fn lifecycle_hook_dispatch_scope(
    event: &LifecycleEvent,
    current_target: Option<&Target>,
) -> ScopeSelector {
    if matches!(event, LifecycleEvent::PaneExited { .. }) {
        if let Some(current_target) = current_target {
            return target_to_scope(current_target);
        }
    }
    event.scope()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmux_proto::{
        HookLifecycle, LinkWindowRequest, NewSessionRequest, NewWindowRequest, ScopeSelector,
        SetHookMutationRequest, SplitDirection, SplitWindowRequest, SplitWindowTarget,
        TerminalSize,
    };

    fn session_name(value: &str) -> rmux_proto::SessionName {
        rmux_proto::SessionName::new(value).expect("valid session name")
    }

    #[tokio::test(flavor = "current_thread")]
    async fn hook_broadcast_reports_one_lost_event_at_capacity_plus_one() {
        let handler = RequestHandler::new();
        let mut observed = handler.subscribe_lifecycle_events();
        assert!(
            matches!(
                observed.try_recv(),
                Err(broadcast::error::TryRecvError::Empty)
            ),
            "A0_EMPTY: the new subscriber must start empty"
        );

        // Fixed contract: publish 257 identities without receiving any of them.
        for index in 0_u32..=256 {
            let event = LifecycleEvent::SessionCreated {
                session_name: session_name(&format!("hook-cap-{index}")),
            };
            let queued = handler.prepare_lifecycle_event_for_test(event).await;
            handler
                .hook_events
                .send(queued)
                .expect("A_SEND: the observed subscriber must receive publications");
        }

        let first = observed.try_recv();
        assert!(
            matches!(&first, Err(broadcast::error::TryRecvError::Lagged(1))),
            "A1_LAG: expected exactly Lagged(1), got {first:?}"
        );

        // These expectations are independent of the channel's observed capacity.
        for index in 1_u32..=256 {
            let queued = observed
                .try_recv()
                .expect("A2_IDENTITIES: every retained identity must be available");
            let expected = LifecycleEvent::SessionCreated {
                session_name: session_name(&format!("hook-cap-{index}")),
            };
            assert_eq!(
                queued.event_for_test(),
                &expected,
                "A2_IDENTITIES: retained identity at index {index} must match in order"
            );
        }

        assert!(
            matches!(
                observed.try_recv(),
                Err(broadcast::error::TryRecvError::Empty)
            ),
            "A3_EMPTY: no extra event, and the handler's sender remains open"
        );
        drop(observed);
        drop(handler);
    }

    #[tokio::test]
    async fn prepared_lifecycle_events_publish_in_commit_order_not_waiter_order() {
        let handler = std::sync::Arc::new(RequestHandler::new());
        let first_name = session_name("commit-order-first");
        let second_name = session_name("commit-order-second");
        let (first, second) = {
            let mut state = handler.state.lock().await;
            (
                prepare_lifecycle_event(
                    &mut state,
                    &LifecycleEvent::SessionCreated {
                        session_name: first_name.clone(),
                    },
                ),
                prepare_lifecycle_event(
                    &mut state,
                    &LifecycleEvent::SessionCreated {
                        session_name: second_name.clone(),
                    },
                ),
            )
        };
        let mut observed = handler.subscribe_lifecycle_events();

        let second_handler = std::sync::Arc::clone(&handler);
        let second_task = tokio::spawn(async move {
            second_handler.emit_prepared(second).await;
        });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(30), observed.recv(),)
                .await
                .is_err(),
            "a later commit must not publish while its predecessor is paused"
        );

        let first_handler = std::sync::Arc::clone(&handler);
        let first_task = tokio::spawn(async move {
            first_handler.emit_prepared(first).await;
        });
        let first_observed = observed.recv().await.expect("first committed event");
        let second_observed = observed.recv().await.expect("second committed event");
        assert!(matches!(
            first_observed.event,
            LifecycleEvent::SessionCreated { session_name } if session_name == first_name
        ));
        assert!(matches!(
            second_observed.event,
            LifecycleEvent::SessionCreated { session_name } if session_name == second_name
        ));
        first_task.await.expect("first publisher");
        second_task.await.expect("second publisher");
    }

    #[tokio::test]
    async fn pane_output_hooks_capture_stable_pane_target_identity() {
        let handler = RequestHandler::new();
        let session = session_name("hook-stable-pane-output");
        let response = handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await;
        crate::session_fixture::assert_new_session(&response);

        let target = PaneTarget::with_window(session, 0, 0);
        let mut state = handler.state.lock().await;
        for event in [
            LifecycleEvent::PaneTitleChanged {
                target: target.clone(),
            },
            LifecycleEvent::PaneSetClipboard {
                target: target.clone(),
            },
        ] {
            let queued = prepare_lifecycle_event(&mut state, &event);
            let identity = queued
                .stable_current_target_identity
                .expect("pane output hook captures a stable target identity");
            assert_eq!(identity.resolve(&state), Some(Target::Pane(target.clone())));
        }
    }

    #[tokio::test]
    async fn stable_alert_hook_chain_survives_removing_a_duplicate_alias() {
        let handler = RequestHandler::new();
        let session = session_name("hook-stable-duplicate-alias");
        let response = handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await;
        crate::session_fixture::assert_new_session(&response);

        let original = WindowTarget::with_window(session.clone(), 0);
        let duplicate = WindowTarget::with_window(session.clone(), 1);
        let response = handler
            .handle(Request::LinkWindow(LinkWindowRequest {
                source: original.clone(),
                target: duplicate.clone(),
                after: false,
                before: false,
                kill_destination: false,
                detached: true,
            }))
            .await;
        assert!(matches!(response, Response::LinkWindow(_)), "{response:?}");

        for (command, append) in [
            (format!("unlink-window -t {duplicate}"), false),
            (
                "set-buffer -b stable-duplicate-alias continued".to_owned(),
                true,
            ),
        ] {
            let response = handler
                .handle(Request::SetHookMutation(SetHookMutationRequest {
                    scope: ScopeSelector::Window(original.clone()),
                    hook: HookName::AlertActivity,
                    command: Some(command),
                    lifecycle: HookLifecycle::Persistent,
                    append,
                    unset: false,
                    run_immediately: false,
                    index: None,
                }))
                .await;
            assert!(matches!(response, Response::SetHook(_)), "{response:?}");
        }

        let (queued, original_window_id) = {
            let mut state = handler.state.lock().await;
            let original_window_id = state
                .sessions
                .session(&session)
                .and_then(|session| session.window_at(original.window_index()))
                .expect("original alias exists before dispatch")
                .id();
            (
                prepare_lifecycle_event(
                    &mut state,
                    &LifecycleEvent::AlertActivity {
                        target: original.clone(),
                    },
                ),
                original_window_id,
            )
        };
        assert_eq!(
            queued.hooks.len(),
            2,
            "both hooks are frozen before dispatch"
        );

        handler.dispatch_lifecycle_hook(queued).await;

        let state = handler.state.lock().await;
        let surviving = state
            .sessions
            .session(&session)
            .and_then(|session| session.window_at(original.window_index()))
            .expect("original alias survives the first hook");
        assert_eq!(surviving.id(), original_window_id);
        assert!(
            state
                .sessions
                .session(&session)
                .and_then(|session| session.window_at(duplicate.window_index()))
                .is_none(),
            "the first hook removes only the duplicate alias"
        );
        assert_eq!(
            state
                .buffers
                .show(Some("stable-duplicate-alias"))
                .expect("second hook stores its sentinel")
                .1,
            b"continued"
        );
    }

    #[tokio::test]
    async fn pane_exit_hook_current_target_skips_exiting_active_pane() {
        let handler = RequestHandler::new();
        let session = session_name("hook-current-target");
        let response = handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await;
        crate::session_fixture::assert_new_session(&response);
        let response = handler
            .handle(Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Session(session.clone()),
                direction: SplitDirection::Vertical,
                before: false,
                environment: None,
            }))
            .await;
        assert!(matches!(response, Response::SplitWindow(_)));

        let exiting = PaneTarget::with_window(session.clone(), 0, 1);
        let event = LifecycleEvent::PaneExited {
            target: exiting,
            pane_id: Some(1),
            window_id: Some(1),
            window_name: Some("hook-current-target".to_owned()),
        };
        let state = handler.state.lock().await;

        assert_eq!(
            lifecycle_hook_current_target(&state, &event),
            Some(Target::Pane(PaneTarget::with_window(session, 0, 0)))
        );
    }

    #[tokio::test]
    async fn pane_exit_hook_current_target_falls_back_when_window_closed() {
        let handler = RequestHandler::new();
        let session = session_name("hook-closed-window");
        let response = handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await;
        crate::session_fixture::assert_new_session(&response);

        let event = LifecycleEvent::PaneExited {
            target: PaneTarget::with_window(session.clone(), 1, 0),
            pane_id: Some(2),
            window_id: Some(2),
            window_name: Some("gone".to_owned()),
        };
        let state = handler.state.lock().await;

        assert_eq!(
            lifecycle_hook_current_target(&state, &event),
            Some(Target::Pane(PaneTarget::with_window(session, 0, 0)))
        );
    }

    #[tokio::test]
    async fn pane_exit_hook_current_target_falls_back_when_last_pane_closes_window() {
        let handler = RequestHandler::new();
        let session = session_name("hook-last-pane-window");
        let response = handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await;
        crate::session_fixture::assert_new_session(&response);
        let response = handler
            .handle(Request::NewWindow(Box::new(NewWindowRequest {
                target: session.clone(),
                name: None,
                detached: false,
                start_directory: None,
                environment: None,
                command: None,
                process_command: None,
                target_window_index: None,
                insert_at_target: false,
            })))
            .await;
        assert!(matches!(response, Response::NewWindow(_)));

        let event = LifecycleEvent::PaneExited {
            target: PaneTarget::with_window(session.clone(), 1, 0),
            pane_id: Some(2),
            window_id: Some(2),
            window_name: Some("gone".to_owned()),
        };
        let state = handler.state.lock().await;

        assert_eq!(
            lifecycle_hook_current_target(&state, &event),
            Some(Target::Pane(PaneTarget::with_window(session, 0, 0)))
        );
    }

    #[tokio::test]
    async fn pane_died_hook_keeps_dead_pane_as_current_target() {
        let handler = RequestHandler::new();
        let session = session_name("hook-pane-died-target");
        let response = handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await;
        crate::session_fixture::assert_new_session(&response);
        let response = handler
            .handle(Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Session(session.clone()),
                direction: SplitDirection::Vertical,
                before: false,
                environment: None,
            }))
            .await;
        assert!(matches!(response, Response::SplitWindow(_)));

        let dead = PaneTarget::with_window(session.clone(), 0, 1);
        let event = LifecycleEvent::PaneDied {
            target: dead.clone(),
            pane_id: Some(1),
            window_id: Some(1),
            window_name: Some("hook-pane-died-target".to_owned()),
        };
        let mut state = handler.state.lock().await;
        state
            .hooks
            .set(
                ScopeSelector::Pane(dead.clone()),
                HookName::PaneDied,
                "set-option -g @pd yes".to_owned(),
                HookLifecycle::Persistent,
            )
            .expect("pane hook set succeeds");

        let queued = prepare_lifecycle_event(&mut state, &event);

        assert_eq!(queued.current_target, Some(Target::Pane(dead)));
        assert_eq!(queued.hooks.len(), 1);
        assert!(queued
            .formats
            .iter()
            .any(|(name, value)| name == "hook_pane" && value == "%1"));
        assert!(queued
            .formats
            .iter()
            .any(|(name, value)| name == "pane_index" && value == "1"));
    }

    #[tokio::test]
    async fn pane_exit_dispatches_pane_hook_from_surviving_current_target() {
        let handler = RequestHandler::new();
        let session = session_name("hook-pane-scope");
        let response = handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await;
        crate::session_fixture::assert_new_session(&response);
        let response = handler
            .handle(Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Session(session.clone()),
                direction: SplitDirection::Vertical,
                before: false,
                environment: None,
            }))
            .await;
        assert!(matches!(response, Response::SplitWindow(_)));

        let survivor = PaneTarget::with_window(session.clone(), 0, 0);
        let exiting = PaneTarget::with_window(session.clone(), 0, 1);
        let event = LifecycleEvent::PaneExited {
            target: exiting,
            pane_id: Some(2),
            window_id: Some(1),
            window_name: Some("hook-pane-scope".to_owned()),
        };
        let mut state = handler.state.lock().await;
        state
            .hooks
            .set(
                ScopeSelector::Pane(survivor.clone()),
                HookName::PaneExited,
                "set-option -g @pt0 yes".to_owned(),
                HookLifecycle::Persistent,
            )
            .expect("pane hook set succeeds");

        let queued = prepare_lifecycle_event(&mut state, &event);

        assert_eq!(queued.current_target, Some(Target::Pane(survivor)));
        assert_eq!(queued.hooks.len(), 1);
        assert_eq!(queued.hooks[0].command(), "set-option -g @pt0 yes");
    }
}
