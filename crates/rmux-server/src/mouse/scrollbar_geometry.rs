use rmux_core::PaneGeometry;
use rmux_proto::{OptionName, PaneTarget, SessionName};

use crate::pane_scrollbar::{PaneScrollbarConfig, PaneScrollbarLayout};
use crate::pane_terminals::HandlerState;
use crate::pane_visible_geometry::visible_pane_content_geometry;
use crate::status_lines::status_line_count;

pub(crate) fn pane_content_geometry_for_target(
    state: &HandlerState,
    target: &PaneTarget,
) -> Option<PaneGeometry> {
    let session = state.sessions.session(target.session_name())?;
    let window = session.window_at(target.window_index())?;
    let pane = window.pane(target.pane_index())?;
    if window.is_zoomed()
        && window
            .active_pane()
            .is_none_or(|active| active.id() != pane.id())
    {
        return None;
    }
    let status_lines = if state
        .options
        .resolve(Some(target.session_name()), OptionName::Status)
        .is_some_and(|value| value == "off")
    {
        0
    } else {
        status_line_count(
            state
                .options
                .resolve(Some(target.session_name()), OptionName::Status),
            window.size().rows,
        )
    };
    let content_rows = window.size().rows.saturating_sub(status_lines);
    let alternate_on = state
        .pane_screen_state(target.session_name(), pane.id())
        .is_some_and(|screen| screen.alternate_on);
    let copy_mode_active = state
        .pane_copy_mode_summary(target.session_name(), pane.id())
        .is_some();

    Some(
        resolve_pane_scrollbar_layout(
            state,
            target.session_name(),
            target.window_index(),
            pane,
            content_rows,
            alternate_on,
            copy_mode_active,
        )
        .1
        .content,
    )
}

pub(super) fn resolve_pane_scrollbar_layout(
    state: &HandlerState,
    session_name: &SessionName,
    window_index: u32,
    pane: &rmux_core::Pane,
    content_rows: u16,
    alternate_on: bool,
    copy_mode_active: bool,
) -> (PaneScrollbarConfig, PaneScrollbarLayout) {
    let config =
        PaneScrollbarConfig::resolve(&state.options, session_name, window_index, pane.index());
    let pane_geometry = state
        .sessions
        .session(session_name)
        .and_then(|session| session.window_at(window_index))
        .filter(|window| window.is_zoomed())
        .map_or_else(
            || pane.geometry(),
            |window| PaneGeometry::new(0, 0, window.size().cols, content_rows),
        );
    let geometry = visible_pane_content_geometry(
        &state.options,
        session_name,
        window_index,
        pane_geometry,
        content_rows,
    );
    let layout = config.layout(geometry, alternate_on, copy_mode_active);
    (config, layout)
}
