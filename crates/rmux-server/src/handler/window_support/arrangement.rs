//! Window arrangement and geometry: `swap-window`, `rotate-window` and
//! `resize-window`.

use super::*;

impl RequestHandler {
    pub(in crate::handler) async fn handle_swap_window(
        &self,
        request: rmux_proto::SwapWindowRequest,
    ) -> Response {
        let (response, mut refresh_sessions, resize_window_ids, focus_effects, lifecycle_events) = {
            let mut state = self.state.lock().await;
            if let Err(error) = require_expected_swap_window_identity(&state, &request) {
                return Response::Error(ErrorResponse { error });
            }
            if request.source.session_name() == request.target.session_name()
                && request.source.window_index() == request.target.window_index()
            {
                return Response::SwapWindow(rmux_proto::SwapWindowResponse {
                    source: request.source,
                    target: request.target,
                });
            }
            let subscription_keys = PaneOutputSubscriptionKeySnapshot::capture_related(
                &state,
                &[
                    request.source.session_name().clone(),
                    request.target.session_name().clone(),
                ],
            );
            let selection_before = SelectionTransitionSnapshot::capture(&state);
            let preferred_selection_sessions = swap_selection_session_order(&state, &request);
            let active_window_ids_before = active_window_ids_by_session(&state);
            let mut resize_window_ids = [&request.source, &request.target]
                .into_iter()
                .filter_map(|target| {
                    state
                        .sessions
                        .session(target.session_name())
                        .and_then(|session| session.window_at(target.window_index()))
                        .map(rmux_core::Window::id)
                })
                .collect::<Vec<_>>();
            let mut refresh_sessions = state.window_linked_session_family_list(
                request.source.session_name(),
                request.source.window_index(),
            );
            for session_name in state.window_linked_session_family_list(
                request.target.session_name(),
                request.target.window_index(),
            ) {
                if !refresh_sessions.contains(&session_name) {
                    refresh_sessions.push(session_name);
                }
            }
            let timer_overrides = swap_window_timer_target_overrides(&state, &request);
            let source = request.source.clone();
            let target = request.target.clone();
            let mut timer_mutation = self.plan_all_window_mutation_silence_timers_locked(&state);
            for (source, destination) in timer_overrides {
                match destination {
                    Some(destination) => timer_mutation.map_target(source, destination),
                    None => timer_mutation.remove_target(source),
                }
            }
            match state.swap_window(request.source, request.target, request.detached) {
                Ok(response) => {
                    timer_mutation.fanout_target_to_destination_group_locked(
                        &state,
                        source,
                        &response.target,
                    );
                    timer_mutation.fanout_target_to_destination_group_locked(
                        &state,
                        target,
                        &response.source,
                    );
                    self.apply_window_mutation_silence_timers_and_arm_all_locked(
                        &state,
                        timer_mutation,
                        Vec::new(),
                        &[],
                    );
                    resize_window_ids
                        .extend(changed_active_window_ids(&active_window_ids_before, &state));
                    resize_window_ids.sort_by_key(|window_id| window_id.as_u32());
                    resize_window_ids.dedup();
                    self.rekey_pane_output_subscriptions(&subscription_keys.rekeys_after(&state));
                    let mut active_attach = self.active_attach.lock().await;
                    let focus_effects =
                        prepare_server_focus_effects(&mut state, &mut active_attach);
                    let lifecycle_events = selection_before
                        .prepare_session_window_changes(&mut state, &preferred_selection_sessions);
                    (
                        Response::SwapWindow(response),
                        refresh_sessions,
                        resize_window_ids,
                        Some(focus_effects),
                        lifecycle_events,
                    )
                }
                Err(error) => (
                    Response::Error(ErrorResponse { error }),
                    Vec::new(),
                    Vec::new(),
                    None,
                    Vec::new(),
                ),
            }
        };

        if matches!(response, Response::SwapWindow(_)) {
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

    pub(in crate::handler) async fn handle_rotate_window(
        &self,
        request: rmux_proto::RotateWindowRequest,
    ) -> Response {
        let target = request.target;
        let (response, refresh_sessions, focus_effects, layout_event) = {
            let mut state = self.state.lock().await;
            match state.rotate_window(target.clone(), request.direction, request.restore_zoom) {
                Ok(response) => {
                    let refresh_sessions = state.window_linked_session_family_list(
                        target.session_name(),
                        target.window_index(),
                    );
                    let mut active_attach = self.active_attach.lock().await;
                    let focus_effects =
                        prepare_server_focus_effects(&mut state, &mut active_attach);
                    let layout_event = prepare_lifecycle_event(
                        &mut state,
                        &LifecycleEvent::WindowLayoutChanged {
                            target: target.clone(),
                        },
                    );
                    (
                        Response::RotateWindow(response),
                        refresh_sessions,
                        Some(focus_effects),
                        Some(layout_event),
                    )
                }
                Err(error) => (
                    Response::Error(ErrorResponse { error }),
                    Vec::new(),
                    None,
                    None,
                ),
            }
        };

        if matches!(response, Response::RotateWindow(_)) {
            if let Some(focus_effects) = focus_effects {
                let _ = focus_effects.execute(self).await;
            }
            if let Some(layout_event) = layout_event {
                self.emit_prepared(layout_event).await;
            }
            for session_name in refresh_sessions {
                self.refresh_attached_session(&session_name).await;
            }
        }

        response
    }

    pub(in crate::handler) async fn handle_resize_window(
        &self,
        request: rmux_proto::ResizeWindowRequest,
    ) -> Response {
        let request = match self
            .resolve_resize_window_linked_session_size(request)
            .await
        {
            Ok(request) => request,
            Err(error) => return Response::Error(ErrorResponse { error }),
        };
        let session_name = request.target.session_name().clone();
        let target = request.target.clone();
        let (response, refresh_sessions) = {
            let mut state = self.state.lock().await;
            let controls = self.active_control.lock().await;
            let maximum =
                super::super::attach_support::control_window_size_limit(&state, &controls, &target);
            match state.resize_window(request, maximum) {
                Ok(response) => {
                    let refresh_sessions = state.window_linked_session_family_list(
                        target.session_name(),
                        target.window_index(),
                    );
                    (Response::ResizeWindow(response), refresh_sessions)
                }
                Err(error) => (Response::Error(ErrorResponse { error }), Vec::new()),
            }
        };

        if matches!(response, Response::ResizeWindow(_)) {
            self.queue_inline_hook(
                HookName::AfterResizeWindow,
                ScopeSelector::Session(session_name.clone()),
                Some(Target::Window(target.clone())),
                PendingInlineHookFormat::AfterCommand,
            );
            // tmux 3.7b's `cmd_resize_window_exec` calls `resize_window()`
            // unconditionally, so `resize-window` publishes even when the
            // requested size equals the current one.
            self.emit_applied_window_resize(target).await;
            for refresh_session in refresh_sessions {
                self.refresh_attached_session(&refresh_session).await;
            }
        }

        response
    }

    /// Turns `resize-window -A` / `-a` into the explicit content geometry the
    /// eligible clients of every linked session imply.
    ///
    /// `-A` is `window-size largest` applied once and `-a` is
    /// `window-size smallest` applied once, so both go through the same
    /// selector as the automatic policies: an `ignore-size`, read-only,
    /// suspended or closing client owns nothing, and the winning client's outer
    /// terminal rows lose the status rows exactly once before they land in the
    /// window's *content* geometry.
    async fn resolve_resize_window_linked_session_size(
        &self,
        mut request: rmux_proto::ResizeWindowRequest,
    ) -> Result<rmux_proto::ResizeWindowRequest, rmux_proto::RmuxError> {
        use rmux_proto::ResizeWindowAdjustment::{LargestLinkedSession, SmallestLinkedSession};

        let policy = match request.adjustment {
            Some(LargestLinkedSession) => AttachedWindowSizePolicy::Largest,
            Some(SmallestLinkedSession) => AttachedWindowSizePolicy::Smallest,
            _ => return Ok(request),
        };

        let selected = {
            let state = self.state.lock().await;
            let session = state
                .sessions
                .session(request.target.session_name())
                .ok_or_else(|| {
                    crate::pane_terminals::session_not_found(request.target.session_name())
                })?;
            let _window = session
                .window_at(request.target.window_index())
                .ok_or_else(|| {
                    rmux_proto::RmuxError::invalid_target(
                        request.target.to_string(),
                        "window index does not exist in session",
                    )
                })?;
            let fallback_size = session.terminal_size();
            let active_attach = self.active_attach.lock().await;
            let active_control = self.active_control.lock().await;
            linked_window_client_content_size(
                &state,
                &active_attach,
                &active_control,
                &request.target,
                policy,
            )
            .unwrap_or(fallback_size)
        };

        request.width = Some(selected.cols);
        request.height = Some(selected.rows);
        request.adjustment = None;
        Ok(request)
    }
}
