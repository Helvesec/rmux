use super::{InputParser, InputState, ScreenWriter};
use crate::kitty_keyboard::{parse_control_frame, FRAME_LIMIT};

impl InputParser {
    fn keyboard_namespace(&self) -> bool {
        matches!(self.interm_str(), b"<" | b"=" | b">" | b"?")
    }

    pub(super) fn collect_keyboard_parameter(&mut self) {
        if matches!(self.state, InputState::CsiEnter | InputState::CsiParameter)
            && self.keyboard_namespace()
        {
            // ESC [ namespace and final occupy four bytes. Other CSI commands
            // keep the existing short signed-parameter limit and discard flag.
            if self.keyboard_parameters.len() < FRAME_LIMIT - 4 {
                self.keyboard_parameters.push(self.ch);
            } else {
                self.keyboard_parameters_overlong = true;
            }
        }
    }

    pub(super) fn dispatch_keyboard_control<W: ScreenWriter + ?Sized>(
        &mut self,
        writer: &mut W,
    ) -> bool {
        if self.ch != b'u' || !self.keyboard_namespace() {
            return false;
        }
        if !self.keyboard_parameters_overlong {
            let mut frame = b"\x1b[".to_vec();
            frame.extend_from_slice(self.interm_str());
            frame.extend_from_slice(&self.keyboard_parameters);
            frame.push(b'u');
            if let Ok(control) = parse_control_frame(&frame) {
                self.reply_buf
                    .extend(writer.keyboard_control(control).bytes());
            }
        }
        true
    }
}
