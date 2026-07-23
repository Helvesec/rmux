use rmux_core::{HookStore, LifecycleEvent, WindowId};
use rmux_proto::{
    BreakPaneRequest, CommandOutput, ErrorResponse, PaneTarget, Response, SessionId, SessionName,
    Target, WindowTarget,
};

use super::super::{
    attach_support::{PreparedAttachedDestroySwitches, SessionDetachOnDestroy},
    defer_lifecycle_event, prepare_deferred_lifecycle_event, prepare_lifecycle_event,
    scripting_support::format_context_for_target,
    DeferredLifecycleEvent, PaneOutputSubscriptionKeySnapshot, QueuedLifecycleEvent,
    RequestHandler,
};
use super::pane_timer_mutations::BreakPaneTimerTargetPlan;
use crate::format_runtime::render_runtime_template;
use crate::pane_terminals::HandlerState;

const DEFAULT_BREAK_PANE_FORMAT: &str = "#{session_name}:#{window_index}.#{pane_index}";

struct PaneTransferEffects {
    source_family_sessions: Vec<SessionName>,
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

struct PreparedPaneTransferEffects {
    refresh_sessions: Vec<SessionName>,
    unlinked_windows: Vec<QueuedLifecycleEvent>,
    layout_events: Vec<QueuedLifecycleEvent>,
    linked_event: Option<QueuedLifecycleEvent>,
    closed_sessions: Vec<(
        SessionName,
        SessionId,
        SessionDetachOnDestroy,
        QueuedLifecycleEvent,
    )>,
}

#[derive(Clone, Copy)]
enum SourceWindowEffect {
    None,
    RemoveLinkedFamily,
    MoveGroupedSlots,
}

struct BreakSourceWindowIdentity {
    session_name: SessionName,
    window_id: WindowId,
}

#[derive(Clone, PartialEq, Eq)]
struct PaneTransferWindowIdentity {
    session_name: SessionName,
    window_id: WindowId,
}

impl PaneTransferWindowIdentity {
    fn capture(state: &HandlerState, target: &PaneTarget) -> Option<Self> {
        let session = state.sessions.session(target.session_name())?;
        let window = session.window_at(target.window_index())?;
        Some(Self {
            session_name: target.session_name().clone(),
            window_id: window.id(),
        })
    }

    fn resolve(&self, state: &HandlerState) -> Option<WindowTarget> {
        let session = state.sessions.session(&self.session_name)?;
        session.windows().iter().find_map(|(window_index, window)| {
            (window.id() == self.window_id)
                .then(|| WindowTarget::with_window(self.session_name.clone(), *window_index))
        })
    }
}

impl BreakSourceWindowIdentity {
    fn capture(state: &HandlerState, request: &BreakPaneRequest) -> Option<Self> {
        let source_session = state.sessions.session(request.source.session_name())?;
        let source_window = source_session.window_at(request.source.window_index())?;
        (source_window.pane_count() > 1).then_some(Self {
            session_name: request.source.session_name().clone(),
            window_id: source_window.id(),
        })
    }

    fn resolve_after_break(&self, state: &HandlerState) -> Option<WindowTarget> {
        let session = state.sessions.session(&self.session_name)?;
        session.windows().iter().find_map(|(window_index, window)| {
            (window.id() == self.window_id)
                .then(|| WindowTarget::with_window(self.session_name.clone(), *window_index))
        })
    }
}

impl PaneTransferEffects {
    fn capture(
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
            refresh_sessions,
            hook_snapshot: state.hooks.clone(),
            unlinked_windows,
            closed_sessions,
        }
    }

    fn removed_sessions(&self, state: &HandlerState, response: &Response) -> Vec<SessionName> {
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

    fn prepare_emitted(
        self,
        state: &mut HandlerState,
        removed_sessions: &[SessionName],
        layout_targets: &[WindowTarget],
        linked_event: Option<LifecycleEvent>,
    ) -> PreparedPaneTransferEffects {
        let Self {
            refresh_sessions,
            mut hook_snapshot,
            unlinked_windows,
            closed_sessions,
            ..
        } = self;
        let unlinked_windows = unlinked_windows
            .into_iter()
            .map(|event| prepare_deferred_lifecycle_event(state, &mut hook_snapshot, event))
            .collect();
        let layout_events = layout_targets
            .iter()
            .cloned()
            .map(|target| {
                prepare_lifecycle_event(state, &LifecycleEvent::WindowLayoutChanged { target })
            })
            .collect();
        let linked_event = linked_event
            .as_ref()
            .map(|event| prepare_lifecycle_event(state, event));
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
            layout_events,
            linked_event,
            closed_sessions,
        }
    }
}

