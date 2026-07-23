use rmux_core::PaneGeometry;
use rmux_proto::{PaneTarget, RmuxError};

use super::mode_tree_model::ModeTreeClientState;
use crate::pane_terminals::{session_not_found, HandlerState};

pub(super) fn content_geometry(
    state: &HandlerState,
    mode: &ModeTreeClientState,
) -> Result<PaneGeometry, RmuxError> {
    let session = state
        .sessions
        .session(&mode.session_name)
        .ok_or_else(|| session_not_found(&mode.session_name))?;
    let status = crate::renderer::StatusGeometry::for_session(session, &state.options);
    let active_target = PaneTarget::with_window(
        session.name().clone(),
        session.active_window_index(),
        session.active_pane_index(),
    );
    let target = mode
        .host_pane
        .as_ref()
        .filter(|target| {
            target.session_name() == session.name()
                && target.window_index() == session.active_window_index()
        })
        .unwrap_or(&active_target);
    let pane = crate::mouse::pane_content_geometry_for_target(state, target)
        .or_else(|| crate::mouse::pane_content_geometry_for_target(state, &active_target))
        .unwrap_or_else(|| {
            PaneGeometry::new(0, 0, session.window().size().cols, status.content_rows())
        });
    Ok(PaneGeometry::new(
        pane.x(),
        pane.y().saturating_add(status.content_y_offset()),
        pane.cols(),
        pane.rows(),
    ))
}
