use rmux_core::input::mode;
use rmux_core::{render_dec_modes_for_snapshot, GridRenderOptions, Screen, ScreenCaptureRange};

const SNAPSHOT_RESET_PREFIX: &[u8] =
    b"\x1b[?2026l\x1b[?1049l\x1b[?6l\x1b[r\x1b[0m\x1b[?25l\x1b[3J\x1b[2J\x1b[H";
const SNAPSHOT_ALT_SCREEN_PREFIX: &[u8] =
    b"\x1b[?1049h\x1b[?6l\x1b[r\x1b[0m\x1b[?25l\x1b[3J\x1b[2J\x1b[H";
const SNAPSHOT_ALT_SCREEN_NO_CURSOR_PREFIX: &[u8] =
    b"\x1b[?47h\x1b[?6l\x1b[r\x1b[0m\x1b[?25l\x1b[3J\x1b[2J\x1b[H";

/// Daemon-owned terminal emulator state used to build a complete ANSI recovery keyframe.
pub(crate) struct PaneRecoveryState {
    pub(crate) cols: u16,
    pub(crate) rows: u16,
    pub(crate) ansi_lines: Vec<Vec<u8>>,
    pub(crate) cursor_row: u16,
    pub(crate) cursor_col: u16,
    pub(crate) cursor_visible: bool,
    pub(crate) mode_bits: u32,
    pub(crate) cursor_style: u32,
    pub(crate) alternate: bool,
    pub(crate) saved_ansi_lines: Option<Vec<Vec<u8>>>,
    pub(crate) saved_cursor: Option<(u16, u16)>,
    pub(crate) scroll_top: u32,
    pub(crate) scroll_bottom: u32,
    /// Undispatched bytes held by the daemon parser at the output boundary.
    pub(crate) pending_bytes: Vec<u8>,
    /// ANSI rendition/charset/hyperlink state for the next raw printable byte.
    pub(crate) active_cell_state: Vec<u8>,
}

impl PaneRecoveryState {
    pub(crate) fn capture(
        screen: &Screen,
        pending_bytes: Vec<u8>,
        active_cell_state: Vec<u8>,
    ) -> Self {
        let size = screen.size();
        let (cursor_col, cursor_row) = screen.cursor_position();
        let (scroll_top, scroll_bottom) = screen.scroll_region();
        let options = GridRenderOptions {
            with_sequences: true,
            trim_spaces: false,
            ..GridRenderOptions::default()
        };
        Self {
            cols: size.cols,
            rows: size.rows,
            ansi_lines: snapshot_ansi_lines(screen),
            cursor_row: cursor_row.min(u32::from(size.rows.saturating_sub(1))) as u16,
            cursor_col: cursor_col.min(u32::from(size.cols.saturating_sub(1))) as u16,
            cursor_visible: screen.mode() & mode::MODE_CURSOR != 0,
            mode_bits: screen.mode(),
            cursor_style: screen.cursor_style(),
            alternate: screen.is_alternate(),
            saved_ansi_lines: screen
                .capture_saved_transcript_lines_independent(ScreenCaptureRange::default(), options),
            saved_cursor: screen.alternate_saved_cursor_position().map(|(col, row)| {
                (
                    row.min(u32::from(size.rows.saturating_sub(1))) as u16,
                    col.min(u32::from(size.cols.saturating_sub(1))) as u16,
                )
            }),
            scroll_top,
            scroll_bottom,
            pending_bytes,
            active_cell_state,
        }
    }

    pub(crate) fn ansi_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.append_ansi_bytes(&mut out);
        out
    }

    pub(crate) fn append_ansi_bytes(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(SNAPSHOT_RESET_PREFIX);
        if self.alternate {
            if let Some(lines) = &self.saved_ansi_lines {
                append_ansi_lines(out, lines);
            }
            if let Some((row, col)) = self.saved_cursor {
                out.extend_from_slice(
                    format!("\x1b[{};{}H", row.saturating_add(1), col.saturating_add(1)).as_bytes(),
                );
                out.extend_from_slice(SNAPSHOT_ALT_SCREEN_PREFIX);
            } else {
                out.extend_from_slice(SNAPSHOT_ALT_SCREEN_NO_CURSOR_PREFIX);
            }
        }
        render_dec_modes_for_snapshot(self.mode_bits, self.cursor_style, out);
        append_ansi_lines(out, &self.ansi_lines);
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
        if self.mode_bits & mode::MODE_ORIGIN != 0 {
            out.extend_from_slice(b"\x1b[?6h");
        }
        out.extend_from_slice(if self.cursor_visible {
            b"\x1b[?25h"
        } else {
            b"\x1b[?25l"
        });
        // The output ring may end while the daemon parser is inside CSI, OSC,
        // DCS, APC, or a UTF-8 codepoint. Replaying that exact undispatched
        // prefix after the complete keyframe leaves the consumer parser at the
        // same boundary before raw post-keyframe bytes begin.
        out.extend_from_slice(b"\x1b[0m\x1b]8;;\x1b\\");
        out.extend_from_slice(&self.active_cell_state);
        out.extend_from_slice(&self.pending_bytes);
    }
}