impl RequestHandler {
    pub(in crate::handler) async fn handle_swap_pane(
        &self,
        request: rmux_proto::SwapPaneRequest,
    ) -> Response {
        let source_session_name = request.source.session_name().clone();
        let target_session_name = request.target.session_name().clone();
        let source_window =
            WindowTarget::with_window(source_session_name.clone(), request.source.window_index());
        let target_window =
            WindowTarget::with_window(target_session_name.clone(), request.target.window_index());
        let (response, layout_targets, refresh_sessions) = {
            let mut state = self.state.lock().await;
            let subscription_keys = PaneOutputSubscriptionKeySnapshot::capture_related(
                &state,
                &[source_session_name.clone(), target_session_name.clone()],
            );
            let source_identity = PaneTransferWindowIdentity::capture(&state, &request.source);
            let target_identity = PaneTransferWindowIdentity::capture(&state, &request.target);
            let refresh_sessions =
                pane_transfer_refresh_sessions(&state, &source_window, Some(&target_window));
            let response = match state.swap_pane(request) {
                Ok(response) => {
                    self.rekey_pane_output_subscriptions(&subscription_keys.rekeys_after(&state));
                    Response::SwapPane(response)
                }
                Err(error) => Response::Error(ErrorResponse { error }),
            };
            let layout_targets = matches!(response, Response::SwapPane(_))
                .then(|| {
                    swap_layout_targets(
                        &state,
                        &source_window,
                        &target_window,
                        source_identity.as_ref(),
                        target_identity.as_ref(),
                    )
                })
                .unwrap_or_default();
            (response, layout_targets, refresh_sessions)
        };

        if !layout_targets.is_empty() {
            for target in layout_targets {
                self.emit(LifecycleEvent::WindowLayoutChanged { target })
                    .await;
            }
            for session_name in refresh_sessions {
                self.refresh_attached_session(&session_name).await;
            }
        }

        response
    }

    pub(in crate::handler) async fn handle_join_pane(
        &self,
        request: rmux_proto::JoinPaneRequest,
    ) -> Response {
        let source_session_name = request.source.session_name().clone();
        let target_session_name = request.target.session_name().clone();
        let source_window =
            WindowTarget::with_window(source_session_name.clone(), request.source.window_index());
        let target_window =
            WindowTarget::with_window(target_session_name.clone(), request.target.window_index());
        let source_window_effect = if source_window == target_window {
            SourceWindowEffect::None
        } else {
            SourceWindowEffect::RemoveLinkedFamily
        };
        let (response, effects, removed_sessions) = {
            let mut state = self.state.lock().await;
            if let Err(error) =
                super::super::require_expected_pane_identity(&state, &request.source).and_then(
                    |()| super::super::require_expected_pane_identity(&state, &request.target),
                )
            {
                return Response::Error(ErrorResponse { error });
            }
            let subscription_keys = PaneOutputSubscriptionKeySnapshot::capture_related(
                &state,
                &[source_session_name.clone(), target_session_name.clone()],
            );
            let timer_mutation = self.plan_all_window_mutation_silence_timers_locked(&state);
            let source_identity = PaneTransferWindowIdentity::capture(&state, &request.source);
            let target_identity = PaneTransferWindowIdentity::capture(&state, &request.target);
            let effects = PaneTransferEffects::capture(
                &state,
                &request.source,
                Some(&target_window),
                source_window_effect,
            );
            let response = match state.join_pane(request) {
                Ok(response) => {
                    state.retire_removed_lifecycle_targets();
                    self.apply_window_mutation_silence_timers_and_arm_all_locked(
                        &state,
                        timer_mutation,
                        Vec::new(),
                        &[],
                    );
                    self.rekey_pane_output_subscriptions(&subscription_keys.rekeys_after(&state));
                    Response::JoinPane(response)
                }
                Err(error) => Response::Error(ErrorResponse { error }),
            };
            let removed_sessions = effects.removed_sessions(&state, &response);
            let layout_targets = matches!(response, Response::JoinPane(_))
                .then(|| {
                    join_layout_targets(
                        &state,
                        &source_window,
                        &target_window,
                        source_identity.as_ref(),
                        target_identity.as_ref(),
                    )
                })
                .unwrap_or_default();
            let effects = matches!(response, Response::JoinPane(_)).then(|| {
                effects.prepare_emitted(&mut state, &removed_sessions, &layout_targets, None)
            });
            (response, effects, removed_sessions)
        };

        if let Some(effects) = effects {
            let prepared_rehomes = self.emit_prepared_pane_transfer_lifecycle(&effects).await;
            self.finish_pane_transfer_effects(
                effects,
                &removed_sessions,
                &target_session_name,
                prepared_rehomes,
            )
            .await;
        }

        response
    }

