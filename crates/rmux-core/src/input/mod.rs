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
pub mod mode;
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

/// Default colours reported to OSC 10/11/12 queries when the application has
/// not set one. Detached sessions have no attached terminal to answer, so the
/// daemon impersonates a conventional dark terminal; override per daemon with
/// `RMUX_DEFAULT_FOREGROUND` / `RMUX_DEFAULT_BACKGROUND` /
/// `RMUX_DEFAULT_CURSOR_COLOUR` (X11 `rgb:RRRR/GGGG/BBBB` format expected by
/// query consumers).
fn default_osc_colours() -> &'static [String; 3] {
    static DEFAULTS: std::sync::OnceLock<[String; 3]> = std::sync::OnceLock::new();
    DEFAULTS.get_or_init(|| {
        let var = |name: &str, fallback: &str| {
            std::env::var(name)
                .ok()
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| fallback.to_owned())
        };
        [
            var("RMUX_DEFAULT_FOREGROUND", "rgb:cccc/cccc/cccc"),
            var("RMUX_DEFAULT_BACKGROUND", "rgb:0000/0000/0000"),
            var("RMUX_DEFAULT_CURSOR_COLOUR", "rgb:cccc/cccc/cccc"),
        ]
    })
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
    input_buf_max: usize,
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

    /// Application-set OSC 10/11/12 colours (fg, bg, cursor); `None` falls
    /// back to the daemon defaults when a query is answered.
    osc_colours: [Option<String>; 3],
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
            input_buf_max: INPUT_BUF_MAX,
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
            osc_colours: [None, None, None],
        }
    }

    /// Returns the colour reported for an OSC 10/11/12 query.
    pub(crate) fn osc_colour(&self, slot: OscColourSlot) -> &str {
        self.osc_colours[slot.index()]
            .as_deref()
            .unwrap_or_else(|| default_osc_colours()[slot.index()].as_str())
    }

    /// Records an application-set OSC 10/11/12 colour so later queries
    /// round-trip the value.
    pub(crate) fn set_osc_colour(&mut self, slot: OscColourSlot, value: &str) {
        // A stored colour is reflected verbatim to every later query, so bound
        // it to a sane colour-spec length: otherwise a pane could set a huge
        // (up to input-buffer-sized) value and then flood queries in one read
        // batch, amplifying it into unbounded reply memory and OOM-ing the
        // daemon. A real X11 / `rgb:` / `#rrggbb` spec is far shorter; longer
        // values are not colours and are left unstored, so queries keep
        // answering the prior or default value.
        const OSC_COLOUR_MAX_LEN: usize = 64;
        if value.len() > OSC_COLOUR_MAX_LEN {
            return;
        }
        self.osc_colours[slot.index()] = Some(value.to_owned());
    }

    /// Resets an OSC colour back to the daemon default (OSC 110/111/112).
    pub(crate) fn reset_osc_colour(&mut self, slot: OscColourSlot) {
        self.osc_colours[slot.index()] = None;
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
    pub fn parse<W: ScreenWriter + ?Sized>(&mut self, buf: &[u8], writer: &mut W) {
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

    fn find_transition(&self) -> Transition {
        self.state.transition_for_byte(self.ch)
    }

    fn execute_transition<W: ScreenWriter + ?Sized>(&mut self, trans: Transition, writer: &mut W) {
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

        if self.should_recover_tmux_passthrough_osc_bel() {
            self.input_end = InputEndType::Bel;
            self.handle_dcs_dispatch(writer);
            self.set_state(InputState::Ground, writer);
            return;
        }

        if let Some(next) = trans.next_state {
            self.set_state(next, writer);
        }

        // If not in ground state, save byte to since_ground.
        if self.state != InputState::Ground && self.since_ground.len() < self.input_buf_max {
            self.since_ground.push(self.ch);
        }
    }

    fn set_state<W: ScreenWriter + ?Sized>(&mut self, next: InputState, writer: &mut W) {
        // Call exit handler for current state.
        self.exit_state(writer);
        self.state = next;
        // Call enter handler for new state.
        self.enter_state(writer);
    }

    fn enter_state<W: ScreenWriter + ?Sized>(&mut self, writer: &mut W) {
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

    fn exit_state<W: ScreenWriter + ?Sized>(&mut self, writer: &mut W) {
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

    fn exit_osc<W: ScreenWriter + ?Sized>(&mut self, writer: &mut W) {
        if self.flags & INPUT_DISCARD != 0 {
            return;
        }
        dispatch::dispatch_osc(self, writer);
    }

    fn exit_apc<W: ScreenWriter + ?Sized>(&mut self, writer: &mut W) {
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

    fn exit_rename<W: ScreenWriter + ?Sized>(&mut self, writer: &mut W) {
        if self.flags & INPUT_DISCARD != 0 {
            return;
        }
        let buf = String::from_utf8_lossy(&self.input_buf).into_owned();
        writer.set_window_name(&buf);
    }

    /// Stop any in-progress UTF-8 sequence and emit U+FFFD.
    fn stop_utf8<W: ScreenWriter + ?Sized>(&mut self, writer: &mut W) {
        if self.utf8_started {
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

    fn should_recover_tmux_passthrough_osc_bel(&self) -> bool {
        if self.state != InputState::DcsHandler || self.ch != 0x07 || self.interm_len != 0 {
            return false;
        }

        let Some(payload) = self.input_buf.strip_prefix(b"tmux;") else {
            return false;
        };

        payload.starts_with(b"\x1b]") || payload.first() == Some(&0x9d)
    }

    fn handle_end_bel(&mut self) -> bool {
        self.input_end = InputEndType::Bel;
        false
    }

    fn handle_c0_dispatch<W: ScreenWriter + ?Sized>(&mut self, writer: &mut W) -> bool {
        self.stop_utf8(writer);
        dispatch::dispatch_c0(self, writer);
        self.flags &= !INPUT_LAST;
        false
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
