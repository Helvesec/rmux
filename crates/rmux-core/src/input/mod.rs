//! tmux-compatible VT parser state machine.
//!
//! It implements DEC-style terminal parsing for tmux-compatible streams.
//! This module provides the parser, state tables, command enums, parameter
//! splitting, and SGR logic as pure safe Rust. Screen-write effects are
//! delegated through the [`crate::input::ScreenWriter`] trait.

mod cell;
mod colour;
mod commands;
mod csi_helpers;
mod dispatch;
mod params;
mod passthrough;
mod sgr;
mod states;
mod tables;
#[cfg(test)]
mod tests;
mod writer;

pub use cell::{CellState, GridAttr, SavedState};
pub use colour::{
    colour_join_rgb, Colour, COLOUR_DEFAULT, COLOUR_FLAG_256, COLOUR_FLAG_RGB, COLOUR_NONE,
    COLOUR_TERMINAL,
};
pub use dispatch::{CsiCommand, DcsPayload, EscCommand, InputAction, OscCommand, ScreenWriter};
pub use params::{InputParam, ParamType};
pub use states::InputState;

use params::ParamList;
use states::Transition;

use crate::terminal_passthrough::MAX_TERMINAL_PASSTHROUGH_PAYLOAD_BYTES;

/// Maximum number of parameters in a CSI/DCS sequence.
const PARAM_LIST_MAX: usize = 24;

/// Intermediate buffer capacity.
const INTERM_BUF_MAX: usize = 4;

/// Initial input buffer size.
const INPUT_BUF_START: usize = 32;

/// Maximum input buffer size (1 MiB, matching `INPUT_BUF_DEFAULT_SIZE`).
const INPUT_BUF_MAX: usize = 1_048_576;

/// Parameter buffer capacity for raw parameter bytes.
const PARAM_BUF_MAX: usize = 64;

/// Parser flags.
const INPUT_DISCARD: u32 = 0x1;
/// Last printable character was emitted (for REP).
const INPUT_LAST: u32 = 0x2;

/// Mode flag bits matching tmux `tmux.h:660–680`.
pub mod mode {
    /// Cursor visible.
    pub const MODE_CURSOR: u32 = 0x1;
    /// Insert mode.
    pub const MODE_INSERT: u32 = 0x2;
    /// Application cursor keys.
    pub const MODE_KCURSOR: u32 = 0x4;
    /// Application keypad.
    pub const MODE_KKEYPAD: u32 = 0x8;
    /// Auto wrap.
    pub const MODE_WRAP: u32 = 0x10;
    /// Standard mouse reporting (1000).
    pub const MODE_MOUSE_STANDARD: u32 = 0x20;
    /// Button-event mouse tracking (1002).
    pub const MODE_MOUSE_BUTTON: u32 = 0x40;
    /// Cursor blinking.
    pub const MODE_CURSOR_BLINKING: u32 = 0x80;
    /// Mouse UTF-8 mode (1005).
    pub const MODE_MOUSE_UTF8: u32 = 0x100;
    /// SGR mouse mode (1006).
    pub const MODE_MOUSE_SGR: u32 = 0x200;
    /// Bracketed paste.
    pub const MODE_BRACKETPASTE: u32 = 0x400;
    /// Focus in/out events.
    pub const MODE_FOCUSON: u32 = 0x800;
    /// All mouse tracking (1003).
    pub const MODE_MOUSE_ALL: u32 = 0x1000;
    /// Origin mode.
    pub const MODE_ORIGIN: u32 = 0x2000;
    /// CR+LF mode.
    pub const MODE_CRLF: u32 = 0x4000;
    /// Extended keys mode.
    pub const MODE_KEYS_EXTENDED: u32 = 0x8000;
    /// Cursor very visible (blinking block, from DECTCEM handling).
    pub const MODE_CURSOR_VERY_VISIBLE: u32 = 0x1_0000;
    /// Cursor blinking explicitly set.
    pub const MODE_CURSOR_BLINKING_SET: u32 = 0x2_0000;
    /// Extended keys mode 2.
    pub const MODE_KEYS_EXTENDED_2: u32 = 0x4_0000;
    /// Theme updates from application.
    pub const MODE_THEME_UPDATES: u32 = 0x8_0000;
    /// Synchronized output.
    pub const MODE_SYNC: u32 = 0x10_0000;

