use rmux_core::LifecycleEvent;
use rmux_proto::{
    types::OptionScopeSelector, ErrorResponse, HookName, OptionName, PaneTarget, Response,
    RmuxError, ScopeSelector, SelectPaneResponse, SetOptionMode, Target, WindowTarget,
};

use super::super::RequestHandler;
use crate::hook_runtime::PendingInlineHookFormat;
use crate::pane_state_journal::PaneStateChange;
use crate::pane_terminals::session_not_found;

impl RequestHandler {
    pub(in crate::handler) async fn handle_last_pane(
        &self,
        request: rmux_proto::LastPaneRequest,
    ) -> Response {
        let session_name = request.target.session_name().clone();
        let response = {
            let mut state = self.state.lock().await;
            match state.last_pane(
                request.target,
                request.preserve_zoom,
                request.input_disabled,
            ) {
                Ok(response) => Response::LastPane(response),
                Err(error) => Response::Error(ErrorResponse { error }),
            }
        };

        if matches!(response, Response::LastPane(_)) {
            if let Response::LastPane(success) = &response {
                self.emit(LifecycleEvent::WindowPaneChanged {
                    target: WindowTarget::with_window(
                        session_name.clone(),
                        success.target.window_index(),
                    ),
                })
                .await;
                self.queue_inline_hook(
                    HookName::AfterSelectPane,
                    ScopeSelector::Session(session_name.clone()),
                    Some(Target::Pane(success.target.clone())),
                    PendingInlineHookFormat::AfterCommand,
                );
            }
            // See handle_select_pane below: skip refresh on a session with no
            // attached/control client so an unrelated deferred pane wait cannot
            // stall a detached select.
            if self.attached_count(&session_name).await > 0 {
                self.refresh_attached_session(&session_name).await;
            }
        }

        response
    }

    pub(in crate::handler) async fn handle_select_pane(
        &self,
        request: rmux_proto::SelectPaneRequest,
    ) -> Response {
        let session_name = request.target.session_name().clone();
        let window_index = request.target.window_index();
        let pane_index = request.target.pane_index();
        let title = request.title.clone();
        let style = request.style.clone();
        let input_disabled = request.input_disabled;
        let (response, pane_changed, title_changed_target) = {
            let mut state = self.state.lock().await;
            let pane_changed = title.is_none()
                && input_disabled.is_none()
                && state
                    .sessions
                    .session(&session_name)
                    .and_then(|session| session.window_at(window_index))
                    .is_some_and(|window| window.active_pane_index() != pane_index);
            let mut title_changed_target = None;
            let mut title_state_event = None;
            let mut pane_option_event = None;
            match (|| -> Result<SelectPaneResponse, RmuxError> {
                if let Some(style) = style.as_deref() {
                    let scope = OptionScopeSelector::Pane(request.target.clone());
                    rmux_core::validate_option_name_mutation(
                        "window-style",
                        &scope,
                        SetOptionMode::Replace,
                        Some(style),
                        false,
                    )?;
                }
                let response_target = if let Some(title) = title.as_deref() {
                    if let Some((old, new)) = state.set_pane_title(&request.target, title)? {
                        title_changed_target = Some(request.target.clone());
                        if let Some(pane_id) = pane_id_for_select_target(&state, &request.target) {
                            let generation =
                                state.pane_output_generation_for_target(&request.target, pane_id);
                            title_state_event = Some((pane_id, generation, old, new));
                        }
                    }
                    request.target.clone()
                } else if let Some(disabled) = input_disabled {
                    state.set_pane_input_disabled(&request.target, disabled)?;
                    request.target.clone()
                } else {
                    let session = state
                        .sessions
                        .session_mut(&session_name)
                        .ok_or_else(|| session_not_found(&session_name))?;
                    session.select_pane_in_window_with_zoom(
                        window_index,
                        pane_index,
                        request.preserve_zoom,
                    )?;
                    let active_pane_index = session
                        .window_at(window_index)
                        .expect("selected pane window must exist")
                        .active_pane_index();
                    PaneTarget::with_window(session_name.clone(), window_index, active_pane_index)
                };
                if let Some(style) = style {
                    let outcome = state.options.set_by_name(
                        OptionScopeSelector::Pane(request.target.clone()),
                        "window-style",
                        Some(style),
                        SetOptionMode::Replace,
                        false,
                        false,
                        false,
                    )?;
                    state.synchronize_pane_alias_options_from_target(&request.target)?;
                    if outcome.changed {
                        if let Some(pane_id) = pane_id_for_select_target(&state, &request.target) {
                            let generation =
                                state.pane_output_generation_for_target(&request.target, pane_id);
                            pane_option_event = Some((pane_id, generation, outcome));
                        }
                    }
                    state.refresh_transcript_limits_for_scope(
                        &ScopeSelector::Pane(request.target.clone()),
                        OptionName::WindowStyle,
                    );
                }

                Ok(SelectPaneResponse {
                    target: response_target,
                })
            })() {
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
                    if let Some((pane_id, generation, outcome)) = pane_option_event.as_ref() {
                        self.record_pane_option_mutation(*pane_id, Some(*generation), outcome);
                    }
                    (
                        Response::SelectPane(response),
                        pane_changed,
                        title_changed_target,
                    )
                }
                Err(error) => (Response::Error(ErrorResponse { error }), false, None),
            }
        };

