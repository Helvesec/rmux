//! Comprehensive parser tests.

use super::cell::GridAttr;
use super::colour::{COLOUR_DEFAULT, COLOUR_FLAG_256, COLOUR_FLAG_RGB};
use super::dispatch::ScreenWriter;
use super::mode::*;
use super::*;

/// A recording screen writer that logs all method calls for verification.
#[derive(Debug, Default)]
struct RecordingWriter {
    calls: Vec<String>,
    mode: u32,
    cx: u32,
    cy: u32,
    sx: u32,
    sy: u32,
    chars: Vec<char>,
}

impl RecordingWriter {
    fn new(sx: u32, sy: u32) -> Self {
        Self {
            sx,
            sy,
            mode: MODE_CURSOR | MODE_WRAP, // tmux defaults
            ..Default::default()
        }
    }

    fn has_call(&self, prefix: &str) -> bool {
        self.calls.iter().any(|c| c.starts_with(prefix))
    }
}

impl ScreenWriter for RecordingWriter {
    fn collect_add(&mut self, ch: char, _cell: &CellState) {
        self.chars.push(ch);
        self.calls.push(format!("collect_add({ch:?})"));
    }

    fn collect_add_with_charset(&mut self, ch: char, _cell: &CellState, acs: bool) {
        self.chars.push(ch);
        self.calls
            .push(format!("collect_add_with_charset({ch:?}, acs={acs})"));
    }

    fn collect_end(&mut self) {
        self.calls.push("collect_end()".to_owned());
    }

    fn cursor_up(&mut self, n: u32) {
        self.calls.push(format!("cursor_up({n})"));
    }
    fn cursor_down(&mut self, n: u32) {
        self.calls.push(format!("cursor_down({n})"));
    }
    fn cursor_left(&mut self, n: u32) {
        self.calls.push(format!("cursor_left({n})"));
    }
    fn cursor_right(&mut self, n: u32) {
        self.calls.push(format!("cursor_right({n})"));
    }
    fn cursor_move(&mut self, col: i32, row: i32, origin: bool) {
        if col >= 0 {
            self.cx = col as u32;
        }
        if row >= 0 {
            self.cy = row as u32;
        }
        self.calls
            .push(format!("cursor_move({col}, {row}, {origin})"));
    }
    fn insert_line(&mut self, n: u32, bg: i32) {
        self.calls.push(format!("insert_line({n}, {bg})"));
    }
    fn delete_line(&mut self, n: u32, bg: i32) {
        self.calls.push(format!("delete_line({n}, {bg})"));
    }
    fn scroll_up(&mut self, n: u32, bg: i32) {
        self.calls.push(format!("scroll_up({n}, {bg})"));
    }
    fn scroll_down(&mut self, n: u32, bg: i32) {
        self.calls.push(format!("scroll_down({n}, {bg})"));
    }
    fn linefeed(&mut self, wrapped: bool, bg: i32) {
        self.calls.push(format!("linefeed({wrapped}, {bg})"));
    }
    fn reverse_index(&mut self, bg: i32) {
        self.calls.push(format!("reverse_index({bg})"));
    }
    fn carriage_return(&mut self) {
        self.calls.push("carriage_return()".to_owned());
    }
    fn backspace(&mut self) {
        self.calls.push("backspace()".to_owned());
    }
    fn insert_character(&mut self, n: u32, bg: i32) {
        self.calls.push(format!("insert_character({n}, {bg})"));
    }
    fn delete_character(&mut self, n: u32, bg: i32) {
        self.calls.push(format!("delete_character({n}, {bg})"));
    }
    fn clear_character(&mut self, n: u32, bg: i32) {
        self.calls.push(format!("clear_character({n}, {bg})"));
    }
    fn clear_end_of_screen(&mut self, bg: i32) {
        self.calls.push(format!("clear_end_of_screen({bg})"));
    }
    fn clear_start_of_screen(&mut self, bg: i32) {
        self.calls.push(format!("clear_start_of_screen({bg})"));
    }
    fn clear_screen(&mut self, bg: i32) {
        self.calls.push(format!("clear_screen({bg})"));
    }
    fn clear_history(&mut self) {
        self.calls.push("clear_history()".to_owned());
    }
    fn clear_end_of_line(&mut self, bg: i32) {
        self.calls.push(format!("clear_end_of_line({bg})"));
    }
    fn clear_start_of_line(&mut self, bg: i32) {
        self.calls.push(format!("clear_start_of_line({bg})"));
    }
    fn clear_line(&mut self, bg: i32) {
        self.calls.push(format!("clear_line({bg})"));
    }
    fn mode_set(&mut self, mode: u32) {
        self.mode |= mode;
        self.calls.push(format!("mode_set({mode:#x})"));
    }
    fn mode_clear(&mut self, mode: u32) {
        self.mode &= !mode;
        self.calls.push(format!("mode_clear({mode:#x})"));
    }
    fn set_scroll_region(&mut self, top: u32, bottom: u32) {
        self.calls
            .push(format!("set_scroll_region({top}, {bottom})"));
    }
    fn alternate_on(&mut self, bg: i32, save: bool) {
        self.calls.push(format!("alternate_on({bg}, {save})"));
    }
    fn alternate_off(&mut self, bg: i32, restore: bool) {
        self.calls.push(format!("alternate_off({bg}, {restore})"));
    }
    fn set_tab_stop(&mut self) {
        self.calls.push("set_tab_stop()".to_owned());
    }
    fn clear_tab_stop(&mut self) {
        self.calls.push("clear_tab_stop()".to_owned());
    }
    fn clear_all_tab_stops(&mut self) {
        self.calls.push("clear_all_tab_stops()".to_owned());
    }
    fn set_title(&mut self, title: &str) {
        self.calls.push(format!("set_title({title:?})"));
    }
    fn set_window_name(&mut self, name: &str) {
        self.calls.push(format!("set_window_name({name:?})"));
    }
    fn set_path(&mut self, path: &str) {
        self.calls.push(format!("set_path({path:?})"));
    }
    fn alignment_test(&mut self) {
        self.calls.push("alignment_test()".to_owned());
    }
    fn full_reset(&mut self) {
        self.calls.push("full_reset()".to_owned());
    }
    fn set_cursor_style(&mut self, n: u32) {
        self.calls.push(format!("set_cursor_style({n})"));
    }
    fn bell(&mut self) {
        self.calls.push("bell()".to_owned());
    }
    fn tab(&mut self) {
        self.calls.push("tab()".to_owned());
    }
    fn cursor_backward_tab(&mut self, n: u32) {
        self.calls.push(format!("cursor_backward_tab({n})"));
    }
    fn start_sync(&mut self) {
        self.calls.push("start_sync()".to_owned());
    }
    fn stop_sync(&mut self) {
        self.calls.push("stop_sync()".to_owned());
    }
    fn push_title(&mut self) {
        self.calls.push("push_title()".to_owned());
    }
    fn pop_title(&mut self) {
        self.calls.push("pop_title()".to_owned());
    }
    fn notify_pane_title_changed(&mut self) {
        self.calls.push("notify_pane_title_changed()".to_owned());
    }
    fn osc_palette(&mut self, data: &str, end: InputEndType) {
        self.calls.push(format!("osc_palette({data:?}, {end:?})"));
    }
    fn osc_hyperlink(&mut self, data: &str) {
        self.calls.push(format!("osc_hyperlink({data:?})"));
    }
    fn osc_fg_colour(&mut self, data: &str, end: InputEndType) {
        self.calls.push(format!("osc_fg_colour({data:?}, {end:?})"));
    }
    fn osc_bg_colour(&mut self, data: &str, end: InputEndType) {
        self.calls.push(format!("osc_bg_colour({data:?}, {end:?})"));
    }
    fn osc_cursor_colour(&mut self, data: &str, end: InputEndType) {
        self.calls
            .push(format!("osc_cursor_colour({data:?}, {end:?})"));
    }
    fn osc_clipboard(&mut self, data: &str, end: InputEndType) {
        self.calls.push(format!("osc_clipboard({data:?}, {end:?})"));
    }
    fn osc_shell_integration(&mut self, data: &str) {
        self.calls.push(format!("osc_shell_integration({data:?})"));
    }
    fn dcs_passthrough(&mut self, data: &[u8]) {
        self.calls.push(format!(
            "dcs_passthrough({:?})",
            String::from_utf8_lossy(data)
        ));
    }
    fn sixel_passthrough(&mut self, data: &[u8]) {
        self.calls.push(format!(
            "sixel_passthrough({:?})",
            String::from_utf8_lossy(data)
        ));
    }
    fn apc_passthrough(&mut self, data: &[u8]) {
        self.calls.push(format!(
            "apc_passthrough({:?})",
            String::from_utf8_lossy(data)
        ));
    }
    fn screen_size_x(&self) -> u32 {
        self.sx
    }
    fn screen_size_y(&self) -> u32 {
        self.sy
    }
    fn cursor_x(&self) -> u32 {
        self.cx
    }
    fn cursor_y(&self) -> u32 {
        self.cy
    }
    fn current_mode(&self) -> u32 {
        self.mode
    }
}

