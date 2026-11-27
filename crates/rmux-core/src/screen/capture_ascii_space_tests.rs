use super::Screen;
use crate::grid::{GridLine, GridLineFlags, GridStringState};
use crate::input::{InputParser, COLOUR_DEFAULT};
use crate::{GridRenderOptions, ScreenCaptureRange};
use rmux_proto::TerminalSize;

fn parse(screen: &mut Screen, bytes: &[u8]) {
    let mut parser = InputParser::new();
    parser.parse(bytes, screen);
}

fn screen(cols: u16, rows: u16, history: usize, payload: &[u8]) -> Screen {
    let mut screen = Screen::new(TerminalSize { cols, rows }, history);
    parse(&mut screen, payload);
    screen
}

fn full_range() -> ScreenCaptureRange {
    ScreenCaptureRange {
        start_is_absolute: true,
        end_is_absolute: true,
        ..ScreenCaptureRange::default()
    }
}

fn raw_capture(screen: &Screen) -> Vec<u8> {
    screen.capture_transcript(
        full_range(),
        GridRenderOptions {
            join_wrapped: false,
            include_empty_cells: true,
            trim_spaces: true,
            ..GridRenderOptions::default()
        },
    )
}

fn joined_capture(screen: &Screen) -> Vec<u8> {
    screen.capture_transcript(
        full_range(),
        GridRenderOptions {
            join_wrapped: true,
            include_empty_cells: false,
            trim_spaces: false,
            ..GridRenderOptions::default()
        },
    )
}

#[test]
fn joined_capture_restores_compact_ascii_boundary_spaces() {
    let cases = [
        (
            "one-space-width-8",
            8,
            4,
            20,
            b"abc def gh".as_slice(),
            b"abc def\ngh\n\n\n".as_slice(),
            b"abc def gh\n\n\n".as_slice(),
        ),
        (
            "two-spaces-width-12",
            12,
            4,
            20,
            b"alpha beta  42".as_slice(),
            b"alpha beta\n42\n\n\n".as_slice(),
            b"alpha beta  42\n\n\n".as_slice(),
        ),
        (
            "one-space-width-20",
            20,
            4,
            20,
            b"hello world foo bar baz".as_slice(),
            b"hello world foo bar\nbaz\n\n\n".as_slice(),
            b"hello world foo bar baz\n\n\n".as_slice(),
        ),
        (
            "utf8-suffix",
            8,
            4,
            20,
            "abc def 界".as_bytes(),
            "abc def\n界\n\n\n".as_bytes(),
            "abc def 界\n\n\n".as_bytes(),
        ),
        (
            "viewport-without-history",
            8,
            4,
            0,
            b"abc def gh".as_slice(),
            b"abc def\ngh\n\n\n".as_slice(),
            b"abc def gh\n\n\n".as_slice(),
        ),
    ];

    for (label, cols, rows, history, payload, expected_raw, expected_joined) in cases {
        let screen = screen(cols, rows, history, payload);
        assert_eq!(raw_capture(&screen), expected_raw, "{label}: raw capture");
        assert_eq!(
            joined_capture(&screen),
            expected_joined,
            "{label}: first joined capture"
        );
        assert_eq!(
            joined_capture(&screen),
            expected_joined,
            "{label}: repeated joined capture"
        );
        let expected_grid = std::str::from_utf8(expected_joined)
            .expect("oracle capture is UTF-8")
            .lines()
            .collect::<Vec<_>>();
        assert_eq!(
            screen.capture_grid(true).lines,
            expected_grid,
            "{label}: joined grid capture"
        );
    }
}

#[test]
fn joined_capture_restores_compact_ascii_space_in_scrollback() {
    let screen = screen(8, 3, 20, b"abc def gh\r\nHARDONE\r\nHARDTWO\r\nTAIL");
    assert!(screen.history_size() > 0, "fixture must reach scrollback");
    assert_eq!(
        raw_capture(&screen),
        b"abc def\ngh\nHARDONE\nHARDTWO\nTAIL\n"
    );
    assert_eq!(
        joined_capture(&screen),
        b"abc def gh\nHARDONE\nHARDTWO\nTAIL\n"
    );
}