        if matches!(response, Response::SelectPane(_)) {
            if pane_changed {
                self.emit(LifecycleEvent::WindowPaneChanged {
                    target: WindowTarget::with_window(session_name.clone(), window_index),
                })
                .await;
            }
            if let Some(target) = title_changed_target {
                self.emit(LifecycleEvent::PaneTitleChanged { target }).await;
            }
            if let Response::SelectPane(success) = &response {
                self.queue_inline_hook(
                    HookName::AfterSelectPane,
                    ScopeSelector::Session(session_name.clone()),
                    Some(Target::Pane(success.target.clone())),
                    PendingInlineHookFormat::AfterCommand,
                );
            }
            // A select-pane on a session with no attached or control client has
            // nothing to draw, so skip the refresh — and, on Windows, its
            // deferred-pane wait — instead of stalling ~2s on the session's own
            // just-spawned pane while it is still starting
            // (unrelated_starting_pane_does_not_block_a_stable_session_refresh).
            if self.attached_count(&session_name).await > 0 {
                self.refresh_attached_session(&session_name).await;
            }
        }

        response
    }

    pub(in crate::handler) async fn handle_select_pane_adjacent(
        &self,
        request: rmux_proto::SelectPaneAdjacentRequest,
    ) -> Response {
        let session_name = request.target.session_name().clone();
        let window_index = request.target.window_index();
        let anchor_pane_index = request.target.pane_index();
        let (response, pane_changed) = {
            let mut state = self.state.lock().await;
            let active_before = state
                .sessions
                .session(&session_name)
                .and_then(|session| session.window_at(window_index))
                .map(|window| window.active_pane_index());
            match (|| -> Result<SelectPaneResponse, RmuxError> {
                let session = state
                    .sessions
                    .session_mut(&session_name)
                    .ok_or_else(|| session_not_found(&session_name))?;
                let active_pane_index = session.select_adjacent_pane_in_window_with_zoom(
                    window_index,
                    anchor_pane_index,
                    request.direction,
                    request.preserve_zoom,
                )?;
                Ok(SelectPaneResponse {
                    target: PaneTarget::with_window(
                        session_name.clone(),
                        window_index,
                        active_pane_index,
                    ),
                })
            })() {
                Ok(response) => {
                    let pane_changed =
                        active_before.is_some_and(|before| before != response.target.pane_index());
                    (Response::SelectPane(response), pane_changed)
                }
                Err(error) => (Response::Error(ErrorResponse { error }), false),
            }
        };

        if matches!(response, Response::SelectPane(_)) {
            if pane_changed {
                self.emit(LifecycleEvent::WindowPaneChanged {
                    target: WindowTarget::with_window(session_name.clone(), window_index),
                })
                .await;
            }
            if self.attached_count(&session_name).await > 0 {
                self.refresh_attached_session(&session_name).await;
            }
        }

        response
    }

    pub(in crate::handler) async fn handle_select_pane_mark(
        &self,
        request: rmux_proto::SelectPaneMarkRequest,
    ) -> Response {
        let session_name = request.target.session_name().clone();
        let window_index = request.target.window_index();
        let (response, title_changed_target) = {
            let mut state = self.state.lock().await;
            let mut title_changed_target = None;
            let mut title_state_event = None;
            match (|| -> Result<SelectPaneResponse, RmuxError> {
                if let Some(title) = request.title.as_deref() {
                    if let Some((old, new)) = state.set_pane_title(&request.target, title)? {
                        title_changed_target = Some(request.target.clone());
                        if let Some(pane_id) = pane_id_for_select_target(&state, &request.target) {
                            let generation =
                                state.pane_output_generation_for_target(&request.target, pane_id);
                            title_state_event = Some((pane_id, generation, old, new));
                        }
                    }
                }
                if request.clear {
                    state.clear_marked_pane();
                } else {
                    let _ = state.toggle_marked_pane(&request.target)?;
                }

                let session = state
                    .sessions
                    .session(&session_name)
                    .ok_or_else(|| session_not_found(&session_name))?;
                let active_pane_index = session
                    .window_at(window_index)
                    .ok_or_else(|| {
                        RmuxError::invalid_target(
                            format!("{session_name}:{window_index}"),
                            "window index does not exist in session",
                        )
                    })?
                    .active_pane_index();
                Ok(SelectPaneResponse {
                    target: PaneTarget::with_window(
                        session_name.clone(),
                        window_index,
                        active_pane_index,
                    ),
                })
            })() {
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
                    (Response::SelectPane(response), title_changed_target)
                }
                Err(error) => (Response::Error(ErrorResponse { error }), None),
            }
        };

        if matches!(response, Response::SelectPane(_)) {
            if let Some(target) = title_changed_target {
                self.emit(LifecycleEvent::PaneTitleChanged { target }).await;
            }
            if self.attached_count(&session_name).await > 0 {
                self.refresh_attached_session(&session_name).await;
            }
            self.refresh_control_session(&session_name).await;
        }

        response
    }
}

fn pane_id_for_select_target(
    state: &crate::pane_terminals::HandlerState,
    target: &PaneTarget,
) -> Option<rmux_core::PaneId> {
    state
        .sessions
        .session(target.session_name())
        .and_then(|session| session.window_at(target.window_index()))
        .and_then(|window| window.pane(target.pane_index()))
        .map(|pane| pane.id())
}
