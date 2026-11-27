use rmux_core::LifecycleEvent;
use rmux_proto::{
    ErrorResponse, NextLayoutResponse, PaneId, PaneTarget, PreviousLayoutResponse,
    ResizePaneAdjustment, ResizePaneResponse, Response, RmuxError, SelectLayoutResponse,
    WindowTarget,
};

use super::super::{prepare_lifecycle_event, RequestHandler};
use crate::pane_terminals::{session_not_found, HandlerState, WindowRuntimeResizeScope};

mod layout_options;

use self::layout_options::{
    layout_option_values_for_select_target, layout_option_values_for_window_target,
};

impl RequestHandler {
    pub(in crate::handler) async fn handle_select_layout(
        &self,
        request: rmux_proto::SelectLayoutRequest,
    ) -> Response {
        let layout = request.layout;
        let session_name = match &request.target {
            rmux_proto::SelectLayoutTarget::Session(session_name) => session_name.clone(),
            rmux_proto::SelectLayoutTarget::Window(target) => target.session_name().clone(),
        };
        let (response, target, refresh_sessions) = {
            let mut state = self.state.lock().await;
            let options = layout_option_values_for_select_target(&state, &request.target);
            let window_index = match layout_window_index_for_state(&state, &request.target) {
                Ok(window_index) => window_index,
                Err(error) => return Response::Error(ErrorResponse { error }),
            };
            let target = WindowTarget::with_window(session_name.clone(), window_index);
            match state.mutate_select_layout_and_resize_terminal_with_family(
                &target,
                |window_layout| {
                    window_layout.save_old_layout();
                    window_layout.select_layout_with_tiled_max_columns(
                        layout,
                        options.main_width,
                        options.main_height,
                        options.tiled_max_columns,
                    );
                    Ok(SelectLayoutResponse { layout })
                },
            ) {
                Ok((response, refresh_sessions)) => {
                    (Response::SelectLayout(response), target, refresh_sessions)
                }
                Err(error) => (Response::Error(ErrorResponse { error }), target, Vec::new()),
            }
        };

        if matches!(response, Response::SelectLayout(_)) {
            self.emit(LifecycleEvent::WindowLayoutChanged { target })
                .await;
            self.refresh_linked_window_sessions(refresh_sessions).await;
        }

        response
    }

    pub(in crate::handler) async fn handle_select_custom_layout(
        &self,
        request: rmux_proto::SelectCustomLayoutRequest,
    ) -> Response {
        let session_name = match &request.target {
            rmux_proto::SelectLayoutTarget::Session(session_name) => session_name.clone(),
            rmux_proto::SelectLayoutTarget::Window(target) => target.session_name().clone(),
        };
        let (response, layout_event, refresh_sessions) = {
            let mut state = self.state.lock().await;
            let window_index = match layout_window_index_for_state(&state, &request.target) {
                Ok(window_index) => window_index,
                Err(error) => return Response::Error(ErrorResponse { error }),
            };
            let target = WindowTarget::with_window(session_name.clone(), window_index);
            let (response, refresh_sessions) = match state
                .mutate_select_layout_and_resize_terminal_with_family(&target, |window_layout| {
                    window_layout.save_old_layout();
                    window_layout.select_custom_layout(&request.layout)?;
                    let layout = window_layout.layout();
                    Ok(SelectLayoutResponse { layout })
                }) {
                Ok((response, refresh_sessions)) => {
                    (Response::SelectLayout(response), refresh_sessions)
                }
                Err(error) => (Response::Error(ErrorResponse { error }), Vec::new()),
            };
            let layout_event = matches!(response, Response::SelectLayout(_)).then(|| {
                prepare_lifecycle_event(&mut state, &LifecycleEvent::WindowLayoutChanged { target })
            });
            (response, layout_event, refresh_sessions)
        };

        if let Some(layout_event) = layout_event {
            self.emit_prepared(layout_event).await;
            self.refresh_linked_window_sessions(refresh_sessions).await;
        }

        response
    }

