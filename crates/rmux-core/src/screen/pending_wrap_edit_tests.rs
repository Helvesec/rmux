//! Erase and character edits while an autowrap is pending.
//!
//! tmux 3.7b keeps its logical cursor at `sx` after the last column is
//! written, so `EL 0`, `ED 0`, `DCH`, `ICH` and `ECH` find no cell under the
//! cursor: they leave the line alone and the next printable character still
//! wraps. `EL 1`, `EL 2` and `ED 1` clear the whole cursor row.
//!
//! Every expectation below is the `capture-pane -p` output and the
//! `#{cursor_x},#{cursor_y}` value measured on tmux 3.7b in a real 10x5 pane
//! fed with `stty -opost; printf <sequence>`. A logical `cursor_x` of `sx` is
//! represented in RMUX by the last column plus a pending wrap.

use super::Screen;
use crate::input::InputParser;
use crate::{GridRenderOptions, ScreenCaptureRange};
use rmux_proto::TerminalSize;

const SIZE: TerminalSize = TerminalSize { cols: 10, rows: 5 };

const PREFILL: &str = "\x1b[1;1Haaaaaaaaa\x1b[2;1Hbbbbbbbbb\x1b[3;1Hccccccccc\x1b[4;1Hddddddddd";

struct Case {
    name: &'static str,
    prefill: bool,
    input: &'static str,
    capture: &'static str,
    tmux_cursor: (u32, u32),
}

const fn case(
    name: &'static str,
    prefill: bool,
    input: &'static str,
    capture: &'static str,
    tmux_cursor: (u32, u32),
) -> Case {
    Case {
        name,
        prefill,
        input,
        capture,
        tmux_cursor,
    }
}

/// The sequences of R3332 §M2 and their siblings, all measured on tmux 3.7b.
const PENDING_WRAP_CASES: &[Case] = &[
    case(
        "el",
        false,
        "\x1b[2;10He\x1b[K",
        "\n         e\n\n\n\n",
        (10, 1),
    ),
    case(
        "el_x",
        false,
        "\x1b[2;10He\x1b[KX",
        "\n         e\nX\n\n\n",
        (1, 2),
    ),
    case(
        "dch",
        false,
        "\x1b[2;10He\x1b[P",
        "\n         e\n\n\n\n",
        (10, 1),
    ),
    case(
        "dch_x",
        false,
        "\x1b[2;10He\x1b[PX",
        "\n         e\nX\n\n\n",
        (1, 2),
    ),
    case(
        "ich",
        false,
        "\x1b[2;10He\x1b[@",
        "\n         e\n\n\n\n",
        (10, 1),
    ),
    case(
        "ich_x",
        false,
        "\x1b[2;10He\x1b[@X",
        "\n         e\nX\n\n\n",
        (1, 2),
    ),
    case(
        "ech",
        false,
        "\x1b[2;10He\x1b[X",
        "\n         e\n\n\n\n",
        (10, 1),
    ),
    case(
        "ech_x",
        false,
        "\x1b[2;10He\x1b[XX",
        "\n         e\nX\n\n\n",
        (1, 2),
    ),
    case(
        "p_el_x",
        true,
        "\x1b[2;10He\x1b[KX",
        "aaaaaaaaa\nbbbbbbbbbe\nXcccccccc\nddddddddd\n\n",
        (1, 2),
    ),
    case(
        "p_el1_x",
        true,
        "\x1b[2;10He\x1b[1KX",
        "aaaaaaaaa\n\nXcccccccc\nddddddddd\n\n",
        (1, 2),
    ),
    case(
        "p_el2_x",
        true,
        "\x1b[2;10He\x1b[2KX",
        "aaaaaaaaa\n\nXcccccccc\nddddddddd\n\n",
        (1, 2),
    ),
    case(
        "p_ed_x",
        true,
        "\x1b[2;10He\x1b[JX",
        "aaaaaaaaa\nbbbbbbbbbe\nX\n\n\n",
        (1, 2),
    ),
    case(
        "p_ed_nox",
        true,
        "\x1b[2;10He\x1b[J",
        "aaaaaaaaa\nbbbbbbbbbe\n\n\n\n",
        (10, 1),
    ),
    case(
        "p_ed1_x",
        true,
        "\x1b[2;10He\x1b[1JX",
        "\n\nXcccccccc\nddddddddd\n\n",
        (1, 2),
    ),
    case(
        "p_ed_r1_x",
        true,
        "\x1b[1;10He\x1b[JX",
        "aaaaaaaaae\nX\n\n\n\n",
        (1, 1),
    ),
    case(
        "p_dch3_x",
        true,
        "\x1b[2;10He\x1b[3PX",
        "aaaaaaaaa\nbbbbbbbbbe\nXcccccccc\nddddddddd\n\n",
        (1, 2),
    ),
    case(
        "p_ich3_x",
        true,
        "\x1b[2;10He\x1b[3@X",
        "aaaaaaaaa\nbbbbbbbbbe\nXcccccccc\nddddddddd\n\n",
        (1, 2),
    ),
    case(
        "p_ech3_x",
        true,
        "\x1b[2;10He\x1b[3XX",
        "aaaaaaaaa\nbbbbbbbbbe\nXcccccccc\nddddddddd\n\n",
        (1, 2),
    ),
    case(
        "wide_el_x",
        false,
        "\x1b[2;9H中\x1b[KX",
        "\n        中\nX\n\n\n",
        (1, 2),
    ),
    case(
        "wide_dch_x",
        false,
        "\x1b[2;9H中\x1b[PX",
        "\n        中\nX\n\n\n",
        (1, 2),
    ),
    case(
        "wide_ich_x",
        false,
        "\x1b[2;9H中\x1b[@X",
        "\n        中\nX\n\n\n",
        (1, 2),
    ),
];

