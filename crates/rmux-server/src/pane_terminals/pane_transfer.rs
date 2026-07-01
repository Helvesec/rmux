use rmux_core::{
    BreakPaneOptions, PaneId, PaneJoinOptions, PaneSwapOptions, Session, SessionPaneTarget,
};
use rmux_proto::{
    BreakPaneRequest, BreakPaneResponse, JoinPaneRequest, JoinPaneResponse, LastPaneResponse,
    MovePaneRequest, MovePaneResponse, PaneTarget, RmuxError, SplitDirection, SwapPaneDirection,
    SwapPaneRequest, SwapPaneResponse, WindowTarget,
};

use super::{session_not_found, HandlerState};

#[path = "pane_transfer/cross_session.rs"]
mod cross_session;

impl HandlerState {
    pub(crate) fn last_pane(
        &mut self,
        target: WindowTarget,
        preserve_zoom: bool,
        input_disabled: Option<bool>,
    ) -> Result<LastPaneResponse, RmuxError> {
        let session = self
            .sessions
            .session_mut(target.session_name())
            .ok_or_else(|| session_not_found(target.session_name()))?;
        let pane_index =
            session.last_pane_in_window_with_zoom(target.window_index(), preserve_zoom)?;
        let response_target = PaneTarget::with_window(
            target.session_name().clone(),
            target.window_index(),
            pane_index,
        );
        if let Some(disabled) = input_disabled {
            self.set_pane_input_disabled(&response_target, disabled)?;
        }

        Ok(LastPaneResponse {
            target: response_target,
        })
    }

    pub(crate) fn swap_pane(
        &mut self,
        request: SwapPaneRequest,
    ) -> Result<SwapPaneResponse, RmuxError> {
        let (source, target) = resolve_swap_targets(&self.sessions, &request)?;
        if source.session_name() == target.session_name() {
            let session_name = source.session_name().clone();
            self.mutate_session_and_resize_terminals(&session_name, |session| {
                session.swap_panes(
                    SessionPaneTarget::from(&source),
                    SessionPaneTarget::from(&target),
                    PaneSwapOptions::new(request.detached, request.preserve_zoom),
                )?;
                Ok(SwapPaneResponse {
                    source: source.clone(),
                    target: target.clone(),
                })
            })
        } else {
            self.swap_pane_across_sessions(source, target, request.detached, request.preserve_zoom)
        }
    }

    pub(crate) fn join_pane(
        &mut self,
        request: JoinPaneRequest,
    ) -> Result<JoinPaneResponse, RmuxError> {
        if request.source == request.target {
            return Err(RmuxError::Server(
                "source and target panes must be different".to_owned(),
            ));
        }
        if request.source.session_name() == request.target.session_name() {
            let session_name = request.source.session_name().clone();
            let target = request.target.clone();
            let moved_pane_id = self
                .sessions
                .session(&session_name)
                .ok_or_else(|| session_not_found(&session_name))
                .and_then(|session| pane_id_for_target(session, &request.source))?;
            let direction = join_pane_internal_direction(request.direction);
            let response = self.mutate_session_and_resize_terminals(&session_name, |session| {
                session.join_pane(
                    SessionPaneTarget::from(&request.source),
                    SessionPaneTarget::from(&request.target),
                    PaneJoinOptions::new(
                        direction,
                        request.detached,
                        request.before,
                        request.full_size,
                        request.size,
                    ),
                )?;
                let moved_index = pane_index_for_id(session, target.window_index(), moved_pane_id)
                    .ok_or_else(|| {
                        RmuxError::Server("moved pane disappeared after join-pane".to_owned())
                    })?;
                Ok(JoinPaneResponse {
                    target: PaneTarget::with_window(
                        session_name.clone(),
                        target.window_index(),
                        moved_index,
                    ),
                })
            })?;
            self.clear_marked_pane_if_id(moved_pane_id);
            return Ok(response);
        }

        self.join_pane_across_sessions(request)
    }

