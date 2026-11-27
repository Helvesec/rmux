//! `IL` (`CSI L`) and `DL` (`CSI M`) with the cursor outside the scroll region.
//!
//! tmux 3.7b then edits from the cursor row to the bottom of the screen
//! (`screen_write_insertline` and `screen_write_deleteline` call
//! `grid_view_insert_lines` and `grid_view_delete_lines`). Every expectation
//! below is the `capture-pane -p` output, `capture-pane -p -J` output and
//! `#{cursor_x},#{cursor_y}` measured on tmux 3.7b in a real 20x6 pane fed
//! with `stty -onlcr; cat` of the prefill and the sequence.

use super::Screen;
use crate::input::{Colour, InputParser, COLOUR_DEFAULT};
use rmux_proto::TerminalSize;

const SIZE: TerminalSize = TerminalSize { cols: 20, rows: 6 };
const PLAIN: &str = "L1\r\nL2\r\nL3\r\nL4\r\nL5\r\nL6";
/// Rows 4 and 5 hold one soft-wrapped line of 25 `a`.
const WRAPPED: &str = "L1\r\nL2\r\nL3\r\naaaaaaaaaaaaaaaaaaaaaaaaa\r\nL6";
const A20: &str = "aaaaaaaaaaaaaaaaaaaa";
/// `SGR 44`, the background the inserted and freed rows must take.
const BLUE: Colour = 4;

struct Case {
    name: &'static str,
    prefill: &'static str,
    input: &'static str,
    rows: [&'static str; 6],
    joined: &'static [&'static str],
    cursor: (u32, u32),
}

fn run(prefill: &str, input: &str) -> Screen {
    let mut screen = Screen::new(SIZE, 100);
    let mut parser = InputParser::new();
    parser.parse(prefill.as_bytes(), &mut screen);
    parser.parse(input.as_bytes(), &mut screen);
    screen
}

fn assert_case(case: &Case) {
    let screen = run(case.prefill, case.input);
    let name = case.name;
    assert_eq!(screen.capture_grid(false).lines, case.rows, "{name}: rows");
    assert_eq!(
        screen.capture_grid(true).lines,
        case.joined,
        "{name}: joined rows"
    );
    assert_eq!(screen.cursor_position(), case.cursor, "{name}: cursor");
    assert_eq!(screen.history_size(), 0, "{name}: history");
}

const MEASURED: &[Case] = &[
    // R3510 M3: below the region, IL inserts at the cursor row.
    Case {
        name: "IL below the region",
        prefill: PLAIN,
        input: "\x1b[1;3r\x1b[5;1H\x1b[L",
        rows: ["L1", "L2", "L3", "L4", "", "L5"],
        joined: &["L1", "L2", "L3", "L4", "", "L5"],
        cursor: (0, 4),
    },
    // R3510 M3: below the region, DL pulls the bottom rows up.
    Case {
        name: "DL below the region",
        prefill: PLAIN,
        input: "\x1b[1;3r\x1b[4;1H\x1b[M",
        rows: ["L1", "L2", "L3", "L5", "L6", ""],
        joined: &["L1", "L2", "L3", "L5", "L6", ""],
        cursor: (0, 3),
    },
    Case {
        name: "IL above the region",
        prefill: PLAIN,
        input: "\x1b[3;5r\x1b[1;1H\x1b[L",
        rows: ["", "L1", "L2", "L3", "L4", "L5"],
        joined: &["", "L1", "L2", "L3", "L4", "L5"],
        cursor: (0, 0),
    },
    Case {
        name: "DL above the region",
        prefill: PLAIN,
        input: "\x1b[3;5r\x1b[2;1H\x1b[M",
        rows: ["L1", "L3", "L4", "L5", "L6", ""],
        joined: &["L1", "L3", "L4", "L5", "L6", ""],
        cursor: (0, 1),
    },
    Case {
        name: "IL keeps the cursor column",
        prefill: PLAIN,
        input: "\x1b[1;3r\x1b[5;4H\x1b[L",
        rows: ["L1", "L2", "L3", "L4", "", "L5"],
        joined: &["L1", "L2", "L3", "L4", "", "L5"],
        cursor: (3, 4),
    },
    Case {
        name: "DL keeps the cursor column",
        prefill: PLAIN,
        input: "\x1b[1;3r\x1b[4;4H\x1b[M",
        rows: ["L1", "L2", "L3", "L5", "L6", ""],
        joined: &["L1", "L2", "L3", "L5", "L6", ""],
        cursor: (3, 3),
    },
    Case {
        name: "IL count of half the rows left",
        prefill: PLAIN,
        input: "\x1b[2;3r\x1b[5;1H\x1b[1L",
        rows: ["L1", "L2", "L3", "L4", "", "L5"],
        joined: &["L1", "L2", "L3", "L4", "", "L5"],
        cursor: (0, 4),
    },
    Case {
        name: "DL count of two",
        prefill: PLAIN,
        input: "\x1b[1;3r\x1b[4;1H\x1b[2M",
        rows: ["L1", "L2", "L3", "L6", "", ""],
        joined: &["L1", "L2", "L3", "L6", "", ""],
        cursor: (0, 3),
    },
    Case {
        name: "DL count beyond the rows left",
        prefill: PLAIN,
        input: "\x1b[1;3r\x1b[4;1H\x1b[9M",
        rows: ["L1", "L2", "L3", "", "", ""],
        joined: &["L1", "L2", "L3", "", "", ""],
        cursor: (0, 3),
    },
    Case {
        name: "DL then a line feed on the last row does not scroll",
        prefill: PLAIN,
        input: "\x1b[1;3r\x1b[4;1H\x1b[M\x1b[6;1H\nZ",
        rows: ["L1", "L2", "L3", "L5", "L6", "Z"],
        joined: &["L1", "L2", "L3", "L5", "L6", "Z"],
        cursor: (1, 5),
    },
    // tmux clears WRAPPED on the row above the move destination before it
    // moves the rows, so inserting under a wrapped head splits that line.
    Case {
        name: "IL under a wrapped head",
        prefill: WRAPPED,
        input: "\x1b[1;3r\x1b[5;1H\x1b[L",
        rows: ["L1", "L2", "L3", A20, "", "aaaaa"],
        joined: &["L1", "L2", "L3", A20, "", "aaaaa"],
        cursor: (0, 4),
    },
    Case {
        name: "IL at a wrapped head",
        prefill: WRAPPED,
        input: "\x1b[1;3r\x1b[4;1H\x1b[L",
        rows: ["L1", "L2", "L3", "", A20, "aaaaa"],
        joined: &["L1", "L2", "L3", "", A20, "aaaaa"],
        cursor: (0, 3),
    },
    Case {
        name: "DL of a wrapped tail",
        prefill: WRAPPED,
        input: "\x1b[1;3r\x1b[5;1H\x1b[M",
        rows: ["L1", "L2", "L3", A20, "L6", ""],
        joined: &["L1", "L2", "L3", A20, "L6", ""],
        cursor: (0, 4),
    },
    // Neighbours: inside the region, both stop at the bottom margin.
    Case {
        name: "IL inside the region",
        prefill: PLAIN,
        input: "\x1b[2;4r\x1b[3;1H\x1b[L",
        rows: ["L1", "L2", "", "L3", "L5", "L6"],
        joined: &["L1", "L2", "", "L3", "L5", "L6"],
        cursor: (0, 2),
    },
    Case {
        name: "DL inside the region",
        prefill: PLAIN,
        input: "\x1b[2;4r\x1b[3;1H\x1b[M",
        rows: ["L1", "L2", "L4", "", "L5", "L6"],
        joined: &["L1", "L2", "L4", "", "L5", "L6"],
        cursor: (0, 2),
    },
];

