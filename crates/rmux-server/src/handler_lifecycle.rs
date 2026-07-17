use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;

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
    hooks_disabled, queue_inline_hook, with_hook_execution, ExactPaneHookTarget, PendingInlineHook,
    PendingInlineHookFormat,
};

use super::{
    active_session_target, active_window_target, fallback_current_target,
    target_for_request_response, target_to_scope, RequestHandler,
};

#[path = "handler_lifecycle/ordered_wait.rs"]
mod ordered_wait;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct QueuedLifecycleEvent {
    pub(in crate::handler) event: LifecycleEvent,
    pub(in crate::handler) control_session_identity: Option<SessionId>,
    pub(in crate::handler) hook_name: HookName,
    pub(in crate::handler) hooks: Vec<HookDispatch>,
    pub(in crate::handler) formats: Vec<(String, String)>,
    pub(in crate::handler) current_target: Option<Target>,
    stable_current_target_identity: Option<StableLifecycleTargetIdentity>,
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

impl StableLifecycleTargetIdentity {
    fn capture_for_event(
        state: &crate::pane_terminals::HandlerState,
        target: &Target,
        event: &LifecycleEvent,
    ) -> Option<Self> {
        let window_loss = if matches!(event, LifecycleEvent::WindowUnlinked { .. }) {
            StableWindowLoss::RequireWindowIdentity
        } else {
            StableWindowLoss::FallbackToSession
        };
        Self::capture_with_window_loss(state, target, window_loss)
    }

    fn capture_with_window_loss(
        state: &crate::pane_terminals::HandlerState,
        target: &Target,
        window_loss: StableWindowLoss,
    ) -> Option<Self> {
        let session = state.sessions.session(target.session_name())?;
        let session_id = session.id();
        match target {
            Target::Session(_) => Some(Self::Session { session_id }),
            Target::Window(target) => Some(Self::Window {
                session_id,
                window: stable_window_identity(
                    state,
                    target.session_name(),
                    session,
                    target.window_index(),
                    window_loss,
                )?,
                window_loss,
            }),
            Target::Pane(target) => {
                let window = session.window_at(target.window_index())?;
                let pane_id = window.pane(target.pane_index())?.id();
                Some(Self::Pane {
                    session_id,
                    window: stable_window_identity(
                        state,
                        target.session_name(),
                        session,
                        target.window_index(),
                        window_loss,
                    )?,
                    pane_id,
                    window_loss,
                })
            }
        }
    }

    fn resolve(&self, state: &crate::pane_terminals::HandlerState) -> Option<Target> {
        match self {
            Self::Session { session_id } => resolve_stable_session_target(state, *session_id),
            Self::Window {
                session_id,
                window,
                window_loss,
            } => resolve_stable_window_target(state, *session_id, *window)
                .map(Target::Window)
                .or_else(|| resolve_stable_window_loss(state, *session_id, *window, *window_loss)),
            Self::Pane {
                session_id,
                window,
                pane_id,
                window_loss,
            } => resolve_stable_pane_target(state, *session_id, *window, *pane_id)
                .map(Target::Pane)
                .or_else(|| {
                    resolve_stable_window_target(state, *session_id, *window).map(Target::Window)
                })
                .or_else(|| resolve_stable_window_loss(state, *session_id, *window, *window_loss)),
            Self::GlobalWindow { window_id } => {
                resolve_global_window_target(state, *window_id).map(Target::Window)
            }
            Self::GlobalPane { pane_id, window_id } => resolve_global_pane_target(state, *pane_id)
                .map(Target::Pane)
                .or_else(|| {
                    window_id.and_then(|window_id| {
                        resolve_global_window_target(state, window_id).map(Target::Window)
                    })
                }),
            Self::Removed => None,
        }
    }