    pub(in crate::handler) async fn handle_select_old_layout(
        &self,
        request: rmux_proto::SelectOldLayoutRequest,
    ) -> Response {
        let session_name = match &request.target {
            rmux_proto::SelectLayoutTarget::Session(session_name) => session_name.clone(),
            rmux_proto::SelectLayoutTarget::Window(target) => target.session_name().clone(),
        };
        let (response, layout_event, refresh_sessions) = {
            let mut state = self.state.lock().await;
            let window_index = match layout_window_index_for_state(&state, &request.target) {
                Ok(window_index) => window_index,
                Err(error) => return Response::Error(ErrorResponse { error }),
            };
            let target = WindowTarget::with_window(session_name.clone(), window_index);
            let (response, refresh_sessions) = match state
                .mutate_select_layout_and_resize_terminal_with_family(&target, |window_layout| {
                    let _ = window_layout.reapply_old_layout()?;
                    let layout = window_layout.layout();
                    Ok(SelectLayoutResponse { layout })
                }) {
                Ok((response, refresh_sessions)) => {
                    (Response::SelectLayout(response), refresh_sessions)
                }
                Err(error) => (Response::Error(ErrorResponse { error }), Vec::new()),
            };
            let layout_event = matches!(response, Response::SelectLayout(_)).then(|| {
                prepare_lifecycle_event(&mut state, &LifecycleEvent::WindowLayoutChanged { target })
            });
            (response, layout_event, refresh_sessions)
        };

        if let Some(layout_event) = layout_event {
            self.emit_prepared(layout_event).await;
            self.refresh_linked_window_sessions(refresh_sessions).await;
        }

        response
    }

    pub(in crate::handler) async fn handle_spread_layout(
        &self,
        request: rmux_proto::SpreadLayoutRequest,
    ) -> Response {
        let session_name = match &request.target {
            rmux_proto::SelectLayoutTarget::Session(session_name) => session_name.clone(),
            rmux_proto::SelectLayoutTarget::Window(target) => target.session_name().clone(),
        };
        let (response, target, refresh_sessions) = {
            let mut state = self.state.lock().await;
            let window_index = match layout_window_index_for_state(&state, &request.target) {
                Ok(window_index) => window_index,
                Err(error) => return Response::Error(ErrorResponse { error }),
            };
            let target = WindowTarget::with_window(session_name.clone(), window_index);
            match state.mutate_select_layout_and_resize_terminal_with_family(
                &target,
                |window_layout| {
                    window_layout.save_old_layout();
                    let _ = window_layout.spread_layout();
                    let layout = window_layout.layout();
                    Ok(SelectLayoutResponse { layout })
                },
            ) {
                Ok((response, refresh_sessions)) => {
                    (Response::SelectLayout(response), target, refresh_sessions)
                }
                Err(error) => (Response::Error(ErrorResponse { error }), target, Vec::new()),
            }
        };

        if matches!(response, Response::SelectLayout(_)) {
            self.emit(LifecycleEvent::WindowLayoutChanged { target })
                .await;
            self.refresh_linked_window_sessions(refresh_sessions).await;
        }

        response
    }

    pub(in crate::handler) async fn handle_next_layout(
        &self,
        request: rmux_proto::NextLayoutRequest,
    ) -> Response {
        let (response, refresh_sessions) = {
            let mut state = self.state.lock().await;
            let options = layout_option_values_for_window_target(&state, &request.target);
            match state.mutate_select_layout_and_resize_terminal_with_family(
                &request.target,
                |window_layout| {
                    window_layout.save_old_layout();
                    let layout = window_layout.next_layout_with_tiled_max_columns(
                        options.main_width,
                        options.main_height,
                        options.tiled_max_columns,
                    );
                    Ok(NextLayoutResponse { layout })
                },
            ) {
                Ok((response, refresh_sessions)) => {
                    (Response::NextLayout(response), refresh_sessions)
                }
                Err(error) => (Response::Error(ErrorResponse { error }), Vec::new()),
            }
        };

        if matches!(response, Response::NextLayout(_)) {
            self.emit(LifecycleEvent::WindowLayoutChanged {
                target: request.target,
            })
            .await;
            self.refresh_linked_window_sessions(refresh_sessions).await;
        }

        response
    }

