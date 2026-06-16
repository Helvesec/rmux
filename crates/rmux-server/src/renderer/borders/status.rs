use rmux_core::formats::FormatContext;
use rmux_core::{OptionStore, Pane, Session, Style, Utf8Config};
use rmux_proto::OptionName;

use crate::format_runtime::{render_runtime_template, RuntimeFormatContext};
use crate::pane_terminals::HandlerState;
use crate::pane_visible_geometry::{
    pane_border_status_row as content_pane_border_status_row, PaneBorderStatusPosition,
};

use super::super::{
    format_draw_content_width, format_draw_line, render_formatted_line, StatusGeometry,
};
use super::{resolve_border_style_for_pane, PaneBorderLineStyle};

pub(in crate::renderer) fn render_pane_border_status_lines(
    session: &Session,
    options: &OptionStore,
    geometry: StatusGeometry,
    state: Option<&HandlerState>,
) -> Vec<u8> {
    if session.window().pane_count() == 0 {
        return Vec::new();
    }

    let Some(position) = PaneBorderStatusPosition::from_options(
        options,
        session.name(),
        session.active_window_index(),
    ) else {
        return Vec::new();
    };

    let mut frame = Vec::new();
    for pane in session.window().panes() {
        if session.window().is_zoomed() && pane.index() != session.active_pane_index() {
            continue;
        }
        render_pane_border_status_line(
            &mut frame, session, options, geometry, state, pane, position,
        );
    }
    frame
}

fn render_pane_border_status_line(
    frame: &mut Vec<u8>,
    session: &Session,
    options: &OptionStore,
    geometry: StatusGeometry,
    state: Option<&HandlerState>,
    pane: &Pane,
    position: PaneBorderStatusPosition,
) {
    let pane_geometry = pane.geometry();
    if pane_geometry.cols() == 0 || pane_geometry.rows() == 0 {
        return;
    }

    let Some(row) = pane_border_status_row(position, pane, geometry) else {
        return;
    };

    let left_padding = if pane_geometry.cols() > 2 { 2 } else { 0 };
    let width = usize::from(pane_geometry.cols().saturating_sub(left_padding));
    if width == 0 {
        return;
    }

    let window_index = session.active_window_index();
    let base = resolve_border_style_for_pane(
        session,
        options,
        window_index,
        pane,
        pane.index() == session.active_pane_index(),
    );
    let line_style = PaneBorderLineStyle::from_option(options.resolve_for_window(
        session.name(),
        window_index,
        OptionName::PaneBorderLines,
    ));
    let expanded = expanded_pane_border_format(session, options, state, pane, window_index);
    let utf8 = Utf8Config::from_options(options);
    let expanded = with_border_fill(expanded, width, line_style, &base, &utf8);
    let line = format_draw_line(&expanded, &base, width, &utf8);

    render_formatted_line(
        frame,
        pane_geometry.x().saturating_add(left_padding),
        row,
        &line,
    );
}

fn pane_border_status_row(
    position: PaneBorderStatusPosition,
    pane: &Pane,
    geometry: StatusGeometry,
) -> Option<u16> {
    let content_row =
        content_pane_border_status_row(position, pane.geometry(), geometry.content_rows)?;
    let row = content_row.saturating_add(geometry.content_y_offset);
    (row < geometry.terminal_size.rows).then_some(row)
}

fn expanded_pane_border_format(
    session: &Session,
    options: &OptionStore,
    state: Option<&HandlerState>,
    pane: &Pane,
    window_index: u32,
) -> String {
    let context = FormatContext::from_session(session)
        .with_window(window_index, session.window(), true, false)
        .with_window_pane(session.window(), pane);
    let mut runtime = RuntimeFormatContext::new(context)
        .with_options(options)
        .with_session(session)
        .with_window(window_index, session.window())
        .with_pane(pane);
    if let Some(state) = state {
        runtime = runtime.with_state(state);
    }
    let template = options
        .resolve_for_pane(
            session.name(),
            window_index,
            pane.index(),
            OptionName::PaneBorderFormat,
        )
        .unwrap_or("");
    render_runtime_template(template, &runtime, true)
}