fn append_ansi_lines(out: &mut Vec<u8>, lines: &[Vec<u8>]) {
    for (index, line) in lines.iter().enumerate() {
        if index > 0 {
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(b"\x1b[0m");
        out.extend_from_slice(line);
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
    use rmux_core::TerminalScreen;
    use rmux_proto::TerminalSize;

    const SIZE: TerminalSize = TerminalSize { cols: 24, rows: 8 };

    #[derive(Debug, PartialEq, Eq)]
    struct RendererState {
        size: TerminalSize,
        lines: Vec<Vec<u8>>,
        cursor: (u32, u32),
        cursor_style: u32,
        cursor_visible: bool,
        mode_bits: u32,
        alternate: bool,
        scroll_region: (u32, u32),
        pending_bytes: Vec<u8>,
    }

    fn renderer_state(terminal: &TerminalScreen) -> RendererState {
        let screen = terminal.screen();
        RendererState {
            size: screen.size(),
            lines: snapshot_ansi_lines(screen),
            cursor: screen.cursor_position(),
            cursor_style: screen.cursor_style(),
            cursor_visible: screen.mode() & mode::MODE_CURSOR != 0,
            mode_bits: screen.mode(),
            alternate: screen.is_alternate(),
            scroll_region: screen.scroll_region(),
            pending_bytes: terminal.pending_bytes(),
        }
    }

    fn assert_all_recovery_boundaries(name: &str, initial: &[u8], output: &[u8]) {
        let mut expected = TerminalScreen::new(SIZE, 100);
        expected.feed(initial);
        expected.feed(output);
        let expected = renderer_state(&expected);

        for split in 0..=output.len() {
            let mut daemon = TerminalScreen::new(SIZE, 100);
            daemon.feed(initial);
            daemon.feed(&output[..split]);
            let recovery = PaneRecoveryState::capture(
                daemon.screen(),
                daemon.pending_bytes(),
                daemon.active_cell_state_ansi(),
            );

            let mut recovered = TerminalScreen::new(SIZE, 100);
            recovered.feed(&recovery.ansi_bytes());
            assert_eq!(
                recovered.pending_bytes(),
                daemon.pending_bytes(),
                "{name} split {split} did not restore parser progress",
            );
            recovered.feed(&output[split..]);
            assert_eq!(
                renderer_state(&recovered),
                expected,
                "{name} split {split} diverged from uninterrupted parsing",
            );
        }
    }

    #[test]
    fn keyframe_restores_every_parser_boundary_before_raw_output() {
        let cases: &[(&str, &[u8], &[u8])] = &[
            ("sgr", b"base", b"\r\n\x1b[38;2;12;34;56mcolored\x1b[0m"),
            (
                "csi-scroll-and-cursor",
                b"one\r\ntwo\r\nthree",
                b"\x1b[3;7r\x1b[4;6Hpositioned",
            ),
            ("osc", b"base", b"\x1b]2;recovery-title\x07\r\nvisible"),
            ("dcs", b"base", b"\x1bP1;2|opaque-payload\x1b\\\r\nafter"),
            ("utf8", b"base", "\r\n界🙂done".as_bytes()),
            (
                "synchronized-redraw",
                b"base",
                b"\x1b[?2026h\x1b[2J\x1b[Hsync\x1b[?2026l",
            ),
            (
                "alternate-screen",
                b"main-buffer",
                b"\x1b[?1049h\x1b[2J\x1b[Halternate\x1b[?25l\x1b[?1049l-after",
            ),
        ];

        for (name, initial, output) in cases {
            assert_all_recovery_boundaries(name, initial, output);
        }
    }
}
