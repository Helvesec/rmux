//! State transitions and string entry/exit lifecycle.

use super::{
    dispatch, passthrough, states, InputEndType, InputParser, InputState, ScreenWriter,
    INPUT_BUF_START, INPUT_DISCARD, INPUT_LAST, INTERM_BUF_MAX, PARAM_BUF_MAX,
};
use states::Transition;

impl InputParser {
    pub(super) fn find_transition(&self) -> Transition {
        self.state.transition_for_byte(self.ch)
    }

    pub(super) fn execute_transition<W: ScreenWriter + ?Sized>(
        &mut self,
        trans: Transition,
        writer: &mut W,
    ) {
        self.finish_pending_shell_integration(writer);
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

        // BEL inside a DCS is payload. Only the outer terminator (or the
        // explicit ground timeout) ends an otherwise incomplete wrapper.
        if let Some(next) = trans.next_state {
            self.set_state(next, writer);
        }

        // If not in ground state, save byte to since_ground.
        if self.state != InputState::Ground && self.since_ground.len() < self.input_buf_max {
            self.since_ground.push(self.ch);
        }
    }

    pub(super) fn set_state<W: ScreenWriter + ?Sized>(&mut self, next: InputState, writer: &mut W) {
        // Call exit handler for current state.
        self.exit_state(writer);
        self.state = next;
        // Call enter handler for new state.
        self.enter_state(writer);
    }

    fn enter_state<W: ScreenWriter + ?Sized>(&mut self, writer: &mut W) {
        match self.state {
            InputState::Ground => self.enter_ground(),
            InputState::EscEnter => {
                self.clear();
                self.ground_timer_active = self.pending_shell_integration.is_some();
            }
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

    pub(super) fn clear(&mut self) {
        self.ground_timer_active = false;
        self.interm_buf = [0; INTERM_BUF_MAX];
        self.interm_len = 0;
        self.param_buf = [0; PARAM_BUF_MAX];
        self.param_len = 0;
        self.keyboard_parameters.clear();
        self.keyboard_parameters_overlong = false;
        self.input_buf.clear();
        self.input_end = InputEndType::St;
        self.flags &= !INPUT_DISCARD;
    }

    pub(super) fn enter_ground(&mut self) {
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
        // A rename is dropped whole when its bytes are not valid, rather than
        // renaming the window to replacement characters: tmux 3.7b checks
        // `utf8_isvalid` before doing anything (input.c:2815). Titles keep
        // their lossy decoding, which is a separate dispatch path.
        let Some(name) = rename_name(&self.input_buf) else {
            return;
        };
        writer.set_window_name(name);
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
}

/// tmux's `utf8_isvalid` as the rename path applies it (utf8.c): a single-byte
/// character must be printable ASCII, and a multi-byte one must be a complete,
/// valid sequence. The rename table already drops C0, so what this rejects in
/// practice is `DEL`, a bare 8-bit byte and a truncated sequence.
fn rename_name(buf: &[u8]) -> Option<&str> {
    let name = std::str::from_utf8(buf).ok()?;
    name.chars()
        .all(|character| !character.is_ascii() || (' '..='~').contains(&character))
        .then_some(name)
}
