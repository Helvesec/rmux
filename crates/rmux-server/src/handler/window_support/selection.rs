use super::*;

impl RequestHandler {
    pub(in crate::handler) async fn handle_select_window(
        &self,
        requester_pid: Option<u32>,
        request: rmux_proto::SelectWindowRequest,
    ) -> Response {
        self.handle_select_window_with_identity(requester_pid, request, None)
            .await
    }

    pub(in crate::handler) async fn handle_select_window_with_identity(
        &self,
        requester_pid: Option<u32>,
        request: rmux_proto::SelectWindowRequest,
        identity: Option<super::super::StableTargetIdentity>,
    ) -> Response {
        let session_name = request.target.session_name().clone();
        let target_window_index = request.target.window_index();
        let (response, window_changed, resize_sessions, focus_effects, selection_event) = {
            let mut state = self.state.lock().await;
            if let Err(error) =
                super::super::require_expected_window_identity(&state, &request.target)
            {
                return Response::Error(ErrorResponse { error });
            }
            if let Some(identity) = identity.as_ref() {
                if let Err(error) = identity.require(
                    &state,
                    &Target::Window(request.target.clone()),
                    "new-window -S",
                ) {
                    return Response::Error(ErrorResponse { error });
                }
            }
            let previous_window_index = state
                .sessions
                .session(&session_name)
                .map(|session| session.active_window_index());
            let window_changed =
                previous_window_index.is_some_and(|window| window != target_window_index);
            let resize_sessions = previous_window_index
                .map(|previous| {
                    linked_resize_sessions_for_window_change(
                        &state,
                        &session_name,
                        previous,
                        target_window_index,
                    )
                })
                .unwrap_or_else(|| vec![session_name.clone()]);
            match state.select_window(request.target) {
                Ok(response) => {
                    let (focus_effects, selection_event) = if window_changed {
                        let mut active_attach = self.active_attach.lock().await;
                        let focus_effects =
                            prepare_server_focus_effects(&mut state, &mut active_attach);
                        let event = prepare_lifecycle_event(
                            &mut state,
                            &LifecycleEvent::SessionWindowChanged {
                                session_name: session_name.clone(),
                            },
                        );
                        (Some(focus_effects), Some(event))
                    } else {
                        (None, None)
                    };
                    (
                        Response::SelectWindow(response),
                        window_changed,
                        resize_sessions,
                        focus_effects,
                        selection_event,
                    )
                }
                Err(error) => (
                    Response::Error(ErrorResponse { error }),
                    false,
                    Vec::new(),
                    None,
                    None,
                ),
            }
        };

        if matches!(response, Response::SelectWindow(_)) {
            if let Some(requester_pid) = requester_pid {
                let mut active_attach = self.active_attach.lock().await;
                active_attach.seed_active_client_for_window(
                    requester_pid,
                    &session_name,
                    target_window_index,
                );
                drop(active_attach);
                self.bump_active_attach_epoch();
            }
            if window_changed {
                if let Some(focus_effects) = focus_effects {
                    let _ = focus_effects.execute(self).await;
                }
                self.emit_prepared(selection_event.expect("changed window prepares event"))
                    .await;
            }
            // tmux -S reuses the selection mutation, but does not execute the
            // select-window command's after hook (nor after-new-window).
            if identity.is_none() {
                self.queue_inline_hook(
                    HookName::AfterSelectWindow,
                    ScopeSelector::Session(session_name.clone()),
                    Some(Target::Window(rmux_proto::WindowTarget::with_window(
                        session_name.clone(),
                        target_window_index,
                    ))),
                    PendingInlineHookFormat::AfterCommand,
                );
            }
            self.reconcile_and_refresh_attached_sessions(resize_sessions)
                .await;
        }

        response
    }
}