    pub(crate) fn move_pane(
        &mut self,
        request: MovePaneRequest,
    ) -> Result<MovePaneResponse, RmuxError> {
        let response = self.join_pane(JoinPaneRequest {
            source: request.source,
            target: request.target,
            direction: request.direction,
            detached: request.detached,
            before: request.before,
            full_size: request.full_size,
            size: request.size,
        })?;
        Ok(MovePaneResponse {
            target: response.target,
        })
    }

    pub(crate) fn break_pane(
        &mut self,
        request: BreakPaneRequest,
    ) -> Result<BreakPaneResponse, RmuxError> {
        let destination_session_name = request.target.as_ref().map_or_else(
            || request.source.session_name().clone(),
            |target| target.session_name().clone(),
        );

        if request.source.session_name() == &destination_session_name {
            let session_name = request.source.session_name().clone();
            let source_pane_id = self
                .sessions
                .session(&session_name)
                .ok_or_else(|| session_not_found(&session_name))
                .and_then(|session| pane_id_for_target(session, &request.source))?;
            let destination_index =
                self.mutate_session_and_resize_terminals(&session_name, |session| {
                    session.break_pane(
                        SessionPaneTarget::from(&request.source),
                        BreakPaneOptions::new(
                            request.target.as_ref().map(WindowTarget::window_index),
                            request.name.clone(),
                            request.detached,
                            request.after,
                            request.before,
                        ),
                    )
                })?;
            self.clear_marked_pane_if_id(source_pane_id);

            return Ok(BreakPaneResponse {
                target: PaneTarget::with_window(destination_session_name, destination_index, 0),
                output: None,
            });
        }

        self.break_pane_across_sessions(request, destination_session_name)
    }
}

fn join_pane_internal_direction(direction: SplitDirection) -> SplitDirection {
    match direction {
        SplitDirection::Horizontal => SplitDirection::Vertical,
        SplitDirection::Vertical => SplitDirection::Horizontal,
    }
}

fn pane_id_for_target(session: &Session, target: &PaneTarget) -> Result<PaneId, RmuxError> {
    session
        .pane_id_in_window(target.window_index(), target.pane_index())
        .ok_or_else(|| {
            RmuxError::invalid_target(target.to_string(), "pane index does not exist in session")
        })
}

fn pane_index_for_id(session: &Session, window_index: u32, pane_id: PaneId) -> Option<u32> {
    session.window_at(window_index).and_then(|window| {
        window
            .panes()
            .iter()
            .find(|pane| pane.id() == pane_id)
            .map(|pane| pane.index())
    })
}

fn resolve_swap_targets(
    sessions: &rmux_core::SessionStore,
    request: &SwapPaneRequest,
) -> Result<(PaneTarget, PaneTarget), RmuxError> {
    if let Some(direction) = request.direction {
        let anchor = &request.target;
        let session = sessions
            .session(anchor.session_name())
            .ok_or_else(|| session_not_found(anchor.session_name()))?;
        let window = session.window_at(anchor.window_index()).ok_or_else(|| {
            RmuxError::invalid_target(
                format!("{}:{}", anchor.session_name(), anchor.window_index()),
                "window index does not exist in session",
            )
        })?;
        let anchor_position = window
            .panes()
            .iter()
            .position(|pane| pane.index() == anchor.pane_index())
            .ok_or_else(|| {
                RmuxError::invalid_target(
                    anchor.to_string(),
                    "pane index does not exist in session",
                )
            })?;
        let pane_count = window.pane_count();
        let source_position = match direction {
            SwapPaneDirection::Down => (anchor_position + 1) % pane_count,
            SwapPaneDirection::Up => (anchor_position + pane_count - 1) % pane_count,
        };
        let source_pane_index = window
            .panes()
            .get(source_position)
            .expect("resolved pane position must exist")
            .index();

        return Ok((
            PaneTarget::with_window(
                anchor.session_name().clone(),
                anchor.window_index(),
                source_pane_index,
            ),
            anchor.clone(),
        ));
    }

    Ok((request.source.clone(), request.target.clone()))
}
