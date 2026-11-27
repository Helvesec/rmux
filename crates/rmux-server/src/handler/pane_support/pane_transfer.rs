use rmux_core::LifecycleEvent;
use rmux_proto::{CommandOutput, ErrorResponse, Response, Target, WindowTarget};

use super::super::{
    focus_support::prepare_server_focus_effects,
    prepare_lifecycle_event,
    scripting_support::{
        apply_client_loop_context, client_name_for_pid, format_context_for_target,
    },
    PaneOutputSubscriptionKeySnapshot, RequestHandler,
};
use super::pane_timer_mutations::BreakPaneTimerTargetPlan;
use crate::format_runtime::render_runtime_template;

mod transfer_effects;
mod window_identity;
use transfer_effects::{
    pane_transfer_refresh_sessions, PaneTransferEffects, PaneTransferEmissionPolicy,
    SourceWindowEffect,
};
use window_identity::{
    join_layout_targets, swap_layout_targets, BreakSourceWindowIdentity,
    PaneTransferWindowIdentity, SwapLayoutSnapshot,
};

const DEFAULT_BREAK_PANE_FORMAT: &str = "#{session_name}:#{window_index}.#{pane_index}";

impl RequestHandler {
    pub(in crate::handler) async fn handle_swap_pane(
        &self,
        request: rmux_proto::SwapPaneRequest,
    ) -> Response {
        // Directional swaps resolve both panes in the target window; the
        // explicit source is unused and must not acquire publication effects.
        let source = if request.direction.is_some() {
            &request.target
        } else {
            &request.source
        };
        let source_session_name = source.session_name().clone();
        let target_session_name = request.target.session_name().clone();
        let source_window =
            WindowTarget::with_window(source_session_name.clone(), source.window_index());
        let target_window =
            WindowTarget::with_window(target_session_name.clone(), request.target.window_index());
        let (response, focus_effects, layout_events, refresh_sessions) = {
            let mut state = self.state.lock().await;
            let subscription_keys = PaneOutputSubscriptionKeySnapshot::capture_related(
                &state,
                &[source_session_name.clone(), target_session_name.clone()],
            );
            let source_identity = PaneTransferWindowIdentity::capture(&state, source);
            let target_identity = PaneTransferWindowIdentity::capture(&state, &request.target);
            let layouts_before = swap_layout_targets(
                &state,
                &source_window,
                &target_window,
                source_identity.as_ref(),
                target_identity.as_ref(),
            )
            .iter()
            .filter_map(|target| SwapLayoutSnapshot::capture(&state, target))
            .collect::<Vec<_>>();
            let response = match state.swap_pane(request) {
                Ok(response) => {
                    self.rekey_pane_output_subscriptions(&subscription_keys.rekeys_after(&state));
                    Response::SwapPane(response)
                }
                Err(error) => Response::Error(ErrorResponse { error }),
            };
            // A refusal or self-swap can still unzoom. Publish the committed
            // observable delta, including partial changes before an error;
            // rolled-back changes and a preserved self-zoom produce no event.
            let layout_targets = layouts_before
                .iter()
                .filter_map(|before| before.changed_target(&state))
                .collect::<Vec<_>>();
            let mut refresh_sessions = Vec::new();
            for target in &layout_targets {
                for session in pane_transfer_refresh_sessions(&state, target, None) {
                    if !refresh_sessions.contains(&session) {
                        refresh_sessions.push(session);
                    }
                }
            }
            let focus_effects = if matches!(response, Response::SwapPane(_)) {
                let mut active_attach = self.active_attach.lock().await;
                Some(prepare_server_focus_effects(&mut state, &mut active_attach))
            } else {
                None
            };
            let layout_events = layout_targets
                .into_iter()
                .map(|target| {
                    prepare_lifecycle_event(
                        &mut state,
                        &LifecycleEvent::WindowLayoutChanged { target },
                    )
                })
                .collect::<Vec<_>>();
            (response, focus_effects, layout_events, refresh_sessions)
        };

        if let Some(focus_effects) = focus_effects {
            let _ = focus_effects.execute(self).await;
        }
        if !layout_events.is_empty() {
            for event in layout_events {
                self.emit_prepared(event).await;
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
        let (response, focus_effects, effects, removed_sessions) = {
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
            let focus_effects = if matches!(response, Response::JoinPane(_)) {
                let mut active_attach = self.active_attach.lock().await;
                Some(prepare_server_focus_effects(&mut state, &mut active_attach))
            } else {
                None
            };
            let effects = matches!(response, Response::JoinPane(_)).then(|| {
                effects.prepare_emitted(
                    &mut state,
                    &removed_sessions,
                    &layout_targets,
                    None,
                    PaneTransferEmissionPolicy::JoinOrMove,
                )
            });
            (response, focus_effects, effects, removed_sessions)
        };

        if let Some(effects) = effects {
            if let Some(focus_effects) = focus_effects {
                let _ = focus_effects.execute(self).await;
            }
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
        let (response, focus_effects, effects, removed_sessions) = {
            let mut state = self.state.lock().await;
            // Same guard, same order and same position as `join-pane`: under
            // the mutation's own lock, before anything is captured, so a slot
            // replaced during the queue's wait refuses without side effects.
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
            let focus_effects = if matches!(response, Response::MovePane(_)) {
                let mut active_attach = self.active_attach.lock().await;
                Some(prepare_server_focus_effects(&mut state, &mut active_attach))
            } else {
                None
            };
            let effects = matches!(response, Response::MovePane(_)).then(|| {
                effects.prepare_emitted(
                    &mut state,
                    &removed_sessions,
                    &layout_targets,
                    None,
                    PaneTransferEmissionPolicy::JoinOrMove,
                )
            });
            (response, focus_effects, effects, removed_sessions)
        };

        if let Some(effects) = effects {
            if let Some(focus_effects) = focus_effects {
                let _ = focus_effects.execute(self).await;
            }
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
        requester_pid: u32,
        request: rmux_proto::BreakPaneRequest,
    ) -> Response {
        let target_session_name = request.target.as_ref().map_or_else(
            || request.source.session_name().clone(),
            |target| target.session_name().clone(),
        );
        let print_target = request.print_target;
        let print_format = request.format.clone();
        let explicit_name = request.name.is_some();
        let client_loop_snapshots = if print_target {
            Some(self.client_loop_snapshots().await)
        } else {
            None
        };
        let client_name = client_loop_snapshots
            .as_deref()
            .and_then(|snapshots| client_name_for_pid(snapshots, requester_pid));
        let (response, focus_effects, effects, removed_sessions) = {
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
            let focus_effects = if matches!(response, Response::BreakPane(_)) {
                let mut active_attach = self.active_attach.lock().await;
                Some(prepare_server_focus_effects(&mut state, &mut active_attach))
            } else {
                None
            };
            let effects = matches!(response, Response::BreakPane(_)).then(|| {
                effects.prepare_emitted(
                    &mut state,
                    &removed_sessions,
                    &layout_targets,
                    linked_event,
                    PaneTransferEmissionPolicy::Break,
                )
            });
            (response, focus_effects, effects, removed_sessions)
        };

        if let Some(effects) = effects {
            if let Some(focus_effects) = focus_effects {
                let _ = focus_effects.execute(self).await;
            }
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
                        Ok(runtime) => {
                            let runtime = apply_client_loop_context(
                                runtime,
                                client_loop_snapshots.as_deref(),
                                client_name,
                            );
                            Some(CommandOutput::from_stdout(
                                format!("{}\n", render_runtime_template(template, &runtime, false))
                                    .into_bytes(),
                            ))
                        }
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
}
