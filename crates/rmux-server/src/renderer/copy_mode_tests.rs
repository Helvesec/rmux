use rmux_core::{
    input::{GridAttr, InputParser},
    OptionStore, Screen,
};
use rmux_proto::{OptionName, ScopeSelector, SessionName, SetOptionMode, TerminalSize};

use crate::copy_mode::{CopyModeOverlayRange, CopyModeRenderOverlays, CopyModeRenderSnapshot};

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

fn screen_with(bytes: &[u8], size: TerminalSize) -> Screen {
    let mut screen = Screen::new(size, 100);
    let mut parser = InputParser::new();
    parser.parse(bytes, &mut screen);
    screen
}

fn visible_colours(screen: &Screen, cols: usize) -> Vec<(i32, i32)> {
    let mut colours = Vec::new();
    assert!(screen.visit_visible_line_cells(0, cols, |cell| {
        colours.push((cell.fg(), cell.bg()));
    }));
    colours
}

fn line_number_options(mode: &str) -> OptionStore {
    let mut options = OptionStore::new();
    options
        .set(
            ScopeSelector::Global,
            OptionName::CopyModeLineNumbers,
            mode.to_owned(),
            SetOptionMode::Replace,
        )
        .expect("copy-mode line-number option set succeeds");
    options
}

fn line_number_snapshot(
    screen: Screen,
    history_size: usize,
    scroll_position: usize,
) -> CopyModeRenderSnapshot {
    CopyModeRenderSnapshot {
        screen,
        overlays: CopyModeRenderOverlays::default(),
        history_size,
        scroll_position,
        alternate_on: false,
        line_numbers_enabled: true,
    }
}

#[test]
fn copy_mode_overlays_follow_tmux_37b_priority_and_mark_the_full_line() {
    let size = TerminalSize { cols: 8, rows: 2 };
    let session = crate::session_fixture::SessionFixture::new(session_name("alpha"), size);
    let pane = session.window().pane(0).expect("pane exists");
    let mut screen = screen_with(b"abcdefgh", size);
    screen.mark_selected_row_range(0, 4, 4);
    let overlays = CopyModeRenderOverlays {
        mark: Some(CopyModeOverlayRange {
            row: 0,
            start_x: 0,
            end_x: 7,
        }),
        matches: vec![CopyModeOverlayRange {
            row: 0,
            start_x: 1,
            end_x: 5,
        }],
        current_match: Some(CopyModeOverlayRange {
            row: 0,
            start_x: 3,
            end_x: 5,
        }),
    };

    let styled = super::pane_screen::overlay_styled_tail_screen(
        session.view(),
        &OptionStore::new(),
        pane,
        &screen,
        Some(&overlays),
    )
    .expect("copy-mode overlays apply");
    let colours = visible_colours(&styled, 8);

    assert_eq!(colours[0], (0, 1), "mark style starts at column zero");
    assert_eq!(colours[1], (0, 6), "regular match overrides mark");
    assert_eq!(colours[3], (0, 5), "current match overrides regular match");
    assert_eq!(colours[4], (0, 3), "selection is the final overlay");
    assert_eq!(colours[5], (0, 5), "current match covers its full range");
    assert_eq!(colours[7], (0, 1), "mark style covers the complete line");
}

#[test]
fn copy_mode_overlay_options_resolve_runtime_templates_and_explicit_styles() {
    let size = TerminalSize { cols: 4, rows: 2 };
    let session = crate::session_fixture::SessionFixture::new(session_name("alpha"), size);
    let pane = session.window().pane(0).expect("pane exists");
    let screen = screen_with(b"abcd", size);
    let mut options = OptionStore::new();
    for (option, value) in [
        (OptionName::ModeStyle, "bg=blue,fg=white"),
        (OptionName::CopyModeMarkStyle, "#{E:mode-style}"),
        (OptionName::CopyModeMatchStyle, "bg=green,fg=white"),
        (OptionName::CopyModeCurrentMatchStyle, "bg=yellow,fg=black"),
    ] {
        options
            .set(
                ScopeSelector::Global,
                option,
                value.to_owned(),
                SetOptionMode::Replace,
            )
            .expect("copy-mode style option set succeeds");
    }
    let overlays = CopyModeRenderOverlays {
        mark: Some(CopyModeOverlayRange {
            row: 0,
            start_x: 0,
            end_x: 3,
        }),
        matches: vec![CopyModeOverlayRange {
            row: 0,
            start_x: 1,
            end_x: 2,
        }],
        current_match: Some(CopyModeOverlayRange {
            row: 0,
            start_x: 2,
            end_x: 2,
        }),
    };

    let styled = super::pane_screen::overlay_styled_tail_screen(
        session.view(),
        &options,
        pane,
        &screen,
        Some(&overlays),
    )
    .expect("copy-mode overlays apply");
    let colours = visible_colours(&styled, 4);

    assert_eq!(colours[0], (7, 4), "mark expands mode-style at runtime");
    assert_eq!(colours[1], (7, 2), "match style option is consumed");
    assert_eq!(
        colours[2],
        (0, 3),
        "current-match style option overrides match"
    );
    assert_eq!(colours[3], (7, 4), "mark resumes after the match");
}

