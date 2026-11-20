//! Issue #29 (Helvesec/rmux): with fish as `default-shell`, the first character
//! typed on an empty command line made the outer terminal's cursor jump to the
//! top-left corner; the second character put it back. fish answers that first
//! key by repainting its whole prompt line in place (carriage return, coloured
//! prompt, coloured key, grey autosuggestion, cursor moved back). The
//! interactive frame carrying that repaint must keep the cursor hidden while it
//! moves and must show it again on the pane cursor, never on the corner.

use rmux_core::OptionStore;
use rmux_proto::{SessionName, TerminalSize};

use crate::pane_transcript::PaneTranscript;
use crate::renderer::PaneRenderDelta;

use super::LivePaneRender;

const PROMPT: &[u8] = b"\x1b[92muser\x1b[39m@host \x1b[32m~/w\x1b[39m> ";
/// `user@host ~/w> ` is 15 columns wide; the key lands in column 16 and the
/// cursor waits in column 17 (1-based), on the first row after `clear`.
const CURSOR_AFTER_FIRST_KEY: (u16, u16) = (1, 17);
const CURSOR_ON_EMPTY_PROMPT: (u16, u16) = (1, 16);

/// What the outer terminal shows while one frame is applied, starting from a
/// visible cursor: how many cursor moves hit the top-left corner while the
/// cursor is visible, and where the cursor stands each time it is shown.
fn visible_cursor_trace(frame: &[u8]) -> (usize, Vec<Option<(u16, u16)>>) {
    let mut visible = true;
    let mut position: Option<(u16, u16)> = None;
    let mut homes_while_visible = 0;
    let mut shown_at = Vec::new();
    let mut index = 0;
    while index < frame.len() {
        if frame[index] != 0x1b || frame.get(index + 1) != Some(&b'[') {
            index += 1;
            continue;
        }
        let start = index + 2;
        let Some(length) = frame[start..]
            .iter()
            .position(|byte| (0x40..=0x7e).contains(byte))
        else {
            break;
        };
        let params = &frame[start..start + length];
        let final_byte = frame[start + length];
        index = start + length + 1;
        match (params, final_byte) {
            (b"?25", b'h') => {
                visible = true;
                shown_at.push(position);
            }
            (b"?25", b'l') => visible = false,
            (_, b'u') => position = None,
            (_, b'H' | b'f') if !params.starts_with(b"?") => {
                let text = std::str::from_utf8(params).expect("CUP parameters are ASCII");
                let mut parts = text.split(';').map(|part| part.parse::<u16>().unwrap_or(1));
                let row = parts.next().unwrap_or(1);
                let col = parts.next().unwrap_or(1);
                position = Some((row, col));
                if visible && (row, col) == (1, 1) {
                    homes_while_visible += 1;
                }
            }
            _ => {}
        }
    }
    (homes_while_visible, shown_at)
}

fn interactive_frame(renderer: &mut LivePaneRender) -> Vec<u8> {
    let PaneRenderDelta::Incremental(delta) = renderer.render_interactive_frame_from_transcript()
    else {
        panic!("issue #29: a prompt-line repaint should render as an incremental delta");
    };
    delta.frame().to_vec()
}

fn assert_cursor_stays_off_the_corner(frame: &[u8], expected: (u16, u16), step: &str) {
    let (homes_while_visible, shown_at) = visible_cursor_trace(frame);
    let text = String::from_utf8_lossy(frame);
    assert_eq!(
        homes_while_visible, 0,
        "issue #29 {step}: the visible cursor must never reach the top-left corner: {text:?}"
    );
    assert_eq!(
        shown_at.last().copied().flatten(),
        Some(expected),
        "issue #29 {step}: the cursor must be shown again on the pane cursor: {text:?}"
    );
    assert!(
        shown_at
            .iter()
            .all(|position| *position != Some((1, 1)) && position.is_some()),
        "issue #29 {step}: every show must follow an explicit move off the corner: {text:?}"
    );
}

#[test]
fn issue_29_fish_first_key_repaint_never_shows_the_cursor_in_the_corner() {
    let session = crate::session_fixture::SessionFixture::new(
        SessionName::new("issue29").expect("valid session name"),
        TerminalSize { cols: 60, rows: 5 },
    );
    let pane = session.window().active_pane().expect("active pane").clone();
    let transcript = PaneTranscript::shared(100, TerminalSize { cols: 60, rows: 4 });
    {
        let mut transcript = transcript.lock();
        transcript.append_bytes(b"\x1b[H\x1b[2J");
        transcript.append_bytes(PROMPT);
    }
    let mut renderer = LivePaneRender::new_from_transcript(
        transcript.clone(),
        session.view(),
        OptionStore::new(),
        pane,
    )
    .expect("initial render snapshot");

    // Steps 1-5 of the report: first key, second key, delete back to an empty
    // prompt, then a first key again.
    let first_key = [
        b"\r".as_slice(),
        PROMPT,
        b"\x1b[91me\x1b[90mcho\x1b[39m\x1b[3D",
    ]
    .concat();
    let second_key = [
        b"\r".as_slice(),
        PROMPT,
        b"\x1b[91mec\x1b[90mho\x1b[39m\x1b[2D",
    ]
    .concat();
    let back_to_empty = [b"\r".as_slice(), PROMPT, b"\x1b[K"].concat();
    let steps: [(&[u8], (u16, u16), &str); 4] = [
        (&first_key, CURSOR_AFTER_FIRST_KEY, "first key"),
        (&second_key, (1, 18), "second key"),
        (
            &back_to_empty,
            CURSOR_ON_EMPTY_PROMPT,
            "deleted back to an empty prompt",
        ),
        (&first_key, CURSOR_AFTER_FIRST_KEY, "first key again"),
    ];
    for (bytes, expected, step) in steps {
        transcript.lock().append_bytes(bytes);
        let frame = interactive_frame(&mut renderer);
        assert_cursor_stays_off_the_corner(&frame, expected, step);
    }
}