    pub(in crate::handler) async fn handle_previous_layout(
        &self,
        request: rmux_proto::PreviousLayoutRequest,
    ) -> Response {
        let (response, refresh_sessions) = {
            let mut state = self.state.lock().await;
            let options = layout_option_values_for_window_target(&state, &request.target);
            match state.mutate_select_layout_and_resize_terminal_with_family(
                &request.target,
                |window_layout| {
                    window_layout.save_old_layout();
                    let layout = window_layout.previous_layout_with_tiled_max_columns(
                        options.main_width,
                        options.main_height,
                        options.tiled_max_columns,
                    );
                    Ok(PreviousLayoutResponse { layout })
                },
            ) {
                Ok((response, refresh_sessions)) => {
                    (Response::PreviousLayout(response), refresh_sessions)
                }
                Err(error) => (Response::Error(ErrorResponse { error }), Vec::new()),
            }
        };

        if matches!(response, Response::PreviousLayout(_)) {
            self.emit(LifecycleEvent::WindowLayoutChanged {
                target: request.target,
            })
            .await;
            self.refresh_linked_window_sessions(refresh_sessions).await;
        }

        response
    }

    pub(in crate::handler) async fn handle_resize_pane(
        &self,
        request: rmux_proto::ResizePaneRequest,
    ) -> Response {
        let session_name = request.target.session_name().clone();
        let window_index = request.target.window_index();
        let pane_index = request.target.pane_index();
        let adjustment = request.adjustment;
        let response_target =
            PaneTarget::with_window(session_name.clone(), window_index, pane_index);
        let (response, refresh_sessions, layout_changed) = {
            let mut state = self.state.lock().await;
            if let Err(error) =
                super::super::require_expected_pane_identity(&state, &response_target)
            {
                return Response::Error(ErrorResponse { error });
            }
            let pane_id = match crate::pane_terminal_lookup::pane_id_for_target(
                &state.sessions,
                &session_name,
                window_index,
                pane_index,
            ) {
                Ok(pane_id) => pane_id,
                Err(error) => return Response::Error(ErrorResponse { error }),
            };
            let window = state
                .sessions
                .session(&session_name)
                .and_then(|session| session.window_at(window_index))
                .expect("validated resize target window remains stored");
            let before_sizes = window
                .panes()
                .iter()
                .map(|pane| (pane.id(), (pane.geometry().cols(), pane.geometry().rows())))
                .collect::<std::collections::BTreeMap<_, _>>();
            match adjustment {
                ResizePaneAdjustment::TrimBelow => {
                    let response = match state.trim_pane_below_cursor(&response_target) {
                        Ok(()) => Response::ResizePane(ResizePaneResponse {
                            target: response_target,
                            adjustment,
                        }),
                        Err(error) => Response::Error(ErrorResponse { error }),
                    };
                    (response, Vec::new(), false)
                }
                _ => {
                    let window_target =
                        WindowTarget::with_window(session_name.clone(), window_index);
                    match state
                        .mutate_store_and_resize_window_terminal_with_family_scoped(
                            &session_name,
                            window_index,
                            |store| {
                                let window_layout =
                                    store.window_layout_context_mut(&window_target)?;
                                let (layout_changed, zoomed_after) = resize_pane_layout_outcome(
                                    window_layout,
                                    pane_index,
                                    adjustment,
                                )?;

                                Ok((
                                    ResizePaneResponse {
                                        target: response_target,
                                        adjustment,
                                    },
                                    layout_changed,
                                    zoomed_after,
                                ))
                            },
                            |state, _| {
                                let window = state
                                    .sessions
                                    .session(&session_name)
                                    .and_then(|session| session.window_at(window_index))
                                    .expect("resized canonical window remains stored");
                                let changed = window
                                    .panes()
                                    .iter()
                                    .filter_map(|pane| {
                                        let after =
                                            (pane.geometry().cols(), pane.geometry().rows());
                                        (before_sizes.get(&pane.id()).copied() != Some(after))
                                            .then_some(pane.id())
                                    })
                                    .collect::<Vec<_>>();
                                if !changed.is_empty() {
                                    // The named pane fails fast before its
                                    // resized layout stays committed.
                                    state.ensure_pane_id_available(
                                        &session_name,
                                        window_index,
                                        pane_id,
                                    )?;
                                }
                                Ok((!changed.is_empty())
                                    .then_some(WindowRuntimeResizeScope::Panes(changed)))
                            },
                        )
                        .map_err(RmuxError::from)
                    {
                        Ok(((response, layout_changed, _), refresh_sessions)) => (
                            Response::ResizePane(response),
                            refresh_sessions,
                            layout_changed,
                        ),
                        Err(error) => (Response::Error(ErrorResponse { error }), Vec::new(), false),
                    }
                }
            }
        };

        if matches!(response, Response::ResizePane(_)) {
            if layout_changed {
                self.emit(LifecycleEvent::WindowLayoutChanged {
                    target: WindowTarget::with_window(session_name.clone(), window_index),
                })
                .await;
            }
            if !matches!(adjustment, ResizePaneAdjustment::NoOp) {
                // See handle_select_pane: skip the refresh (and its
                // deferred-pane wait on Windows) when nothing is attached to
                // the session.
                if refresh_sessions.is_empty() {
                    if self.attached_count(&session_name).await > 0 {
                        self.refresh_attached_session(&session_name).await;
                    }
                } else {
                    self.refresh_linked_window_sessions(refresh_sessions).await;
                }
            }
        }

        response
    }
}

