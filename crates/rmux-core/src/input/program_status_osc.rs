//! OSC 7501 program status: capability query and report forwarding.

use super::{InputEndType, InputParser, ScreenWriter};

impl InputParser {
    pub(super) fn dispatch_program_status<W: ScreenWriter + ?Sized>(
        &mut self,
        input_len: usize,
        data: &str,
        end: InputEndType,
        writer: &mut W,
    ) {
        let terminator = match end {
            InputEndType::St => "\x1b\\",
            InputEndType::Bel => "\x07",
        };
        // Include the actual numeric prefix (possibly zero-padded) in the cap.
        if input_len + 2 + terminator.len() > 4096 {
            return;
        }
        if data == "?" {
            self.reply(&format!("\x1b]7501;?:states=idle,working,done,blocked,error:kinds=permission,question,auth{terminator}"));
        } else {
            writer.osc_program_status(data, end);
        }
    }
}
