use rmux_core::formats::FormatContext;
use rmux_core::{OptionStore, Pane, Style, Utf8Config};
use rmux_proto::OptionName;

use crate::format_runtime::{render_runtime_template, RuntimeFormatContext};
use crate::pane_terminals::HandlerState;
#[cfg(test)]
use crate::pane_visible_geometry::pane_border_status_row as content_pane_border_status_row;
use crate::pane_visible_geometry::PaneBorderStatusPosition;

use super::super::projection::TerminalSceneProjection;
use super::super::{
    format_draw_content_width, format_draw_line, render_formatted_line_segment, StatusGeometry,
};
use super::{resolve_border_style_for_pane, PaneBorderLineStyle};

pub(in crate::renderer) fn render_pane_border_status_lines(
    session: rmux_core::ManagedSessionView<'_>,
    options: &OptionStore,
    _geometry: StatusGeometry,
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
    let projection = TerminalSceneProjection::resolve(session, options);
    for pane in session.window().panes() {
        if session.window().is_zoomed() && pane.index() != session.active_pane_index() {
            continue;
        }
        render_pane_border_status_line(
            &mut frame,
            session,
            options,
            &projection,
            state,
            pane,
            position,
        );
    }
    frame
}

fn render_pane_border_status_line(
    frame: &mut Vec<u8>,
    session: rmux_core::ManagedSessionView<'_>,
    options: &OptionStore,
    projection: &TerminalSceneProjection,
    state: Option<&HandlerState>,
    pane: &Pane,
    _position: PaneBorderStatusPosition,
) {
    let Some(region) = projection.pane_border_status_region(pane) else {
        return;
    };
    if region.spans().is_empty() {
        return;
    }
    let left_padding = if region.source_width() > 2 { 2 } else { 0 };
    let width = region.source_width().saturating_sub(left_padding);
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

    for span in region.spans() {
        let span_start = span.source_col;
        let span_end = span_start.saturating_add(usize::from(span.cols));
        let content_start = left_padding;
        let content_end = left_padding.saturating_add(width);
        let start = span_start.max(content_start);
        let end = span_end.min(content_end);
        if start >= end {
            continue;
        }
        let x = span.x.saturating_add(
            u16::try_from(start.saturating_sub(span.source_col)).unwrap_or(u16::MAX),
        );
        render_formatted_line_segment(
            frame,
            x,
            span.y,
            &line,
            start.saturating_sub(left_padding),
            end.saturating_sub(start),
        );
    }
}

#[cfg(test)]
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
    session: rmux_core::ManagedSessionView<'_>,
    options: &OptionStore,
    state: Option<&HandlerState>,
    pane: &Pane,
    window_index: u32,
) -> String {
    let context = FormatContext::new()
        .with_managed_session(session)
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
    use rmux_core::OptionStore;
    use rmux_proto::{OptionName, ScopeSelector, SetOptionMode, SplitDirection, TerminalSize};

    use super::*;

    fn session_name(value: &str) -> rmux_proto::SessionName {
        rmux_proto::SessionName::new(value).expect("valid session name")
    }

    fn two_pane_session() -> crate::session_fixture::SessionFixture {
        let mut session = crate::session_fixture::SessionFixture::new(
            session_name("alpha"),
            TerminalSize { cols: 40, rows: 10 },
        );
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
            session.view(),
            &options,
            StatusGeometry::without_status(session.window().size(), session.window().size()),
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
        let geometry =
            StatusGeometry::without_status(session.window().size(), session.window().size());

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
            session.view(),
            &options,
            StatusGeometry::without_status(session.window().size(), session.window().size()),
            None,
        ))
        .expect("frame is utf8");

        assert!(frame.contains('━'), "{frame:?}");
        assert!(!frame.contains('─'), "{frame:?}");
    }
}