    /// All mouse modes combined.
    pub const ALL_MOUSE_MODES: u32 = MODE_MOUSE_STANDARD | MODE_MOUSE_BUTTON | MODE_MOUSE_ALL;
    /// Extended key modes combined.
    pub const EXTENDED_KEY_MODES: u32 = MODE_KEYS_EXTENDED | MODE_KEYS_EXTENDED_2;
}

/// Type of string terminator seen for OSC/DCS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputEndType {
    /// ESC \\ (ST)
    St,
    /// BEL (0x07)
    Bel,
}

/// Per-pane VT input parser, matching tmux `input_ctx`.
pub struct InputParser {
    /// Current parser state.
    state: InputState,
    /// Parser flags (INPUT_DISCARD, INPUT_LAST).
    flags: u32,

    /// Current character being processed.
    ch: u8,

    /// Intermediate character buffer.
    interm_buf: [u8; INTERM_BUF_MAX],
    interm_len: usize,

    /// Raw parameter buffer.
    param_buf: [u8; PARAM_BUF_MAX],
    param_len: usize,

    /// Dynamic input/string buffer.
    input_buf: Vec<u8>,
    /// Which terminator ended the string.
    input_end: InputEndType,

    /// Parsed parameter list.
    param_list: ParamList,

    /// Cell state (current attributes, character set, etc.).
    cell: CellState,
    /// Saved cell state for DECSC/DECRC.
    saved: SavedState,

    /// UTF-8 accumulator.
    utf8_buf: [u8; 4],
    utf8_len: u8,
    utf8_expected: u8,
    utf8_started: bool,

    /// Last printed character data for REP.
    last_char: Option<char>,

    /// Bytes accumulated since last ground state, for control-mode catch-up.
    since_ground: Vec<u8>,

    /// Whether ground timer would be active (modeled as flag; actual timer
    /// is a server-side concern).
    ground_timer_active: bool,

    /// Reply buffer: replies to be sent back to the PTY.
    reply_buf: Vec<u8>,
    /// Dropped terminal passthrough events caused by parser string limits.
    terminal_passthrough_dropped_count: u64,
}

