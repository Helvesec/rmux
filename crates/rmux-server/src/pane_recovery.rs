use rmux_core::input::mode;
use rmux_core::{render_dec_modes_for_snapshot, GridRenderOptions, Screen, ScreenCaptureRange};

use crate::pane_transcript::PaneTranscript;

const RESET_PREFIX: &[u8] =
    b"\x1b[?2026l\x1b[?1049l\x1b[?6l\x1b[r\x1b[0m\x1b]8;;\x1b\\\x1b[?25l\x1b[3J\x1b[2J\x1b[H";
const ALT_SCREEN_PREFIX: &[u8] =
    b"\x1b[?1049h\x1b[?6l\x1b[r\x1b[0m\x1b]8;;\x1b\\\x1b[?25l\x1b[3J\x1b[2J\x1b[H";
const ALT_SCREEN_NO_CURSOR_PREFIX: &[u8] =
    b"\x1b[?47h\x1b[?6l\x1b[r\x1b[0m\x1b]8;;\x1b\\\x1b[?25l\x1b[3J\x1b[2J\x1b[H";

/// Owned terminal state copied at an atomic pane boundary.
pub(crate) struct PaneRecoverySeed {
    screen: Screen,
    pending_bytes: Vec<u8>,
    active_cell_state: Vec<u8>,
    saved_cell_state: Vec<u8>,
    saved_cursor: (u32, u32, bool),
    output_sequence: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PaneRecoveryKeyframe {
    pub(crate) cols: u16,
    pub(crate) rows: u16,
    pub(crate) bytes: Vec<u8>,
    pub(crate) alternate: bool,
}

impl PaneRecoverySeed {
    pub(crate) fn capture(transcript: &PaneTranscript) -> Self {
        Self {
            screen: transcript.clone_recovery_screen(),
            pending_bytes: transcript.pending_bytes(),
            active_cell_state: transcript.active_cell_state_ansi(),
            saved_cell_state: transcript.saved_cell_state_ansi(),
            saved_cursor: transcript.saved_cursor_state(),
            output_sequence: transcript.output_sequence(),
        }
    }

    pub(crate) const fn screen(&self) -> &Screen {
        &self.screen
    }

    pub(crate) const fn output_sequence(&self) -> u64 {
        self.output_sequence
    }

    pub(crate) fn keyframe(&self) -> PaneRecoveryKeyframe {
        let size = self.screen.size();
        let mut bytes = Vec::new();
        self.append_ansi(&mut bytes);
        PaneRecoveryKeyframe {
            cols: size.cols,
            rows: size.rows,
            bytes,
            alternate: self.screen.is_alternate(),
        }
    }

    fn append_ansi(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(RESET_PREFIX);
        append_osc_text(out, 2, self.screen.title());
        if self.screen.is_alternate() {
            self.append_saved_main_screen(out);
            out.extend_from_slice(if self.screen.alternate_saved_cursor().is_some() {
                ALT_SCREEN_PREFIX
            } else {
                ALT_SCREEN_NO_CURSOR_PREFIX
            });
        }

        append_ansi_rows(out, &snapshot_ansi_rows(&self.screen, true));
        append_scroll_region(out, &self.screen);
        // Repaint in the neutral reset state. Origin/insert modes and a
        // restricted scrolling region can otherwise redirect or scroll the
        // reconstruction itself. Restore runtime modes only after the grid.
        render_dec_modes_for_snapshot(self.screen.mode(), self.screen.cursor_style(), out);
        append_tab_stops(out, &self.screen);
        self.append_saved_decsc(out);
        self.append_active_cursor_state(out);
        out.extend_from_slice(&self.pending_bytes);
    }

    fn append_saved_main_screen(&self, out: &mut Vec<u8>) {
        let rows = self
            .screen
            .capture_saved_transcript_rows_independent(complete_capture_range(), capture_options());
        if let Some(rows) = rows.as_deref() {
            append_ansi_rows(out, rows);
        }
        let Some((x, y, pending_wrap)) = self.screen.alternate_saved_cursor() else {
            return;
        };
        let visible = self.screen.capture_saved_transcript_lines_independent(
            ScreenCaptureRange::default(),
            capture_options(),
        );
        append_cursor_state(
            out,
            &self.screen,
            x,
            y,
            pending_wrap,
            visible.as_deref(),
            None,
        );
    }