#[test]
fn rendered_copy_mode_frame_paints_marked_trailing_cells() {
    let size = TerminalSize { cols: 6, rows: 2 };
    let session = crate::session_fixture::SessionFixture::new(session_name("alpha"), size);
    let pane = session.window().pane(0).expect("pane exists");
    let snapshot = CopyModeRenderSnapshot {
        screen: screen_with(b"x", size),
        history_size: 0,
        scroll_position: 0,
        alternate_on: false,
        line_numbers_enabled: false,
        overlays: CopyModeRenderOverlays {
            mark: Some(CopyModeOverlayRange {
                row: 0,
                start_x: 0,
                end_x: 5,
            }),
            ..CopyModeRenderOverlays::default()
        },
    };

    let frame = super::pane_screen::render_copy_mode_pane_screen(
        session.view(),
        &OptionStore::new(),
        pane,
        &snapshot,
    );

    assert!(
        frame
            .windows(b"x     ".len())
            .any(|window| window == b"x     "),
        "mark rendering must paint trailing cells instead of clearing them: {frame:?}"
    );
}

#[test]
fn gh217_copy_mode_treats_application_space_tails_as_empty_without_an_overlay() {
    let size = TerminalSize { cols: 20, rows: 2 };
    let session = crate::session_fixture::SessionFixture::new(session_name("alpha"), size);
    let pane = session.window().pane(0).expect("pane exists");
    let snapshot = CopyModeRenderSnapshot {
        screen: screen_with(b"A       ", size),
        history_size: 0,
        scroll_position: 0,
        alternate_on: false,
        line_numbers_enabled: false,
        overlays: CopyModeRenderOverlays::default(),
    };

    let frame = super::pane_screen::render_copy_mode_pane_screen(
        session.view(),
        &OptionStore::new(),
        pane,
        &snapshot,
    );

    assert!(
        frame
            .windows(b"A\x1b[K".len())
            .any(|window| window == b"A\x1b[K"),
        "copy mode without a visual tail overlay must clear matching application spaces: {frame:?}"
    );
    assert!(
        !frame
            .windows(b"A       \x1b[K".len())
            .any(|window| window == b"A       \x1b[K"),
        "copy mode must not retain the application-space discriminator literally: {frame:?}"
    );
}

#[test]
fn copy_mode_line_number_gutter_uses_tmux_default_and_current_styles() {
    let size = TerminalSize { cols: 20, rows: 3 };
    let session = crate::session_fixture::SessionFixture::new(session_name("alpha"), size);
    let pane = session.window().pane(0).expect("pane exists");
    let snapshot = line_number_snapshot(screen_with(b"alpha\r\nbeta", size), 0, 0);

    let frame = String::from_utf8(super::pane_screen::render_copy_mode_pane_screen(
        session.view(),
        &line_number_options("absolute"),
        pane,
        &snapshot,
    ))
    .expect("copy-mode frame is utf-8");

    assert!(
        frame.contains("\u{1b}[0;2;37m  1 \u{1b}[0malpha"),
        "non-current numbers should use fg=white,dim: {frame:?}"
    );
    assert!(
        frame.contains("\u{1b}[33m  2 \u{1b}[0mbeta"),
        "the cursor row should use fg=yellow: {frame:?}"
    );
}

