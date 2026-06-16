use rmux_core::input::mode;
use rmux_core::{
    render_dec_modes_for_snapshot, GridRenderOptions, PaneGeometry, PaneId, Screen,
    ScreenCaptureRange,
};
use rmux_proto::TerminalSize;

const SNAPSHOT_RESET_PREFIX: &[u8] =
    b"\x1b[?2026l\x1b[?1049l\x1b[?6l\x1b[r\x1b[0m\x1b[?25l\x1b[3J\x1b[2J\x1b[H";
const SNAPSHOT_ALT_SCREEN_PREFIX: &[u8] =
    b"\x1b[?1049h\x1b[?6l\x1b[r\x1b[0m\x1b[?25l\x1b[3J\x1b[2J\x1b[H";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct WebPaneSnapshot {
    pub(crate) cols: u16,
    pub(crate) rows: u16,
    pub(crate) output_sequence: u64,
    pub(crate) ansi_lines: Vec<Vec<u8>>,
    pub(crate) cursor_row: u16,
    pub(crate) cursor_col: u16,
    pub(crate) cursor_visible: bool,
    /// Inner-program DEC private mode bitmap at capture time, re-asserted on
    /// (re)join so the late viewer is interactive, not just visually correct.
    pub(crate) mode_bits: u32,
    /// DECSCUSR cursor style at capture time (0 = terminal default).
    pub(crate) cursor_style: u32,
    /// Whether the pane is on the alternate screen at capture time.
    pub(crate) alternate: bool,
    /// DECSTBM scroll region (top, bottom), 0-based inclusive.
    pub(crate) scroll_top: u32,
    pub(crate) scroll_bottom: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct WebSessionSnapshot {
    pub(crate) size: TerminalSize,
    pub(crate) view: WebSessionView,
    frame: Vec<u8>,
    /// Active pane's DEC mode bitmap, re-asserted so the single browser
    /// emulator routes mouse/paste/keys to the focused pane's program.
    active_mode_bits: u32,
    /// Active pane's DECSCUSR cursor style (0 = terminal default).
    active_cursor_style: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WebSessionPaneFrame {
    pub(crate) size: TerminalSize,
    pub(crate) pane: WebSessionPaneView,
    pub(crate) frame: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct WebSessionView {
    pub(crate) size: TerminalSize,
    pub(crate) panes: Vec<WebSessionPaneView>,
    pub(crate) windows: Vec<WebSessionWindowView>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct WebSessionPaneView {
    pub(crate) id: u32,
    pub(crate) x: u16,
    pub(crate) y: u16,
    pub(crate) cols: u16,
    pub(crate) rows: u16,
    pub(crate) active: bool,
    pub(crate) history_size: usize,
    pub(crate) scroll_offset: usize,
    pub(crate) alternate_on: bool,
    pub(crate) mouse_on: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct WebSessionWindowView {
    pub(crate) index: u32,
    pub(crate) name: String,
    pub(crate) active: bool,
}

impl WebPaneSnapshot {
    pub(crate) fn ansi_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(SNAPSHOT_RESET_PREFIX);
        // Match the inner program's alternate-screen state so the browser's
        // emulator stays in sync with the later 1049h/l toggles in the live
        // stream (otherwise a TUI exit restores a buffer we never painted).
        if self.alternate {
            out.extend_from_slice(SNAPSHOT_ALT_SCREEN_PREFIX);
        }
        // Emit a complete interactive mode state, not just the ON bits. Resyncs
        // can arrive after lost live bytes, so the browser may still be in a
        // stale mode such as synchronized output, alt-screen, bracketed paste,
        // mouse reporting, or modifyOtherKeys.
        render_dec_modes_for_snapshot(self.mode_bits, self.cursor_style, &mut out);
        for (index, line) in self.ansi_lines.iter().enumerate() {
            if index > 0 {
                out.extend_from_slice(b"\r\n");
            }
            out.extend_from_slice(b"\x1b[0m");
            out.extend_from_slice(line);
        }
        // Scroll region (DECSTBM) goes *after* painting: it homes the cursor and
        // would scroll the content if it were set before the line writes above.
        let default_bottom = u32::from(self.rows.max(1)).saturating_sub(1);
        if self.scroll_top != 0 || self.scroll_bottom != default_bottom {
            out.extend_from_slice(
                format!(
                    "\x1b[{};{}r",
                    self.scroll_top.saturating_add(1),
                    self.scroll_bottom.saturating_add(1),
                )
                .as_bytes(),
            );
        }
        let cursor_row = self.cursor_row.min(self.rows.saturating_sub(1)) + 1;
        let cursor_col = self.cursor_col.min(self.cols.saturating_sub(1)) + 1;
        out.extend_from_slice(format!("\x1b[0m\x1b[{cursor_row};{cursor_col}H").as_bytes());
        // Origin mode (DECOM) last, so the absolute cursor positioning above is
        // not reinterpreted relative to the scroll region.
        if self.mode_bits & mode::MODE_ORIGIN != 0 {
            out.extend_from_slice(b"\x1b[?6h");
        }
        out.extend_from_slice(if self.cursor_visible {
            b"\x1b[?25h"
        } else {
            b"\x1b[?25l"
        });
        out
    }
}

impl WebSessionSnapshot {
    pub(crate) fn new(
        size: TerminalSize,
        frame: Vec<u8>,
        view: WebSessionView,
        active_mode_bits: u32,
        active_cursor_style: u32,
    ) -> Self {
        Self {
            size,
            view,
            frame,
            active_mode_bits,
            active_cursor_style,
        }
    }

    pub(crate) fn ansi_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.frame.len() + SNAPSHOT_RESET_PREFIX.len());
        out.extend_from_slice(SNAPSHOT_RESET_PREFIX);
        // Re-assert only the active pane's *interactive* modes. The composited
        // multi-pane frame owns the layout, so layout modes stay reset here.
        render_dec_modes_for_snapshot(self.active_mode_bits, self.active_cursor_style, &mut out);
        out.extend_from_slice(&self.frame);
        out
    }
}

impl WebSessionPaneFrame {
    pub(crate) fn new(size: TerminalSize, pane: WebSessionPaneView, frame: Vec<u8>) -> Self {
        Self { size, pane, frame }
    }
}

impl WebSessionView {
    pub(crate) fn new(size: TerminalSize) -> Self {
        Self {
            size,
            panes: Vec::new(),
            windows: Vec::new(),
        }
    }

    pub(crate) fn add_window(&mut self, index: u32, name: Option<&str>, active: bool) {
        self.windows.push(WebSessionWindowView {
            index,
            name: name.unwrap_or_default().to_owned(),
            active,
        });
    }

    pub(crate) fn push_pane(&mut self, pane: WebSessionPaneView) {
        self.panes.push(pane);
    }
}

impl WebSessionPaneView {
    pub(crate) fn new(
        id: PaneId,
        geometry: PaneGeometry,
        active: bool,
        history_size: usize,
        scroll_offset: usize,
        alternate_on: bool,
        mouse_on: bool,
    ) -> Self {
        Self {
            id: id.as_u32(),
            x: geometry.x(),
            y: geometry.y(),
            cols: geometry.cols(),
            rows: geometry.rows(),
            active,
            history_size,
            scroll_offset,
            alternate_on,
            mouse_on,
        }
    }
}

pub(crate) fn session_content_geometry(
    geometry: PaneGeometry,
    session_size: TerminalSize,
) -> Option<PaneGeometry> {
    let content_rows = session_size.rows.saturating_sub(1);
    if geometry.y() >= content_rows {
        return None;
    }
    let rows = geometry.rows().min(content_rows - geometry.y());
    if rows == 0 || geometry.cols() == 0 {
        return None;
    }
    Some(PaneGeometry::new(
        geometry.x(),
        geometry.y(),
        geometry.cols(),
        rows,
    ))
}

pub(crate) fn overlay_pane_lines(frame: &mut Vec<u8>, geometry: PaneGeometry, lines: &[Vec<u8>]) {
    for row in 0..usize::from(geometry.rows()) {
        let terminal_row = usize::from(geometry.y()) + row + 1;
        let terminal_col = usize::from(geometry.x()) + 1;
        frame.extend_from_slice(format!("\x1b[{terminal_row};{terminal_col}H\x1b[0m").as_bytes());
        if let Some(line) = lines.get(row) {
            frame.extend_from_slice(line);
        }
    }
    frame.extend_from_slice(b"\x1b[?25l");
}

pub(crate) fn snapshot_ansi_lines(screen: &Screen) -> Vec<Vec<u8>> {
    screen.capture_transcript_lines_independent(
        ScreenCaptureRange::default(),
        GridRenderOptions {
            with_sequences: true,
            trim_spaces: false,
            ..GridRenderOptions::default()
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmux_core::input::{mode, InputParser};
    use rmux_core::Screen;
    use rmux_proto::TerminalSize;

    fn default_pane_snapshot(ansi_lines: Vec<Vec<u8>>) -> WebPaneSnapshot {
        WebPaneSnapshot {
            cols: 80,
            rows: 24,
            output_sequence: 7,
            ansi_lines,
            cursor_row: 3,
            cursor_col: 7,
            cursor_visible: true,
            // Post-reset defaults.
            mode_bits: mode::MODE_CURSOR | mode::MODE_WRAP,
            cursor_style: 0,
            alternate: false,
            scroll_top: 0,
            scroll_bottom: 23,
        }
    }

    #[test]
    fn web_snapshot_bytes_preserve_ansi_style_and_cursor() {
        let snapshot = default_pane_snapshot(vec![b"\x1b[32muser@host\x1b[0m".to_vec()]);

        let bytes = snapshot.ansi_bytes();
        let rendered = String::from_utf8(bytes).expect("snapshot bytes are utf8");

        assert!(rendered.starts_with("\x1b[?2026l\x1b[?1049l\x1b[?6l\x1b[r"));
        assert!(rendered.contains("\x1b[32muser@host"));
        assert!(rendered.contains("\x1b[4;8H\x1b[?25h"));
    }

    #[test]
    fn web_snapshot_reasserts_dec_modes_and_scroll_region() {
        let mut snapshot = default_pane_snapshot(vec![b"hi".to_vec()]);
        snapshot.mode_bits = mode::MODE_CURSOR
            | mode::MODE_WRAP
            | mode::MODE_MOUSE_BUTTON
            | mode::MODE_MOUSE_SGR
            | mode::MODE_BRACKETPASTE;
        snapshot.alternate = true;
        snapshot.scroll_top = 2;
        snapshot.scroll_bottom = 20;

        let rendered = String::from_utf8(snapshot.ansi_bytes()).expect("snapshot bytes are utf8");

        // Interactive modes re-asserted right after the reset prefix.
        assert!(rendered.contains("\x1b[?2026l"), "{rendered:?}");
        assert!(rendered.contains("\x1b[?1049l"), "{rendered:?}");
        assert!(rendered.contains("\x1b[?1049h"), "{rendered:?}");
        assert!(rendered.contains("\x1b[?1002h"), "{rendered:?}");
        assert!(rendered.contains("\x1b[?1006h"), "{rendered:?}");
        assert!(rendered.contains("\x1b[?2004h"), "{rendered:?}");
        // Scroll region applied after the painted content.
        assert!(rendered.contains("\x1b[3;21r"), "{rendered:?}");
        // ... and the modes precede the painted line.
        let modes_at = rendered.find("\x1b[?1002h").unwrap();
        let content_at = rendered.find("hi").unwrap();
        assert!(modes_at < content_at, "modes must precede content");
    }

    #[test]
    fn web_snapshot_resets_stale_modes_when_target_is_normal_screen() {
        let snapshot = default_pane_snapshot(vec![b"normal".to_vec()]);
        let rendered = String::from_utf8(snapshot.ansi_bytes()).expect("snapshot bytes are utf8");

        assert!(rendered.contains("\x1b[?2026l"), "{rendered:?}");
        assert!(rendered.contains("\x1b[?1049l"), "{rendered:?}");
        assert!(rendered.contains("\x1b[?6l"), "{rendered:?}");
        assert!(rendered.contains("\x1b[r"), "{rendered:?}");
        assert!(rendered.contains("\x1b[?2004l"), "{rendered:?}");
        assert!(!rendered.contains("\x1b[?1049h"), "{rendered:?}");
        assert!(!rendered.contains("\x1b[?2026h"), "{rendered:?}");
    }

    #[test]
    fn web_session_snapshot_clears_saved_lines_before_rendering() {
        let size = TerminalSize { cols: 80, rows: 24 };
        let snapshot = WebSessionSnapshot::new(
            size,
            b"frame".to_vec(),
            WebSessionView::new(size),
            mode::MODE_CURSOR | mode::MODE_WRAP,
            0,
        );
        let rendered = String::from_utf8(snapshot.ansi_bytes()).expect("snapshot bytes are utf8");

        assert!(rendered.starts_with("\x1b[?2026l\x1b[?1049l\x1b[?6l\x1b[r"));
        assert!(rendered.contains("\x1b[?2004l"), "{rendered:?}");
        assert!(rendered.ends_with("frame"), "{rendered:?}");
    }

    #[test]
    fn web_session_snapshot_reasserts_active_pane_modes() {
        let size = TerminalSize { cols: 80, rows: 24 };
        let snapshot = WebSessionSnapshot::new(
            size,
            b"frame".to_vec(),
            WebSessionView::new(size),
            mode::MODE_CURSOR | mode::MODE_WRAP | mode::MODE_MOUSE_BUTTON | mode::MODE_MOUSE_SGR,
            0,
        );
        let rendered = String::from_utf8(snapshot.ansi_bytes()).expect("snapshot bytes are utf8");

        assert!(rendered.contains("\x1b[?1002h"), "{rendered:?}");
        assert!(rendered.contains("\x1b[?1006h"), "{rendered:?}");
        assert!(rendered.ends_with("frame"), "{rendered:?}");
    }

    #[test]
    fn web_snapshot_capture_preserves_screen_sequences() {
        let mut screen = Screen::new(TerminalSize { cols: 12, rows: 3 }, 100);
        let mut parser = InputParser::new();
        parser.parse(b"\x1b[32muser\x1b[0m@host", &mut screen);

        let lines = snapshot_ansi_lines(&screen);
        let joined = String::from_utf8(lines.concat()).expect("snapshot lines are utf8");

        assert!(joined.contains("\x1b[32m"));
        assert!(joined.contains("user"));
    }

    #[test]
    fn web_session_content_geometry_reserves_status_row() {
        let size = TerminalSize {
            cols: 120,
            rows: 32,
        };

        assert_eq!(
            session_content_geometry(PaneGeometry::new(0, 0, 120, 32), size),
            Some(PaneGeometry::new(0, 0, 120, 31)),
        );
        assert_eq!(
            session_content_geometry(PaneGeometry::new(60, 16, 60, 16), size),
            Some(PaneGeometry::new(60, 16, 60, 15)),
        );
        assert_eq!(
            session_content_geometry(PaneGeometry::new(0, 31, 120, 1), size),
            None,
        );
    }
}