    fn append_saved_decsc(&self, out: &mut Vec<u8>) {
        let (x, y, origin) = self.saved_cursor;
        out.extend_from_slice(b"\x1b[?6l");
        out.extend_from_slice(&self.saved_cell_state);
        if origin {
            out.extend_from_slice(b"\x1b[?6h");
            let row = y
                .saturating_sub(self.screen.scroll_region().0)
                .saturating_add(1);
            append_cup(out, x.saturating_add(1), row);
        } else {
            append_cup(out, x.saturating_add(1), y.saturating_add(1));
        }
        out.extend_from_slice(b"\x1b7");
        out.extend_from_slice(b"\x1b[?6l");
    }

    fn append_active_cursor_state(&self, out: &mut Vec<u8>) {
        let (x, y) = self.screen.cursor_position();
        if self.screen.mode() & mode::MODE_ORIGIN != 0 {
            out.extend_from_slice(b"\x1b[?6h");
        } else {
            out.extend_from_slice(b"\x1b[?6l");
        }
        out.extend_from_slice(b"\x1b[0m\x1b]8;;\x1b\\");
        let lines = snapshot_ansi_lines(&self.screen);
        append_cursor_state(
            out,
            &self.screen,
            x,
            y,
            self.screen.pending_wrap(),
            Some(&lines),
            (self.screen.mode() & mode::MODE_ORIGIN != 0).then(|| self.screen.scroll_region().0),
        );
        // Recreating pending-wrap may repaint the cursor cell and therefore
        // leave that cell's rendition active. Restore the parser rendition
        // after positioning so future raw bytes continue from the boundary.
        out.extend_from_slice(b"\x1b[0m\x1b]8;;\x1b\\");
        out.extend_from_slice(&self.active_cell_state);
        out.extend_from_slice(if self.screen.mode() & mode::MODE_CURSOR != 0 {
            b"\x1b[?25h"
        } else {
            b"\x1b[?25l"
        });
    }
}

fn append_osc_text(out: &mut Vec<u8>, command: u8, value: &str) {
    out.extend_from_slice(b"\x1b]");
    out.extend_from_slice(command.to_string().as_bytes());
    out.push(b';');
    for character in value.chars().filter(|character| !character.is_control()) {
        let mut encoded = [0_u8; 4];
        out.extend_from_slice(character.encode_utf8(&mut encoded).as_bytes());
    }
    out.extend_from_slice(b"\x1b\\");
}

fn append_scroll_region(out: &mut Vec<u8>, screen: &Screen) {
    let (top, bottom) = screen.scroll_region();
    let default_bottom = u32::from(screen.size().rows.max(1)).saturating_sub(1);
    if top != 0 || bottom != default_bottom {
        out.extend_from_slice(
            format!(
                "\x1b[{};{}r",
                top.saturating_add(1),
                bottom.saturating_add(1)
            )
            .as_bytes(),
        );
    }
}

fn append_tab_stops(out: &mut Vec<u8>, screen: &Screen) {
    out.extend_from_slice(b"\x1b[3g");
    for (column, enabled) in screen.tab_stops().iter().copied().enumerate() {
        if enabled {
            append_cup(out, column.saturating_add(1) as u32, 1);
            out.extend_from_slice(b"\x1bH");
        }
    }
}

fn append_cursor_state(
    out: &mut Vec<u8>,
    screen: &Screen,
    x: u32,
    y: u32,
    pending_wrap: bool,
    lines: Option<&[Vec<u8>]>,
    origin_top: Option<u32>,
) {
    let cursor_row = y.saturating_sub(origin_top.unwrap_or(0)).saturating_add(1);
    if pending_wrap {
        if let Some(line) = lines.and_then(|lines| lines.get(y as usize)) {
            append_cup(out, 1, cursor_row);
            out.extend_from_slice(b"\x1b[0m\x1b]8;;\x1b\\");
            out.extend_from_slice(line);
            return;
        }
    }
    let size = screen.size();
    append_cup(
        out,
        x.min(u32::from(size.cols.saturating_sub(1)))
            .saturating_add(1),
        cursor_row,
    );
}

fn append_cup(out: &mut Vec<u8>, col: u32, row: u32) {
    out.extend_from_slice(format!("\x1b[{row};{col}H").as_bytes());
}

fn append_ansi_rows(out: &mut Vec<u8>, rows: &[(Vec<u8>, bool)]) {
    for (index, (line, _wrapped)) in rows.iter().enumerate() {
        if index > 0 && !rows[index - 1].1 {
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(b"\x1b[0m\x1b]8;;\x1b\\");
        out.extend_from_slice(line);
    }
}

fn snapshot_ansi_lines(screen: &Screen) -> Vec<Vec<u8>> {
    screen.capture_transcript_lines_independent(ScreenCaptureRange::default(), capture_options())
}

fn snapshot_ansi_rows(screen: &Screen, include_history: bool) -> Vec<(Vec<u8>, bool)> {
    screen.capture_transcript_rows_independent(
        if include_history {
            complete_capture_range()
        } else {
            ScreenCaptureRange::default()
        },
        capture_options(),
    )
}

const fn complete_capture_range() -> ScreenCaptureRange {
    ScreenCaptureRange {
        start: None,
        end: None,
        start_is_absolute: true,
        end_is_absolute: true,
    }
}

fn capture_options() -> GridRenderOptions {
    GridRenderOptions {
        with_sequences: true,
        // Recovery must retain explicit styled blanks (notably background
        // colour at the right edge) without padding every untouched row to the
        // full terminal width.
        include_empty_cells: false,
        trim_spaces: false,
        ..GridRenderOptions::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pane_transcript::PaneTranscript;
    use rmux_core::TerminalScreen;
    use rmux_proto::TerminalSize;
    use std::io::Write;
    use std::process::{Command, Stdio};

    const SIZE: TerminalSize = TerminalSize { cols: 16, rows: 6 };

    fn recovered(initial: &[u8], tail: &[u8]) -> (TerminalScreen, TerminalScreen) {
        let mut transcript = PaneTranscript::new(100, SIZE);
        transcript.append_bytes(initial);
        let keyframe = PaneRecoverySeed::capture(&transcript).keyframe();

        let mut actual = TerminalScreen::new(SIZE, 100);
        actual.feed(&keyframe.bytes);
        actual.feed(tail);

        let mut expected = TerminalScreen::new(SIZE, 100);
        expected.feed(initial);
        expected.feed(tail);
        (actual, expected)
    }

    fn assert_visible_equal(actual: &TerminalScreen, expected: &TerminalScreen) {
        assert_eq!(
            snapshot_ansi_lines(actual.screen()),
            snapshot_ansi_lines(expected.screen())
        );
        assert_eq!(
            actual.screen().cursor_position(),
            expected.screen().cursor_position()
        );
        assert_eq!(actual.screen().mode(), expected.screen().mode());
        assert_eq!(
            actual.screen().scroll_region(),
            expected.screen().scroll_region()
        );
        assert_eq!(actual.pending_bytes(), expected.pending_bytes());
    }

    fn assert_complete_equal(actual: &TerminalScreen, expected: &TerminalScreen) {
        assert_visible_equal(actual, expected);
        assert_eq!(
            snapshot_ansi_rows(actual.screen(), true),
            snapshot_ansi_rows(expected.screen(), true)
        );
        assert_eq!(
            actual.screen().history_size(),
            expected.screen().history_size()
        );
    }

    #[test]
    fn keyframe_preserves_pending_wrap_before_continuation() {
        let (actual, expected) = recovered(b"0123456789abcdef", b"X");
        assert_visible_equal(&actual, &expected);
    }

    #[test]
    fn keyframe_preserves_custom_tabs() {
        let (actual, expected) = recovered(b"\x1b[3g\x1b[1;4H\x1bH\x1b[H", b"\tX");
        assert_visible_equal(&actual, &expected);
    }

    #[test]
    fn keyframe_preserves_decsc_state() {
        let initial = b"\x1b[31m\x1b[2;3H\x1b7\x1b[0m\x1b[5;10H";
        let (actual, expected) = recovered(initial, b"\x1b8X");
        assert_visible_equal(&actual, &expected);
    }

    #[test]
    fn keyframe_preserves_incomplete_parser_state() {
        let (actual, expected) = recovered(b"base\x1b[38;2;1", b"2;34;56mX");
        assert_visible_equal(&actual, &expected);
    }

    #[test]
    fn keyframe_preserves_main_and_alternate_buffers() {
        let initial = b"main\x1b[?1049h\x1b[2J\x1b[Halt";
        let (actual, expected) = recovered(initial, b"\x1b[?1049lX");
        assert_visible_equal(&actual, &expected);
    }

    #[test]
    fn keyframe_preserves_saved_main_pending_wrap_while_alternate_is_active() {
        let initial = b"0123456789abcdef\x1b[?1049h\x1b[2J\x1b[Hdifferent-alt";
        let (actual, expected) = recovered(initial, b"\x1b[?1049lX");
        assert_visible_equal(&actual, &expected);
    }

    #[test]
    fn keyframe_preserves_scrollback_and_wrapped_rows() {
        let initial = b"abcdefghABCDEFGH\r\nline-2\r\nline-3\r\nline-4";
        let (mut actual, mut expected) = recovered(initial, b"\r\nsuffix");
        assert_complete_equal(&actual, &expected);

        let resized = TerminalSize { cols: 10, rows: 4 };
        actual.resize(resized);
        expected.resize(resized);
        assert_complete_equal(&actual, &expected);
    }

    #[test]
    fn keyframe_preserves_unicode_titles_without_treating_utf8_as_c1() {
        let (actual, expected) = recovered("\u{1b}]2;hé-界\u{7}".as_bytes(), b"");
        assert_eq!(actual.screen().title(), expected.screen().title());
        assert_eq!(actual.screen().title(), "hé-界");
    }

    #[test]
    #[ignore = "release gate installs and runs the pinned xterm.js oracle"]
    fn keyframes_converge_in_independent_xterm_oracle() {
        let vectors = [
            (
                "scrollback-wrap-title",
                TerminalSize { cols: 12, rows: 4 },
                b"\x1b]2;build\x07\x1b[31mred-wide-\xe7\x95\x8c\r\nline-2\r\nline-3\r\nline-4\r\nline-5"
                    .as_slice(),
                b"\r\nTAIL".as_slice(),
            ),
            (
                "alternate-saved-main",
                TerminalSize { cols: 16, rows: 5 },
                b"main-one\r\nmain-two\x1b[3;4H\x1b[?1049h\x1b[32mALT\x1b[5;8H"
                    .as_slice(),
                b"\x1b[?1049lZ".as_slice(),
            ),
            (
                "alternate-saved-main-pending-wrap",
                TerminalSize { cols: 16, rows: 5 },
                b"0123456789abcdef\x1b[?1049h\x1b[2J\x1b[Hdifferent-alt".as_slice(),
                b"\x1b[?1049lX".as_slice(),
            ),
            (
                "tabs-decsc-parser",
                TerminalSize { cols: 16, rows: 6 },
                b"\x1b[3g\x1b[1;5H\x1bH\x1b[31m\x1b[2;3H\x1b7\x1b[0m\x1b[5;10H\x1b[38;2;1"
                    .as_slice(),
                b"2;34;56mX\x1b8\tY".as_slice(),
            ),
            (
                "styled-trailing-blanks",
                TerminalSize { cols: 12, rows: 4 },
                b"\x1b[44mA    \x1b[0m\x1b[2;1Hplain\x1b[2;12H\x1b[41m \x1b[0m"
                    .as_slice(),
                b"\x1b[3;1Htail".as_slice(),
            ),
            (
                "pending-wrap-wide-unicode-title",
                TerminalSize { cols: 12, rows: 4 },
                "0123456789ab\u{1b}]2;hé-界\u{7}".as_bytes(),
                "界".as_bytes(),
            ),
            (
                "origin-region-cursor-style",
                TerminalSize { cols: 16, rows: 6 },
                b"top\r\nmiddle\r\nbottom\x1b[2;5r\x1b[?6h\x1b[3 q\x1b[2;4H\x1b[7mX"
                    .as_slice(),
                b"\x1b[0mY".as_slice(),
            ),
        ];
        let vectors = vectors
            .into_iter()
            .map(|(name, size, initial, tail)| {
                let mut transcript = PaneTranscript::new(100, size);
                transcript.append_bytes(initial);
                let keyframe = PaneRecoverySeed::capture(&transcript).keyframe();
                serde_json::json!({
                    "name": name,
                    "cols": size.cols,
                    "rows": size.rows,
                    "scrollback": 100,
                    "initial": initial,
                    "keyframe": keyframe.bytes,
                    "tail": tail,
                })
            })
            .collect::<Vec<_>>();
        let oracle_dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/xterm-oracle");
        let mut child = Command::new("node")
            .arg("recovery-oracle.mjs")
            .current_dir(&oracle_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start pinned xterm.js recovery oracle");
        child
            .stdin
            .as_mut()
            .expect("oracle stdin")
            .write_all(
                serde_json::to_vec(&serde_json::json!({ "vectors": vectors }))
                    .expect("encode oracle vectors")
                    .as_slice(),
            )
            .expect("write oracle vectors");
        let output = child.wait_with_output().expect("wait for xterm.js oracle");
        assert!(
            output.status.success(),
            "xterm.js recovery oracle failed:\nstdout={}\nstderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
}