#[test]
fn copy_mode_line_number_gutter_is_inside_the_scrollbar_content_geometry() {
    let size = TerminalSize { cols: 20, rows: 3 };
    let session = crate::session_fixture::SessionFixture::new(session_name("alpha"), size);
    let pane = session.window().pane(0).expect("pane exists");
    let mut options = line_number_options("absolute");
    options
        .set(
            ScopeSelector::Global,
            OptionName::PaneScrollbars,
            "on".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("pane scrollbar option set succeeds");
    let snapshot = line_number_snapshot(screen_with(b"content", size), 23, 23);

    let frame = String::from_utf8(super::pane_screen::render_copy_mode_pane_screen(
        session.view(),
        &options,
        pane,
        &snapshot,
    ))
    .expect("copy-mode frame is utf-8");

    assert!(
        frame.contains("\u{1b}[33m  1 \u{1b}[0mcontent"),
        "the gutter should precede pane content: {frame:?}"
    );
    assert!(
        frame.contains("\u{1b}[1;20H"),
        "the right scrollbar should remain in physical column 20: {frame:?}"
    );
}

#[test]
fn copy_mode_line_number_gutter_marks_a_cursor_beyond_reduced_content() {
    let size = TerminalSize { cols: 8, rows: 2 };
    let session = crate::session_fixture::SessionFixture::new(session_name("alpha"), size);
    let pane = session.window().pane(0).expect("pane exists");
    let snapshot = line_number_snapshot(screen_with(b"\x1b[1;8H", size), 0, 0);

    let frame = String::from_utf8(super::pane_screen::render_copy_mode_pane_screen(
        session.view(),
        &line_number_options("absolute"),
        pane,
        &snapshot,
    ))
    .expect("copy-mode frame is utf-8");

    assert!(
        frame.contains("\u{1b}[1;8H\u{1b}[0m$\u{1b}[0m"),
        "tmux marks a cursor clipped by the gutter with '$': {frame:?}"
    );
}

// GH-217 copy-mode residual: the renderer used to disable synthetic-tail
// clearing for the whole pane as soon as any selected cell, mark, search match
// or current match existed, so a selection on one row padded every other row
// with literal spaces. tmux decides per cell instead: `tty_draw_line`
// (`tty-draw.c:246-261`) styles the cell copy through `screen_select_cell`
// (`screen.c:599`) and only then asks whether it is empty, so an unhighlighted
// row is never affected by a highlight somewhere else.
//
// Every expectation below was measured against the pinned tmux 3.7b oracle
// (`/usr/local/bin/tmux`, `tmux -V` = 3.7b) on an `xterm-256color` client, by
// capturing the bytes tmux writes across `refresh-client` while copy mode is
// live. The relevant capture is quoted above each test. Only the escape
// spellings (`\e[39m\e[49m` where tmux writes `\e(B\e[m`) and the save/restore
// wrapper are pre-existing rmux frame conventions this change does not touch.
//
// Overlay-painted blanks that reach the row edge must remain literal. An EL or
// ECH under the overlay background happens to agree on a BCE terminal, but a
// non-BCE terminal erases with its default background. tmux keeps `NOATTR` on
// the styled cell copy, making the overlay an internal nonempty cell without
// emitting another SGR. The regressions below replay RMUX's bytes under both
// erase models so BCE behavior cannot mask a lost tail.

const ORACLE_ROWS: [&[u8]; 3] = [b"AAAA", b"BBBBBBBB", b"CCCC"];

/// Pane rows for a terminal of `rows`; the status line owns the last one.
const fn pane_rows(rows: u16) -> usize {
    (rows - 1) as usize
}

fn oracle_screen(size: TerminalSize) -> Screen {
    let mut bytes = Vec::new();
    for row in ORACLE_ROWS {
        bytes.extend_from_slice(row);
        bytes.extend_from_slice(b"\r\n");
    }
    screen_with(&bytes, size)
}

fn copy_mode_snapshot(screen: Screen, overlays: CopyModeRenderOverlays) -> CopyModeRenderSnapshot {
    CopyModeRenderSnapshot {
        screen,
        overlays,
        history_size: 0,
        scroll_position: 0,
        alternate_on: false,
        line_numbers_enabled: false,
    }
}

/// The longest run of literal `0x20` bytes in a frame.
///
/// tmux never spells an *empty* run of ten or more columns literally; past
/// that width it uses EL, EL1 or ECH (`tty_draw_line_clear`, `tty-draw.c:62`).
/// A run of ten or more literal spaces therefore proves the pane fell back to
/// full-width padding, which is the GH-217 symptom.
fn longest_literal_space_run(frame: &[u8]) -> usize {
    let mut longest = 0;
    let mut run = 0;
    for byte in frame {
        if *byte == b' ' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    longest
}

fn erase_to_eol_count(frame: &[u8]) -> usize {
    frame
        .windows(b"\x1b[K".len())
        .filter(|window| *window == b"\x1b[K")
        .count()
}

fn contains(frame: &[u8], needle: &[u8]) -> bool {
    frame.windows(needle.len()).any(|window| window == needle)
}

/// Replays `frame` onto a screen pre-painted with `filler`, so assertions run
/// against the cells an outer terminal would end up with rather than against
/// one particular spelling of the escape sequences.
fn replay_frame(frame: &[u8], size: TerminalSize, filler: &[u8]) -> Screen {
    let mut outer = Screen::new(size, 0);
    let mut parser = InputParser::new();
    parser.parse(filler, &mut outer);
    parser.parse(frame, &mut outer);
    outer
}

fn row_cells(screen: &Screen, row: usize, cols: usize) -> Vec<(String, i32, i32)> {
    let mut cells = Vec::new();
    assert!(screen.visit_visible_line_cells(row, cols, |cell| {
        cells.push((cell.text().to_owned(), cell.fg(), cell.bg()));
    }));
    cells
}

fn row_text(screen: &Screen, row: usize, cols: usize) -> String {
    row_cells(screen, row, cols)
        .into_iter()
        .map(|(text, _, _)| text)
        .collect()
}

/// Minimal outer-terminal model for the renderer sequences exercised here.
///
/// RMUX's `InputParser` models background-colour erase. This independent model
/// makes BCE an input so the same emitted frame is checked against both erase
/// semantics.
struct OuterBackgroundReplay {
    cols: usize,
    rows: usize,
    cursor_x: usize,
    cursor_y: usize,
    saved_cursor: (usize, usize),
    background: i16,
    bce: bool,
    cells: Vec<i16>,
}

impl OuterBackgroundReplay {
    fn new(size: TerminalSize, bce: bool) -> Self {
        let cols = usize::from(size.cols);
        let rows = usize::from(size.rows);
        Self {
            cols,
            rows,
            cursor_x: 0,
            cursor_y: 0,
            saved_cursor: (0, 0),
            background: -1,
            bce,
            cells: vec![6; cols * rows],
        }
    }

    fn replay(mut self, frame: &[u8]) -> Self {
        let mut index = 0;
        while index < frame.len() {
            match frame[index] {
                0x1b if frame.get(index + 1) == Some(&b'[') => {
                    let mut end = index + 2;
                    while end < frame.len() && !(0x40..=0x7e).contains(&frame[end]) {
                        end += 1;
                    }
                    assert!(end < frame.len(), "unterminated CSI in {frame:?}");
                    self.apply_csi(&frame[index + 2..end], frame[end]);
                    index = end + 1;
                }
                0x1b if frame.get(index + 1) == Some(&b']') => {
                    index += 2;
                    while index < frame.len() {
                        if frame[index] == 0x07 {
                            index += 1;
                            break;
                        }
                        if frame[index] == 0x1b && frame.get(index + 1) == Some(&b'\\') {
                            index += 2;
                            break;
                        }
                        index += 1;
                    }
                }
                0x1b if matches!(frame.get(index + 1), Some(b'(' | b')')) => {
                    index = (index + 3).min(frame.len());
                }
                0x1b => index = (index + 2).min(frame.len()),
                b'\r' => {
                    self.cursor_x = 0;
                    index += 1;
                }
                b'\n' => {
                    self.cursor_y = self
                        .cursor_y
                        .saturating_add(1)
                        .min(self.rows.saturating_sub(1));
                    index += 1;
                }
                byte if (0x20..=0x7e).contains(&byte) => {
                    let cell_index = self.cursor_y * self.cols + self.cursor_x;
                    if let Some(cell) = self.cells.get_mut(cell_index) {
                        *cell = self.background;
                    }
                    self.cursor_x = self
                        .cursor_x
                        .saturating_add(1)
                        .min(self.cols.saturating_sub(1));
                    index += 1;
                }
                _ => index += 1,
            }
        }
        self
    }

    fn apply_csi(&mut self, raw_params: &[u8], final_byte: u8) {
        let params_text = std::str::from_utf8(raw_params).expect("CSI params are ASCII");
        let params = params_text
            .trim_start_matches('?')
            .split(';')
            .map(|part| part.parse::<usize>().unwrap_or(0))
            .collect::<Vec<_>>();
        let value = |slot: usize, default: usize| {
            params
                .get(slot)
                .copied()
                .filter(|value| *value != 0)
                .unwrap_or(default)
        };

        match final_byte {
            b'H' | b'f' => {
                self.cursor_y = value(0, 1)
                    .saturating_sub(1)
                    .min(self.rows.saturating_sub(1));
                self.cursor_x = value(1, 1)
                    .saturating_sub(1)
                    .min(self.cols.saturating_sub(1));
            }
            b'C' => {
                self.cursor_x = self
                    .cursor_x
                    .saturating_add(value(0, 1))
                    .min(self.cols.saturating_sub(1));
            }
            b'D' => self.cursor_x = self.cursor_x.saturating_sub(value(0, 1)),
            b'X' => self.erase(self.cursor_x, self.cursor_x.saturating_add(value(0, 1))),
            b'K' => match params.first().copied().unwrap_or(0) {
                1 => self.erase(0, self.cursor_x.saturating_add(1)),
                2 => self.erase(0, self.cols),
                _ => self.erase(self.cursor_x, self.cols),
            },
            b'm' => self.apply_sgr(&params),
            b's' => self.saved_cursor = (self.cursor_x, self.cursor_y),
            b'u' => (self.cursor_x, self.cursor_y) = self.saved_cursor,
            _ => {}
        }
    }

    fn erase(&mut self, start: usize, end: usize) {
        let background = if self.bce { self.background } else { -1 };
        for x in start.min(self.cols)..end.min(self.cols) {
            self.cells[self.cursor_y * self.cols + x] = background;
        }
    }

    fn apply_sgr(&mut self, params: &[usize]) {
        let default = [0];
        let params = if params.is_empty() { &default } else { params };
        let mut slot = 0;
        while slot < params.len() {
            match params[slot] {
                0 => self.background = -1,
                40..=47 => self.background = (params[slot] - 40) as i16,
                49 => self.background = -1,
                100..=107 => self.background = (params[slot] - 100 + 8) as i16,
                48 if params.get(slot + 1) == Some(&5) && params.get(slot + 2).is_some() => {
                    self.background = params[slot + 2] as i16;
                    slot += 2;
                }
                _ => {}
            }
            slot += 1;
        }
    }

    fn background_count(&self, row: usize, start: usize, background: i16) -> usize {
        self.cells[row * self.cols + start..(row + 1) * self.cols]
            .iter()
            .filter(|cell| **cell == background)
            .count()
    }
}

fn assert_overlay_edge_survives_without_bce(
    frame: &[u8],
    size: TerminalSize,
    literal_prefix: &[u8],
    literal_spaces: usize,
    styled_start: usize,
    background: i16,
    styled_cells: usize,
) {
    let mut literal_tail = literal_prefix.to_vec();
    literal_tail.extend(std::iter::repeat_n(b' ', literal_spaces));
    assert!(
        contains(frame, &literal_tail),
        "overlay marker must not emit an SGR or collapse the literal tail: {frame:?}"
    );

    for bce in [true, false] {
        let replay = OuterBackgroundReplay::new(size, bce).replay(frame);
        assert_eq!(
            replay.background_count(0, styled_start, background),
            styled_cells,
            "overlay background must survive with bce={bce}: {frame:?}"
        );
    }
}

fn selected_edge_frame(preserve_prompt_cursor: bool) -> Vec<u8> {
    let size = TerminalSize { cols: 40, rows: 3 };
    let session = crate::session_fixture::SessionFixture::new(session_name("selected-edge"), size);
    let pane = session.window().pane(0).expect("pane exists");
    let mut screen = screen_with(b"BBBB", size);
    screen.mark_selected_row_range(0, 4, 39);
    let snapshot = copy_mode_snapshot(screen, CopyModeRenderOverlays::default());
    if preserve_prompt_cursor {
        super::pane_screen::render_copy_mode_pane_screen_preserving_prompt_cursor(
            session.view(),
            &OptionStore::new(),
            pane,
            &snapshot,
        )
    } else {
        super::pane_screen::render_copy_mode_pane_screen(
            session.view(),
            &OptionStore::new(),
            pane,
            &snapshot,
        )
    }
}

fn selected_edge_delta_frame() -> Vec<u8> {
    let size = TerminalSize { cols: 40, rows: 3 };
    let session =
        crate::session_fixture::SessionFixture::new(session_name("selected-edge-delta"), size);
    let pane = session.window().pane(0).expect("pane exists");
    let mut screen = screen_with(b"BBBB", size);
    screen.mark_selected_row_range(0, 4, 39);
    super::PaneRenderSnapshot::capture(session.view(), &OptionStore::new(), pane, &screen)
        .expect("delta snapshot captures")
        .full_frame()
}

fn visual_overlay_edge_frame(overlays: CopyModeRenderOverlays) -> Vec<u8> {
    let size = TerminalSize { cols: 40, rows: 3 };
    let session =
        crate::session_fixture::SessionFixture::new(session_name("visual-overlay-edge"), size);
    let pane = session.window().pane(0).expect("pane exists");
    let snapshot = copy_mode_snapshot(screen_with(b"BBBB", size), overlays);
    super::pane_screen::render_copy_mode_pane_screen(
        session.view(),
        &OptionStore::new(),
        pane,
        &snapshot,
    )
}

#[test]
fn gh217_selected_blank_edge_is_literal_on_non_bce_full_redraw() {
    let size = TerminalSize { cols: 40, rows: 3 };
    let frame = selected_edge_frame(false);
    assert_overlay_edge_survives_without_bce(&frame, size, b"BBBB\x1b[30m\x1b[43m", 36, 4, 3, 36);
}

#[test]
fn gh217_selected_blank_edge_is_literal_on_non_bce_prompt_cursor_redraw() {
    let size = TerminalSize { cols: 40, rows: 3 };
    let frame = selected_edge_frame(true);
    assert_overlay_edge_survives_without_bce(&frame, size, b"BBBB\x1b[30m\x1b[43m", 36, 4, 3, 36);
}

#[test]
fn gh217_selected_blank_edge_is_literal_on_non_bce_delta_capture() {
    let size = TerminalSize { cols: 40, rows: 3 };
    let frame = selected_edge_delta_frame();
    assert_overlay_edge_survives_without_bce(&frame, size, b"BBBB\x1b[30m\x1b[43m", 36, 4, 3, 36);
}

#[test]
fn gh217_mark_blank_edge_is_literal_on_non_bce() {
    let size = TerminalSize { cols: 40, rows: 3 };
    let frame = visual_overlay_edge_frame(CopyModeRenderOverlays {
        mark: Some(CopyModeOverlayRange {
            row: 0,
            start_x: 0,
            end_x: 39,
        }),
        ..CopyModeRenderOverlays::default()
    });
    assert_overlay_edge_survives_without_bce(&frame, size, b"\x1b[30m\x1b[41mBBBB", 36, 0, 1, 40);
}

#[test]
fn gh217_match_blank_edge_is_literal_on_non_bce() {
    let size = TerminalSize { cols: 40, rows: 3 };
    let frame = visual_overlay_edge_frame(CopyModeRenderOverlays {
        matches: vec![CopyModeOverlayRange {
            row: 0,
            start_x: 4,
            end_x: 39,
        }],
        ..CopyModeRenderOverlays::default()
    });
    assert_overlay_edge_survives_without_bce(&frame, size, b"BBBB\x1b[30m\x1b[46m", 36, 4, 6, 36);
}

#[test]
fn gh217_current_match_blank_edge_is_literal_on_non_bce() {
    let size = TerminalSize { cols: 40, rows: 3 };
    let frame = visual_overlay_edge_frame(CopyModeRenderOverlays {
        current_match: Some(CopyModeOverlayRange {
            row: 0,
            start_x: 4,
            end_x: 39,
        }),
        ..CopyModeRenderOverlays::default()
    });
    assert_overlay_edge_survives_without_bce(&frame, size, b"BBBB\x1b[30m\x1b[45m", 36, 4, 5, 36);
}

#[test]
fn gh217_overlay_nonempty_marker_is_scoped_to_painted_cells() {
    let size = TerminalSize { cols: 8, rows: 2 };
    let session = crate::session_fixture::SessionFixture::new(session_name("overlay-marker"), size);
    let pane = session.window().pane(0).expect("pane exists");
    let screen = screen_with(b"abcdefgh", size);
    let overlays = CopyModeRenderOverlays {
        matches: vec![CopyModeOverlayRange {
            row: 0,
            start_x: 2,
            end_x: 4,
        }],
        ..CopyModeRenderOverlays::default()
    };
    let overlaid = super::pane_screen::overlay_styled_tail_screen(
        session.view(),
        &OptionStore::new(),
        pane,
        &screen,
        Some(&overlays),
    )
    .expect("match overlay applies");
    let mut attrs = Vec::new();
    assert!(overlaid.visit_visible_line_cells(0, 8, |cell| attrs.push(cell.attr())));
    assert_eq!(
        attrs
            .iter()
            .map(|attr| attr & GridAttr::NOATTR != 0)
            .collect::<Vec<_>>(),
        [false, false, true, true, true, false, false, false],
        "only cells painted by the overlay carry the internal nonempty marker"
    );
}

#[test]
fn gh217_copy_mode_selection_keeps_tail_clears_on_unrelated_rows() {
    // tmux 3.7b, 40 columns, selection over `BBBB` on row 1:
    //   \e[HAAAA\e[31X\e[30m\e[43m\e[31C[0/0]\e(B\e[m\e[30m\e[43m\e[2;1H
    //   BBBB\e(B\e[mBBBB\e[K\r\nCCCC\e[K\r\n\e[K\r\n\e[K
    // Every row keeps its own EL; only the four selected cells are styled.
    // tmux spends row 0's tail on ECH because its `[0/0]` position indicator
    // occupies the right edge; this snapshot paints no indicator, so the same
    // rule reaches EL there instead (`tty_draw_line_clear`, `tty-draw.c:65`).
    let size = TerminalSize { cols: 40, rows: 6 };
    let session = crate::session_fixture::SessionFixture::new(session_name("alpha"), size);
    let pane = session.window().pane(0).expect("pane exists");
    let mut screen = oracle_screen(size);
    screen.mark_selected_row_range(1, 0, 3);
    let snapshot = copy_mode_snapshot(screen, CopyModeRenderOverlays::default());

    let frame = super::pane_screen::render_copy_mode_pane_screen(
        session.view(),
        &OptionStore::new(),
        pane,
        &snapshot,
    );

    assert_eq!(
        frame,
        b"\x1b[s\x1b[?25l\x1b[0m\
          \x1b[1;1H\x1b[0mAAAA\x1b[K\
          \x1b[2;1H\x1b[0m\x1b[30m\x1b[43mBBBB\x1b[39m\x1b[49mBBBB\x1b[K\
          \x1b[3;1H\x1b[0mCCCC\x1b[K\
          \x1b[4;1H\x1b[0m\x1b[K\
          \x1b[5;1H\x1b[0m\x1b[K\
          \x1b[0m\x1b[u\x1b[4;1H\x1b[?25h"
            .to_vec(),
        "a selection on one row must leave every other row's EL in place"
    );

    let outer = replay_frame(&frame, size, &b"Z".repeat(usize::from(size.cols) * 6));
    assert_eq!(row_text(&outer, 0, 40), format!("AAAA{}", " ".repeat(36)));
    assert_eq!(
        row_text(&outer, 1, 40),
        format!("BBBBBBBB{}", " ".repeat(32))
    );
    assert_eq!(
        row_text(&outer, pane_rows(size.rows) - 1, 40),
        " ".repeat(40)
    );

    let selected = row_cells(&outer, 1, 40);
    for cell in selected.iter().take(4) {
        assert_eq!(
            (cell.1, cell.2),
            (0, 3),
            "the selection highlight must survive the tail clear: {selected:?}"
        );
    }
    assert_eq!(
        (selected[4].1, selected[4].2),
        (8, 8),
        "the unselected remainder keeps default colours: {selected:?}"
    );
}

#[test]
fn gh217_copy_mode_selected_tail_keeps_its_style_while_the_rest_of_the_row_clears() {
    // tmux 3.7b, 40 columns, a selection running into a row's blank tail paints
    // the selected cells, paints the one cell whose background changes back,
    // and clears only what follows. The measured search capture shows that
    // shape directly: `CCC\e[45mC\e[39m\e[49m \e[K` — highlight, then the one
    // background-change cell, then the clear.
    //
    // tmux never treats a selected cell as empty: `mode-style` defaults to
    // `noattr,bg=yellow,fg=black`, `screen_select_cell` (`screen.c:613`) keeps
    // `GRID_ATTR_NOATTR` in the styled copy, and `tty_draw_line_get_empty`
    // (`tty-draw.c:111`) rejects any cell with a non-zero `attr`. RMUX mirrors
    // that internal marker. The unselected cell at column 10 changes back to
    // the default background and is painted literally; the row clears from
    // column 11.
    let size = TerminalSize { cols: 40, rows: 3 };
    let session = crate::session_fixture::SessionFixture::new(session_name("alpha"), size);
    let pane = session.window().pane(0).expect("pane exists");
    let mut screen = screen_with(b"BBBB", size);
    screen.mark_selected_row_range(0, 4, 9);
    let snapshot = copy_mode_snapshot(screen, CopyModeRenderOverlays::default());

    let frame = super::pane_screen::render_copy_mode_pane_screen(
        session.view(),
        &OptionStore::new(),
        pane,
        &snapshot,
    );

    assert!(
        contains(&frame, b"BBBB\x1b[30m\x1b[43m      \x1b[39m\x1b[49m \x1b[K"),
        "the six selected cells and the background-change cell are painted, \
         then the row is cleared: {frame:?}"
    );
    assert!(
        longest_literal_space_run(&frame) < 10,
        "no row may fall back to full-width literal padding: {frame:?}"
    );
    assert_eq!(
        erase_to_eol_count(&frame),
        pane_rows(size.rows),
        "both pane rows keep their own tmux EL: {frame:?}"
    );

    let outer = replay_frame(&frame, size, &b"Z".repeat(usize::from(size.cols) * 3));
    let cells = row_cells(&outer, 0, 40);
    assert_eq!(row_text(&outer, 0, 40), format!("BBBB{}", " ".repeat(36)));
    for cell in cells.iter().take(4) {
        assert_eq!(
            (cell.1, cell.2),
            (8, 8),
            "unselected text keeps its colours"
        );
    }
    for cell in cells.iter().take(10).skip(4) {
        assert_eq!(
            (cell.1, cell.2),
            (0, 3),
            "every selected trailing cell keeps the selection style: {cells:?}"
        );
    }
    for cell in cells.iter().skip(10) {
        assert_eq!(
            (cell.1, cell.2),
            (8, 8),
            "cells past the selection are cleared to the pane default: {cells:?}"
        );
    }
    assert_eq!(row_text(&outer, 1, 40), " ".repeat(40));
}

#[test]
fn gh217_copy_mode_search_overlay_keeps_tail_clears_on_unrelated_rows() {
    // tmux 3.7b, 40 columns, `search-backward C` with the search mark live:
    //   \e[2;1HBBBBBBBB\e[K\e[30m\e[46m\r\nCCC\e[45mC\e[39m\e[49m \e[K
    //   \r\n\e[K\r\n\e[K
    // The match row paints through the highlight plus the single cell whose
    // background changes back, then clears; no other row is affected. That
    // single cell is the `gcp->bg != last.bg` guard in `tty_draw_line`
    // (`tty-draw.c:259`), which rmux mirrors as `cell.bg() == last_bg` in
    // `visible_line_tmux_empty_tail_start`.
    let size = TerminalSize { cols: 40, rows: 6 };
    let session = crate::session_fixture::SessionFixture::new(session_name("alpha"), size);
    let pane = session.window().pane(0).expect("pane exists");
    let match_range = CopyModeOverlayRange {
        row: 2,
        start_x: 3,
        end_x: 3,
    };
    let snapshot = copy_mode_snapshot(
        oracle_screen(size),
        CopyModeRenderOverlays {
            mark: None,
            matches: vec![match_range],
            current_match: Some(match_range),
        },
    );

    let frame = super::pane_screen::render_copy_mode_pane_screen(
        session.view(),
        &OptionStore::new(),
        pane,
        &snapshot,
    );

    assert!(
        contains(&frame, b"CCC\x1b[30m\x1b[45mC\x1b[39m\x1b[49m \x1b[K"),
        "the match row clears right after the background-change cell: {frame:?}"
    );
    assert!(
        longest_literal_space_run(&frame) < 10,
        "a search overlay must not pad unrelated rows with literal spaces: {frame:?}"
    );
    assert_eq!(
        erase_to_eol_count(&frame),
        pane_rows(size.rows),
        "each pane row keeps its own tmux EL: {frame:?}"
    );

    let outer = replay_frame(&frame, size, &b"Z".repeat(usize::from(size.cols) * 6));
    assert_eq!(row_text(&outer, 0, 40), format!("AAAA{}", " ".repeat(36)));
    assert_eq!(row_text(&outer, 2, 40), format!("CCCC{}", " ".repeat(36)));
    assert_eq!(
        row_text(&outer, pane_rows(size.rows) - 1, 40),
        " ".repeat(40)
    );

    let cells = row_cells(&outer, 2, 40);
    assert_eq!(
        (cells[3].1, cells[3].2),
        (0, 5),
        "the current match keeps its highlight: {cells:?}"
    );
    assert_eq!(
        (cells[4].1, cells[4].2),
        (8, 8),
        "the cell after the match returns to the pane default: {cells:?}"
    );
}

#[test]
fn gh217_copy_mode_selection_tail_clears_stay_inside_a_split_pane() {
    // tmux 3.7b, 41 columns split in two, selection on the left pane's row 1:
    //   \e[HAAAA\e[11X...\r\nBBBB\e(B\e[mBBBB\e[12X\r\nCCCC\e[16X
    //   \e[4;20H\e[1K\n\e[1K
    // The left pane uses bounded ECH and EL1, never EL, so the border and the
    // right pane survive a redraw taken while the selection is live. Only the
    // right pane, which does reach the terminal edge, is drawn with EL
    // (`\e[1;22H\e[K\n\e[K...` later in the same capture).
    let size = TerminalSize { cols: 41, rows: 6 };
    let mut session = crate::session_fixture::SessionFixture::new(session_name("alpha"), size);
    session.split_active_pane().expect("split succeeds");
    let pane = session.window().pane(0).expect("left pane exists");
    assert_eq!((pane.geometry().x(), pane.geometry().cols()), (0, 20));
    let mut screen = oracle_screen(size);
    screen.mark_selected_row_range(1, 0, 3);
    let snapshot = copy_mode_snapshot(screen, CopyModeRenderOverlays::default());

    let frame = super::pane_screen::render_copy_mode_pane_screen(
        session.view(),
        &OptionStore::new(),
        pane,
        &snapshot,
    );

    assert_eq!(
        erase_to_eol_count(&frame),
        0,
        "a left split must never clear through its neighbour: {frame:?}"
    );
    assert!(
        contains(&frame, b"BBBB\x1b[39m\x1b[49mBBBB\x1b[12X"),
        "the selected row keeps the bounded twelve-column ECH: {frame:?}"
    );
    assert!(
        contains(&frame, b"\x1b[4;20H\x1b[1K"),
        "an empty left split row keeps the bounded EL1: {frame:?}"
    );
    assert!(
        longest_literal_space_run(&frame) < 10,
        "a selection must not pad the split pane with literal spaces: {frame:?}"
    );

    let filler = format!("{}|{}", "L".repeat(20), "R".repeat(20)).repeat(6);
    let outer = replay_frame(&frame, size, filler.as_bytes());
    for row in 0..pane_rows(size.rows) {
        assert_eq!(
            &row_text(&outer, row, 41)[20..],
            &format!("|{}", "R".repeat(20)),
            "row {row} must leave the border and the neighbour intact"
        );
    }
    assert_eq!(&row_text(&outer, 1, 41)[..20], "BBBBBBBB            ");
}

#[test]
fn gh217_delta_snapshot_selection_keeps_tail_clears_on_unrelated_rows() {
    // The incremental snapshot path used the same screen-wide carve-out and
    // truncated every row to the full pane width. It must reach the same
    // per-row verdict as the full redraw above.
    let size = TerminalSize { cols: 40, rows: 6 };
    let session = crate::session_fixture::SessionFixture::new(session_name("alpha"), size);
    let pane = session.window().pane(0).expect("pane exists");
    let mut screen = oracle_screen(size);
    screen.mark_selected_row_range(1, 0, 3);

    let snapshot =
        super::PaneRenderSnapshot::capture(session.view(), &OptionStore::new(), pane, &screen)
            .expect("pane snapshot captures");
    let frame = snapshot.full_frame();

    assert!(
        longest_literal_space_run(&frame) < 10,
        "the delta snapshot must not pad rows with literal spaces: {frame:?}"
    );
    assert_eq!(
        erase_to_eol_count(&frame),
        pane_rows(size.rows),
        "each pane row keeps its own tmux EL: {frame:?}"
    );
    assert!(
        contains(&frame, b"\x1b[30m\x1b[43mBBBB\x1b[39m\x1b[49mBBBB\x1b[K"),
        "the delta snapshot keeps the highlight and clears the row tail: {frame:?}"
    );

    let outer = replay_frame(&frame, size, &b"Z".repeat(usize::from(size.cols) * 6));
    assert_eq!(row_text(&outer, 0, 40), format!("AAAA{}", " ".repeat(36)));
    assert_eq!(
        row_text(&outer, pane_rows(size.rows) - 1, 40),
        " ".repeat(40)
    );
    let selected = row_cells(&outer, 1, 40);
    for cell in selected.iter().take(4) {
        assert_eq!(
            (cell.1, cell.2),
            (0, 3),
            "the delta path keeps the selection highlight: {selected:?}"
        );
    }
}
