use rmux_core::{PaneId, Session};
use rmux_proto::{
    BreakPaneRequest, BreakPaneResponse, JoinPaneRequest, JoinPaneResponse, LastPaneResponse,
    MovePaneRequest, MovePaneResponse, PaneTarget, RmuxError, SplitDirection, SwapPaneDirection,
    SwapPaneRequest, SwapPaneResponse, WindowTarget,
};

use super::{session_not_found, HandlerState};

#[path = "pane_transfer/cross_session.rs"]
mod cross_session;
#[path = "pane_transfer/grouped.rs"]
mod grouped;
#[path = "pane_transfer/linked_last.rs"]
mod linked_last;
#[path = "pane_transfer/window_metadata.rs"]
mod window_metadata;

impl HandlerState {
    pub(crate) fn last_pane(
        &mut self,
        target: WindowTarget,
        preserve_zoom: bool,
        input_disabled: Option<bool>,
    ) -> Result<(LastPaneResponse, Vec<rmux_proto::SessionName>), RmuxError> {
        let session_name = target.session_name().clone();
        let window_index = target.window_index();
        let (pane_index, synchronized_sessions) = self
            .mutate_session_and_resize_window_terminal_with_family(
                &session_name,
                window_index,
                |session| session.last_pane_in_window_with_zoom(window_index, preserve_zoom),
            )?;
        let response_target = PaneTarget::with_window(session_name, window_index, pane_index);
        if let Some(disabled) = input_disabled {
            self.set_pane_input_disabled(&response_target, disabled)?;
        }

        Ok((
            LastPaneResponse {
                target: response_target,
            },
            synchronized_sessions,
        ))
    }

    pub(crate) fn swap_pane(
        &mut self,
        request: SwapPaneRequest,
    ) -> Result<SwapPaneResponse, RmuxError> {
        let (source, target) = resolve_swap_targets(&self.sessions, &request)?;
        if pane_targets_share_window_identity(&self.sessions, &source, &target) {
            let normalized_target = PaneTarget::with_window(
                source.session_name().clone(),
                source.window_index(),
                target.pane_index(),
            );
            self.swap_pane_within_group(
                source.clone(),
                normalized_target,
                request.detached,
                request.preserve_zoom,
            )?;
            return Ok(SwapPaneResponse { source, target });
        }
        if sessions_share_grouped_window_state(
            &self.sessions,
            source.session_name(),
            target.session_name(),
        ) {
            self.swap_pane_within_group(source, target, request.detached, request.preserve_zoom)
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
        if pane_targets_share_window_identity(&self.sessions, &request.source, &request.target) {
            let mut normalized_request = request;
            normalized_request.source = PaneTarget::with_window(
                normalized_request.target.session_name().clone(),
                normalized_request.target.window_index(),
                normalized_request.source.pane_index(),
            );
            return self.join_pane_within_group(normalized_request);
        }
        if sessions_share_grouped_window_state(
            &self.sessions,
            request.source.session_name(),
            request.target.session_name(),
        ) {
            return self.join_pane_within_group(request);
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
        mut request: BreakPaneRequest,
    ) -> Result<BreakPaneResponse, RmuxError> {
        let explicit_name = request.name.is_some();
        let destination_session_name = request.target.as_ref().map_or_else(
            || request.source.session_name().clone(),
            |target| target.session_name().clone(),
        );
        if request.target.is_none() && !(request.after || request.before) {
            let destination_index = self.first_available_window_index(&destination_session_name)?;
            request.target = Some(WindowTarget::with_window(
                destination_session_name.clone(),
                destination_index,
            ));
        }
        let shares_grouped_window_state = sessions_share_grouped_window_state(
            &self.sessions,
            request.source.session_name(),
            &destination_session_name,
        );

        if request.source.session_name() != &destination_session_name
            && shares_grouped_window_state
            && pane_is_only_pane_in_window(&self.sessions, &request.source)
        {
            return Err(RmuxError::Server("sessions are grouped".to_owned()));
        }

        let response = if shares_grouped_window_state {
            self.break_pane_within_group(request, destination_session_name)
        } else {
            self.break_pane_across_sessions(request, destination_session_name)
        }?;
        if explicit_name {
            // `-n` pins the destination name. A moved window can carry the
            // source's auto-name marker, which would otherwise override the
            // explicit name on the next pane activity callback.
            self.clear_auto_named_window_family(
                response.target.session_name(),
                response.target.window_index(),
            );
        }
        Ok(response)
    }
}

fn pane_targets_share_window_identity(
    sessions: &rmux_core::SessionStore,
    first: &PaneTarget,
    second: &PaneTarget,
) -> bool {
    let first_window_id = sessions
        .session(first.session_name())
        .and_then(|session| session.window_at(first.window_index()))
        .map(rmux_core::Window::id);
    let second_window_id = sessions
        .session(second.session_name())
        .and_then(|session| session.window_at(second.window_index()))
        .map(rmux_core::Window::id);
    first_window_id.is_some() && first_window_id == second_window_id
}

fn pane_is_only_pane_in_window(sessions: &rmux_core::SessionStore, target: &PaneTarget) -> bool {
    let Some(session) = sessions.session(target.session_name()) else {
        return false;
    };
    let Some(window) = session.window_at(target.window_index()) else {
        return false;
    };

    window.panes().len() == 1
        && session
            .pane_id_in_window(target.window_index(), target.pane_index())
            .is_some()
}

fn sessions_share_grouped_window_state(
    sessions: &rmux_core::SessionStore,
    first: &rmux_proto::SessionName,
    second: &rmux_proto::SessionName,
) -> bool {
    if first == second {
        return true;
    }

    let Some(first_group) = sessions.session_group_name(first) else {
        return false;
    };
    sessions.session_group_name(second) == Some(first_group)
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
