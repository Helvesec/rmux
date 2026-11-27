use super::*;
use crate::{Screen, TerminalPassthroughKind};
use rmux_proto::TerminalSize;

#[test]
fn opaque_dcs_preserves_raw_screen_events_at_every_feed_boundary() {
    for (packet, payload, kind) in [
        (
            b"\x1bPtmux;\x1b]0;outer\x07X\x1b\\".as_slice(),
            b"\x1b]0;outer\x07X".as_slice(),
            TerminalPassthroughKind::Raw,
        ),
        (
            b"\x1bPtmux;\x1b\x1b]0;outer\x07X\x1b\\",
            b"\x1b]0;outer\x07X",
            TerminalPassthroughKind::Raw,
        ),
        (
            b"\x1bPtmux;\x9d0;outer\x07X\x1b\\",
            b"\x9d0;outer\x07X",
            TerminalPassthroughKind::Raw,
        ),
        (
            b"\x1bPtmux;not-osc\x07X\x1b\\",
            b"not-osc\x07X",
            TerminalPassthroughKind::Raw,
        ),
        (
            b"\x1bPq#0!10~\x07X\x1b\\",
            b"q#0!10~\x07X",
            TerminalPassthroughKind::Sixel,
        ),
    ] {
        for split in 0..=packet.len() {
            let mut parser = InputParser::new();
            let mut screen = Screen::new(TerminalSize { cols: 80, rows: 24 }, 0);
            parser.parse(&packet[..split], &mut screen);
            if split < packet.len() {
                assert!(screen.take_terminal_passthrough().is_empty());
            }
            parser.parse(&packet[split..], &mut screen);
            let events = screen.take_terminal_passthrough();
            assert_eq!(events.len(), 1, "packet {packet:?}, split {split}");
            assert_eq!(events[0].kind(), kind);
            assert_eq!(events[0].payload(), payload);
            assert_eq!(parser.state(), InputState::Ground);
            assert_eq!(screen.cursor_position(), (0, 0));
            assert_eq!(screen.title(), "");
            for row in 0..24 {
                assert!(screen.visit_visible_line_cells(row, 80, |cell| {
                    assert_eq!(cell.text(), " ");
                }));
            }
        }
    }
}

#[test]
fn opaque_dcs_discard_does_not_dispatch_a_partial_payload_at_bel() {
    let mut parser = InputParser::new();
    let mut writer = RecordingWriter::new(80, 24);
    parser.parse(b"\x1bPtmux;\x1b]0;outer", &mut writer);
    parser.flags |= INPUT_DISCARD;
    parser.parse(b"\x07X\x1b\\", &mut writer);
    assert_eq!(parser.state(), InputState::Ground);
    assert!(!writer.has_call("dcs_passthrough("));
    assert!(writer.chars.is_empty());
}