impl InputParser {
    /// Creates a new parser in the ground state with default cell attributes.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: InputState::Ground,
            flags: 0,
            ch: 0,
            interm_buf: [0; INTERM_BUF_MAX],
            interm_len: 0,
            param_buf: [0; PARAM_BUF_MAX],
            param_len: 0,
            input_buf: Vec::with_capacity(INPUT_BUF_START),
            input_end: InputEndType::St,
            param_list: ParamList::new(),
            cell: CellState::default(),
            saved: SavedState::default(),
            utf8_buf: [0; 4],
            utf8_len: 0,
            utf8_expected: 0,
            utf8_started: false,
            last_char: None,
            since_ground: Vec::new(),
            ground_timer_active: false,
            reply_buf: Vec::new(),
            terminal_passthrough_dropped_count: 0,
        }
    }

    /// Returns the current parser state.
    #[must_use]
    pub fn state(&self) -> InputState {
        self.state
    }

    /// Returns and drains accumulated reply bytes.
    pub fn take_replies(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.reply_buf)
    }

    /// Returns and drains terminal passthrough drops caused by parser limits.
    pub(crate) fn take_terminal_passthrough_dropped_count(&mut self) -> u64 {
        let dropped = self.terminal_passthrough_dropped_count;
        self.terminal_passthrough_dropped_count = 0;
        dropped
    }

    /// Returns and drains accumulated since-ground bytes.
    pub fn take_since_ground(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.since_ground)
    }

    /// Returns any bytes still buffered in an incomplete parser state.
    #[must_use]
    pub fn pending_bytes(&self) -> Vec<u8> {
        if self.state != InputState::Ground {
            return self.since_ground.clone();
        }
        if self.utf8_started {
            return self.utf8_buf[..usize::from(self.utf8_len)].to_vec();
        }
        Vec::new()
    }

    /// Returns true if the ground timer should be running.
    #[must_use]
    pub fn ground_timer_active(&self) -> bool {
        self.ground_timer_active
    }

    /// Called by the server when the ground timer expires (5s timeout).
    pub fn ground_timer_expired(&mut self) {
        self.reset_to_ground();
    }

    /// Resets the parser to ground state.
    pub fn reset_to_ground(&mut self) {
        self.clear();
        self.state = InputState::Ground;
        self.flags = 0;
        self.enter_ground();
    }

    /// Returns a reference to the current cell state.
    #[must_use]
    pub fn cell_state(&self) -> &CellState {
        &self.cell
    }

    /// Parse a buffer of bytes, dispatching actions to the screen writer.
    pub fn parse(&mut self, buf: &[u8], writer: &mut dyn ScreenWriter) {
        for &byte in buf {
            self.ch = byte;
            let transition = self.find_transition();
            self.execute_transition(transition, writer);
        }
    }

    fn find_transition(&self) -> Transition {
        let table = self.state.transition_table();
        for entry in table {
            if self.ch >= entry.first && self.ch <= entry.last {
                return Transition {
                    handler: entry.handler,
                    next_state: entry.next_state,
                };
            }
        }
        // Should never happen with complete tables, but be safe.
        Transition {
            handler: states::Handler::None,
            next_state: None,
        }
    }

    fn execute_transition(&mut self, trans: Transition, writer: &mut dyn ScreenWriter) {
        // Any state except print stops collect_end equivalent.
        if !matches!(
            trans.handler,
            states::Handler::Print | states::Handler::TopBitSet
        ) {
            writer.collect_end();
        }

        // Execute handler; if it returns true, skip state transition.
        let skip_state = match trans.handler {
            states::Handler::None => false,
            states::Handler::Print => self.handle_print(writer),
            states::Handler::C0Dispatch => self.handle_c0_dispatch(writer),
            states::Handler::EscDispatch => self.handle_esc_dispatch(writer),
            states::Handler::CsiDispatch => self.handle_csi_dispatch(writer),
            states::Handler::DcsDispatch => self.handle_dcs_dispatch(writer),
            states::Handler::Intermediate => self.handle_intermediate(),
            states::Handler::Parameter => self.handle_parameter(),
            states::Handler::Input => self.handle_input(),
            states::Handler::TopBitSet => self.handle_top_bit_set(writer),
            states::Handler::EndBel => self.handle_end_bel(),
        };

        if skip_state {
            return;
        }

        if let Some(next) = trans.next_state {
            self.set_state(next, writer);
        }

        // If not in ground state, save byte to since_ground.
        if self.state != InputState::Ground {
            self.since_ground.push(self.ch);
        }
    }

    fn set_state(&mut self, next: InputState, writer: &mut dyn ScreenWriter) {
        // Call exit handler for current state.
        self.exit_state(writer);
        self.state = next;
        // Call enter handler for new state.
        self.enter_state(writer);
    }

    fn enter_state(&mut self, writer: &mut dyn ScreenWriter) {
        match self.state {
            InputState::Ground => self.enter_ground(),
            InputState::EscEnter => self.clear(),
            InputState::CsiEnter => self.clear(),
            InputState::DcsEnter => self.enter_dcs(),
            InputState::OscString => self.enter_osc(),
            InputState::ApcString => self.enter_apc(),
            InputState::RenameString => self.enter_rename(),
            InputState::ConsumeSt => self.enter_rename(), // same as rename in tmux
            _ => {}
        }
        let _ = writer; // writer not needed for enter handlers currently
    }

    fn exit_state(&mut self, writer: &mut dyn ScreenWriter) {
        match self.state {
            InputState::OscString => self.exit_osc(writer),
            InputState::ApcString => self.exit_apc(writer),
            InputState::RenameString => self.exit_rename(writer),
            _ => {}
        }
    }

    fn clear(&mut self) {
        self.ground_timer_active = false;
        self.interm_buf = [0; INTERM_BUF_MAX];
        self.interm_len = 0;
        self.param_buf = [0; PARAM_BUF_MAX];
        self.param_len = 0;
        self.input_buf.clear();
        self.input_end = InputEndType::St;
        self.flags &= !INPUT_DISCARD;
    }

    fn enter_ground(&mut self) {
        self.ground_timer_active = false;
        self.since_ground.clear();
        // Shrink input buffer back to start size.
        if self.input_buf.capacity() > INPUT_BUF_START {
            self.input_buf = Vec::with_capacity(INPUT_BUF_START);
        }
    }

    fn enter_dcs(&mut self) {
        self.clear();
        self.ground_timer_active = true;
        self.flags &= !INPUT_LAST;
    }

    fn enter_osc(&mut self) {
        self.clear();
        self.ground_timer_active = true;
        self.flags &= !INPUT_LAST;
    }

    fn enter_apc(&mut self) {
        self.clear();
        self.ground_timer_active = true;
        self.flags &= !INPUT_LAST;
    }

    fn enter_rename(&mut self) {
        self.clear();
        self.ground_timer_active = true;
        self.flags &= !INPUT_LAST;
    }

    fn exit_osc(&mut self, writer: &mut dyn ScreenWriter) {
        if self.flags & INPUT_DISCARD != 0 {
            return;
        }
        dispatch::dispatch_osc(self, writer);
    }

    fn exit_apc(&mut self, writer: &mut dyn ScreenWriter) {
        if self.flags & INPUT_DISCARD != 0 {
            return;
        }
        if passthrough::is_kitty_graphics_apc(&self.input_buf) {
            writer.apc_passthrough(&self.input_buf);
            return;
        }
        let buf = String::from_utf8_lossy(&self.input_buf).into_owned();
        writer.set_title(&buf);
    }

    fn exit_rename(&mut self, writer: &mut dyn ScreenWriter) {
        if self.flags & INPUT_DISCARD != 0 {
            return;
        }
        let buf = String::from_utf8_lossy(&self.input_buf).into_owned();
        writer.set_window_name(&buf);
    }

    /// Stop any in-progress UTF-8 sequence and emit U+FFFD.
    fn stop_utf8(&mut self, writer: &mut dyn ScreenWriter) {
        if self.utf8_started {
            writer.collect_add('\u{FFFD}', &self.cell);
            self.utf8_started = false;
            self.utf8_len = 0;
            self.utf8_expected = 0;
        }
    }

    fn handle_print(&mut self, writer: &mut dyn ScreenWriter) -> bool {
        self.stop_utf8(writer);

        let ch = self.ch as char;
        let set = if self.cell.set == 0 {
            self.cell.g0set
        } else {
            self.cell.g1set
        };

        writer.collect_add_with_charset(ch, &self.cell, set != 0);

        self.last_char = Some(ch);
        self.flags |= INPUT_LAST;

        false
    }

    fn handle_intermediate(&mut self) -> bool {
        if self.interm_len >= INTERM_BUF_MAX - 1 {
            self.flags |= INPUT_DISCARD;
        } else {
            self.interm_buf[self.interm_len] = self.ch;
            self.interm_len += 1;
        }
        false
    }

    fn handle_parameter(&mut self) -> bool {
        if self.param_len >= PARAM_BUF_MAX - 1 {
            self.flags |= INPUT_DISCARD;
        } else {
            self.param_buf[self.param_len] = self.ch;
            self.param_len += 1;
        }
        false
    }

    fn handle_input(&mut self) -> bool {
        let input_limit = self.input_buffer_limit();
        if self.input_buf.len() + 1 >= input_limit {
            if self.flags & INPUT_DISCARD == 0 && self.is_terminal_passthrough_string() {
                self.terminal_passthrough_dropped_count =
                    self.terminal_passthrough_dropped_count.saturating_add(1);
            }
            self.flags |= INPUT_DISCARD;
        } else {
            self.input_buf.push(self.ch);
        }
        false
    }

    fn input_buffer_limit(&self) -> usize {
        if self.is_terminal_passthrough_string() {
            return MAX_TERMINAL_PASSTHROUGH_PAYLOAD_BYTES;
        }
        INPUT_BUF_MAX
    }

    fn is_terminal_passthrough_string(&self) -> bool {
        (self.state == InputState::ApcString && passthrough::is_kitty_graphics_apc(&self.input_buf))
            || (self.state == InputState::DcsHandler
                && self.interm_len == 0
                && self.input_buf.first() == Some(&b'q'))
    }

    fn handle_end_bel(&mut self) -> bool {
        self.input_end = InputEndType::Bel;
        false
    }

    fn handle_c0_dispatch(&mut self, writer: &mut dyn ScreenWriter) -> bool {
        self.stop_utf8(writer);
        dispatch::dispatch_c0(self, writer);
        self.flags &= !INPUT_LAST;
        false
    }

    fn handle_esc_dispatch(&mut self, writer: &mut dyn ScreenWriter) -> bool {
        if self.flags & INPUT_DISCARD != 0 {
            return false;
        }
        dispatch::dispatch_esc(self, writer);
        self.flags &= !INPUT_LAST;
        false
    }

    fn handle_csi_dispatch(&mut self, writer: &mut dyn ScreenWriter) -> bool {
        if self.flags & INPUT_DISCARD != 0 {
            return false;
        }
        dispatch::dispatch_csi(self, writer);
        self.flags &= !INPUT_LAST;
        false
    }

    fn handle_dcs_dispatch(&mut self, writer: &mut dyn ScreenWriter) -> bool {
        if self.flags & INPUT_DISCARD != 0 {
            return false;
        }
        dispatch::dispatch_dcs(self, writer);
        false
    }

    fn handle_top_bit_set(&mut self, writer: &mut dyn ScreenWriter) -> bool {
        self.flags &= !INPUT_LAST;

        if !self.utf8_started {
            self.utf8_started = true;
            self.utf8_len = 0;
            // Determine expected byte count from first byte.
            let expected = if self.ch & 0xE0 == 0xC0 {
                2
            } else if self.ch & 0xF0 == 0xE0 {
                3
            } else if self.ch & 0xF8 == 0xF0 {
                4
            } else {
                // Invalid start byte.
                self.stop_utf8(writer);
                return false;
            };
            self.utf8_expected = expected;
            self.utf8_buf[0] = self.ch;
            self.utf8_len = 1;
            return false;
        }

        // Continuation byte.
        if self.ch & 0xC0 != 0x80 {
            // Not a valid continuation: emit replacement and re-process.
            self.stop_utf8(writer);
            // Re-start UTF-8 with current byte if it's a start byte.
            if self.ch >= 0x80 {
                return self.handle_top_bit_set(writer);
            }
            return false;
        }

        self.utf8_buf[self.utf8_len as usize] = self.ch;
        self.utf8_len += 1;

        if self.utf8_len < self.utf8_expected {
            return false; // More bytes expected.
        }

        // Complete: decode.
        self.utf8_started = false;
        let bytes = &self.utf8_buf[..self.utf8_len as usize];
        let s = match std::str::from_utf8(bytes) {
            Ok(s) => s,
            Err(_) => {
                writer.collect_add('\u{FFFD}', &self.cell);
                return false;
            }
        };
        let c = match s.chars().next() {
            Some(c) => c,
            None => {
                writer.collect_add('\u{FFFD}', &self.cell);
                return false;
            }
        };

        writer.collect_add(c, &self.cell);

        self.last_char = Some(c);
        self.flags |= INPUT_LAST;

        false
    }

    /// Append a reply string to the reply buffer.
    fn reply(&mut self, s: &str) {
        self.reply_buf.extend_from_slice(s.as_bytes());
    }

    /// Interm buf as a string slice for table lookups.
    fn interm_str(&self) -> &[u8] {
        &self.interm_buf[..self.interm_len]
    }
}

impl Default for InputParser {
    fn default() -> Self {
        Self::new()
    }
}