/// Neighbours that never had a pending wrap: autowrap off, or a cursor in
/// the middle of the row. They must keep editing the cell under the cursor.
const NO_PENDING_WRAP_CASES: &[Case] = &[
    case(
        "nowrap_el_x",
        false,
        "\x1b[?7l\x1b[2;10He\x1b[KX",
        "\n         X\n\n\n\n",
        (9, 1),
    ),
    case(
        "nowrap_dch_x",
        false,
        "\x1b[?7l\x1b[2;10He\x1b[PX",
        "\n         X\n\n\n\n",
        (9, 1),
    ),
    case(
        "nowrap_ich_x",
        false,
        "\x1b[?7l\x1b[2;10He\x1b[@X",
        "\n         X\n\n\n\n",
        (9, 1),
    ),
    case(
        "mid_el_x",
        false,
        "\x1b[2;5He\x1b[KX",
        "\n    eX\n\n\n\n",
        (6, 1),
    ),
    case(
        "mid_dch_x",
        false,
        "\x1b[1;1Haaaaaaaaa\x1b[2;1Hbbbbbbbbb\x1b[2;5H\x1b[PX",
        "aaaaaaaaa\nbbbbXbbb\n\n\n\n",
        (5, 1),
    ),
    case(
        "mid_ich_x",
        false,
        "\x1b[1;1Haaaaaaaaa\x1b[2;1Hbbbbbbbbb\x1b[2;5H\x1b[@X",
        "aaaaaaaaa\nbbbbXbbbbb\n\n\n\n",
        (5, 1),
    ),
];

fn run(case: &Case) -> Screen {
    let mut screen = Screen::new(SIZE, 100);
    let mut parser = InputParser::new();
    if case.prefill {
        parser.parse(PREFILL.as_bytes(), &mut screen);
    }
    parser.parse(case.input.as_bytes(), &mut screen);
    screen
}

fn capture(screen: &Screen) -> String {
    let bytes = screen.capture_transcript(
        ScreenCaptureRange {
            start_is_absolute: true,
            end_is_absolute: true,
            ..ScreenCaptureRange::default()
        },
        GridRenderOptions::default(),
    );
    String::from_utf8(bytes).expect("utf-8 capture")
}

/// tmux reports `cursor_x == sx` while a wrap is pending; RMUX stores the
/// last column and the pending flag.
fn expected_cursor(tmux_cursor: (u32, u32)) -> ((u32, u32), bool) {
    let sx = u32::from(SIZE.cols);
    let (x, y) = tmux_cursor;
    if x >= sx {
        ((sx - 1, y), true)
    } else {
        ((x, y), false)
    }
}

fn check(cases: &[Case]) {
    let mut failures = Vec::new();
    for case in cases {
        let screen = run(case);
        let actual = (
            capture(&screen),
            screen.cursor_position(),
            screen.pending_wrap(),
        );
        let (cursor, pending) = expected_cursor(case.tmux_cursor);
        let expected = (case.capture.to_owned(), cursor, pending);
        if actual != expected {
            failures.push(format!(
                "{}: expected {expected:?}, got {actual:?}",
                case.name
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn erase_and_edits_with_a_pending_wrap_match_tmux() {
    check(PENDING_WRAP_CASES);
}

#[test]
fn erase_and_edits_without_a_pending_wrap_keep_editing_the_cursor_cell() {
    check(NO_PENDING_WRAP_CASES);
}

#[test]
fn erase_with_a_pending_wrap_does_not_paint_the_last_column_background() {
    for (name, input) in [
        ("el", "\x1b[2;10He\x1b[44m\x1b[K"),
        ("ech", "\x1b[2;10He\x1b[44m\x1b[X"),
        ("ed", "\x1b[2;10He\x1b[44m\x1b[J"),
    ] {
        let mut screen = Screen::new(SIZE, 100);
        InputParser::new().parse(input.as_bytes(), &mut screen);
        let cell = screen
            .grid()
            .visible_line(1)
            .and_then(|line| line.cell(9))
            .expect("last cell of row 2");
        assert_eq!(cell.bg(), crate::input::COLOUR_DEFAULT, "{name}");
        assert!(capture(&screen).starts_with("\n         e\n"), "{name}");
        assert!(screen.pending_wrap(), "{name}");
    }
}
