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
mod extended_keys;
mod graphics_query;
mod keyboard;
pub mod mode;
mod mode_query;
mod params;
mod passthrough;
mod program_status_osc;
mod recovery;
mod rgb;
mod sgr;
mod shell_marks;
mod state_machine;
mod states;
mod tables;
mod theme;
pub use extended_keys::ExtendedKeyPolicy;

pub use theme::{ThemeAction, ThemeEvent};
#[cfg(test)]
mod tests;
mod writer;
pub use rgb::colour_to_rgb;

pub use cell::{CellState, GridAttr, SavedState};
pub use colour::{
    colour_join_rgb, Colour, COLOUR_DEFAULT, COLOUR_FLAG_256, COLOUR_FLAG_RGB, COLOUR_NONE,
    COLOUR_TERMINAL,
};
pub use dispatch::{CsiCommand, DcsPayload, EscCommand, InputAction, OscCommand, ScreenWriter};
pub use params::{InputParam, ParamType};
pub(crate) use shell_marks::ShellMark;
pub use states::InputState;

use params::ParamList;

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

/// Type of string terminator seen for OSC/DCS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputEndType {
    /// ESC \\ (ST)
    St,
    /// BEL (0x07)
    Bel,
}

/// Colour slots reported to OSC 10/11/12 queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OscColourSlot {
    /// OSC 10 — default foreground.
    Foreground,
    /// OSC 11 — default background.
    Background,
    /// OSC 12 — cursor colour.
    Cursor,
}

impl OscColourSlot {
    pub(crate) const fn osc_number(self) -> u32 {
        match self {
            Self::Foreground => 10,
            Self::Background => 11,
            Self::Cursor => 12,
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::Foreground => 0,
            Self::Background => 1,
            Self::Cursor => 2,
        }
    }
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
    keyboard_parameters: Vec<u8>,
    keyboard_parameters_overlong: bool,

    /// Dynamic input/string buffer.
    input_buf: Vec<u8>,
    input_buf_max: usize,
    /// OSC 133 waiting for the second byte of ST.
    pending_shell_integration: Option<String>,
    /// Bounded OSC 7501 payload waiting for the second byte of ST.
    pending_program_status: Option<String>,
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
    /// Whether this parse call emitted a printable character before REP.
    printed_in_current_parse: bool,
    /// Parsing consumed state a raw ANSI continuation cannot restore.
    recovery_rebase_required: bool,

    /// Bytes accumulated since last ground state, for control-mode catch-up.
    since_ground: Vec<u8>,

    /// Whether ground timer would be active (modeled as flag; actual timer
    /// is a server-side concern).
    ground_timer_active: bool,

    /// Reply buffer: replies to be sent back to the PTY.
    reply_buf: Vec<u8>,
    theme_events: Vec<ThemeEvent>,
    theme_events_enabled: bool,
    /// Dropped terminal passthrough events caused by parser string limits.
    terminal_passthrough_dropped_count: u64,