#[test]
fn joined_capture_restores_repeated_compact_ascii_boundaries() {
    let screen = screen(8, 5, 20, b"abc def gh\r\nabc def gh");
    assert_eq!(raw_capture(&screen), b"abc def\ngh\nabc def\ngh\n\n");
    assert_eq!(joined_capture(&screen), b"abc def gh\nabc def gh\n\n");
}

#[test]
fn joined_capture_keeps_exact_utf8_and_sgr_sentinels() {
    let cases = [
        (
            "exact-wrap",
            8,
            b"ABCDEFGHIJKLMNOP".as_slice(),
            b"ABCDEFGH\nIJKLMNOP\n\n\n".as_slice(),
            b"ABCDEFGHIJKLMNOP\n\n\n".as_slice(),
        ),
        (
            "utf8-line",
            12,
            "été café ab çà".as_bytes(),
            "été café ab\nçà\n\n\n".as_bytes(),
            "été café ab çà\n\n\n".as_bytes(),
        ),
        (
            "sgr-line",
            8,
            b"\x1b[31mabc def \x1b[0mgh".as_slice(),
            b"abc def\ngh\n\n\n".as_slice(),
            b"abc def gh\n\n\n".as_slice(),
        ),
    ];

    for (label, cols, payload, expected_raw, expected_joined) in cases {
        let screen = screen(cols, 4, 20, payload);
        assert_eq!(raw_capture(&screen), expected_raw, "{label}: raw capture");
        assert_eq!(
            joined_capture(&screen),
            expected_joined,
            "{label}: joined capture"
        );
    }
}

#[test]
fn joined_capture_preserves_explicit_hard_terminal_and_blank_suffix_spaces() {
    // Frozen tmux 3.7b, measured 2026-07-27 in an 8x4 pane: ordinary capture
    // trims these suffixes, while -J preserves exactly the cells written by
    // the application without extending them through implicit blank padding.
    let cases = [
        (
            "hard-one",
            b"abc \r\nnext".as_slice(),
            b"abc\nnext\n\n\n".as_slice(),
            b"abc \nnext\n\n\n".as_slice(),
        ),
        (
            "hard-three",
            b"abc   \r\nnext".as_slice(),
            b"abc\nnext\n\n\n".as_slice(),
            b"abc   \nnext\n\n\n".as_slice(),
        ),
        (
            "terminal-one",
            b"abc ".as_slice(),
            b"abc\n\n\n\n".as_slice(),
            b"abc \n\n\n\n".as_slice(),
        ),
        (
            "terminal-three",
            b"abc   ".as_slice(),
            b"abc\n\n\n\n".as_slice(),
            b"abc   \n\n\n\n".as_slice(),
        ),
        (
            "terminal-full-width",
            b"abc def ".as_slice(),
            b"abc def\n\n\n\n".as_slice(),
            b"abc def \n\n\n\n".as_slice(),
        ),
        (
            "blank-two",
            b"  \r\nnext".as_slice(),
            b"\nnext\n\n\n".as_slice(),
            b"  \nnext\n\n\n".as_slice(),
        ),
        (
            "blank-only-three",
            b"   ".as_slice(),
            b"\n\n\n\n".as_slice(),
            b"   \n\n\n\n".as_slice(),
        ),
        (
            "wrapped-suffix-three",
            b"abc def gh   ".as_slice(),
            b"abc def\ngh\n\n\n".as_slice(),
            b"abc def gh   \n\n\n".as_slice(),
        ),
        (
            "repeated-hard-one",
            b"abc \r\nabc \r\nabc ".as_slice(),
            b"abc\nabc\nabc\n\n".as_slice(),
            b"abc \nabc \nabc \n\n".as_slice(),
        ),
    ];

    for (label, payload, expected_raw, expected_joined) in cases {
        let screen = screen(8, 4, 20, payload);
        assert_eq!(raw_capture(&screen), expected_raw, "{label}: raw capture");
        assert_eq!(
            joined_capture(&screen),
            expected_joined,
            "{label}: joined capture"
        );
        assert_eq!(
            joined_capture(&screen),
            expected_joined,
            "{label}: repeated joined capture"
        );
    }
}