    pub(in crate::handler) async fn handle_move_pane(
        &self,
        request: rmux_proto::MovePaneRequest,
    ) -> Response {
        let source_session_name = request.source.session_name().clone();
        let target_session_name = request.target.session_name().clone();
        let source_window =
            WindowTarget::with_window(source_session_name.clone(), request.source.window_index());
        let target_window =
            WindowTarget::with_window(target_session_name.clone(), request.target.window_index());
        let source_window_effect = if source_window == target_window {
            SourceWindowEffect::None
        } else {
            SourceWindowEffect::RemoveLinkedFamily
        };
        let (response, effects, removed_sessions) = {
            let mut state = self.state.lock().await;
            let subscription_keys = PaneOutputSubscriptionKeySnapshot::capture_related(
                &state,
                &[source_session_name.clone(), target_session_name.clone()],
            );
            let timer_mutation = self.plan_all_window_mutation_silence_timers_locked(&state);
            let source_identity = PaneTransferWindowIdentity::capture(&state, &request.source);
            let target_identity = PaneTransferWindowIdentity::capture(&state, &request.target);
            let effects = PaneTransferEffects::capture(
                &state,
                &request.source,
                Some(&target_window),
                source_window_effect,
            );
            let response = match state.move_pane(request) {
                Ok(response) => {
                    state.retire_removed_lifecycle_targets();
                    self.apply_window_mutation_silence_timers_and_arm_all_locked(
                        &state,
                        timer_mutation,
                        Vec::new(),
                        &[],
                    );
                    self.rekey_pane_output_subscriptions(&subscription_keys.rekeys_after(&state));
                    Response::MovePane(response)
                }
                Err(error) => Response::Error(ErrorResponse { error }),
            };
            let removed_sessions = effects.removed_sessions(&state, &response);
            let layout_targets = matches!(response, Response::MovePane(_))
                .then(|| {
                    join_layout_targets(
                        &state,
                        &source_window,
                        &target_window,
                        source_identity.as_ref(),
                        target_identity.as_ref(),
                    )
                })
                .unwrap_or_default();
            let effects = matches!(response, Response::MovePane(_)).then(|| {
                effects.prepare_emitted(&mut state, &removed_sessions, &layout_targets, None)
            });
            (response, effects, removed_sessions)
        };

        if let Some(effects) = effects {
            let prepared_rehomes = self.emit_prepared_pane_transfer_lifecycle(&effects).await;
            self.finish_pane_transfer_effects(
                effects,
                &removed_sessions,
                &target_session_name,
                prepared_rehomes,
            )
            .await;
        }

        response
    }