#[derive(PartialEq, Eq)]
pub(super) struct LayoutNotificationState {
    pub(super) layout: String,
    pub(super) floating_stack: rmux_core::FloatingStack,
    pub(super) zoomed: bool,
    pub(super) active_pane_id: Option<PaneId>,
    pub(super) last_pane_id: Option<PaneId>,
}

fn resize_pane_layout_outcome(
    mut window_layout: rmux_core::WindowLayoutContext<'_>,
    pane_index: u32,
    adjustment: ResizePaneAdjustment,
) -> Result<(bool, bool), rmux_proto::RmuxError> {
    let before = layout_notification_state(&window_layout);
    window_layout.resize_pane(pane_index, adjustment)?;
    let after = layout_notification_state(&window_layout);
    let zoomed_after = after.zoomed;
    Ok((before != after, zoomed_after))
}

pub(super) fn layout_notification_state(
    window_layout: &rmux_core::WindowLayoutContext<'_>,
) -> LayoutNotificationState {
    LayoutNotificationState {
        layout: window_layout.layout_dump(),
        floating_stack: window_layout.floating_stack(),
        zoomed: window_layout.is_zoomed(),
        active_pane_id: window_layout.active_pane_id(),
        last_pane_id: window_layout.last_pane_id(),
    }
}

fn layout_window_index(
    session: rmux_core::ManagedSessionView<'_>,
    target: &rmux_proto::SelectLayoutTarget,
) -> u32 {
    match target {
        rmux_proto::SelectLayoutTarget::Session(_) => session.active_window_index(),
        rmux_proto::SelectLayoutTarget::Window(target) => target.window_index(),
    }
}

fn layout_window_index_for_state(
    state: &HandlerState,
    target: &rmux_proto::SelectLayoutTarget,
) -> Result<u32, rmux_proto::RmuxError> {
    let session_name = match target {
        rmux_proto::SelectLayoutTarget::Session(session_name) => session_name,
        rmux_proto::SelectLayoutTarget::Window(target) => target.session_name(),
    };
    let session = state
        .sessions
        .session(session_name)
        .ok_or_else(|| session_not_found(session_name))?;
    Ok(layout_window_index(session, target))
}

impl RequestHandler {
    pub(in crate::handler) async fn handle_resize_pane_axes(
        &self,
        request: rmux_proto::ResizePaneAxesRequest,
    ) -> Response {
        if let Some(columns) = request.columns {
            let response = self
                .handle_resize_pane(rmux_proto::ResizePaneRequest {
                    target: request.target.clone(),
                    adjustment: ResizePaneAdjustment::AbsoluteWidth { columns },
                })
                .await;
            if matches!(response, Response::Error(_)) {
                return response;
            }
        }
        match request.rows {
            Err(error) => Response::Error(ErrorResponse { error }),
            Ok(rows) => {
                self.handle_resize_pane(rmux_proto::ResizePaneRequest {
                    target: request.target,
                    adjustment: rows.map_or(ResizePaneAdjustment::NoOp, |rows| {
                        ResizePaneAdjustment::AbsoluteHeight { rows }
                    }),
                })
                .await
            }
        }
    }
}
