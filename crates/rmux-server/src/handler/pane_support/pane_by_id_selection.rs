use super::super::{
    focus_support::prepare_server_focus_effects, prepare_lifecycle_event, RequestHandler,
};
use super::resolve_pane_target_ref;
use crate::hook_runtime::PendingInlineHookFormat;
use crate::pane_state_journal::PaneStateChange;
use crate::pane_terminals::HandlerState;
use rmux_core::LifecycleEvent;
use rmux_proto::{
    ErrorResponse, HookName, PaneSelectRequest, PaneTarget, Response, RmuxError, ScopeSelector,
    SelectPaneResponse, Target, WindowTarget,
};

impl RequestHandler {
    pub(in crate::handler) async fn handle_pane_select_ref(
        &self,
        request: PaneSelectRequest,
    ) -> Response {
        let session_name = request.target.session_name().clone();
        let title = request.title.clone();
        #[cfg(windows)]
        let mut waited_for_deferred_session = false;

        // The retry is compiled only on Windows; other targets execute this
        // body exactly once after the conditional branch is removed.
        #[cfg_attr(not(windows), allow(clippy::never_loop))]
        let (
            response,
            pane_changed,
            _window_index,
            title_changed_target,
            refresh_sessions,
            focus_effects,
            selection_event,
        ) = loop {
            let mut state = self.state.lock().await;
            let target = match resolve_pane_target_ref(&state, &request.target) {
                Ok(target) => target,
                Err(error) => return Response::Error(ErrorResponse { error }),
            };
            if let Err(error) =
                super::super::require_expected_session_identity(&state, target.session_name())
            {
                return Response::Error(ErrorResponse { error });
            }
            let window_index = target.window_index();
            let pane_index = target.pane_index();
            let pane_changed = title.is_none()
                && state
                    .sessions
                    .session(&session_name)
                    .and_then(|session| session.window_at(window_index))
                    .is_some_and(|window| window.active_pane_index() != pane_index);

            // A changed selection resizes every terminal in its window. On
            // Windows, defer that mutation until ConPTY startup has settled.
            // Keep the no-op decision and the mutation under this same state
            // lock so a concurrent selection cannot slip between them.
            #[cfg(windows)]
            if pane_changed && !waited_for_deferred_session {
                drop(state);
                self.wait_for_windows_deferred_target_pane_pids(&Target::Session(
                    session_name.clone(),
                ))
                .await;
                waited_for_deferred_session = true;
                continue;
            }

            let mut title_changed_target = None;
            let mut title_state_event = None;
            let mut refresh_sessions = Vec::new();
            let result = (|| -> Result<SelectPaneResponse, RmuxError> {
                let response_target = if let Some(title) = title.as_deref() {
                    if let Some((old, new)) = state.set_pane_title(&target, title)? {
                        title_changed_target = Some(target.clone());
                        if let Some(pane_id) = pane_id_for_select_target(&state, &target) {
                            let generation =
                                state.pane_output_generation_for_target(&target, pane_id);
                            title_state_event = Some((pane_id, generation, old, new));
                        }
                    }
                    target.clone()
                } else {
                    let window_target =
                        WindowTarget::with_window(session_name.clone(), window_index);
                    let select_target = |selection: rmux_core::WindowSelection<'_>| {
                        selection.select(pane_index, false)
                    };
                    let (active_pane_index, synchronized_sessions) = if pane_changed {
                        state
                            .mutate_window_naming_pane_and_resize_terminal_with_family_if(
                                &target,
                                select_target,
                                |_| true,
                            )
                            .map_err(RmuxError::from)
                    } else {
                        state.mutate_window_and_synchronize_family(&window_target, select_target)
                    }?;
                    refresh_sessions = synchronized_sessions;
                    PaneTarget::with_window(session_name.clone(), window_index, active_pane_index)
                };
                Ok(SelectPaneResponse {
                    target: response_target,
                })
            })();

            break match result {
                Ok(response) => {
                    if let Some((pane_id, generation, old, new)) = &title_state_event {
                        self.record_pane_state_change(
                            *pane_id,
                            Some(*generation),
                            PaneStateChange::TitleChanged {
                                old: old.clone(),
                                new: new.clone(),
                            },
                        );
                    }
                    let (focus_effects, selection_event) = if pane_changed {
                        let mut active_attach = self.active_attach.lock().await;
                        let focus_effects =
                            prepare_server_focus_effects(&mut state, &mut active_attach);
                        let event = prepare_lifecycle_event(
                            &mut state,
                            &LifecycleEvent::WindowPaneChanged {
                                target: WindowTarget::with_window(
                                    session_name.clone(),
                                    window_index,
                                ),
                            },
                        );
                        (Some(focus_effects), Some(event))
                    } else {
                        (None, None)
                    };
                    (
                        Response::SelectPane(response),
                        pane_changed,
                        window_index,
                        title_changed_target,
                        refresh_sessions,
                        focus_effects,
                        selection_event,
                    )
                }
                Err(error) => (
                    Response::Error(ErrorResponse { error }),
                    false,
                    window_index,
                    None,
                    Vec::new(),
                    None,
                    None,
                ),
            };
        };

        if matches!(response, Response::SelectPane(_)) {
            if pane_changed {
                if let Some(focus_effects) = focus_effects {
                    let _ = focus_effects.execute(self).await;
                }
                self.emit_prepared(selection_event.expect("changed pane prepares event"))
                    .await;
            }
            if let Some(target) = title_changed_target {
                self.emit(LifecycleEvent::PaneTitleChanged { target }).await;
            }
            if pane_changed {
                let Response::SelectPane(success) = &response else {
                    unreachable!("successful pane select response was checked above");
                };
                self.queue_inline_hook(
                    HookName::AfterSelectPane,
                    ScopeSelector::Session(session_name.clone()),
                    Some(Target::Pane(success.target.clone())),
                    PendingInlineHookFormat::AfterCommand,
                );
            } else {
                self.queue_suppressed_inline_hook(
                    HookName::AfterSelectPane,
                    PendingInlineHookFormat::AfterCommand,
                );
            }
            // See handle_select_pane in handler_pane/selection.rs: skip the
            // refresh (and its Windows deferred-pane wait) when nothing is
            // attached so a still-starting sibling cannot stall a detached
            // select via the pane-id-typed SDK API either.
            if refresh_sessions.is_empty() {
                if self.attached_count(&session_name).await > 0 {
                    self.refresh_attached_session(&session_name).await;
                }
            } else {
                self.refresh_linked_window_sessions(refresh_sessions).await;
            }
        }

        response
    }
}

fn pane_id_for_select_target(
    state: &HandlerState,
    target: &PaneTarget,
) -> Option<rmux_core::PaneId> {
    state
        .sessions
        .session(target.session_name())
        .and_then(|session| session.window_at(target.window_index()))
        .and_then(|window| window.pane(target.pane_index()))
        .map(|pane| pane.id())
}