fn with_border_fill(
    mut expanded: String,
    width: usize,
    line_style: PaneBorderLineStyle,
    base: &Style,
    utf8: &Utf8Config,
) -> String {
    let content_width = format_draw_content_width(&expanded, base, utf8);
    let fill_width = width.saturating_sub(content_width);
    if fill_width == 0 {
        return expanded;
    }
    expanded.extend(std::iter::repeat_n(line_style.map_glyph('─'), fill_width));
    expanded
}

#[cfg(test)]
mod tests {
    use rmux_core::{OptionStore, Session};
    use rmux_proto::{OptionName, ScopeSelector, SetOptionMode, SplitDirection, TerminalSize};

    use super::*;

    fn session_name(value: &str) -> rmux_proto::SessionName {
        rmux_proto::SessionName::new(value).expect("valid session name")
    }

    fn two_pane_session() -> Session {
        let mut session = Session::new(session_name("alpha"), TerminalSize { cols: 40, rows: 10 });
        session
            .split_active_pane_with_direction(SplitDirection::Horizontal)
            .expect("split succeeds");
        session
    }

    #[test]
    fn pane_border_status_top_renders_edge_and_border_lines() {
        let mut session = two_pane_session();
        session.select_pane(1).expect("select bottom pane");
        let mut options = OptionStore::new();
        options
            .set(
                ScopeSelector::Window(rmux_proto::WindowTarget::with_window(
                    session.name().clone(),
                    session.active_window_index(),
                )),
                OptionName::PaneBorderStatus,
                "top".to_owned(),
                SetOptionMode::Replace,
            )
            .expect("pane border status set succeeds");
        options
            .set(
                ScopeSelector::Window(rmux_proto::WindowTarget::with_window(
                    session.name().clone(),
                    session.active_window_index(),
                )),
                OptionName::PaneBorderFormat,
                "pane=#{pane_index},active=#{pane_active}".to_owned(),
                SetOptionMode::Replace,
            )
            .expect("pane border format set succeeds");

        let frame = String::from_utf8(render_pane_border_status_lines(
            &session,
            &options,
            StatusGeometry::without_status(session.window().size()),
            None,
        ))
        .expect("frame is utf8");

        assert!(frame.contains("pane=1"), "{frame:?}");
        assert!(frame.contains("pane=0"), "{frame:?}");
        assert!(frame.contains("active=1"), "{frame:?}");
    }

    #[test]
    fn pane_border_status_edge_rows_are_reserved_inside_content() {
        let session = two_pane_session();
        let top_pane = session.window().pane(0).expect("top pane exists");
        let bottom_pane = session.window().pane(1).expect("bottom pane exists");
        let geometry = StatusGeometry::without_status(session.window().size());

        assert_eq!(
            pane_border_status_row(PaneBorderStatusPosition::Top, top_pane, geometry),
            Some(0)
        );
        assert_eq!(
            pane_border_status_row(PaneBorderStatusPosition::Bottom, bottom_pane, geometry),
            Some(9)
        );
    }

    #[test]
    fn pane_border_status_uses_configured_border_line_fill() {
        let session = two_pane_session();
        let mut options = OptionStore::new();
        let scope = ScopeSelector::Window(rmux_proto::WindowTarget::with_window(
            session.name().clone(),
            session.active_window_index(),
        ));
        options
            .set(
                scope.clone(),
                OptionName::PaneBorderStatus,
                "top".to_owned(),
                SetOptionMode::Replace,
            )
            .expect("pane border status set succeeds");
        options
            .set(
                scope.clone(),
                OptionName::PaneBorderFormat,
                "p#{pane_index}".to_owned(),
                SetOptionMode::Replace,
            )
            .expect("pane border format set succeeds");
        options
            .set(
                scope,
                OptionName::PaneBorderLines,
                "heavy".to_owned(),
                SetOptionMode::Replace,
            )
            .expect("pane border lines set succeeds");

        let frame = String::from_utf8(render_pane_border_status_lines(
            &session,
            &options,
            StatusGeometry::without_status(session.window().size()),
            None,
        ))
        .expect("frame is utf8");

        assert!(frame.contains('━'), "{frame:?}");
        assert!(!frame.contains('─'), "{frame:?}");
    }
}