#[test]
fn insert_and_delete_line_outside_region_match_tmux() {
    for case in MEASURED {
        assert_case(case);
    }
}

/// tmux 3.7b's `grid_view_insert_lines` only moves the rows that survive.
/// When the count exceeds them, the rows they vacate are never cleared:
/// measured, `CSI 4;1H CSI 2L` leaves `L1 L2 L3 <blank> L5 L4`, and a count
/// reaching the bottom (`CSI 5;1H CSI 2L` or `CSI 9L`) changes nothing.
/// RMUX deliberately clears every inserted row, as its own `DL` and tmux's
/// in-region `IL` do.
#[test]
fn insert_line_outside_region_clears_every_inserted_row_product_divergence() {
    let cases: &[(&str, &str, [&str; 6])] = &[
        (
            "count above half the rows left",
            "\x1b[1;3r\x1b[4;1H\x1b[2L",
            ["L1", "L2", "L3", "", "", "L4"],
        ),
        (
            "count equal to the rows left",
            "\x1b[1;3r\x1b[5;1H\x1b[2L",
            ["L1", "L2", "L3", "L4", "", ""],
        ),
        (
            "count beyond the rows left",
            "\x1b[1;3r\x1b[5;1H\x1b[9L",
            ["L1", "L2", "L3", "L4", "", ""],
        ),
    ];
    for &(name, input, rows) in cases {
        let screen = run(PLAIN, input);
        assert_eq!(screen.capture_grid(false).lines, rows, "{name}");
        assert_eq!(screen.history_size(), 0, "{name}: history");
    }
}

fn row_backgrounds(screen: &Screen, y: u32) -> Vec<Colour> {
    let line = screen.grid().visible_line(y).expect("visible row");
    (0..u32::from(SIZE.cols))
        .map(|x| line.cell(x).unwrap_or_default().bg())
        .collect()
}

/// Measured on tmux 3.7b with one `capture-pane -p -e -S y -E y` per row:
/// the inserted rows and the freed bottom rows carry `SGR 44`, the moved
/// rows keep the default background.
#[test]
fn insert_and_delete_line_outside_region_paint_the_active_background() {
    let cases: &[(&str, &str, &[u32], [&str; 6])] = &[
        (
            "IL",
            "\x1b[1;2r\x1b[3;1H\x1b[44m\x1b[2L\x1b[0m",
            &[2, 3],
            ["L1", "L2", "", "", "L3", "L4"],
        ),
        (
            "DL",
            "\x1b[1;3r\x1b[4;1H\x1b[44m\x1b[2M\x1b[0m",
            &[4, 5],
            ["L1", "L2", "L3", "L6", "", ""],
        ),
    ];
    for &(name, input, painted, rows) in cases {
        let screen = run(PLAIN, input);
        assert_eq!(screen.capture_grid(false).lines, rows, "{name}");
        for y in 0..u32::from(SIZE.rows) {
            let expected = if painted.contains(&y) {
                BLUE
            } else {
                COLOUR_DEFAULT
            };
            assert!(
                row_backgrounds(&screen, y).iter().all(|&bg| bg == expected),
                "{name}: row {y} background"
            );
        }
    }
}