    /// Application-set OSC 10/11/12 colours (fg, bg, cursor). The daemon does
    /// not invent colours for an unknown attached terminal palette.
    osc_colours: [Option<String>; 3],
    pub(crate) query_colours: [Option<[u8; 3]>; 2],
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
            keyboard_parameters: Vec::new(),
            keyboard_parameters_overlong: false,
            input_buf: Vec::with_capacity(INPUT_BUF_START),
            input_buf_max: INPUT_BUF_MAX,
            pending_shell_integration: None,
            pending_program_status: None,
            input_end: InputEndType::St,
            param_list: ParamList::new(),
            cell: CellState::default(),
            saved: SavedState::default(),
            utf8_buf: [0; 4],
            utf8_len: 0,
            utf8_expected: 0,
            utf8_started: false,
            last_char: None,
            printed_in_current_parse: false,
            recovery_rebase_required: false,
            since_ground: Vec::new(),
            ground_timer_active: false,
            reply_buf: Vec::new(),
            theme_events: Vec::new(),
            theme_events_enabled: false,
            terminal_passthrough_dropped_count: 0,
            osc_colours: [None, None, None],
            query_colours: [None, None],
        }
    }

    /// Updates the maximum regular string buffer size used for OSC/DCS input.
    pub fn set_input_buffer_limit(&mut self, limit: usize) {
        self.input_buf_max = limit.max(INPUT_BUF_START);
    }

    pub(crate) const fn configured_input_buffer_limit(&self) -> usize {
        self.input_buf_max
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
        std::mem::take(&mut self.terminal_passthrough_dropped_count)
    }

    /// Returns and drains accumulated since-ground bytes.
    pub fn take_since_ground(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.since_ground)
    }

    /// Returns any bytes still buffered in an incomplete parser state.
    #[must_use]
    pub fn pending_bytes(&self) -> Vec<u8> {
        self.pending_bytes_ref().to_vec()
    }

    /// Borrows bytes still buffered in an incomplete parser state.
    #[must_use]
    pub(crate) fn pending_bytes_ref(&self) -> &[u8] {
        if self.state != InputState::Ground {
            return &self.since_ground;
        }
        if self.utf8_started {
            return &self.utf8_buf[..usize::from(self.utf8_len)];
        }
        &[]
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
        self.pending_shell_integration = None;
        self.pending_program_status = None;
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

    /// Returns the state restored by DECRC/SCP.
    #[must_use]
    pub const fn saved_state(&self) -> &SavedState {
        &self.saved
    }

    pub(crate) fn plain_output_forwarding_safe(&self) -> bool {
        self.state == InputState::Ground && !self.utf8_started && self.cell == CellState::default()
    }

    /// Parse a buffer of bytes, dispatching actions to the screen writer.
    pub fn parse<W: ScreenWriter + ?Sized>(&mut self, buf: &[u8], writer: &mut W) {
        self.printed_in_current_parse = false;
        let mut index = 0;
        while index < buf.len() {
            if self.state == InputState::Ground && !self.utf8_started {
                let printable_end = buf[index..]
                    .iter()
                    .position(|byte| !byte.is_ascii_graphic() && *byte != b' ')
                    .map_or(buf.len(), |offset| index + offset);
                if printable_end > index {
                    self.handle_printable_ascii_run(&buf[index..printable_end], writer);
                    index = printable_end;
                    continue;
                }
                if self.handle_ground_c0_fast_path(buf[index], writer) {
                    index += 1;
                    continue;
                }
            }

            self.ch = buf[index];
            let transition = self.find_transition();
            self.execute_transition(transition, writer);
            index += 1;
        }
    }

    fn handle_printable_ascii_run<W: ScreenWriter + ?Sized>(
        &mut self,
        bytes: &[u8],
        writer: &mut W,
    ) {
        debug_assert_eq!(self.state, InputState::Ground);
        let set = if self.cell.set == 0 {
            self.cell.g0set
        } else {
            self.cell.g1set
        };
        let acs = set != 0;
        writer.collect_add_ascii_run(bytes, &self.cell, acs);
        if let Some(&last) = bytes.last() {
            self.last_char = Some(char::from(last));
            self.printed_in_current_parse = true;
        }
        self.flags |= INPUT_LAST;
    }

    fn handle_ground_c0_fast_path<W: ScreenWriter + ?Sized>(
        &mut self,
        byte: u8,
        writer: &mut W,
    ) -> bool {
        match byte {
            0x0a..=0x0c => {
                writer.collect_end();
                writer.linefeed(false, self.cell.bg());
                if writer.current_mode() & mode::MODE_CRLF != 0 {
                    writer.carriage_return();
                }
            }
            0x0d => {
                writer.collect_end();
                writer.carriage_return();
            }
            _ => return false,
        }
        self.flags &= !INPUT_LAST;
        true
    }

    /// Stop any in-progress UTF-8 sequence and emit U+FFFD.
    fn stop_utf8<W: ScreenWriter + ?Sized>(&mut self, writer: &mut W) {
        if self.utf8_started {
            writer.collect_end();
            writer.collect_add('\u{FFFD}', &self.cell);
            self.utf8_started = false;
            self.utf8_len = 0;
            self.utf8_expected = 0;
        }
    }

    fn handle_print<W: ScreenWriter + ?Sized>(&mut self, writer: &mut W) -> bool {
        self.stop_utf8(writer);

        let ch = self.ch as char;
        let set = if self.cell.set == 0 {
            self.cell.g0set
        } else {
            self.cell.g1set
        };

        writer.collect_add_with_charset(ch, &self.cell, set != 0);

        self.last_char = Some(ch);
        self.printed_in_current_parse = true;
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
        self.collect_keyboard_parameter();
        if self.param_len >= PARAM_BUF_MAX - 1 {
            self.flags |= INPUT_DISCARD;
        } else {
            self.param_buf[self.param_len] = self.ch;
            self.param_len += 1;
        }
        false
    }

    fn handle_input(&mut self) -> bool {
        let escaped_dcs_byte = self.state == InputState::DcsEscape;
        let bytes_to_push = if escaped_dcs_byte && self.ch != 0x1b {
            2
        } else {
            1
        };
        let input_limit = self.input_buffer_limit();
        if self.input_buf.len() + bytes_to_push >= input_limit {
            if self.flags & INPUT_DISCARD == 0 && self.is_terminal_passthrough_string() {
                self.terminal_passthrough_dropped_count =
                    self.terminal_passthrough_dropped_count.saturating_add(1);
            }
            self.flags |= INPUT_DISCARD;
        } else if escaped_dcs_byte && self.ch == 0x1b {
            self.input_buf.push(0x1b);
        } else if escaped_dcs_byte {
            self.input_buf.push(0x1b);
            self.input_buf.push(self.ch);
        } else {
            self.input_buf.push(self.ch);
        }
        false
    }

    fn input_buffer_limit(&self) -> usize {
        if self.is_terminal_passthrough_string() {
            return MAX_TERMINAL_PASSTHROUGH_PAYLOAD_BYTES;
        }
        self.input_buf_max
    }

    fn is_terminal_passthrough_string(&self) -> bool {
        (self.state == InputState::ApcString && passthrough::is_kitty_graphics_apc(&self.input_buf))
            || (matches!(self.state, InputState::DcsHandler | InputState::DcsEscape)
                && self.interm_len == 0
                && (self.input_buf.first() == Some(&b'q') || self.input_buf.starts_with(b"tmux;")))
    }

    fn handle_esc_dispatch<W: ScreenWriter + ?Sized>(&mut self, writer: &mut W) -> bool {
        if self.flags & INPUT_DISCARD != 0 {
            return false;
        }
        dispatch::dispatch_esc(self, writer);
        self.flags &= !INPUT_LAST;
        false
    }

    fn handle_csi_dispatch<W: ScreenWriter + ?Sized>(&mut self, writer: &mut W) -> bool {
        if self.dispatch_keyboard_control(writer) {
            self.flags &= !INPUT_LAST;
            return false;
        }
        if self.flags & INPUT_DISCARD != 0 {
            return false;
        }
        dispatch::dispatch_csi(self, writer);
        self.flags &= !INPUT_LAST;
        false
    }

    fn handle_dcs_dispatch<W: ScreenWriter + ?Sized>(&mut self, writer: &mut W) -> bool {
        if self.flags & INPUT_DISCARD != 0 {
            return false;
        }
        dispatch::dispatch_dcs(self, writer);
        false
    }

    fn handle_top_bit_set<W: ScreenWriter + ?Sized>(&mut self, writer: &mut W) -> bool {
        self.flags &= !INPUT_LAST;

        if !self.utf8_started {
            self.utf8_started = true;
            self.utf8_len = 0;
            // tmux utf8_open: only C2..DF, E0..EF and F0..F4 open a sequence;
            // any other byte is one U+FFFD on its own.
            let Some(expected) = utf8_sequence_size(self.ch) else {
                self.stop_utf8(writer);
                return false;
            };
            self.utf8_expected = expected;
            self.utf8_buf[0] = self.ch;
            self.utf8_len = 1;
            return false;
        }

        // tmux utf8_append: every top-bit byte is consumed until the size
        // announced by the lead byte is reached, even one that is not a
        // continuation; such a byte only makes the whole sequence one U+FFFD.
        self.utf8_buf[usize::from(self.utf8_len)] = self.ch;
        self.utf8_len += 1;

        if self.utf8_len < self.utf8_expected {
            return false; // More bytes expected.
        }

        // Complete: decode. A non-continuation byte, an overlong form or a
        // surrogate fails here and yields a single U+FFFD for the sequence.
        let decoded = std::str::from_utf8(&self.utf8_buf[..usize::from(self.utf8_len)])
            .ok()
            .and_then(|s| s.chars().next());
        let Some(c) = decoded else {
            self.stop_utf8(writer);
            return false;
        };
        self.utf8_started = false;

        writer.collect_add(c, &self.cell);

        self.last_char = Some(c);
        self.printed_in_current_parse = true;
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

    fn note_effective_rep(&mut self) {
        if !self.printed_in_current_parse {
            self.recovery_rebase_required = true;
        }
    }

    /// Returns and clears the signal that raw recovery needs an authoritative
    /// rebase for inter-feed REP or a cursor reset using a configured default.
    #[must_use]
    pub(crate) fn take_recovery_rebase_required(&mut self) -> bool {
        std::mem::take(&mut self.recovery_rebase_required)
    }
}

impl Default for InputParser {
    fn default() -> Self {
        Self::new()
    }
}

/// Byte count announced by a UTF-8 lead byte, as tmux `utf8_open` accepts it,
/// or `None` for a byte that cannot open a sequence (C0, C1, F5..FF and the
/// continuation range).
const fn utf8_sequence_size(lead: u8) -> Option<u8> {
    match lead {
        0xC2..=0xDF => Some(2),
        0xE0..=0xEF => Some(3),
        0xF0..=0xF4 => Some(4),
        _ => None,
    }
}
