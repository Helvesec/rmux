use rmux_core::{GridRenderOptions, Screen, ScreenCaptureRange};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct WebPaneSnapshot {
    pub(crate) cols: u16,
    pub(crate) rows: u16,
    pub(crate) output_sequence: u64,
    pub(crate) ansi_lines: Vec<Vec<u8>>,
    pub(crate) cursor_row: u16,
    pub(crate) cursor_col: u16,
    pub(crate) cursor_visible: bool,
}

impl WebPaneSnapshot {
    pub(crate) fn ansi_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"\x1bc\x1b[?25l\x1b[H");
        for (index, line) in self.ansi_lines.iter().enumerate() {
            if index > 0 {
                out.extend_from_slice(b"\r\n");
            }
            out.extend_from_slice(b"\x1b[0m");
            out.extend_from_slice(line);
        }
        let cursor_row = self.cursor_row.min(self.rows.saturating_sub(1)) + 1;
        let cursor_col = self.cursor_col.min(self.cols.saturating_sub(1)) + 1;
        out.extend_from_slice(format!("\x1b[0m\x1b[{cursor_row};{cursor_col}H").as_bytes());
        out.extend_from_slice(if self.cursor_visible {
            b"\x1b[?25h"
        } else {
            b"\x1b[?25l"
        });
        out
    }
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
    use rmux_core::{input::InputParser, Screen};
    use rmux_proto::TerminalSize;

    #[test]
    fn web_snapshot_bytes_preserve_ansi_style_and_cursor() {
        let snapshot = WebPaneSnapshot {
            cols: 80,
            rows: 24,
            output_sequence: 7,
            ansi_lines: vec![b"\x1b[32mpingu@host\x1b[0m".to_vec()],
            cursor_row: 3,
            cursor_col: 7,
            cursor_visible: true,
        };

        let bytes = snapshot.ansi_bytes();
        let rendered = String::from_utf8(bytes).expect("snapshot bytes are utf8");

        assert!(rendered.contains("\x1b[32mpingu@host"));
        assert!(rendered.contains("\x1b[4;8H\x1b[?25h"));
    }

    #[test]
    fn web_snapshot_capture_preserves_screen_sequences() {
        let mut screen = Screen::new(TerminalSize { cols: 12, rows: 3 }, 100);
        let mut parser = InputParser::new();
        parser.parse(b"\x1b[32mpingu\x1b[0m@host", &mut screen);

        let lines = snapshot_ansi_lines(&screen);
        let joined = String::from_utf8(lines.concat()).expect("snapshot lines are utf8");

        assert!(joined.contains("\x1b[32m"));
        assert!(joined.contains("pingu"));
    }
}
