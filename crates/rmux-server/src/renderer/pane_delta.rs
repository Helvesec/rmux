use rmux_core::{GridRenderOptions, OptionStore, Pane, Screen, ScreenCaptureRange, Session};
use rmux_proto::OptionName;

use super::{
    content_pane_geometry, cursor_position_bytes, truncate_rendered_pane_line, StatusGeometry,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PaneRenderDelta {
    Incremental(Vec<u8>),
    RequiresFullRefresh,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PaneRenderSnapshot {
    x: u16,
    y: u16,
    rows: u16,
    cols: u16,
    lines: Vec<Vec<u8>>,
    cursor: Vec<u8>,
    cursor_style: u32,
    title: String,
    path: String,
    mode: u32,
    alternate_on: bool,
}

impl PaneRenderSnapshot {
    pub(crate) fn capture(
        session: &Session,
        options: &OptionStore,
        pane: &Pane,
        screen: &Screen,
    ) -> Option<Self> {
        let geometry = StatusGeometry::for_session(session, options);
        let pane_geometry = content_pane_geometry(pane, geometry.content_rows);
        if pane_geometry.cols() == 0 || pane_geometry.rows() == 0 {
            return None;
        }

        let mut styled_screen = screen.clone();
        if let Some(style) = options.resolve_for_pane(
            session.name(),
            session.active_window_index(),
            pane.index(),
            OptionName::CopyModeSelectionStyle,
        ) {
            styled_screen.overlay_style_on_selected(style);
        }

        let rendered = styled_screen.capture_transcript(
            ScreenCaptureRange::default(),
            GridRenderOptions {
                with_sequences: true,
                include_empty_cells: true,
                trim_spaces: false,
                ..GridRenderOptions::default()
            },
        );
        let utf8 = rmux_core::Utf8Config::from_options(options);
        let lines = rendered
            .split(|byte| *byte == b'\n')
            .take(usize::from(pane_geometry.rows()))
            .map(|line| truncate_rendered_pane_line(line, usize::from(pane_geometry.cols()), &utf8))
            .collect::<Vec<_>>();

        let (cursor_x, cursor_y) = screen.cursor_position();
        let cursor = cursor_position_bytes(
            pane_geometry
                .y()
                .saturating_add(geometry.content_y_offset)
                .saturating_add(
                    cursor_y.min(u32::from(pane_geometry.rows().saturating_sub(1))) as u16,
                ),
            pane_geometry.x().saturating_add(
                cursor_x.min(u32::from(pane_geometry.cols().saturating_sub(1))) as u16,
            ),
        );

        Some(Self {
            x: pane_geometry.x(),
            y: pane_geometry.y().saturating_add(geometry.content_y_offset),
            rows: pane_geometry.rows(),
            cols: pane_geometry.cols(),
            lines,
            cursor,
            cursor_style: screen.cursor_style(),
            title: screen.title().to_owned(),
            path: screen.path().to_owned(),
            mode: screen.mode(),
            alternate_on: screen.is_alternate(),
        })
    }

    pub(crate) fn diff_to(&self, next: &Self) -> PaneRenderDelta {
        if self.requires_full_refresh(next) {
            return PaneRenderDelta::RequiresFullRefresh;
        }

        let mut frame = Vec::new();
        let blank_line = vec![b' '; usize::from(next.cols)];
        let changed_rows = self.lines.len().max(next.lines.len());
        for row in 0..changed_rows {
            let previous_line = self
                .lines
                .get(row)
                .map(Vec::as_slice)
                .unwrap_or(blank_line.as_slice());
            let next_line = next
                .lines
                .get(row)
                .map(Vec::as_slice)
                .unwrap_or(blank_line.as_slice());
            if previous_line == next_line {
                continue;
            }
            if frame.is_empty() {
                frame.extend_from_slice(b"\x1b[s\x1b[0m");
            }
            frame.extend_from_slice(
                cursor_position_bytes(next.y.saturating_add(row as u16), next.x).as_slice(),
            );
            frame.extend_from_slice(next_line);
        }

        if !frame.is_empty() {
            frame.extend_from_slice(b"\x1b[0m\x1b[u");
        }
        if self.cursor != next.cursor {
            frame.extend_from_slice(&next.cursor);
        }

        PaneRenderDelta::Incremental(frame)
    }

    fn requires_full_refresh(&self, next: &Self) -> bool {
        self.x != next.x
            || self.y != next.y
            || self.rows != next.rows
            || self.cols != next.cols
            || self.cursor_style != next.cursor_style
            || self.title != next.title
            || self.path != next.path
            || self.mode != next.mode
            || self.alternate_on != next.alternate_on
    }
}

#[cfg(test)]
mod tests {
    use rmux_core::{input::InputParser, OptionStore, Screen, Session};
    use rmux_proto::{SessionName, TerminalSize};

    use super::{PaneRenderDelta, PaneRenderSnapshot};

    fn session_name(value: &str) -> SessionName {
        SessionName::new(value).expect("valid session name")
    }

    fn screen_with(bytes: &[u8]) -> Screen {
        let mut screen = Screen::new(TerminalSize { cols: 10, rows: 3 }, 100);
        let mut parser = InputParser::new();
        parser.parse(bytes, &mut screen);
        screen
    }

    #[test]
    fn pane_delta_renders_only_changed_lines_and_cursor() {
        let session = Session::new(session_name("alpha"), TerminalSize { cols: 10, rows: 4 });
        let pane = session.window().active_pane().expect("active pane");
        let options = OptionStore::new();
        let before = screen_with(b"abc");
        let after = screen_with(b"abcd");
        let before = PaneRenderSnapshot::capture(&session, &options, pane, &before)
            .expect("before snapshot");
        let after =
            PaneRenderSnapshot::capture(&session, &options, pane, &after).expect("after snapshot");

        let PaneRenderDelta::Incremental(delta) = before.diff_to(&after) else {
            panic!("line update should not require a full refresh");
        };
        let text = String::from_utf8(delta).expect("delta is utf8");

        assert!(text.contains("\u{1b}[1;1H"));
        assert!(text.contains("abcd"));
        assert!(!text.contains("\u{1b}[2;1H"));
        assert!(!text.contains("\u{1b}[4;1H"));
        assert!(text.ends_with("\u{1b}[1;5H"));
    }

    #[test]
    fn pane_delta_requests_full_refresh_for_title_changes() {
        let session = Session::new(session_name("alpha"), TerminalSize { cols: 10, rows: 4 });
        let pane = session.window().active_pane().expect("active pane");
        let options = OptionStore::new();
        let before = screen_with(b"abc");
        let mut after = screen_with(b"abc");
        after.set_title("new title");
        let before = PaneRenderSnapshot::capture(&session, &options, pane, &before)
            .expect("before snapshot");
        let after =
            PaneRenderSnapshot::capture(&session, &options, pane, &after).expect("after snapshot");

        assert_eq!(before.diff_to(&after), PaneRenderDelta::RequiresFullRefresh);
    }

    #[test]
    fn pane_delta_renders_new_prompt_lines_incrementally() {
        let session = Session::new(session_name("alpha"), TerminalSize { cols: 10, rows: 4 });
        let pane = session.window().active_pane().expect("active pane");
        let options = OptionStore::new();
        let before = screen_with(b"abc");
        let after = screen_with(b"abc\r\ndef");
        let before = PaneRenderSnapshot::capture(&session, &options, pane, &before)
            .expect("before snapshot");
        let after =
            PaneRenderSnapshot::capture(&session, &options, pane, &after).expect("after snapshot");

        let PaneRenderDelta::Incremental(delta) = before.diff_to(&after) else {
            panic!("new shell prompt lines should not force a full refresh");
        };
        let text = String::from_utf8(delta).expect("delta is utf8");

        assert!(text.contains("\u{1b}[2;1H"));
        assert!(text.contains("def"));
        assert!(text.ends_with("\u{1b}[2;4H"));
    }
}