    fn capture_removed_event(event: &LifecycleEvent) -> Option<Self> {
        match event {
            LifecycleEvent::SessionClosed { session_id, .. } => Some(match session_id {
                Some(session_id) => Self::Session {
                    session_id: SessionId::new(*session_id),
                },
                None => Self::Removed,
            }),
            LifecycleEvent::WindowUnlinked { window_id, .. } => Some(match window_id {
                Some(window_id) => Self::GlobalWindow {
                    window_id: WindowId::new(*window_id),
                },
                None => Self::Removed,
            }),
            LifecycleEvent::PaneExited {
                pane_id, window_id, ..
            }
            | LifecycleEvent::PaneDied {
                pane_id, window_id, ..
            } => Some(match (pane_id, window_id) {
                (Some(pane_id), window_id) => Self::GlobalPane {
                    pane_id: PaneId::new(*pane_id),
                    window_id: window_id.map(WindowId::new),
                },
                (None, Some(window_id)) => Self::GlobalWindow {
                    window_id: WindowId::new(*window_id),
                },
                (None, None) => Self::Removed,
            }),
            _ => None,
        }
    }
}

fn exact_pane_hook_target(
    state: &crate::pane_terminals::HandlerState,
    identity: ExactPaneHookTarget,
) -> Option<Target> {
    let target = if let Some(occurrence_id) = identity.window_occurrence_id {
        state.window_link_occurrence_target(
            occurrence_id,
            identity.session_id,
            identity.window_id,
        )?
    } else {
        let (session_name, session) = state
            .sessions
            .iter()
            .find(|(_, session)| session.id() == identity.session_id)?;
        let (window_index, _) = session
            .windows()
            .iter()
            .filter(|(_, window)| window.id() == identity.window_id)
            .min_by_key(|(window_index, _)| {
                (
                    **window_index != identity.preferred_window_index,
                    **window_index,
                )
            })?;
        WindowTarget::with_window(session_name.clone(), *window_index)
    };
    let session = state.sessions.session(target.session_name())?;
    let window = session.window_at(target.window_index())?;
    window
        .panes()
        .iter()
        .filter(|pane| pane.id() == identity.pane_id)
        .min_by_key(|pane| pane.index())
        .map(|pane| {
            Target::Pane(PaneTarget::with_window(
                target.session_name().clone(),
                target.window_index(),
                pane.index(),
            ))
        })
}

fn resolve_stable_window_loss(
    state: &crate::pane_terminals::HandlerState,
    session_id: SessionId,
    window: StableWindowIdentity,
    window_loss: StableWindowLoss,
) -> Option<Target> {
    match window_loss {
        StableWindowLoss::FallbackToSession => {
            resolve_stable_session_parent_target(state, session_id, window)
        }
        StableWindowLoss::RequireWindowIdentity => None,
    }
}

fn resolve_stable_session_parent_target(
    state: &crate::pane_terminals::HandlerState,
    session_id: SessionId,
    child_window: StableWindowIdentity,
) -> Option<Target> {
    if stable_window_slot_was_replaced(state, session_id, child_window) {
        return None;
    }
    resolve_stable_session_target(state, session_id)
}

fn stable_window_slot_was_replaced(
    state: &crate::pane_terminals::HandlerState,
    session_id: SessionId,
    identity: StableWindowIdentity,
) -> bool {
    let Some((session_name, session)) = state
        .sessions
        .iter()
        .find(|(_, session)| session.id() == session_id)
    else {
        return false;
    };
    session
        .window_at(identity.preferred_index)
        .is_some_and(|window| {
            window.id() != identity.window_id
                || identity.occurrence_id.is_some_and(|occurrence_id| {
                    state.window_link_occurrence_id(session_name, identity.preferred_index)
                        != Some(occurrence_id)
                })
        })
}

fn resolve_stable_session_target(
    state: &crate::pane_terminals::HandlerState,
    session_id: SessionId,
) -> Option<Target> {
    state
        .sessions
        .iter()
        .find(|(_, session)| session.id() == session_id)
        .map(|(session_name, _)| Target::Session(session_name.clone()))
}

fn resolve_global_window_target(
    state: &crate::pane_terminals::HandlerState,
    window_id: WindowId,
) -> Option<WindowTarget> {
    state
        .sessions
        .iter()
        .filter_map(|(session_name, session)| {
            first_window_target_with_id(session_name, session, window_id)
                .map(|target| (session.id(), target))
        })
        .max_by_key(|(session_id, _)| *session_id)
        .map(|(_, target)| target)
}

fn resolve_global_pane_target(
    state: &crate::pane_terminals::HandlerState,
    pane_id: PaneId,
) -> Option<PaneTarget> {
    state
        .sessions
        .iter()
        .filter_map(|(session_name, session)| {
            first_pane_target_with_id(session_name, session, pane_id)
                .map(|target| (session.id(), target))
        })
        .max_by_key(|(session_id, _)| *session_id)
        .map(|(_, target)| target)
}

fn first_window_target_with_id(
    session_name: &rmux_proto::SessionName,
    session: &rmux_core::Session,
    window_id: WindowId,
) -> Option<WindowTarget> {
    session
        .windows()
        .iter()
        .find(|(_, window)| window.id() == window_id)
        .map(|(&window_index, _)| WindowTarget::with_window(session_name.clone(), window_index))
}

fn first_pane_target_with_id(
    session_name: &rmux_proto::SessionName,
    session: &rmux_core::Session,
    pane_id: PaneId,
) -> Option<PaneTarget> {
    session
        .windows()
        .iter()
        .find_map(|(&window_index, window)| {
            window
                .panes()
                .iter()
                .filter(|pane| pane.id() == pane_id)
                .min_by_key(|pane| pane.index())
                .map(|pane| {
                    PaneTarget::with_window(session_name.clone(), window_index, pane.index())
                })
        })
}

fn resolve_stable_window_target(
    state: &crate::pane_terminals::HandlerState,
    session_id: SessionId,
    identity: StableWindowIdentity,
) -> Option<WindowTarget> {
    if let Some(occurrence_id) = identity.occurrence_id {
        return state.window_link_occurrence_target(occurrence_id, session_id, identity.window_id);
    }
    let original_session = state
        .sessions
        .iter()
        .find(|(_, session)| session.id() == session_id);
    if let Some((session_name, session)) = original_session {
        if let Some((window_index, _)) = resolve_stable_window_identity(session, identity) {
            return Some(WindowTarget::with_window(
                session_name.clone(),
                window_index,
            ));
        }
    }

    resolve_global_window_target(state, identity.window_id)
}

fn resolve_stable_pane_target(
    state: &crate::pane_terminals::HandlerState,
    session_id: SessionId,
    window_identity: StableWindowIdentity,
    pane_id: PaneId,
) -> Option<PaneTarget> {
    if window_identity.occurrence_id.is_some() {
        let window_target = resolve_stable_window_target(state, session_id, window_identity)?;
        let pane_index = state
            .sessions
            .session(window_target.session_name())?
            .window_at(window_target.window_index())?
            .panes()
            .iter()
            .find(|pane| pane.id() == pane_id)?
            .index();
        return Some(PaneTarget::with_window(
            window_target.session_name().clone(),
            window_target.window_index(),
            pane_index,
        ));
    }

    let pane_in_session = |session_name: &rmux_proto::SessionName,
                           session: &rmux_core::Session|
     -> Option<PaneTarget> {
        let (window_index, window) = resolve_stable_window_identity(session, window_identity)?;
        let pane_index = window
            .panes()
            .iter()
            .find(|pane| pane.id() == pane_id)?
            .index();
        Some(PaneTarget::with_window(
            session_name.clone(),
            window_index,
            pane_index,
        ))
    };

    if let Some(target) = state
        .sessions
        .iter()
        .find(|(_, session)| session.id() == session_id)
        .and_then(|(session_name, session)| pane_in_session(session_name, session))
    {
        return Some(target);
    }

    resolve_global_pane_target(state, pane_id)
}

fn stable_window_identity(
    state: &crate::pane_terminals::HandlerState,
    session_name: &rmux_proto::SessionName,
    session: &rmux_core::Session,
    window_index: u32,
    window_loss: StableWindowLoss,
) -> Option<StableWindowIdentity> {
    let window_id = session.window_at(window_index)?.id();
    Some(StableWindowIdentity {
        window_id,
        preferred_index: window_index,
        occurrence_id: if matches!(window_loss, StableWindowLoss::RequireWindowIdentity) {
            state.window_link_occurrence_id(session_name, window_index)
        } else {
            None
        },
    })
}

fn resolve_stable_window_identity(
    session: &rmux_core::Session,
    identity: StableWindowIdentity,
) -> Option<(u32, &rmux_core::Window)> {
    if let Some(window) = session
        .window_at(identity.preferred_index)
        .filter(|window| window.id() == identity.window_id)
    {
        return Some((identity.preferred_index, window));
    }

    session.windows().iter().find_map(|(window_index, window)| {
        (window.id() == identity.window_id).then_some((*window_index, window))
    })
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

impl RequestHandler {
    #[cfg(test)]
    pub(crate) fn subscribe_lifecycle_events(&self) -> broadcast::Receiver<QueuedLifecycleEvent> {
        self.hook_events.subscribe()
    }

    pub(crate) fn take_lifecycle_dispatch_receiver(
        &self,
    ) -> Option<mpsc::Receiver<LifecycleDispatchItem>> {
        self.lifecycle_dispatch.activate()
    }

    pub(crate) async fn consume_lifecycle_hooks(
        &self,
        mut events: mpsc::Receiver<LifecycleDispatchItem>,
        mut shutdown: oneshot::Receiver<()>,
    ) {
        loop {
            tokio::select! {
                result = events.recv() => {
                    match result {
                        Some(item) => self.dispatch_lifecycle_item(item).await,
                        None => break,
                    }
                }
                result = &mut shutdown => {
                    let _ = result;
                    self.lifecycle_dispatch.deactivate();
                    events.close();
                    while let Some(item) = events.recv().await {
                        self.dispatch_lifecycle_item(item).await;
                    }
                    self.shutdown_wait_for();
                    break;
                }
            }
        }
    }

    pub(in crate::handler) async fn emit(&self, event: LifecycleEvent) {
        if let LifecycleEvent::PaneModeChanged { target } = &event {
            self.refresh_automatic_window_name_for_pane_target(target)
                .await;
        }
        if hooks_disabled() {
            self.refresh_control_sessions_for_event(&event).await;
            return;
        }
        let queued = {
            let mut state = self.state.lock().await;
            prepare_lifecycle_event(&mut state, &event)
        };
        self.emit_prepared(queued).await;
    }

    pub(in crate::handler) async fn emit_without_attached_refresh(&self, event: LifecycleEvent) {
        if hooks_disabled() {
            self.refresh_control_sessions_for_event(&event).await;
            return;
        }
        let queued = {
            let mut state = self.state.lock().await;
            prepare_lifecycle_event(&mut state, &event)
        };
        self.emit_prepared(queued).await;
    }

    pub(in crate::handler) async fn emit_prepared(&self, event: QueuedLifecycleEvent) {
        if hooks_disabled() {
            self.refresh_control_sessions_for_event(&event.event).await;
            return;
        }
        let _ = self.hook_events.send(event.clone());
        let item = LifecycleDispatchItem {
            event,
            completion: None,
        };
        if self.lifecycle_dispatch.send_if_active(item).await.is_err() {
            warn!("lifecycle dispatch queue closed before server shutdown");
        }
    }

    pub(in crate::handler) async fn emit_prepared_and_wait(&self, event: QueuedLifecycleEvent) {
        if hooks_disabled() {
            self.refresh_control_sessions_for_event(&event.event).await;
            return;
        }
        let _ = self.hook_events.send(event.clone());
        let (completion_tx, completion_rx) = oneshot::channel();
        let item = LifecycleDispatchItem {
            event,
            completion: Some(completion_tx),
        };
        ordered_wait::dispatch_without_unbounded_caller_wait(
            self.lifecycle_dispatch.clone(),
            item,
            completion_rx,
        )
        .await;
    }

    async fn dispatch_lifecycle_item(&self, item: LifecycleDispatchItem) {
        self.dispatch_lifecycle_hook(item.event).await;
        if let Some(completion) = item.completion {
            let _ = completion.send(());
        }
    }

    pub(crate) fn shutdown_wait_for(&self) {
        if let Ok(mut wait_for) = self.wait_for.lock() {
            wait_for.shutdown();
        }
    }

    pub(crate) async fn emit_client_attached_identity(
        &self,
        requester_pid: u32,
        session_name: rmux_proto::SessionName,
        session_id: SessionId,
    ) {
        self.emit_for_session_identity(
            LifecycleEvent::ClientAttached {
                session_name: session_name.clone(),
                client_name: Some(requester_pid.to_string()),
            },
            &session_name,
            session_id,
        )
        .await;
    }

    pub(in crate::handler) async fn emit_client_session_changed(
        &self,
        requester_pid: u32,
        session_name: rmux_proto::SessionName,
        session_id: SessionId,
    ) {
        self.emit_for_session_identity(
            LifecycleEvent::ClientSessionChanged {
                session_name: session_name.clone(),
                client_name: Some(requester_pid.to_string()),
            },
            &session_name,
            session_id,
        )
        .await;
    }

    pub(in crate::handler) async fn emit_for_session_identity(
        &self,
        event: LifecycleEvent,
        _session_name: &rmux_proto::SessionName,
        session_id: SessionId,
    ) {
        let prepared = {
            let mut state = self.state.lock().await;
            let Some(session_name) = state.sessions.iter().find_map(|(session_name, session)| {
                (session.id() == session_id).then(|| session_name.clone())
            }) else {
                return;
            };
            let Some(event) = canonicalize_exact_session_event(event, session_name) else {
                debug_assert!(
                    false,
                    "exact session emission requires a client session event"
                );
                return;
            };
            if hooks_disabled() {
                drop(state);
                self.refresh_control_sessions_for_event(&event).await;
                return;
            }
            let mut queued = prepare_lifecycle_event(&mut state, &event);
            queued.control_session_identity = Some(session_id);
            queued
        };
        self.emit_prepared(prepared).await;
    }

    pub(in crate::handler) async fn dispatch_lifecycle_hook(&self, event: QueuedLifecycleEvent) {
        self.dispatch_lifecycle_control_effects(&event).await;

        if event.hooks.is_empty() {
            return;
        }
        let current_target = if let Some(identity) = event.stable_current_target_identity.clone() {
            if self.stable_hook_command_target(&identity).await.is_some() {
                HookDispatchCurrentTarget::Stable(identity)
            } else if lifecycle_event_allows_missing_stable_target(&event.event, &identity) {
                HookDispatchCurrentTarget::MissingStable
            } else {
                warn!(
                    hook = ?event.hook_name,
                    "skipping lifecycle hook dispatch because its stable target was replaced or removed"
                );
                return;
            }
        } else {
            HookDispatchCurrentTarget::Dynamic(self.lifecycle_dispatch_current_target(&event).await)
        };

        self.execute_hook_dispatches(
            std::process::id(),
            event.hooks,
            current_target,
            event.formats,
            event.hook_name,
            "lifecycle",
        )
        .await;
    }

    async fn dispatch_lifecycle_control_effects(&self, event: &QueuedLifecycleEvent) {
        self.dispatch_control_notifications(event).await;
        self.refresh_control_sessions_for_event(&event.event).await;
    }

    pub(in crate::handler) fn queue_inline_hook(
        &self,
        hook: HookName,
        scope: ScopeSelector,
        current_target: Option<Target>,
        format_mode: PendingInlineHookFormat,
    ) {
        queue_inline_hook(PendingInlineHook {
            hook,
            scope,
            current_target,
            exact_pane_target: None,
            exact_session_id: None,
            skip_dispatch: false,
            format_mode,
        });
    }

    pub(in crate::handler) fn queue_exact_pane_inline_hook(
        &self,
        hook: HookName,
        target: PaneTarget,
        identity: ExactPaneHookTarget,
        format_mode: PendingInlineHookFormat,
    ) {
        queue_inline_hook(PendingInlineHook {
            hook,
            scope: ScopeSelector::Pane(target.clone()),
            current_target: Some(Target::Pane(target)),
            exact_pane_target: Some(identity),
            exact_session_id: None,
            skip_dispatch: false,
            format_mode,
        });
    }

    pub(in crate::handler) fn queue_exact_session_inline_hook(
        &self,
        hook: HookName,
        session_name: rmux_proto::SessionName,
        session_id: SessionId,
        current_target: Option<Target>,
        format_mode: PendingInlineHookFormat,
    ) {
        queue_inline_hook(PendingInlineHook {
            hook,
            scope: ScopeSelector::Session(session_name),
            current_target,
            exact_pane_target: None,
            exact_session_id: Some(session_id),
            skip_dispatch: false,
            format_mode,
        });
    }

    pub(in crate::handler) fn queue_missing_target_inline_hook(
        &self,
        hook: HookName,
        format_mode: PendingInlineHookFormat,
    ) {
        self.queue_suppressed_inline_hook(hook, format_mode);
    }

    pub(in crate::handler) fn queue_suppressed_inline_hook(
        &self,
        hook: HookName,
        format_mode: PendingInlineHookFormat,
    ) {
        queue_inline_hook(PendingInlineHook {
            hook,
            scope: ScopeSelector::Global,
            current_target: None,
            exact_pane_target: None,
            exact_session_id: None,
            skip_dispatch: true,
            format_mode,
        });
    }

    pub(in crate::handler) async fn run_inline_hooks(
        &self,
        requester_pid: u32,
        inline_hooks: Vec<PendingInlineHook>,
        parsed_command: Option<&ParsedCommand>,
    ) {
        for pending in inline_hooks {
            if pending.skip_dispatch {
                continue;
            }
            let formats = match pending.format_mode {
                PendingInlineHookFormat::HookOnly => hook_only_format_values(pending.hook),
                PendingInlineHookFormat::AfterCommand => {
                    after_hook_format_values(pending.hook, parsed_command)
                }
            };
            let current_target = match (pending.exact_pane_target, pending.exact_session_id) {
                (Some(identity), None) => HookDispatchCurrentTarget::ExactPane(identity),
                (None, Some(session_id)) => HookDispatchCurrentTarget::ExactSession(session_id),
                (None, None) => HookDispatchCurrentTarget::Dynamic(pending.current_target),
                (Some(_), Some(_)) => {
                    unreachable!("inline hook cannot carry pane and session identities")
                }
            };
            self.run_built_in_hook_dispatch_with_target(
                requester_pid,
                pending.hook,
                pending.scope,
                current_target,
                formats,
                "inline",
            )
            .await;
        }
    }

    pub(in crate::handler) async fn run_request_hooks(
        &self,
        requester_pid: u32,
        request: &Request,
        response: &Response,
        parsed_command: Option<&ParsedCommand>,
        suppressed_success_hooks: &[HookName],
    ) {
        if hooks_disabled() {
            return;
        }

        let current_target = self
            .current_target_for_request_response(requester_pid, request, response)
            .await;
        let scope = current_target
            .as_ref()
            .map(target_to_scope)
            .unwrap_or(ScopeSelector::Global);

        if matches!(response, Response::Error(_)) {
            self.run_built_in_hook_dispatch(
                requester_pid,
                HookName::CommandError,
                scope,
                current_target,
                after_hook_format_values(HookName::CommandError, parsed_command),
                "command-error",
            )
            .await;
            return;
        }

        let hook_name = format!("after-{}", request.command_name());
        let Some(hook) = HookName::from_str(&hook_name) else {
            return;
        };
        if suppressed_success_hooks.contains(&hook) {
            return;
        }
        self.run_built_in_hook_dispatch(
            requester_pid,
            hook,
            scope,
            current_target,
            after_hook_format_values(hook, parsed_command),
            "after",
        )
        .await;
    }

    pub(in crate::handler) async fn run_command_error_hook_for_parsed_command(
        &self,
        requester_pid: u32,
        command: &ParsedCommand,
        current_target: Option<Target>,
        attached_session: Option<&rmux_proto::SessionName>,
    ) {
        if hooks_disabled() {
            return;
        }

        let current_target = if current_target.is_some() {
            current_target
        } else {
            let state = self.state.lock().await;
            fallback_current_target(&state, attached_session)
        };
        let scope = current_target
            .as_ref()
            .map(target_to_scope)
            .unwrap_or(ScopeSelector::Global);
        self.run_built_in_hook_dispatch(
            requester_pid,
            HookName::CommandError,
            scope,
            current_target,
            after_hook_format_values(HookName::CommandError, Some(command)),
            "command-error",
        )
        .await;
    }

    async fn run_built_in_hook_dispatch(
        &self,
        requester_pid: u32,
        hook_name: HookName,
        scope: ScopeSelector,
        current_target: Option<Target>,
        formats: Vec<(String, String)>,
        source: &'static str,
    ) {
        self.run_built_in_hook_dispatch_with_target(
            requester_pid,
            hook_name,
            scope,
            HookDispatchCurrentTarget::Dynamic(current_target),
            formats,
            source,
        )
        .await;
    }

    async fn run_built_in_hook_dispatch_with_target(
        &self,
        requester_pid: u32,
        hook_name: HookName,
        scope: ScopeSelector,
        current_target: HookDispatchCurrentTarget,
        formats: Vec<(String, String)>,
        source: &'static str,
    ) {
        if hooks_disabled() {
            return;
        }

        let hooks = {
            let mut state = self.state.lock().await;
            let scope = match &current_target {
                HookDispatchCurrentTarget::ExactPane(identity) => {
                    let Some(target) = exact_pane_hook_target(&state, *identity) else {
                        return;
                    };
                    target_to_scope(&target)
                }
                HookDispatchCurrentTarget::ExactSession(session_id) => {
                    let Some(target) = resolve_stable_session_target(&state, *session_id) else {
                        return;
                    };
                    target_to_scope(&target)
                }
                _ => scope,
            };
            match super::resolve_hook_scope_identity(&state, &scope) {
                Ok(identity) => state
                    .hooks
                    .dispatch_with_identity_or_scope(&identity, &scope, hook_name),
                Err(_) => state.hooks.dispatch(&scope, hook_name),
            }
        };
        if hooks.is_empty() {
            return;
        }

        self.execute_hook_dispatches(
            requester_pid,
            hooks,
            current_target,
            formats,
            hook_name,
            source,
        )
        .await;
    }

    fn execute_hook_dispatches(
        &self,
        requester_pid: u32,
        hooks: Vec<HookDispatch>,
        current_target: HookDispatchCurrentTarget,
        formats: Vec<(String, String)>,
        hook_name: HookName,
        source: &'static str,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            with_hook_execution(formats, async {
                for hook in hooks {
                    let command_current_target = match &current_target {
                        HookDispatchCurrentTarget::Stable(identity) => {
                            let Some(target) = self.stable_hook_command_target(identity).await
                            else {
                                warn!(
                                    hook = ?hook_name,
                                    source,
                                    "stopping lifecycle hook dispatch because its stable target no longer exists"
                                );
                                break;
                            };
                            Some(target)
                        }
                        HookDispatchCurrentTarget::ExactPane(identity) => {
                            let state = self.state.lock().await;
                            let Some(target) = exact_pane_hook_target(&state, *identity) else {
                                warn!(
                                    hook = ?hook_name,
                                    source,
                                    "stopping inline hook dispatch because its exact pane target no longer exists"
                                );
                                break;
                            };
                            Some(target)
                        }
                        HookDispatchCurrentTarget::ExactSession(session_id) => {
                            let identity = StableLifecycleTargetIdentity::Session {
                                session_id: *session_id,
                            };
                            let Some(target) = self.stable_hook_command_target(&identity).await
                            else {
                                warn!(
                                    hook = ?hook_name,
                                    source,
                                    "stopping inline hook dispatch because its exact session target no longer exists"
                                );
                                break;
                            };
                            Some(target)
                        }
                        HookDispatchCurrentTarget::Dynamic(target) => {
                            self.valid_hook_command_target(target.clone()).await
                        }
                        HookDispatchCurrentTarget::MissingStable => None,
                    };
                    if let Err(error) = self
                        .execute_hook_command_with_context(
                            requester_pid,
                            hook.command(),
                            command_current_target,
                        )
                        .await
                    {
                        warn!(hook = ?hook_name, source, "failed to execute hook command: {error}");
                    }
                }
            })
            .await;
        })
    }

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
    let deferred = defer_lifecycle_event(state, event);
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
    deferred.with_hooks(hooks)
}

