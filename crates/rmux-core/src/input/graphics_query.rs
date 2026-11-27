use super::{InputParser, ScreenWriter};

/// Match tmux 3.7b's SIXEL-enabled XTSMGRAPHICS replies. Size queries and
/// palette mutations are unsupported; only the colour-register count is known.
pub(super) fn dispatch<W: ScreenWriter + ?Sized>(parser: &mut InputParser, writer: &W) {
    if !writer.sixel_passthrough_available() || parser.param_list.len() > 3 {
        return;
    }
    let item = parser.param_list.get(0, 0, 0);
    let action = parser.param_list.get(1, 0, 0);
    let value = parser.param_list.get(2, 0, 0);
    if item == 1 && matches!(action, 1 | 2 | 4) {
        parser.reply("\x1b[?1;0;1024S");
    } else {
        parser.reply(&format!("\x1b[?{item};3;{value}S"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Screen;
    use rmux_proto::TerminalSize;

    #[test]
    fn sixel_capability_replies_match_tmux_37b_at_every_read_boundary() {
        // Captured from tmux 3.7b built with --enable-sixel, on a native PTY.
        let cases: &[(&[u8], &[u8])] = &[
            (b"\x1b[c", b"\x1b[?1;2;4c"),
            (b"\x1b[0c", b"\x1b[?1;2;4c"),
            (b"\x1b[1c", b""),
            (b"\x1b[>c", b"\x1b[>84;0;0c"),
            (b"\x1b[?1;1S", b"\x1b[?1;0;1024S"),
            (b"\x1b[?1;2S", b"\x1b[?1;0;1024S"),
            (b"\x1b[?1;4S", b"\x1b[?1;0;1024S"),
            (b"\x1b[?1;3;16S", b"\x1b[?1;3;16S"),
            (b"\x1b[?2;1S", b"\x1b[?2;3;0S"),
            (b"\x1b[?2;4S", b"\x1b[?2;3;0S"),
            (b"\x1b[?3;1S", b"\x1b[?3;3;0S"),
            (b"\x1b[?1;1;0;0S", b""),
        ];
        for &(query, expected) in cases {
            for split in 0..=query.len() {
                let mut parser = InputParser::new();
                let mut screen = Screen::new(TerminalSize::new(80, 24), 0);
                screen.set_sixel_passthrough_available(true);
                parser.parse(&query[..split], &mut screen);
                parser.parse(&query[split..], &mut screen);
                assert_eq!(parser.take_replies(), expected, "{query:?}, split {split}");
                assert!(parser.take_replies().is_empty());
            }
        }
    }

    #[test]
    fn sixel_capability_can_be_revoked_without_changing_pixel_queries() {
        let mut parser = InputParser::new();
        let mut screen = Screen::new(TerminalSize::new(80, 24), 0);
        screen.set_cell_pixels(Some(rmux_types::TerminalPixels::new(10, 20)));
        screen.set_sixel_passthrough_available(true);
        parser.parse(b"\x1b[c", &mut screen);
        assert_eq!(parser.take_replies(), b"\x1b[?1;2;4c");
        screen.set_sixel_passthrough_available(false);
        parser.parse(b"\x1b[c\x1b[?1;1S\x1b[14t\x1b[16t", &mut screen);
        assert_eq!(
            parser.take_replies(),
            b"\x1b[?1;2c\x1b[4;480;800t\x1b[6;20;10t"
        );
    }
}
