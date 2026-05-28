use rmux_core::LifecycleEvent;
use rmux_proto::{
    ErrorResponse, HookName, PaneTarget, Response, RmuxError, ScopeSelector, SelectPaneResponse,
    Target, WindowTarget,
};

use super::super::RequestHandler;
use crate::hook_runtime::PendingInlineHookFormat;
use crate::pane_terminals::session_not_found;

impl RequestHandler {
    pub(in crate::handler) async fn handle_last_pane(
        &self,
        request: rmux_proto::LastPaneRequest,
    ) -> Response {
        let session_name = request.target.session_name().clone();
        let response = {
            let mut state = self.state.lock().await;
            match state.last_pane(request.target) {
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
            self.refresh_attached_session(&session_name).await;
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
        let (response, pane_changed) = {
            let mut state = self.state.lock().await;
            // `set-pane-title` (title.is_some) is harmless in passthrough mode
            // — it doesn't change pane structure. Reject only true selection.
            if title.is_none() {
                if let Err(error) = crate::handler_support::reject_pane_op_in_passthrough(
                    &state,
                    &session_name,
                    "select-pane",
                ) {
                    return Response::Error(ErrorResponse { error });
                }
            }
            let pane_changed = title.is_none()
                && state
                    .sessions
                    .session(&session_name)
                    .and_then(|session| session.window_at(window_index))
                    .is_some_and(|window| window.active_pane_index() != pane_index);
            match (|| -> Result<SelectPaneResponse, RmuxError> {
                let response_target = if let Some(title) = title.as_deref() {
                    state.set_pane_title(&request.target, title)?;
                    request.target.clone()
                } else {
                    let session = state
                        .sessions
                        .session_mut(&session_name)
                        .ok_or_else(|| session_not_found(&session_name))?;
                    session.select_pane_in_window(window_index, pane_index)?;
                    let active_pane_index = session
                        .window_at(window_index)
                        .expect("selected pane window must exist")
                        .active_pane_index();
                    PaneTarget::with_window(session_name.clone(), window_index, active_pane_index)
                };

                Ok(SelectPaneResponse {
                    target: response_target,
                })
            })() {
                Ok(response) => (Response::SelectPane(response), pane_changed),
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
            if let Response::SelectPane(success) = &response {
                self.queue_inline_hook(
                    HookName::AfterSelectPane,
                    ScopeSelector::Session(session_name.clone()),
                    Some(Target::Pane(success.target.clone())),
                    PendingInlineHookFormat::AfterCommand,
                );
            }
            self.refresh_attached_session(&session_name).await;
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
            if let Err(error) = crate::handler_support::reject_pane_op_in_passthrough(
                &state,
                &session_name,
                "select-pane",
            ) {
                return Response::Error(ErrorResponse { error });
            }
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
                let active_pane_index = session.select_adjacent_pane_in_window(
                    window_index,
                    anchor_pane_index,
                    request.direction,
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
            self.refresh_attached_session(&session_name).await;
        }

        response
    }

    pub(in crate::handler) async fn handle_select_pane_mark(
        &self,
        request: rmux_proto::SelectPaneMarkRequest,
    ) -> Response {
        let session_name = request.target.session_name().clone();
        let window_index = request.target.window_index();
        let response = {
            let mut state = self.state.lock().await;
            if let Err(error) = crate::handler_support::reject_pane_op_in_passthrough(
                &state,
                &session_name,
                "select-pane -m",
            ) {
                return Response::Error(ErrorResponse { error });
            }
            match (|| -> Result<SelectPaneResponse, RmuxError> {
                if let Some(title) = request.title.as_deref() {
                    state.set_pane_title(&request.target, title)?;
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
                Ok(response) => Response::SelectPane(response),
                Err(error) => Response::Error(ErrorResponse { error }),
            }
        };

        if matches!(response, Response::SelectPane(_)) {
            self.refresh_attached_session(&session_name).await;
            self.refresh_control_session(&session_name).await;
        }

        response
    }
}