#[test]
fn compact_ascii_boundary_space_respects_include_empty_cells() {
    let screen = screen(8, 4, 20, b"abc def gh");
    let line = screen.grid().visible_line(0).expect("first line exists");
    assert_eq!(line.plain_text(), Some("abc def"));
    assert!(line.cells().is_empty(), "fixture must use compact storage");
    assert!(line.flags().contains(GridLineFlags::WRAPPED));

    let render = |options| {
        screen
            .grid()
            .render_absolute_line(0, options, &mut GridStringState::default(), None)
            .expect("first line renders")
    };
    assert_eq!(
        render(GridRenderOptions {
            join_wrapped: true,
            include_empty_cells: false,
            trim_spaces: false,
            ..GridRenderOptions::default()
        }),
        "abc def "
    );
    assert_eq!(
        render(GridRenderOptions {
            join_wrapped: false,
            include_empty_cells: false,
            trim_spaces: false,
            ..GridRenderOptions::default()
        }),
        "abc def"
    );
    assert_eq!(
        render(GridRenderOptions {
            join_wrapped: false,
            include_empty_cells: true,
            trim_spaces: false,
            ..GridRenderOptions::default()
        }),
        "abc def "
    );
    assert_eq!(
        render(GridRenderOptions {
            join_wrapped: false,
            include_empty_cells: true,
            trim_spaces: true,
            ..GridRenderOptions::default()
        }),
        "abc def"
    );
}

#[test]
fn compact_ascii_grid_capture_joins_boundary_space() {
    let screen = screen(8, 4, 20, b"abc def gh");
    assert_eq!(screen.capture_grid(false).lines[..2], ["abc def", "gh"]);
    assert_eq!(screen.capture_grid(true).lines[0], "abc def gh");
}

#[test]
fn compact_ascii_data_end_survives_materialization_without_absorbing_padding() {
    let mut line = GridLine::new(8);
    assert!(line.write_plain_ascii_run(0, b"A  "));
    assert_eq!(line.plain_text(), Some("A"));
    assert!(!line.cell(1).expect("first data space").is_blank());
    assert!(!line.cell(2).expect("second data space").is_blank());
    assert!(line.cell(3).expect("cleared suffix").is_blank());

    line.materialize_for_cell_mutation();
    assert!(line.plain_text().is_none());
    assert!(!line.cell(1).expect("materialized data space").is_blank());
    assert!(!line.cell(2).expect("materialized data space").is_blank());
    assert!(line
        .cell(3)
        .expect("materialized cleared suffix")
        .is_blank());

    line.clear(COLOUR_DEFAULT);
    assert_eq!(line.plain_text(), Some(""));
    assert!(line.cell(0).expect("cleared cell").is_blank());
}

