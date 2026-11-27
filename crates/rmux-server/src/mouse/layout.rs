use rmux_proto::{OptionName, PaneTarget, SessionName};

use crate::pane_terminals::HandlerState;
use crate::status_lines::status_line_count;

use super::scrollbar_geometry::resolve_pane_scrollbar_layout;
use super::types::{MouseLayout, PaneBorderStatus, PaneMouseTarget, PaneScrollbar};

pub(crate) fn layout_for_session(
    state: &HandlerState,
    session_name: &SessionName,
    attached_count: usize,
) -> Option<MouseLayout> {
    let session = state.sessions.session(session_name)?;
    layout_for_view(state, session, attached_count)
}

pub(crate) fn layout_for_view(
    state: &HandlerState,
    session: rmux_core::ManagedSessionView<'_>,
    attached_count: usize,
) -> Option<MouseLayout> {
    let session_name = session.name();
    let window_index = session.active_window_index();
    let window = session.window_at(window_index)?;
    let terminal_size = session.terminal_size();
    let status_enabled = terminal_size.cols != 0
        && terminal_size.rows != 0
        && !matches!(
            state
                .options
                .resolve(Some(session_name), OptionName::Status),
            Some("off")
        );
    let (status_at, status_lines) = if status_enabled {
        let status_lines = status_line_count(
            state
                .options
                .resolve(Some(session_name), OptionName::Status),
            terminal_size.rows,
        );
        match state
            .options
            .resolve(Some(session_name), OptionName::StatusPosition)
        {
            Some("top") => (Some(0), status_lines),
            _ => (
                Some(terminal_size.rows.saturating_sub(status_lines)),
                status_lines,
            ),
        }
    } else {
        (None, 0)
    };
    let pane_border_status = parse_pane_border_status(state.options.resolve_for_window(
        session_name,
        window_index,
        OptionName::PaneBorderStatus,
    ));
    let content_rows = window
        .size()
        .rows
        .min(terminal_size.rows.saturating_sub(status_lines));
    let content_size = rmux_proto::TerminalSize {
        cols: window.size().cols.min(terminal_size.cols),
        rows: content_rows,
    };
    let scene =
        crate::pane_visible_geometry::resolved_session_scene(session, &state.options, content_size);
    let focus_follows_mouse = state
        .options
        .resolve(Some(session_name), OptionName::FocusFollowsMouse)
        .is_some_and(|value| value == "on");

    Some(MouseLayout {
        client_viewport: session.viewport_offset().is_some(),
        session_id: session.id().as_u32(),
        status_at,
        status_lines,
        status: crate::renderer::status_line_layout(
            session,
            &state.options,
            attached_count,
            None,
            Some(state),
        ),
        pane_border_status,
        focus_follows_mouse,
        active_pane: scene.active_pane_id(),
        panes: scene
            .panes()
            .iter()
            .filter(|pane_scene| pane_scene.visible())
            .filter_map(|pane_scene| {
                let pane = window.pane(pane_scene.pane_index())?;
                let history_size = state
                    .pane_history_size_stats(session_name, pane.id())
                    .map(|stats| stats.size)
                    .unwrap_or_default();
                let alternate_on = state
                    .pane_screen_state(session_name, pane.id())
                    .map(|screen| screen.alternate_on)
                    .unwrap_or(false);
                let copy_mode_offset = state
                    .pane_copy_mode_summary(session_name, pane.id())
                    .map(|summary| summary.scroll_position);
                let (scrollbar_config, scrollbar_layout, mut projection) =
                    resolve_pane_scrollbar_layout(
                        state,
                        session_name,
                        window_index,
                        pane.index(),
                        pane_scene,
                        alternate_on,
                        copy_mode_offset.is_some(),
                    );
                projection.coordinates = projection
                    .coordinates
                    .with_viewport_offset(session.viewport_offset().unwrap_or_default());
                let geometry = projection.coordinates.clip.pane_geometry()?;
                let scrollbar = PaneScrollbar::from_layout(
                    scrollbar_layout,
                    history_size,
                    alternate_on,
                    &scrollbar_config,
                    copy_mode_offset,
                );
                Some(PaneMouseTarget {
                    pane_id: pane.id(),
                    pane_target: Some(PaneTarget::with_window(
                        session_name.clone(),
                        window_index,
                        pane.index(),
                    )),
                    window_id: window.id().as_u32(),
                    layer: pane_scene.layer().z_index(),
                    geometry,
                    projection: Some(projection),
                    scrollbar,
                    border_controls: Vec::new(),
                })
            })
            .collect(),
    })
}

fn parse_pane_border_status(value: Option<&str>) -> PaneBorderStatus {
    match value {
        Some("top") => PaneBorderStatus::Top,
        Some("bottom") => PaneBorderStatus::Bottom,
        _ => PaneBorderStatus::Off,
    }
}