pub(in crate::handler) fn prepare_lifecycle_event_if_enabled(
    state: &mut crate::pane_terminals::HandlerState,
    event: &LifecycleEvent,
) -> Option<QueuedLifecycleEvent> {
    if hooks_disabled() {
        return None;
    }
    Some(prepare_lifecycle_event(state, event))
}

pub(in crate::handler) fn defer_lifecycle_event(
    state: &crate::pane_terminals::HandlerState,
    event: &LifecycleEvent,
) -> DeferredLifecycleEvent {
    let hook_name = event.hook_name();
    let (current_target, stable_current_target_identity) =
        lifecycle_hook_current_target_snapshot(state, event);
    let dispatch_scope = lifecycle_hook_dispatch_scope(event, current_target.as_ref());
    let dispatch_identity = super::lifecycle_hook_scope_identity(state, &dispatch_scope, event);
    DeferredLifecycleEvent {
        queued: QueuedLifecycleEvent {
            event: event.clone(),
            control_session_identity: None,
            hook_name,
            hooks: Vec::new(),
            formats: lifecycle_hook_formats(state, event),
            current_target,
            stable_current_target_identity,
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
    if hooks_disabled() {
        return deferred.queued;
    }
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
    deferred.with_hooks(hooks)
}

impl DeferredLifecycleEvent {
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

fn hook_only_format_values(hook: HookName) -> Vec<(String, String)> {
    vec![("hook".to_owned(), hook.to_string())]
}

pub(in crate::handler) fn after_hook_format_values(
    hook: HookName,
    parsed_command: Option<&ParsedCommand>,
) -> Vec<(String, String)> {
    let mut formats = hook_only_format_values(hook);
    let Some(parsed_command) = parsed_command else {
        return formats;
    };

    let arguments = parsed_command
        .arguments()
        .iter()
        .map(CommandArgument::to_tmux_string)
        .collect::<Vec<_>>();
    formats.push(("hook_arguments".to_owned(), arguments.join(" ")));
    for (index, argument) in arguments.iter().enumerate() {
        formats.push((format!("hook_argument_{index}"), argument.clone()));
    }

    let scalar_arguments = parsed_command
        .arguments()
        .iter()
        .filter_map(CommandArgument::as_string)
        .collect::<Vec<_>>();
    let mut flag_values = BTreeMap::<char, Vec<String>>::new();
    let mut index = 0;
    while index < scalar_arguments.len() {
        let token = scalar_arguments[index];
        if token == "--" {
            break;
        }
        let Some(flags) = token.strip_prefix('-') else {
            index += 1;
            continue;
        };
        if flags.is_empty()
            || token.starts_with("--")
            || !flags.chars().all(|flag| flag.is_ascii_alphabetic())
        {
            index += 1;
            continue;
        }

        if flags.len() == 1 {
            let flag = flags.chars().next().expect("single-char flag");
            if let Some(value) = scalar_arguments.get(index + 1).copied() {
                if !value.starts_with('-') {
                    flag_values.entry(flag).or_default().push(value.to_owned());
                    index += 2;
                    continue;
                }
            }
        }

        for flag in flags.chars() {
            let _ = flag_values.entry(flag).or_default();
        }
        index += 1;
    }

    for (flag, values) in flag_values {
        if let Some(value) = values.last() {
            formats.push((format!("hook_flag_{flag}"), value.clone()));
            for (index, value) in values.into_iter().enumerate() {
                formats.push((format!("hook_flag_{flag}_{index}"), value));
            }
        } else {
            formats.push((format!("hook_flag_{flag}"), "1".to_owned()));
        }
    }

    formats
}

fn lifecycle_hook_formats(
    state: &crate::pane_terminals::HandlerState,
    event: &LifecycleEvent,
) -> Vec<(String, String)> {
    let mut formats = hook_only_format_values(event.hook_name());
    if let Some(client_name) = event.client_name() {
        formats.push(("hook_client".to_owned(), client_name.to_owned()));
    }
    if let Some(session_name) = event.session_name() {
        if let Some(session) = state.sessions.session(session_name) {
            formats.push(("hook_session".to_owned(), session.id().to_string()));
            formats.push(("hook_session_name".to_owned(), session.name().to_string()));
        } else {
            if let Some(session_id) = event.session_id() {
                formats.push(("hook_session".to_owned(), format!("${session_id}")));
            }
            formats.push(("hook_session_name".to_owned(), session_name.to_string()));
        }
    }
    if let LifecycleEvent::WindowUnlinked {
        window_id,
        window_name,
        ..
    } = event
    {
        if let Some(window_id) = window_id {
            formats.push(("hook_window".to_owned(), format!("@{window_id}")));
        }
        if let Some(window_name) = window_name {
            formats.push(("hook_window_name".to_owned(), window_name.clone()));
        }
    } else if let Some(window_target) = event.window_target() {
        let mut resolved_window = false;
        if let Some(session) = state.sessions.session(window_target.session_name()) {
            if let Some(window) = session.window_at(window_target.window_index()) {
                formats.push(("hook_window".to_owned(), window.id().to_string()));
                formats.push((
                    "hook_window_name".to_owned(),
                    window.name().unwrap_or_default().to_owned(),
                ));
                resolved_window = true;
            }
        }
        if !resolved_window {
            if let Some(window_id) = event.window_id() {
                formats.push(("hook_window".to_owned(), format!("@{window_id}")));
                if let Some(window_name) = event.window_name_snapshot() {
                    formats.push(("hook_window_name".to_owned(), window_name.to_owned()));
                }
            }
        }
    }
    if let Some(pane_target) = event.pane_target() {
        let mut resolved_pane = false;
        if let Some(session) = state.sessions.session(pane_target.session_name()) {
            if let Some(window) = session.window_at(pane_target.window_index()) {
                if let Some(pane) = window.pane(pane_target.pane_index()) {
                    formats.push(("hook_pane".to_owned(), format!("%{}", pane.id().as_u32())));
                    resolved_pane = true;
                }
            }
        }
        if !resolved_pane {
            if let Some(pane_id) = event.pane_id() {
                formats.push(("hook_pane".to_owned(), format!("%{pane_id}")));
            }
        }
    }
    if matches!(
        event,
        LifecycleEvent::PaneExited { .. } | LifecycleEvent::PaneDied { .. }
    ) {
        match lifecycle_hook_current_target(state, event) {
            Some(Target::Pane(target)) => append_pane_format_values(state, &target, &mut formats),
            _ => {
                if let Some(session_name) = event.session_name() {
                    if let Some(Target::Pane(target)) =
                        first_session_pane_target(state, session_name)
                    {
                        append_pane_format_values(state, &target, &mut formats);
                    }
                }
            }
        }
    }
    formats
}

fn append_pane_format_values(
    state: &crate::pane_terminals::HandlerState,
    target: &PaneTarget,
    formats: &mut Vec<(String, String)>,
) {
    let Some(session) = state.sessions.session(target.session_name()) else {
        return;
    };
    let Some(window) = session.window_at(target.window_index()) else {
        return;
    };
    let Some(pane) = window.pane(target.pane_index()) else {
        return;
    };
    formats.push(("window_index".to_owned(), target.window_index().to_string()));
    formats.push(("pane_index".to_owned(), target.pane_index().to_string()));
    formats.push(("pane_id".to_owned(), pane.id().to_string()));
}

fn lifecycle_hook_current_target(
    state: &crate::pane_terminals::HandlerState,
    event: &LifecycleEvent,
) -> Option<Target> {
    if let LifecycleEvent::WindowUnlinked {
        session_name,
        window_id,
        ..
    } = event
    {
        return window_id
            .map(WindowId::new)
            .and_then(|window_id| resolve_global_window_target(state, window_id))
            .and_then(|target| active_window_target(&state.sessions, &target))
            .or_else(|| active_session_target(&state.sessions, session_name));
    }

    match event.current_target() {
        Some(Target::Session(session_name)) => {
            active_session_target(&state.sessions, &session_name)
        }
        Some(Target::Window(target)) => active_window_target(&state.sessions, &target)
            .or_else(|| active_session_target(&state.sessions, target.session_name())),
        Some(Target::Pane(target)) => {
            if matches!(event, LifecycleEvent::PaneExited { .. }) {
                return surviving_pane_event_target(state, &target);
            }
            let window_target =
                WindowTarget::with_window(target.session_name().clone(), target.window_index());
            let pane_exists = state
                .sessions
                .session(target.session_name())
                .and_then(|session| session.window_at(target.window_index()))
                .and_then(|window| window.pane(target.pane_index()))
                .is_some();
            if pane_exists {
                Some(Target::Pane(target))
            } else {
                active_window_target(&state.sessions, &window_target)
                    .or_else(|| active_session_target(&state.sessions, target.session_name()))
            }
        }
        None => None,
    }
}

fn lifecycle_hook_current_target_snapshot(
    state: &crate::pane_terminals::HandlerState,
    event: &LifecycleEvent,
) -> (Option<Target>, Option<StableLifecycleTargetIdentity>) {
    let current_target = lifecycle_hook_current_target(state, event);
    let stable_current_target_identity = current_target
        .as_ref()
        .and_then(|target| StableLifecycleTargetIdentity::capture_for_event(state, target, event))
        .or_else(|| StableLifecycleTargetIdentity::capture_removed_event(event));
    (current_target, stable_current_target_identity)
}

fn surviving_pane_event_target(
    state: &crate::pane_terminals::HandlerState,
    exiting_target: &PaneTarget,
) -> Option<Target> {
    let session = state.sessions.session(exiting_target.session_name())?;
    let Some(window) = session.window_at(exiting_target.window_index()) else {
        return fallback_session_pane_target_after_exit(state, exiting_target);
    };
    if let Some(active_pane) = window.active_pane() {
        if active_pane.index() != exiting_target.pane_index() {
            return Some(Target::Pane(PaneTarget::with_window(
                exiting_target.session_name().clone(),
                exiting_target.window_index(),
                active_pane.index(),
            )));
        }
    }
    window
        .panes()
        .iter()
        .find(|pane| pane.index() != exiting_target.pane_index())
        .map(|pane| {
            Target::Pane(PaneTarget::with_window(
                exiting_target.session_name().clone(),
                exiting_target.window_index(),
                pane.index(),
            ))
        })
        .or_else(|| fallback_session_pane_target_after_exit(state, exiting_target))
}

fn fallback_session_pane_target_after_exit(
    state: &crate::pane_terminals::HandlerState,
    exiting_target: &PaneTarget,
) -> Option<Target> {
    if let Some(Target::Pane(target)) =
        active_session_target(&state.sessions, exiting_target.session_name())
    {
        if &target != exiting_target {
            return Some(Target::Pane(target));
        }
    }

    let session = state.sessions.session(exiting_target.session_name())?;
    session.windows().iter().find_map(|(window_index, window)| {
        let target_for_pane = |pane_index| {
            Target::Pane(PaneTarget::with_window(
                exiting_target.session_name().clone(),
                *window_index,
                pane_index,
            ))
        };
        if let Some(active_pane) = window.active_pane() {
            if *window_index != exiting_target.window_index()
                || active_pane.index() != exiting_target.pane_index()
            {
                return Some(target_for_pane(active_pane.index()));
            }
        }
        window.panes().iter().find_map(|pane| {
            (*window_index != exiting_target.window_index()
                || pane.index() != exiting_target.pane_index())
            .then(|| target_for_pane(pane.index()))
        })
    })
}

fn first_session_pane_target(
    state: &crate::pane_terminals::HandlerState,
    session_name: &rmux_proto::SessionName,
) -> Option<Target> {
    let session = state.sessions.session(session_name)?;
    session.windows().iter().find_map(|(window_index, window)| {
        window
            .active_pane()
            .or_else(|| window.panes().first())
            .map(|pane| {
                Target::Pane(PaneTarget::with_window(
                    session_name.clone(),
                    *window_index,
                    pane.index(),
                ))
            })
    })
}

fn hook_format_session_target(
    state: &crate::pane_terminals::HandlerState,
    formats: &[(String, String)],
) -> Option<Target> {
    let session_name = formats
        .iter()
        .rev()
        .find(|(name, _)| name == "hook_session_name")
        .and_then(|(_, value)| rmux_proto::SessionName::new(value.clone()).ok())?;
    first_session_pane_target(state, &session_name)
}

fn target_exists(state: &crate::pane_terminals::HandlerState, target: &Target) -> bool {
    match target {
        Target::Session(session_name) => state.sessions.session(session_name).is_some(),
        Target::Window(target) => state
            .sessions
            .session(target.session_name())
            .and_then(|session| session.window_at(target.window_index()))
            .is_some(),
        Target::Pane(target) => state
            .sessions
            .session(target.session_name())
            .and_then(|session| session.window_at(target.window_index()))
            .and_then(|window| window.pane(target.pane_index()))
            .is_some(),
    }
}

fn pane_target_for_existing_target(
    state: &crate::pane_terminals::HandlerState,
    target: &Target,
) -> Option<Target> {
    match target {
        Target::Pane(target) => Some(Target::Pane(target.clone())),
        Target::Window(target) => {
            let session = state.sessions.session(target.session_name())?;
            let window = session.window_at(target.window_index())?;
            let pane = window.active_pane().or_else(|| window.panes().first())?;
            Some(Target::Pane(PaneTarget::with_window(
                target.session_name().clone(),
                target.window_index(),
                pane.index(),
            )))
        }
        Target::Session(session_name) => first_session_pane_target(state, session_name),
    }
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
        assert!(matches!(response, Response::NewSession(_)));

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
        assert!(matches!(response, Response::NewSession(_)));

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
        assert!(matches!(response, Response::NewSession(_)));
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
        assert!(matches!(response, Response::NewSession(_)));

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
        assert!(matches!(response, Response::NewSession(_)));
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
        assert!(matches!(response, Response::NewSession(_)));
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
        assert!(matches!(response, Response::NewSession(_)));
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