fn parse(input: &[u8]) -> (InputParser, RecordingWriter) {
    let mut parser = InputParser::new();
    let mut writer = RecordingWriter::new(80, 24);
    parser.parse(input, &mut writer);
    (parser, writer)
}

// ─── State machine tests ───────────────────────────────────────────

#[test]
fn transition_tables_cover_every_state_byte_once() {
    for state in InputState::ALL {
        let table = state.transition_table();
        for byte in u8::MIN..=u8::MAX {
            let matches = table
                .iter()
                .filter(|entry| byte >= entry.first && byte <= entry.last)
                .count();
            assert_eq!(
                matches, 1,
                "state {state:?} must have exactly one transition for byte {byte:#04x}"
            );
        }
    }
}

#[test]
fn transition_lut_matches_linear_tables_for_every_state_byte() {
    for state in InputState::ALL {
        let table = state.transition_table();
        for byte in u8::MIN..=u8::MAX {
            let linear = table
                .iter()
                .find(|entry| byte >= entry.first && byte <= entry.last)
                .map(|entry| super::states::Transition {
                    handler: entry.handler,
                    next_state: entry.next_state,
                })
                .expect("coverage test guarantees every byte has a transition");
            assert_eq!(
                state.transition_for_byte(byte),
                linear,
                "state {state:?} LUT differs for byte {byte:#04x}"
            );
        }
    }
}

#[path = "tests/state_control.rs"]
mod state_control;

#[path = "tests/csi_modes.rs"]
mod csi_modes;

#[path = "tests/sgr_utf8.rs"]
mod sgr_utf8;

#[path = "tests/osc_dcs_misc.rs"]
mod osc_dcs_misc;

#[path = "tests/extended_timers_utf8.rs"]
mod extended_timers_utf8;

#[path = "tests/winops_and_resets.rs"]
mod winops_and_resets;
