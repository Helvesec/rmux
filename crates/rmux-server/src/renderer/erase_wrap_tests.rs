use super::render_pane_screen;
use rmux_core::input::InputParser;
use rmux_core::{GridRenderOptions, OptionStore, Screen, ScreenCaptureRange};
use rmux_proto::{SessionName, TerminalSize};

const PANE_SIZE: TerminalSize = TerminalSize { cols: 8, rows: 6 };

fn parsed_screen(bytes: &[u8], size: TerminalSize) -> Screen {
    let mut screen = Screen::new(size, 100);
    let mut parser = InputParser::new();
    parser.parse(bytes, &mut screen);
    screen
}

fn visible_transcript(screen: &Screen) -> Vec<u8> {
    screen.capture_transcript(ScreenCaptureRange::default(), GridRenderOptions::default())
}

#[test]
fn attached_render_keeps_the_blank_row_created_by_complete_continuation_erase() {
    let terminal_size = TerminalSize { cols: 8, rows: 7 };
    let session = crate::session_fixture::SessionFixture::new(
        SessionName::new("alpha").expect("valid session name"),
        terminal_size,
    );
    let pane = session.window().pane(0).expect("pane 0");
    let screen = parsed_screen(b"abcdefghijkl\x1b[2;1H\x1b[2K\x1b[3;1HNEXT", PANE_SIZE);

    let frame = render_pane_screen(session.view(), &OptionStore::new(), pane, &screen);
    assert_eq!(visible_transcript(&screen), b"abcdefgh\n\nNEXT\n\n\n\n");
    let first = frame
        .windows(b"\x1b[1;1H\x1b[0mabcdefgh".len())
        .position(|bytes| bytes == b"\x1b[1;1H\x1b[0mabcdefgh")
        .expect("attached frame paints the wrapped predecessor");
    let blank = frame
        .windows(b"\x1b[2;1H\x1b[0m        ".len())
        .position(|bytes| bytes == b"\x1b[2;1H\x1b[0m        ")
        .expect("attached frame paints the narrow erased continuation row with spaces");
    let next = frame
        .windows(b"\x1b[3;1H\x1b[0mNEXT".len())
        .position(|bytes| bytes == b"\x1b[3;1H\x1b[0mNEXT")
        .expect("attached frame paints the following hard line");
    assert!(first < blank && blank < next);
}