    pub(in crate::handler) async fn handle_break_pane(
        &self,
        request: rmux_proto::BreakPaneRequest,
    ) -> Response {
        let target_session_name = request.target.as_ref().map_or_else(
            || request.source.session_name().clone(),
            |target| target.session_name().clone(),
        );
        let print_target = request.print_target;
        let print_format = request.format.clone();
        let explicit_name = request.name.is_some();
        let (response, effects, removed_sessions) = {
            let mut state = self.state.lock().await;
            let subscription_keys = PaneOutputSubscriptionKeySnapshot::capture_related(
                &state,
                &[
                    request.source.session_name().clone(),
                    target_session_name.clone(),
                ],
            );
            let mut timer_mutation = self.plan_all_window_mutation_silence_timers_locked(&state);
            let timer_target_plan = BreakPaneTimerTargetPlan::capture(&state, &request);
            let timer_fanout_source = WindowTarget::with_window(
                request.source.session_name().clone(),
                request.source.window_index(),
            );
            let source_window = BreakSourceWindowIdentity::capture(&state, &request);
            let effects = PaneTransferEffects::capture(
                &state,
                &request.source,
                request.target.as_ref(),
                SourceWindowEffect::MoveGroupedSlots,
            );
            let response = match state.break_pane(request) {
                Ok(response) => {
                    state.retire_removed_lifecycle_targets();
                    let destination = WindowTarget::with_window(
                        response.target.session_name().clone(),
                        response.target.window_index(),
                    );
                    for (source, destination) in
                        timer_target_plan.overrides_after(&state, &destination)
                    {
                        match destination {
                            Some(destination) => timer_mutation.map_target(source, destination),
                            None => timer_mutation.remove_target(source),
                        }
                    }
                    timer_mutation.fanout_target_to_destination_group_locked(
                        &state,
                        timer_fanout_source,
                        &destination,
                    );
                    self.apply_window_mutation_silence_timers_and_arm_all_locked(
                        &state,
                        timer_mutation,
                        Vec::new(),
                        &[],
                    );
                    self.rekey_pane_output_subscriptions(&subscription_keys.rekeys_after(&state));
                    Response::BreakPane(response)
                }
                Err(error) => Response::Error(ErrorResponse { error }),
            };
            let removed_sessions = effects.removed_sessions(&state, &response);
            let source_layout_target = match (&response, source_window) {
                (Response::BreakPane(_), Some(source_window)) => {
                    source_window.resolve_after_break(&state)
                }
                _ => None,
            };
            let linked_event = if let Response::BreakPane(success) = &response {
                let target_window = WindowTarget::with_window(
                    success.target.session_name().clone(),
                    success.target.window_index(),
                );
                Some(LifecycleEvent::WindowLinked {
                    session_name: target_session_name.clone(),
                    target: Some(target_window),
                })
            } else {
                None
            };
            let layout_targets = source_layout_target.into_iter().collect::<Vec<_>>();
            let effects = matches!(response, Response::BreakPane(_)).then(|| {
                effects.prepare_emitted(
                    &mut state,
                    &removed_sessions,
                    &layout_targets,
                    linked_event,
                )
            });
            (response, effects, removed_sessions)
        };

        if let Some(effects) = effects {
            let prepared_rehomes = self.emit_prepared_pane_transfer_lifecycle(&effects).await;
            self.finish_pane_transfer_effects(
                effects,
                &removed_sessions,
                &target_session_name,
                prepared_rehomes,
            )
            .await;
            if !explicit_name {
                if let Response::BreakPane(success) = &response {
                    self.refresh_automatic_window_name_for_pane_target(&success.target)
                        .await;
                }
            }
        }

        if print_target {
            let template = print_format.as_deref().unwrap_or(DEFAULT_BREAK_PANE_FORMAT);
            if let Response::BreakPane(success) = &response {
                let attached_count = self.attached_count(success.target.session_name()).await;
                let output = {
                    let state = self.state.lock().await;
                    let runtime = format_context_for_target(
                        &state,
                        &Target::Pane(success.target.clone()),
                        attached_count,
                    )
                    .map_err(|error| ErrorResponse { error });
                    match runtime {
                        Ok(runtime) => Some(CommandOutput::from_stdout(
                            format!("{}\n", render_runtime_template(template, &runtime, false))
                                .into_bytes(),
                        )),
                        Err(error) => return Response::Error(error),
                    }
                };
                return Response::BreakPane(rmux_proto::BreakPaneResponse {
                    target: success.target.clone(),
                    output,
                });
            }
        }

        response
    }

    async fn emit_prepared_pane_transfer_lifecycle(
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
        for event in &effects.layout_events {
            self.emit_prepared(event.clone()).await;
        }
        if let Some(event) = &effects.linked_event {
            self.pause_before_window_lifecycle_emit().await;
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

    async fn finish_pane_transfer_effects(
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

fn pane_transfer_refresh_sessions(
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

fn swap_layout_targets(
    state: &HandlerState,
    source: &WindowTarget,
    target: &WindowTarget,
    source_identity: Option<&PaneTransferWindowIdentity>,
    target_identity: Option<&PaneTransferWindowIdentity>,
) -> Vec<WindowTarget> {
    let resolved_target = target_identity
        .and_then(|identity| identity.resolve(state))
        .unwrap_or_else(|| target.clone());
    if source_identity
        .zip(target_identity)
        .is_some_and(|(source, target)| source.window_id == target.window_id)
    {
        return vec![resolved_target];
    }

    let resolved_source = source_identity
        .and_then(|identity| identity.resolve(state))
        .unwrap_or_else(|| source.clone());
    if source == target {
        vec![resolved_target]
    } else {
        vec![resolved_source, resolved_target]
    }
}

fn join_layout_targets(
    state: &HandlerState,
    source: &WindowTarget,
    target: &WindowTarget,
    source_identity: Option<&PaneTransferWindowIdentity>,
    target_identity: Option<&PaneTransferWindowIdentity>,
) -> Vec<WindowTarget> {
    let resolved_target = target_identity
        .and_then(|identity| identity.resolve(state))
        .unwrap_or_else(|| target.clone());
    let same_window_identity = source_identity
        .zip(target_identity)
        .is_some_and(|(source, target)| source.window_id == target.window_id);
    let resolved_source = if same_window_identity {
        resolved_target.clone()
    } else {
        source_identity
            .and_then(|identity| identity.resolve(state))
            .unwrap_or_else(|| resolved_target.clone())
    };

    if source == target {
        vec![resolved_target]
    } else {
        vec![resolved_source, resolved_target]
    }
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
