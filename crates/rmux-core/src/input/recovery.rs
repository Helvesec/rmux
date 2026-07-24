use super::{InputParser, OscColourSlot};

impl InputParser {
    /// Serializes parser state that has a faithful ANSI representation.
    pub(crate) fn recovery_dynamic_colours_ansi(&self) -> Vec<u8> {
        let mut out = b"\x1b]110\x1b\\\x1b]111\x1b\\\x1b]112\x1b\\".to_vec();
        for slot in [
            OscColourSlot::Foreground,
            OscColourSlot::Background,
            OscColourSlot::Cursor,
        ] {
            let Some(value) = self.osc_colour(slot) else {
                continue;
            };
            if value.chars().any(char::is_control) {
                continue;
            }
            out.extend_from_slice(b"\x1b]");
            out.extend_from_slice(slot.osc_number().to_string().as_bytes());
            out.push(b';');
            out.extend_from_slice(value.as_bytes());
            out.extend_from_slice(b"\x1b\\");
        }
        out
    }
}
