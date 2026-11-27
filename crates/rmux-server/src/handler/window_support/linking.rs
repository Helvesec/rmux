//! Winlink topology: `link-window`, `move-window` and `unlink-window`.

use super::*;

impl RequestHandler {
    pub(in crate::handler) async fn handle_link_window(
        &self,
        request: rmux_proto::LinkWindowRequest,
    ) -> Response {
        self.pause_before_window_lifecycle_mutation().await;
        let (
            response,
            removed_destination_pane_ids,
            mut refresh_sessions,
            resize_window_ids,
            focus_effects,
            lifecycle_events,
            subscriptions_removed,
        ) = {
            let mut state = self.state.lock().await;
            if let Err(error) = require_expected_link_window_identity(&state, &request) {
                return Response::Error(ErrorResponse { error });
            }
            let subscription_keys = PaneOutputSubscriptionKeySnapshot::capture_related(
                &state,
                &[
                    request.source.session_name().clone(),
                    request.target.session_name().clone(),
                ],
            );
            let active_window_ids_before = active_window_ids_by_session(&state);
            let selection_before = SelectionTransitionSnapshot::capture(&state);
            let mut resize_window_ids = [
                state
                    .sessions
                    .session(request.source.session_name())
                    .and_then(|session| session.window_at(request.source.window_index()))
                    .map(rmux_core::Window::id),
                state
                    .sessions
                    .session(request.target.session_name())
                    .and_then(|session| session.window_at(request.target.window_index()))
                    .map(rmux_core::Window::id),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
            let deadline_fanout =
                self.plan_silence_timer_deadline_fanout_locked(&state, &request.source);
            match state.link_window(request.clone()) {
                Ok(result) => {
                    state.retire_removed_lifecycle_targets();
                    resize_window_ids
                        .extend(changed_active_window_ids(&active_window_ids_before, &state));
                    resize_window_ids.sort_by_key(|window_id| window_id.as_u32());
                    resize_window_ids.dedup();
                    let destination_timer_sessions = state
                        .sessions
                        .session_group_members(result.response.target.session_name());
                    let destination_timer_targets = destination_timer_sessions
                        .iter()
                        .cloned()
                        .map(|session_name| {
                            rmux_proto::WindowTarget::with_window(
                                session_name,
                                result.response.target.window_index(),
                            )
                        })
                        .collect::<Vec<_>>();
                    let reindexed_timer_sessions = if result.reindexed_windows.is_empty() {
                        Vec::new()
                    } else {
                        destination_timer_sessions
                    };
                    let reindexed_windows = result.reindexed_windows.clone();
                    let refresh_sessions = state.window_linked_session_family_list(
                        result.response.target.session_name(),
                        result.response.target.window_index(),
                    );
                    self.record_panes_closed_as_killed(&result.removed_pane_ids);
                    if let Some(deadline_fanout) = deadline_fanout {
                        deadline_fanout
                            .apply_expired_state_locked(&mut state, &destination_timer_targets);
                    }
                    self.sync_inserted_window_silence_timers_locked(
                        &state,
                        destination_timer_targets,
                        reindexed_timer_sessions,
                        reindexed_windows,
                        deadline_fanout,
                    );
                    let mut active_attach = self.active_attach.lock().await;
                    let focus_effects =
                        prepare_server_focus_effects(&mut state, &mut active_attach);
                    let mut lifecycle_events = super::super::prepare_lifecycle_event_if_enabled(
                        &mut state,
                        &LifecycleEvent::WindowLinked {
                            session_name: result.response.target.session_name().clone(),
                            target: Some(result.response.target.clone()),
                        },
                    )
                    .into_iter()
                    .collect::<Vec<_>>();
                    lifecycle_events.extend(selection_before.prepare_session_window_changes(
                        &mut state,
                        std::slice::from_ref(result.response.target.session_name()),
                    ));
                    let subscriptions_removed = self.apply_pane_output_subscription_reconciliation(
                        subscription_keys.reconcile_after(&state),
                    );
                    (
                        Response::LinkWindow(result.response),
                        result.removed_pane_ids,
                        refresh_sessions,
                        resize_window_ids,
                        Some(focus_effects),
                        lifecycle_events,
                        subscriptions_removed,
                    )
                }
                Err(error) => (
                    Response::Error(ErrorResponse { error }),
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                    None,
                    Vec::new(),
                    false,
                ),
            }
        };

        if subscriptions_removed {
            let _ = self.request_shutdown_if_pending();
        }

        if matches!(response, Response::LinkWindow(_)) {
            self.forget_pane_snapshot_coalescers(&removed_destination_pane_ids);
            if let Some(focus_effects) = focus_effects {
                let _ = focus_effects.execute(self).await;
            }
            if !lifecycle_events.is_empty() {
                self.pause_before_window_lifecycle_emit().await;
                for event in lifecycle_events {
                    self.emit_prepared(event).await;
                }
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
        }

        response
    }

    pub(in crate::handler) async fn handle_move_window(
        &self,
        request: MoveWindowRequest,
    ) -> Response {
        self.pause_before_window_lifecycle_mutation().await;
        let committed = {
            let mut state = self.state.lock().await;
            let mut active_attach = self.active_attach.lock().await;
            self.commit_move_window_locked(&mut state, &mut active_attach, &request)
        };
        match committed {
            Ok(committed) => self.finish_committed_move_window(committed).await,
            Err(error) => Response::Error(ErrorResponse { error }),
        }
    }

    fn commit_move_window_locked(
        &self,
        state: &mut HandlerState,
        active_attach: &mut ActiveAttachState,
        request: &MoveWindowRequest,
    ) -> Result<CommittedMoveWindow, RmuxError> {
        require_expected_move_window_identity(state, request)?;
        self.commit_prevalidated_move_window_locked(state, active_attach, request)
    }

    pub(in crate::handler) fn commit_prevalidated_move_window_locked(
        &self,
        state: &mut HandlerState,
        active_attach: &mut ActiveAttachState,
        request: &MoveWindowRequest,
    ) -> Result<CommittedMoveWindow, RmuxError> {
        let mut subscription_roots = request
            .source
            .as_ref()
            .map(|source| vec![source.session_name().clone()])
            .unwrap_or_default();
        match &request.target {
            rmux_proto::MoveWindowTarget::Session(session_name) => {
                subscription_roots.push(session_name.clone());
            }
            rmux_proto::MoveWindowTarget::Window(target) => {
                subscription_roots.push(target.session_name().clone());
            }
        }
        let subscription_keys =
            PaneOutputSubscriptionKeySnapshot::capture_related(state, &subscription_roots);
        let effects = MoveWindowEffects::capture(state, request);
        let timer_overrides = move_window_timer_target_overrides(state, request);
        let mut timer_mutation = self.plan_all_window_mutation_silence_timers_locked(state);
        for (source, destination) in timer_overrides {
            match destination {
                Some(destination) => timer_mutation.map_target(source, destination),
                None => timer_mutation.remove_target(source),
            }
        }
        let result = state.move_window(request.clone())?;
        state.retire_removed_lifecycle_targets();
        self.record_panes_closed_as_killed(&result.removed_pane_ids);
        if let (Some(source), Some(destination)) =
            (request.source.clone(), result.response.target.as_ref())
        {
            timer_mutation.fanout_target_to_destination_group_locked(state, source, destination);
        }
        self.apply_window_mutation_silence_timers_and_arm_all_locked(
            state,
            timer_mutation,
            Vec::new(),
            &[],
        );
        let subscriptions_removed = self.apply_pane_output_subscription_reconciliation(
            subscription_keys.reconcile_after(state),
        );
        // Focus is drained before the move lifecycle batch, so its ticket must
        // also be reserved first. Both projections still observe one atomic
        // post-mutation state under the established state -> attach lock order.
        let focus_effects = prepare_server_focus_effects(state, active_attach);
        let effects = effects.prepare_success(state, &result.response);
        Ok(CommittedMoveWindow {
            response: result.response,
            removed_destination_pane_ids: result.removed_pane_ids,
            effects,
            focus_effects: Some(focus_effects),
            subscriptions_removed,
        })
    }

    pub(in crate::handler) async fn finish_committed_move_window(
        &self,
        mut committed: CommittedMoveWindow,
    ) -> Response {
        if committed.subscriptions_removed {
            let _ = self.request_shutdown_if_pending();
        }
        self.forget_pane_snapshot_coalescers(&committed.removed_destination_pane_ids);
        if let Some(focus_effects) = committed.focus_effects.take() {
            let _ = focus_effects.execute(self).await;
        }
        self.finish_move_window_effects(committed.effects).await;
        Response::MoveWindow(committed.response)
    }

    pub(in crate::handler) async fn handle_unlink_window(
        &self,
        request: UnlinkWindowRequest,
    ) -> Response {
        let session_name = request.target.session_name().clone();
        self.pause_before_window_lifecycle_mutation().await;
        if let Some(response) = self.destroy_session_for_only_linked_window(&request).await {
            return response;
        }
        let (
            response,
            removed_pane_ids,
            refresh_sessions,
            resize_targets,
            focus_effects,
            lifecycle_events,
            subscriptions_removed,
        ) = {
            let mut state = self.state.lock().await;
            if let Err(error) =
                super::super::require_expected_window_identity(&state, &request.target)
            {
                return Response::Error(ErrorResponse { error });
            }
            let subscription_keys = PaneOutputSubscriptionKeySnapshot::capture_related(
                &state,
                std::slice::from_ref(&session_name),
            );
            let mut refresh_sessions = state
                .window_linked_session_family_list(&session_name, request.target.window_index());
            let linked_targets =
                state.window_linked_window_targets(&session_name, request.target.window_index());
            let removed_window_id = state
                .sessions
                .session(&session_name)
                .and_then(|session| session.window_at(request.target.window_index()))
                .map(rmux_core::Window::id);
            refresh_sessions.sort_by(|left, right| left.as_str().cmp(right.as_str()));
            refresh_sessions.dedup();
            let timer_sessions = state
                .sessions
                .iter()
                .map(|session| (session.name(), session))
                .map(|(session_name, _)| session_name.clone())
                .collect();
            let timer_mutation =
                self.plan_window_mutation_silence_timers_locked(&state, timer_sessions);
            let selection_before = SelectionTransitionSnapshot::capture(&state);
            match state.unlink_window(request.target, request.kill_if_last) {
                Ok(result) => {
                    state.retire_removed_lifecycle_targets();
                    state.expand_with_active_window_linked_session_families(&mut refresh_sessions);
                    let resize_targets = removed_window_id
                        .and_then(|window_id| {
                            linked_targets.into_iter().find(|target| {
                                state
                                    .sessions
                                    .session(target.session_name())
                                    .and_then(|session| session.window_at(target.window_index()))
                                    .is_some_and(|window| window.id() == window_id)
                            })
                        })
                        .into_iter()
                        .collect();
                    self.record_panes_closed_as_killed(&result.removed_pane_ids);
                    self.apply_window_mutation_silence_timers_locked(
                        &state,
                        timer_mutation,
                        result.removed_timer_targets,
                        &result.reindexed_windows,
                        Vec::new(),
                    );
                    let mut active_attach = self.active_attach.lock().await;
                    let focus_effects =
                        prepare_server_focus_effects(&mut state, &mut active_attach);
                    let lifecycle_events = if lifecycle_hooks_disabled() {
                        Vec::new()
                    } else {
                        let mut events = selection_before.prepare_session_window_changes(
                            &mut state,
                            std::slice::from_ref(&session_name),
                        );
                        events.push(super::super::prepare_lifecycle_event(
                            &mut state,
                            &LifecycleEvent::WindowUnlinked {
                                session_name: session_name.clone(),
                                target: Some(result.removed_window.target.clone()),
                                window_id: Some(result.removed_window.window_id),
                                window_name: Some(result.removed_window.window_name.clone()),
                            },
                        ));
                        events
                    };
                    let subscriptions_removed = self.apply_pane_output_subscription_reconciliation(
                        subscription_keys.reconcile_after(&state),
                    );
                    (
                        Response::UnlinkWindow(result.response),
                        result.removed_pane_ids,
                        refresh_sessions,
                        resize_targets,
                        Some(focus_effects),
                        lifecycle_events,
                        subscriptions_removed,
                    )
                }
                Err(error) => (
                    Response::Error(ErrorResponse { error }),
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                    None,
                    Vec::new(),
                    false,
                ),
            }
        };

        if subscriptions_removed {
            let _ = self.request_shutdown_if_pending();
        }

        if matches!(response, Response::UnlinkWindow(_)) {
            self.pause_before_window_lifecycle_emit().await;
            self.forget_pane_snapshot_coalescers(&removed_pane_ids);
            if let Some(focus_effects) = focus_effects {
                let _ = focus_effects.execute(self).await;
            }
            for lifecycle_event in lifecycle_events {
                self.emit_prepared(lifecycle_event).await;
            }
            for resize_target in resize_targets {
                let _ = self
                    .reconcile_attached_window_size_and_emit(&resize_target)
                    .await;
            }
            for refresh_session in refresh_sessions {
                let _ = self
                    .reconcile_attached_session_size_and_emit(&refresh_session)
                    .await;
                self.refresh_attached_session(&refresh_session).await;
            }
        }

        response
    }

    async fn destroy_session_for_only_linked_window(
        &self,
        request: &UnlinkWindowRequest,
    ) -> Option<Response> {
        let (session_id, window_id) = {
            let state = self.state.lock().await;
            if let Err(error) =
                super::super::require_expected_window_identity(&state, &request.target)
            {
                return Some(Response::Error(ErrorResponse { error }));
            }
            let session = state.sessions.session(request.target.session_name())?;
            let link_count = state
                .window_link_count(request.target.session_name(), request.target.window_index());
            if session.windows().count() != 1 || (link_count <= 1 && !request.kill_if_last) {
                return None;
            }
            let window = session.window_at(request.target.window_index())?;
            (session.id(), window.id())
        };
        let target = request.target.clone();
        let response = self
            .handle_kill_session_if_only_linked_window_identity(
                KillSessionRequest {
                    target: target.session_name().clone(),
                    kill_all_except_target: false,
                    clear_alerts: false,
                    kill_group: false,
                },
                session_id,
                window_id,
                request.kill_if_last,
            )
            .await?;
        Some(match response {
            Response::KillSession(_) => Response::UnlinkWindow(UnlinkWindowResponse { target }),
            other => other,
        })
    }
}
