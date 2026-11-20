use crate::{GridRenderOptions, ScreenCaptureRange, TerminalScreen};
use rmux_proto::TerminalSize;

#[test]
fn width_resize_recovery_extent_is_streaming_and_preserves_written_spaces() {
    let size = TerminalSize { cols: 8, rows: 4 };
    for chunk_size in [1, 3, 100] {
        for (end, expected) in [(2, "abcd"), (4, "ab  cd")] {
            let mut terminal = TerminalScreen::new(size, 10);
            let bytes = format!("ab      \x1bPrmux;row-extent;1;{end}\x1b\\cd");
            for chunk in bytes.as_bytes().chunks(chunk_size) {
                terminal.feed(chunk);
            }
            terminal.resize(TerminalSize { cols: 6, rows: 4 });
            let text = terminal.screen().capture_transcript(
                ScreenCaptureRange::default(),
                GridRenderOptions {
                    join_wrapped: true,
                    ..GridRenderOptions::default()
                },
            );
            assert_eq!(
                String::from_utf8(text).unwrap().lines().next(),
                Some(expected)
            );
        }
    }
}

#[test]
fn recovery_extent_rejects_invalid_or_destructive_metadata() {
    for payload in [
        "rmux;row-extent;2;2",
        "rmux;row-extent;1;",
        "rmux;row-extent;1;-1",
        "rmux;row-extent;1;+2",
        "rmux;row-extent;1;9",
        "rmux;row-extent;1;65536",
        "rmux;row-extent;1;1",
        "rmux;row-extent;1;2;3",
    ] {
        let size = TerminalSize { cols: 8, rows: 4 };
        let mut actual = TerminalScreen::new(size, 10);
        let mut expected = TerminalScreen::new(size, 10);
        actual.feed(format!("ab      \x1bP{payload}\x1b\\cd").as_bytes());
        expected.feed(b"ab      cd");
        for cols in [8, 4, 12] {
            let resized = TerminalSize { cols, rows: 4 };
            actual.resize(resized);
            expected.resize(resized);
            let options = GridRenderOptions {
                join_wrapped: true,
                trim_spaces: false,
                ..GridRenderOptions::default()
            };
            assert_eq!(
                actual
                    .screen()
                    .capture_transcript(ScreenCaptureRange::default(), options),
                expected
                    .screen()
                    .capture_transcript(ScreenCaptureRange::default(), options),
                "{payload}"
            );
            assert_eq!(
                actual.screen().cursor_position(),
                expected.screen().cursor_position()
            );
        }
    }
}
