//! Window destruction: `kill-window`, including its lifecycle and hook batch.

use super::*;

impl RequestHandler {
    pub(in crate::handler) async fn handle_kill_window(
        &self,
        request: rmux_proto::KillWindowRequest,
    ) -> Response {
        let session_name = request.target.session_name().clone();
        let (
            response,
            removed_windows,
            removed_pane_ids,
            destroyed_sessions,
            destroyed_attached_sessions,
            lifecycle_events,
            focus_effects,
            resize_window_ids,
            subscriptions_removed,
        ) = {
            let mut state = self.state.lock().await;
            if let Err(error) =
                super::super::require_expected_window_identity(&state, &request.target)
            {
                return Response::Error(ErrorResponse { error });
            }
            #[cfg(test)]
            self.pause_before_kill_window_commit(&request.target).await;
            let mut affected_session_candidates = state.window_linked_session_family_list(
                request.target.session_name(),
                request.target.window_index(),
            );
            if !affected_session_candidates.contains(&session_name) {
                affected_session_candidates.push(session_name.clone());
            }
            affected_session_candidates.sort_by(|left, right| left.as_str().cmp(right.as_str()));
            affected_session_candidates.dedup();
            let subscription_keys = PaneOutputSubscriptionKeySnapshot::capture_related(
                &state,
                &affected_session_candidates,
            );
            let active_window_ids_before = active_window_ids_by_session(&state);
            let selection_before = SelectionTransitionSnapshot::capture(&state);
            let grouped_peer_sessions = state
                .sessions
                .session_group_members(&session_name)
                .into_iter()
                .filter(|candidate| candidate != &session_name)
                .collect::<HashSet<_>>();
            let detach_on_destroy = SessionDetachOnDestroy::capture_all(&state);
            let timer_sessions = state
                .sessions
                .iter()
                .map(|session| (session.name(), session))
                .map(|(session_name, _)| session_name.clone())
                .collect();
            let timer_mutation =
                self.plan_window_mutation_silence_timers_locked(&state, timer_sessions);
            match state.kill_window(request.target, request.kill_all_others) {
                Ok(result) => {
                    state.retire_removed_lifecycle_targets();
                    self.record_panes_closed_as_killed(&result.removed_pane_ids);
                    let removed_timer_targets = result
                        .removed_windows
                        .iter()
                        .map(|removed| removed.target.clone())
                        .collect();
                    self.apply_window_mutation_silence_timers_locked(
                        &state,
                        timer_mutation,
                        removed_timer_targets,
                        &result.reindexed_windows,
                        Vec::new(),
                    );
                    let mut active_attach = self.active_attach.lock().await;
                    let focus_effects =
                        prepare_server_focus_effects(&mut state, &mut active_attach);
                    let lifecycle_events = if lifecycle_hooks_disabled() {
                        Vec::new()
                    } else {
                        let destroyed_session_ids = result
                            .destroyed_sessions
                            .iter()
                            .cloned()
                            .collect::<HashMap<_, _>>();
                        let mut events = Vec::new();
                        let mut ordered_windows = result.removed_windows.iter().collect::<Vec<_>>();
                        ordered_windows.sort_by(|left, right| {
                            (left.target.session_name() != &session_name)
                                .cmp(&(right.target.session_name() != &session_name))
                                .then_with(|| {
                                    left.target
                                        .session_name()
                                        .as_str()
                                        .cmp(right.target.session_name().as_str())
                                })
                                .then_with(|| {
                                    left.target.window_index().cmp(&right.target.window_index())
                                })
                        });
                        let mut selection_notified = HashSet::new();
                        for removed_window in ordered_windows {
                            let removed_session = removed_window.target.session_name();
                            let destroyed_session_id =
                                destroyed_session_ids.get(removed_session).copied();
                            if selection_notified.insert(removed_session.clone()) {
                                events.extend(
                                    selection_before
                                        .prepare_session_window_change(&mut state, removed_session),
                                );
                            }
                            if grouped_peer_sessions.contains(removed_session) {
                                if let Some(session_id) = destroyed_session_id {
                                    events.push(super::super::prepare_lifecycle_event(
                                        &mut state,
                                        &LifecycleEvent::SessionClosed {
                                            session_name: removed_session.clone(),
                                            session_id: Some(session_id.as_u32()),
                                        },
                                    ));
                                }
                            }
                            events.push(super::super::prepare_lifecycle_event(
                                &mut state,
                                &LifecycleEvent::WindowUnlinked {
                                    session_name: removed_session.clone(),
                                    target: Some(removed_window.target.clone()),
                                    window_id: Some(removed_window.window_id),
                                    window_name: Some(removed_window.window_name.clone()),
                                },
                            ));
                            if !grouped_peer_sessions.contains(removed_session) {
                                if let Some(session_id) = destroyed_session_id {
                                    events.push(super::super::prepare_lifecycle_event(
                                        &mut state,
                                        &LifecycleEvent::SessionClosed {
                                            session_name: removed_session.clone(),
                                            session_id: Some(session_id.as_u32()),
                                        },
                                    ));
                                }
                            }
                        }
                        events
                    };
                    for (destroyed_session, _) in &result.destroyed_sessions {
                        let _ = state.hooks.remove_session(destroyed_session);
                    }
                    let destroyed_attached_sessions = result
                        .destroyed_sessions
                        .iter()
                        .filter_map(|(destroyed_session, session_id)| {
                            detach_on_destroy
                                .get(session_id)
                                .copied()
                                .map(|detach| (destroyed_session.clone(), *session_id, detach))
                        })
                        .collect::<Vec<_>>();
                    let mut resize_window_ids = result.removed_window_ids;
                    resize_window_ids
                        .extend(changed_active_window_ids(&active_window_ids_before, &state));
                    resize_window_ids.sort_by_key(|window_id| window_id.as_u32());
                    resize_window_ids.dedup();
                    let subscriptions_removed = self.apply_pane_output_subscription_reconciliation(
                        subscription_keys.reconcile_after(&state),
                    );
                    (
                        Response::KillWindow(result.response),
                        result.removed_windows,
                        result.removed_pane_ids,
                        result.destroyed_sessions,
                        destroyed_attached_sessions,
                        lifecycle_events,
                        Some(focus_effects),
                        resize_window_ids,
                        subscriptions_removed,
                    )
                }
                Err(error) => (
                    Response::Error(ErrorResponse { error }),
                    Vec::new(),
                    Vec::new(),
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

        if matches!(response, Response::KillWindow(_)) {
            if let Some(focus_effects) = focus_effects {
                let _ = focus_effects.execute(self).await;
            }
            let mut prepared_rehomes = Vec::with_capacity(destroyed_attached_sessions.len());
            for (destroyed_session, session_id, detach_on_destroy) in destroyed_attached_sessions {
                prepared_rehomes.push(
                    self.prepare_destroy_session_rehome(
                        &destroyed_session,
                        session_id,
                        detach_on_destroy,
                    )
                    .await,
                );
            }

            // Teardown tickets were reserved before the prepared control
            // switches. Publish them first, then ClientSessionChanged, then
            // apply interactive attach switches.
            for lifecycle_event in lifecycle_events {
                self.emit_prepared(lifecycle_event).await;
            }
            for prepared in &mut prepared_rehomes {
                for event in prepared.control_lifecycle_events.drain(..) {
                    self.emit_prepared(event).await;
                }
            }
            for prepared in prepared_rehomes {
                self.exit_prepared_attached_session_identity(prepared).await;
            }
            #[cfg(all(any(unix, windows), feature = "web"))]
            {
                self.web_shares.remove_targets_for_panes(&removed_pane_ids);
                self.web_shares
                    .remove_targets_for_sessions(&destroyed_sessions);
            }
            self.pause_before_window_lifecycle_emit().await;
            self.forget_pane_snapshot_coalescers(&removed_pane_ids);
            let mut affected_sessions = removed_windows
                .iter()
                .map(|removed_window| removed_window.target.session_name().clone())
                .collect::<HashSet<_>>();
            let _ = affected_sessions.insert(session_name.clone());
            {
                let mut active_attach = self.active_attach.lock().await;
                for removed_window in &removed_windows {
                    active_attach.forget_window(&removed_window.target);
                }
            }
            self.bump_active_attach_epoch();
            self.remove_session_leases(&destroyed_sessions);
            for (destroyed_session, _) in &destroyed_sessions {
                self.cancel_session_silence_timers(destroyed_session).await;
            }
            let resize_targets = {
                let state = self.state.lock().await;
                let resize_targets = surviving_attached_resize_targets(&state, resize_window_ids);
                for resize_target in &resize_targets {
                    affected_sessions.extend(state.window_linked_session_family_list(
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
            let mut affected_sessions = affected_sessions.into_iter().collect::<Vec<_>>();
            affected_sessions.sort_by(|left, right| left.as_str().cmp(right.as_str()));
            for affected_session in affected_sessions {
                self.refresh_attached_session(&affected_session).await;
            }
            let _ = self.queue_shutdown_if_server_empty().await;
        }

        response
    }
}
