use rmux_core::LifecycleEvent;
use rmux_proto::{
    ErrorResponse, NextLayoutResponse, OptionName, PaneTarget, PreviousLayoutResponse,
    ResizePaneAdjustment, ResizePaneResponse, Response, SelectLayoutResponse, WindowTarget,
};

use super::super::RequestHandler;
use crate::pane_terminals::HandlerState;

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
        let response = {
            let mut state = self.state.lock().await;
            let option_size = layout_main_pane_size_for_select_target(&state, &request.target);
            match state.mutate_session_and_resize_terminals(&session_name, |session| {
                let window_index = layout_window_index(session, &request.target);
                session.save_old_layout_in_window(window_index)?;
                session.select_layout_in_window_with_main_pane_size(
                    window_index,
                    layout,
                    option_size.width,
                    option_size.height,
                )?;
                Ok(SelectLayoutResponse { layout })
            }) {
                Ok(response) => Response::SelectLayout(response),
                Err(error) => Response::Error(ErrorResponse { error }),
            }
        };

        if matches!(response, Response::SelectLayout(_)) {
            let target = match &request.target {
                rmux_proto::SelectLayoutTarget::Session(session_name) => {
                    let state = self.state.lock().await;
                    state.sessions.session(session_name).map(|session| {
                        WindowTarget::with_window(
                            session_name.clone(),
                            session.active_window_index(),
                        )
                    })
                }
                rmux_proto::SelectLayoutTarget::Window(target) => Some(target.clone()),
            };
            if let Some(target) = target {
                self.emit(LifecycleEvent::WindowLayoutChanged { target })
                    .await;
            }
            self.refresh_attached_session(&session_name).await;
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
        let response = {
            let mut state = self.state.lock().await;
            match state.mutate_session_and_resize_terminals(&session_name, |session| {
                let window_index = layout_window_index(session, &request.target);
                session.save_old_layout_in_window(window_index)?;
                session.select_custom_layout_in_window(window_index, &request.layout)?;
                let layout = session
                    .window_at(window_index)
                    .expect("selected layout window exists")
                    .layout();
                Ok(SelectLayoutResponse { layout })
            }) {
                Ok(response) => Response::SelectLayout(response),
                Err(error) => Response::Error(ErrorResponse { error }),
            }
        };

        if matches!(response, Response::SelectLayout(_)) {
            if let Some(target) =
                layout_change_target_from_select_target(self, &request.target).await
            {
                self.emit(LifecycleEvent::WindowLayoutChanged { target })
                    .await;
            }
            self.refresh_attached_session(&session_name).await;
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
        let response = {
            let mut state = self.state.lock().await;
            match state.mutate_session_and_resize_terminals(&session_name, |session| {
                let window_index = layout_window_index(session, &request.target);
                let _ = session.reapply_old_layout_in_window(window_index)?;
                let layout = session
                    .window_at(window_index)
                    .expect("selected layout window exists")
                    .layout();
                Ok(SelectLayoutResponse { layout })
            }) {
                Ok(response) => Response::SelectLayout(response),
                Err(error) => Response::Error(ErrorResponse { error }),
            }
        };

        if matches!(response, Response::SelectLayout(_)) {
            if let Some(target) =
                layout_change_target_from_select_target(self, &request.target).await
            {
                self.emit(LifecycleEvent::WindowLayoutChanged { target })
                    .await;
            }
            self.refresh_attached_session(&session_name).await;
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
        let response = {
            let mut state = self.state.lock().await;
            match state.mutate_session_and_resize_terminals(&session_name, |session| {
                let window_index = layout_window_index(session, &request.target);
                session.save_old_layout_in_window(window_index)?;
                let _ = session.spread_layout_in_window(window_index)?;
                let layout = session
                    .window_at(window_index)
                    .expect("selected layout window exists")
                    .layout();
                Ok(SelectLayoutResponse { layout })
            }) {
                Ok(response) => Response::SelectLayout(response),
                Err(error) => Response::Error(ErrorResponse { error }),
            }
        };

        if matches!(response, Response::SelectLayout(_)) {
            if let Some(target) =
                layout_change_target_from_select_target(self, &request.target).await
            {
                self.emit(LifecycleEvent::WindowLayoutChanged { target })
                    .await;
            }
            self.refresh_attached_session(&session_name).await;
        }

        response
    }

    pub(in crate::handler) async fn handle_next_layout(
        &self,
        request: rmux_proto::NextLayoutRequest,
    ) -> Response {
        let session_name = request.target.session_name().clone();
        let response = {
            let mut state = self.state.lock().await;
            let option_size = layout_main_pane_size_for_window_target(&state, &request.target);
            match state.mutate_session_and_resize_terminals(&session_name, |session| {
                session.save_old_layout_in_window(request.target.window_index())?;
                let layout = session.next_layout_in_window_with_main_pane_size(
                    request.target.window_index(),
                    option_size.width,
                    option_size.height,
                )?;
                Ok(NextLayoutResponse { layout })
            }) {
                Ok(response) => Response::NextLayout(response),
                Err(error) => Response::Error(ErrorResponse { error }),
            }
        };

        if matches!(response, Response::NextLayout(_)) {
            self.emit(LifecycleEvent::WindowLayoutChanged {
                target: request.target,
            })
            .await;
            self.refresh_attached_session(&session_name).await;
        }

        response
    }

    pub(in crate::handler) async fn handle_previous_layout(
        &self,
        request: rmux_proto::PreviousLayoutRequest,
    ) -> Response {
        let session_name = request.target.session_name().clone();
        let response = {
            let mut state = self.state.lock().await;
            let option_size = layout_main_pane_size_for_window_target(&state, &request.target);
            match state.mutate_session_and_resize_terminals(&session_name, |session| {
                session.save_old_layout_in_window(request.target.window_index())?;
                let layout = session.previous_layout_in_window_with_main_pane_size(
                    request.target.window_index(),
                    option_size.width,
                    option_size.height,
                )?;
                Ok(PreviousLayoutResponse { layout })
            }) {
                Ok(response) => Response::PreviousLayout(response),
                Err(error) => Response::Error(ErrorResponse { error }),
            }
        };

        if matches!(response, Response::PreviousLayout(_)) {
            self.emit(LifecycleEvent::WindowLayoutChanged {
                target: request.target,
            })
            .await;
            self.refresh_attached_session(&session_name).await;
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
        let response = {
            let mut state = self.state.lock().await;
            match adjustment {
                ResizePaneAdjustment::TrimBelow => {
                    match state.trim_pane_below_cursor(&response_target) {
                        Ok(()) => Response::ResizePane(ResizePaneResponse {
                            target: response_target,
                            adjustment,
                        }),
                        Err(error) => Response::Error(ErrorResponse { error }),
                    }
                }
                _ => match state.mutate_session_and_resize_terminals(&session_name, |session| {
                    session.resize_pane_in_window(window_index, pane_index, adjustment)?;

                    Ok(ResizePaneResponse {
                        target: response_target,
                        adjustment,
                    })
                }) {
                    Ok(response) => Response::ResizePane(response),
                    Err(error) => Response::Error(ErrorResponse { error }),
                },
            }
        };

        if matches!(response, Response::ResizePane(_))
            && !matches!(adjustment, ResizePaneAdjustment::NoOp)
        {
            self.emit(LifecycleEvent::WindowLayoutChanged {
                target: WindowTarget::with_window(session_name.clone(), window_index),
            })
            .await;
            self.refresh_attached_session(&session_name).await;
        }

        response
    }
}

fn layout_window_index(
    session: &rmux_core::Session,
    target: &rmux_proto::SelectLayoutTarget,
) -> u32 {
    match target {
        rmux_proto::SelectLayoutTarget::Session(_) => session.active_window_index(),
        rmux_proto::SelectLayoutTarget::Window(target) => target.window_index(),
    }
}

#[derive(Clone, Copy, Default)]
struct LayoutMainPaneSize {
    width: Option<u16>,
    height: Option<u16>,
}

fn layout_main_pane_size_for_select_target(
    state: &HandlerState,
    target: &rmux_proto::SelectLayoutTarget,
) -> LayoutMainPaneSize {
    match target {
        rmux_proto::SelectLayoutTarget::Session(session_name) => state
            .sessions
            .session(session_name)
            .map_or_else(LayoutMainPaneSize::default, |session| {
                layout_main_pane_size_for_window(state, session_name, session.active_window_index())
            }),
        rmux_proto::SelectLayoutTarget::Window(target) => {
            layout_main_pane_size_for_window_target(state, target)
        }
    }
}

fn layout_main_pane_size_for_window_target(
    state: &HandlerState,
    target: &WindowTarget,
) -> LayoutMainPaneSize {
    layout_main_pane_size_for_window(state, target.session_name(), target.window_index())
}

fn layout_main_pane_size_for_window(
    state: &HandlerState,
    session_name: &rmux_proto::SessionName,
    window_index: u32,
) -> LayoutMainPaneSize {
    LayoutMainPaneSize {
        width: option_dimension(state.options.resolve_for_window(
            session_name,
            window_index,
            OptionName::MainPaneWidth,
        )),
        height: option_dimension(state.options.resolve_for_window(
            session_name,
            window_index,
            OptionName::MainPaneHeight,
        )),
    }
}

fn option_dimension(value: Option<&str>) -> Option<u16> {
    value.and_then(|value| value.parse::<u16>().ok())
}

async fn layout_change_target_from_select_target(
    handler: &RequestHandler,
    target: &rmux_proto::SelectLayoutTarget,
) -> Option<WindowTarget> {
    match target {
        rmux_proto::SelectLayoutTarget::Session(session_name) => {
            let state = handler.state.lock().await;
            state.sessions.session(session_name).map(|session| {
                WindowTarget::with_window(session_name.clone(), session.active_window_index())
            })
        }
        rmux_proto::SelectLayoutTarget::Window(target) => Some(target.clone()),
    }
}