#[test]
fn wide_prewrap_preserves_cached_explicit_spaces_across_cursor_paths_and_reflow() {
    // tmux 3.7b, measured 2026-09-05: the explicit last-column space remains
    // part of the joined text through 80 -> 40 -> 80, including an all-space row.
    for position in ["\x1b[80G", "\x1b[1;80H", "\r\x1b[79C"] {
        for prefix in [String::new(), "abc".to_owned(), "a".repeat(79)] {
            let mut screen = screen(80, 24, 1_000, b"");
            let mut parser = InputParser::new();
            parser.parse(format!("{prefix}{position} ").as_bytes(), &mut screen);
            let line = screen.grid().visible_line(0).unwrap();
            assert_eq!(line.plain_text(), Some(prefix.as_str()));
            assert!(!line.cell(79).unwrap().is_blank());

            // The empty-prefix CHA case is exactly ESC[80G SP ESC[80G 표.
            // It asserted in mark_reflow_gap before the mutation guard fix.
            parser.parse(format!("{position}표").as_bytes(), &mut screen);
            let line = screen.grid().visible_line(0).unwrap();
            assert!(line.flags().contains(GridLineFlags::WRAPPED));
            assert!(!line.cell(79).unwrap().is_reflow_gap());
            assert!(!line.cell(79).unwrap().is_blank());
            assert_eq!(screen.cursor_position(), (2, 1));
            assert!(screen.grid().visible_line(1).unwrap().is_padding_cell(1));

            let padded = format!("{prefix:<80}");
            let logical = format!("{padded}표");
            let joined = format!("{logical}{}", "\n".repeat(23));
            for cols in [80, 40, 80] {
                if screen.size().cols != cols {
                    screen.resize(TerminalSize { cols, rows: 24 });
                }
                let mut raw = String::new();
                for chunk in padded.as_bytes().chunks(usize::from(cols)) {
                    raw.push_str(std::str::from_utf8(chunk).unwrap().trim_end_matches(' '));
                    raw.push('\n');
                }
                raw.push('표');
                raw.push_str(&"\n".repeat(23));
                assert_eq!(raw_capture(&screen), raw.as_bytes(), "{position:?}, {cols}");
                assert_eq!(
                    joined_capture(&screen),
                    joined.as_bytes(),
                    "{position:?}, {cols}"
                );
                assert_eq!(screen.capture_grid(true).lines[0], logical);
            }
        }
    }
}

#[test]
fn wide_prewrap_distinguishes_unused_suffix_and_ordinary_overwrite() {
    for (payload, gap, cursor, wrapped) in [
        ("\x1b[80G표", 1, (2, 1), true),
        (" \x1b[80G표", 1, (2, 1), true),
        ("\x1b[80Gx\x1b[80G표", 0, (2, 1), true),
        ("\x1b[80G \x1b[80GA", 0, (79, 0), false),
    ] {
        let screen = screen(80, 24, 1_000, payload.as_bytes());
        assert_eq!(screen.cursor_position(), cursor, "{payload:?}");
        assert_eq!(
            screen.grid().absolute_line_trailing_reflow_gap(0),
            Some(gap)
        );
        assert_eq!(
            screen
                .grid()
                .visible_line(0)
                .unwrap()
                .flags()
                .contains(GridLineFlags::WRAPPED),
            wrapped
        );
        let last = screen.grid().visible_line(0).unwrap().cell(79).unwrap();
        assert_eq!(last.is_blank(), gap == 1);
        if !wrapped {
            assert_eq!(last.text(), "A");
        }
    }
}

#[test]
fn wide_prewrap_line_edits_restore_only_existing_trailing_gaps() {
    // tmux 3.7b, measured 2026-09-05 in an 8x4 pane. DCH/ICH retain an
    // existing final placement gap only on the applicable mutation paths;
    // an explicit space must never acquire that metadata.
    for (edit, raw_prefix, gap) in [
        ("\x1b[1;8H\x1b[@", "abcdefg", 1),
        ("\x1b[1;1H\x1b[P", "bcdefg", 1),
        ("\x1b[1;8H\x1b[P", "abcdefg", 1),
        ("\x1b[1;1H\x1b[@", " abcdefg", 0),
    ] {
        for explicit in [false, true] {
            let suffix = if explicit { " \x1b[8G" } else { "" };
            let payload = format!("abcdefg{suffix}표{edit}");
            let screen = screen(8, 4, 20, payload.as_bytes());
            assert_eq!(
                raw_capture(&screen),
                format!("{raw_prefix}\n표\n\n\n").as_bytes()
            );
            assert_eq!(
                screen.grid().absolute_line_trailing_reflow_gap(0),
                Some(if explicit { 0 } else { gap }),
                "{edit:?}, explicit={explicit}"
            );
            let line = screen.grid().visible_line(0).unwrap();
            for cell in line.cells().iter().filter(|cell| cell.is_reflow_gap()) {
                assert!(cell.is_blank());
            }
        }
    }
}
