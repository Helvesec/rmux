use rmux_core::{PaneGeometry, ResolvedZoomVisibility};
use rmux_proto::{PaneTarget, SessionName};

use crate::pane_scrollbar::{PaneScrollbarConfig, PaneScrollbarLayout};
use crate::pane_terminals::HandlerState;
use crate::pane_visible_geometry::resolved_scene_context;

pub(crate) fn pane_content_geometry_for_target(
    state: &HandlerState,
    target: &PaneTarget,
) -> Option<PaneGeometry> {
    pane_mouse_coordinates_for_target(state, target)?
        .clip
        .pane_geometry()
}

pub(crate) fn pane_mouse_coordinates_for_target(
    state: &HandlerState,
    target: &PaneTarget,
) -> Option<super::projection::MouseCoordinates> {
    // Use the addressed window, including when it is not the active window.
    let session = state.sessions.session(target.session_name())?;
    let window = session.window_at(target.window_index())?;
    let pane = window.pane(target.pane_index())?;
    let terminal = session.terminal_size();
    let status_lines = if terminal.cols == 0 {
        0
    } else {
        crate::status_lines::status_line_count(
            state
                .options
                .resolve(Some(target.session_name()), rmux_proto::OptionName::Status),
            terminal.rows,
        )
    };
    let content_size = rmux_proto::TerminalSize {
        cols: window.size().cols.min(terminal.cols),
        rows: window
            .size()
            .rows
            .min(terminal.rows.saturating_sub(status_lines)),
    };
    let scene = window.resolved_scene(resolved_scene_context(
        &state.options,
        target.session_name(),
        target.window_index(),
        content_size,
        ResolvedZoomVisibility::Rendered,
    ));
    let pane_scene = scene.pane_by_id(pane.id()).filter(|pane| pane.visible())?;
    let alternate = state
        .pane_screen_state(target.session_name(), pane.id())
        .is_some_and(|screen| screen.alternate_on);
    let copy_mode = state
        .pane_copy_mode_summary(target.session_name(), pane.id())
        .is_some();
    Some(
        resolve_pane_scrollbar_layout(
            state,
            target.session_name(),
            target.window_index(),
            pane.index(),
            pane_scene,
            alternate,
            copy_mode,
        )
        .2
        .coordinates,
    )
}

pub(super) fn resolve_pane_scrollbar_layout(
    state: &HandlerState,
    session_name: &SessionName,
    window_index: u32,
    pane_index: u32,
    pane_scene: &rmux_core::ResolvedPaneScene,
    alternate_on: bool,
    copy_mode_active: bool,
) -> (
    PaneScrollbarConfig,
    PaneScrollbarLayout,
    super::projection::MouseProjection,
) {
    let config =
        PaneScrollbarConfig::resolve(&state.options, session_name, window_index, pane_index);
    let (projection, layout) = super::projection::MouseProjection::new(
        pane_scene,
        &config,
        alternate_on,
        copy_mode_active,
    );
    (config, layout, projection)
}
