use super::*;
use rmux_core::{ScreenCaptureRange, TerminalScreen};
use rmux_proto::TerminalSize;

#[test]
fn keyframe_preserves_dead_footer_written_extent_after_width_resize() {
    let size = TerminalSize { cols: 8, rows: 4 };
    for first in ["ab", "ab  ", ""] {
        for alternate in [false, true] {
            let mut transcript = PaneTranscript::new(100, size);
            transcript.append_bytes(format!("\x1b[4;1H{first}").as_bytes());
            transcript.append_dead_pane_footer(b"dead");
            if alternate {
                transcript.append_bytes(b"\x1b[?1049hOTHER");
            }
            let keyframe = PaneRecoverySeed::capture(&transcript).unwrap().keyframe();
            let mut actual = TerminalScreen::new(size, 100);
            actual.feed(&keyframe.bytes);
            if alternate {
                actual.feed(b"\x1b[?1049l");
                transcript.append_bytes(b"\x1b[?1049l");
            }
            for cols in [8, 4, 12, 3, 8] {
                let resized = TerminalSize { cols, rows: 4 };
                actual.resize(resized);
                transcript.resize(resized);
                let expected = transcript.screen();
                let options = GridRenderOptions {
                    join_wrapped: true,
                    trim_spaces: false,
                    include_empty_cells: false,
                    ..GridRenderOptions::default()
                };
                assert_eq!(
                    actual
                        .screen()
                        .capture_transcript(ScreenCaptureRange::default(), options),
                    expected.capture_transcript(ScreenCaptureRange::default(), options),
                    "first={first:?}, alternate={alternate}, cols={cols}"
                );
                assert_eq!(
                    actual.screen().cursor_position(),
                    expected.cursor_position()
                );
            }
        }
    }
}
