use super::{Screen, MAX_TERMINAL_PASSTHROUGH_EVENTS, TITLE_STACK_MAX};
use crate::input::InputParser;
use crate::terminal_passthrough::MAX_TERMINAL_PASSTHROUGH_PAYLOAD_BYTES;
use crate::{GridRenderOptions, OptionStore, ScreenCaptureRange, Utf8Config, COLOUR_DEFAULT};
use rmux_proto::{OptionName, ScopeSelector, SetOptionMode, TerminalSize};

fn parse(screen: &mut Screen, bytes: &[u8]) {
    let mut parser = InputParser::new();
    parser.parse(bytes, screen);
}

fn new_screen(cols: u16, rows: u16, history: usize) -> Screen {
    Screen::new(TerminalSize { cols, rows }, history)
}

fn reflow_overflow_screen(history_limit: usize) -> Screen {
    let mut screen = new_screen(20, 1, history_limit);
    parse(&mut screen, b"abcdefghijklmnopqrst");
    screen.resize(TerminalSize { cols: 2, rows: 1 });
    screen
}

#[test]
fn visit_visible_line_cells_returns_exact_padded_row() {
    let mut screen = new_screen(4, 1, 10);
    parse(&mut screen, b"ab");

    let mut cells = Vec::new();
    assert!(screen.visit_visible_line_cells(0, 4, |cell| {
        cells.push((cell.text().to_owned(), cell.width(), cell.is_padding()));
    }));

    assert_eq!(
        cells,
        vec![
            ("a".to_owned(), 1, false),
            ("b".to_owned(), 1, false),
            (" ".to_owned(), 1, false),
            (" ".to_owned(), 1, false),
        ],
    );
    assert!(!screen.visit_visible_line_cells(1, 4, |_| {}));
}

#[test]
fn visit_visible_line_cells_preserves_wide_padding_metadata() {
    let mut screen = new_screen(4, 1, 10);
    parse(&mut screen, "表x".as_bytes());

    let mut cells = Vec::new();
    assert!(screen.visit_visible_line_cells(0, 4, |cell| {
        cells.push((cell.text().to_owned(), cell.width(), cell.is_padding()));
    }));

    assert_eq!(cells[0], ("表".to_owned(), 2, false));
    assert_eq!(cells[1], (" ".to_owned(), 0, true));
    assert_eq!(cells[2], ("x".to_owned(), 1, false));
}

#[test]
fn selected_cell_tracking_is_updated_when_selection_is_marked() {
    let mut screen = new_screen(10, 2, 10);
    assert!(!screen.has_selected_cells());

    let before = screen
        .visible_line_revision(0)
        .expect("visible row revision");
    screen.mark_selected_row_range(0, 2, 4);

    assert!(screen.has_selected_cells());
    assert_ne!(
        screen
            .visible_line_revision(0)
            .expect("visible row revision"),
        before,
        "selection paint must invalidate delta-render caches"
    );
}

#[test]
fn selected_cell_tracking_is_cleared_when_selection_is_removed() {
    let mut screen = new_screen(10, 2, 10);
    screen.mark_selected_row_range(0, 2, 4);
    let before = screen
        .visible_line_revision(0)
        .expect("visible row revision");

    screen.clear_selected_cells();

    assert!(!screen.has_selected_cells());
    assert_ne!(
        screen
            .visible_line_revision(0)
            .expect("visible row revision"),
        before,
        "selection clear must invalidate delta-render caches"
    );
}

#[test]
fn selection_style_overlay_composes_like_tmux_screen_select_cell() {
    use crate::input::GridAttr;

    // Cell with its own colours and bold, as a shell prompt would have.
    let mut screen = new_screen(10, 1, 10);
    parse(&mut screen, b"\x1b[1;32mP\x1b[0m");
    screen.mark_selected_row_range(0, 0, 0);

    // Style with explicit fg/bg and no attributes (the default mode-style
    // shape): fg/bg replace the cell's, and the cell's bold is dropped
    // (tmux screen_select_cell keeps only charset when the style has no
    // attributes).
    screen.overlay_style_on_selected("fg=black,bg=yellow");
    let cell = screen
        .grid()
        .visible_line(0)
        .and_then(|line| line.cell(0))
        .expect("cell 0");
    assert_eq!(cell.fg(), 0, "selection fg replaces the cell fg");
    assert_eq!(cell.bg(), 3, "selection bg replaces the cell bg");
    assert_eq!(
        cell.attr() & GridAttr::BRIGHT,
        0,
        "cell attributes are dropped when the selection style sets none"
    );

    // A complete style with its own attributes still applies them while
    // dropping the cell's (oracle probe 2026-07-11: bg=red,fg=white,bold
    // over plain text paints 37;41;1).
    let mut screen = new_screen(10, 1, 10);
    parse(&mut screen, b"\x1b[32mP\x1b[0m");
    screen.mark_selected_row_range(0, 0, 0);
    screen.overlay_style_on_selected("fg=white,bg=red,bold");
    let cell = screen
        .grid()
        .visible_line(0)
        .and_then(|line| line.cell(0))
        .expect("cell 0");
    assert_ne!(
        cell.attr() & GridAttr::BRIGHT,
        0,
        "a complete style's own attributes apply to selected cells"
    );

    // Style leaving fg at default: the cell keeps its own fg, and a style
    // attribute is unioned with the cell's remaining attributes.
    let mut screen = new_screen(10, 1, 10);
    parse(&mut screen, b"\x1b[1;32mP\x1b[0m");
    screen.mark_selected_row_range(0, 0, 0);
    screen.overlay_style_on_selected("bg=red,underscore");
    let cell = screen
        .grid()
        .visible_line(0)
        .and_then(|line| line.cell(0))
        .expect("cell 0");
    assert_eq!(cell.fg(), 2, "default selection fg inherits the cell fg");
    assert_eq!(cell.bg(), 1, "selection bg replaces the cell bg");
    assert_ne!(
        cell.attr() & GridAttr::UNDERSCORE,
        0,
        "selection style attributes apply"
    );
    assert_ne!(
        cell.attr() & GridAttr::BRIGHT,
        0,
        "cell attributes are unioned when the selection style sets some"
    );
}

#[test]
fn row_range_style_overlay_is_inclusive_and_paints_blank_cells() {
    let mut screen = new_screen(6, 1, 10);
    parse(&mut screen, b"ab");
    let style = crate::style::Style::parse("bg=cyan,fg=black").expect("valid overlay style");

    screen.overlay_style_on_row_range(0, 1, 5, &style);

    let mut colours = Vec::new();
    assert!(screen.visit_visible_line_cells(0, 6, |cell| {
        colours.push((cell.fg(), cell.bg()));
    }));
    assert_eq!(colours[0], (COLOUR_DEFAULT, COLOUR_DEFAULT));
    assert_eq!(&colours[1..], &[(0, 6); 5]);
}

#[test]
fn row_range_style_overlay_clamps_to_the_visible_grid() {
    let mut screen = new_screen(3, 1, 10);
    parse(&mut screen, b"abc");
    let style = crate::style::Style::parse("bg=red").expect("valid overlay style");

    screen.overlay_style_on_row_range(0, 2, u32::MAX, &style);

    let mut backgrounds = Vec::new();
    assert!(screen.visit_visible_line_cells(0, 3, |cell| {
        backgrounds.push(cell.bg());
    }));
    assert_eq!(backgrounds, vec![COLOUR_DEFAULT, COLOUR_DEFAULT, 1]);
}

#[test]
fn selection_style_overlay_consumes_selected_cell_markers() {
    let mut screen = new_screen(10, 2, 10);
    screen.mark_selected_row_range(0, 2, 4);

    screen.overlay_style_on_selected("bg=cyan,fg=black");

    assert!(!screen.has_selected_cells());
}

#[test]
fn selected_cell_tracking_is_cleared_when_terminal_writes_text() {
    let mut screen = new_screen(10, 2, 10);
    screen.mark_selected_row_range(0, 2, 4);

    parse(&mut screen, b"A");

    assert!(!screen.has_selected_cells());
}

#[test]
fn selected_cell_tracking_is_cleared_when_terminal_clears_screen() {
    let mut screen = new_screen(10, 2, 10);
    screen.mark_selected_row_range(0, 2, 4);

    parse(&mut screen, b"\x1b[2J");

    assert!(!screen.has_selected_cells());
}

#[test]
fn selected_cell_tracking_is_cleared_when_alternate_screen_changes() {
    let mut screen = new_screen(10, 2, 10);
    screen.mark_selected_row_range(0, 2, 4);

    parse(&mut screen, b"\x1b[?1049h");

    assert!(!screen.has_selected_cells());
}

#[test]
fn selected_cell_tracking_is_cleared_when_screen_resizes() {
    let mut screen = new_screen(10, 2, 10);
    screen.mark_selected_row_range(0, 2, 4);

    screen.resize(TerminalSize { cols: 12, rows: 2 });

    assert!(!screen.has_selected_cells());
}

#[test]
fn terminal_passthrough_drops_oversized_payloads() {
    let mut screen = new_screen(10, 2, 10);
    let payload = vec![b'A'; MAX_TERMINAL_PASSTHROUGH_PAYLOAD_BYTES + 1];

    screen.push_terminal_passthrough(crate::TerminalPassthrough::kitty_graphics(0, 0, payload));

    assert!(screen.take_terminal_passthrough().is_empty());
    assert_eq!(screen.take_terminal_passthrough_dropped_count(), 1);
    assert_eq!(screen.take_terminal_passthrough_dropped_count(), 0);
}

#[test]
fn terminal_passthrough_keeps_newest_events_when_queue_is_full() {
    let mut screen = new_screen(10, 2, 10);
    for index in 0..=MAX_TERMINAL_PASSTHROUGH_EVENTS {
        let payload = format!("Gf=100;{index}");
        screen.push_terminal_passthrough(crate::TerminalPassthrough::kitty_graphics(
            index as u32,
            0,
            payload.into_bytes(),
        ));
    }

    let passthroughs = screen.take_terminal_passthrough();

    assert_eq!(passthroughs.len(), MAX_TERMINAL_PASSTHROUGH_EVENTS);
    assert_eq!(screen.take_terminal_passthrough_dropped_count(), 1);
    assert_eq!(passthroughs[0].payload(), b"Gf=100;1");
    assert_eq!(
        passthroughs
            .last()
            .expect("newest passthrough is retained")
            .payload(),
        format!("Gf=100;{MAX_TERMINAL_PASSTHROUGH_EVENTS}").as_bytes()
    );
}

#[test]
fn osc4_palette_queries_are_bounded_split_and_canonicalized() {
    let mut screen = new_screen(10, 2, 10);
    let mut parser = InputParser::new();

    parser.parse(b"\x1b]4;0;?;1;rgb:ffff/0000/0000;255;?", &mut screen);
    assert!(screen.take_terminal_passthrough().is_empty());
    parser.parse(b"\x07", &mut screen);

    let queries = screen.take_terminal_passthrough();
    assert_eq!(queries.len(), 2);
    assert_eq!(queries[0].render_sequence(), b"\x1b]4;0;?\x1b\\");
    assert_eq!(queries[1].render_sequence(), b"\x1b]4;255;?\x1b\\");
    assert_eq!(
        queries[0].palette_query_index(),
        Some(crate::TerminalPaletteIndex::from(0))
    );
    assert_eq!(
        queries[1].palette_query_index(),
        Some(crate::TerminalPaletteIndex::from(255))
    );

    parser.parse(
        b"\x1b]4;256;?;not-an-index;?;2;rgb:0000/0000/0000\x1b\\",
        &mut screen,
    );
    assert!(
        screen.take_terminal_passthrough().is_empty(),
        "invalid indices and palette sets are never reflected to the outer terminal"
    );
}

#[test]
fn osc4_bel_and_st_queries_survive_every_fragmentation_boundary() {
    for sequence in [b"\x1b]4;7;?\x07".as_slice(), b"\x1b]4;7;?\x1b\\".as_slice()] {
        for split in 0..=sequence.len() {
            let mut screen = new_screen(10, 2, 10);
            let mut parser = InputParser::new();

            parser.parse(&sequence[..split], &mut screen);
            // The parser completes ST at its ESC introducer, before the
            // trailing backslash arrives. Preserve any event emitted there
            // and ensure the continuation neither loses nor duplicates it.
            let mut queries = screen.take_terminal_passthrough();
            parser.parse(&sequence[split..], &mut screen);

            queries.extend(screen.take_terminal_passthrough());
            assert_eq!(queries.len(), 1, "split at {split}");
            assert_eq!(queries[0].render_sequence(), b"\x1b]4;7;?\x1b\\");
        }
    }
}

#[test]
fn title_stack_keeps_newest_entries_when_full() {
    let mut screen = new_screen(10, 2, 10);

    for index in 0..(TITLE_STACK_MAX + 5) {
        screen.set_title(format!("title-{index}"));
        <Screen as crate::input::ScreenWriter>::push_title(&mut screen);
    }

    assert_eq!(screen.title_stack.len(), TITLE_STACK_MAX);
    assert_eq!(
        screen.title_stack.first().map(String::as_str),
        Some("title-5")
    );
    assert_eq!(
        screen.title_stack.last().map(String::as_str),
        Some("title-104")
    );
}

fn utf8_config(codepoint_widths: &[&str], vs16_wide: bool) -> Utf8Config {
    let mut options = OptionStore::new();
    for entry in codepoint_widths {
        options
            .set(
                ScopeSelector::Global,
                OptionName::CodepointWidths,
                (*entry).to_owned(),
                SetOptionMode::Append,
            )
            .expect("codepoint-widths append succeeds");
    }
    options
        .set(
            ScopeSelector::Global,
            OptionName::VariationSelectorAlwaysWide,
            if vs16_wide { "on" } else { "off" }.to_owned(),
            SetOptionMode::Replace,
        )
        .expect("variation-selector-always-wide set succeeds");
    Utf8Config::from_options(&options)
}

fn full_range() -> ScreenCaptureRange {
    ScreenCaptureRange {
        start_is_absolute: true,
        end_is_absolute: true,
        ..ScreenCaptureRange::default()
    }
}

#[test]
fn trim_below_cursor_truncates_transcript_and_pulls_history_into_view() {
    let mut screen = new_screen(10, 5, 20);
    parse(
        &mut screen,
        b"01\r\n02\r\n03\r\n04\r\n05\r\n06\r\n07\r\n08\r\n09\r\n10\x1b[3;1H",
    );

    assert_eq!(screen.cursor_position(), (0, 2));
    assert_eq!(screen.history_size(), 5);

    assert!(screen.trim_below_cursor());

    let output = screen.capture_transcript(full_range(), GridRenderOptions::default());
    let output = String::from_utf8(output).expect("screen text is utf-8");
    assert_eq!(
        output.lines().collect::<Vec<_>>(),
        vec!["01", "02", "03", "04", "05", "06", "07", "08"]
    );
    assert_eq!(screen.cursor_position(), (0, 4));
    assert_eq!(screen.history_size(), 3);
}

#[test]
fn wrapped_line_sets_wrapped_flag() {
    let mut screen = new_screen(3, 2, 10);
    parse(&mut screen, b"abcdef");

    assert!(screen
        .grid()
        .visible_line(0)
        .expect("first visible line")
        .flags()
        .contains(crate::grid::GridLineFlags::WRAPPED));
    assert_eq!(screen.capture_grid(false).lines, vec!["abc", "def"]);
}

#[test]
fn wrapped_ascii_history_uses_plain_text_storage() {
    let mut screen = new_screen(4, 2, 10);
    parse(&mut screen, b"abcdefghijklmnop");

    let history = screen.grid().absolute_line(0).expect("history line exists");
    assert!(history
        .flags()
        .contains(crate::grid::GridLineFlags::WRAPPED));
    assert_eq!(history.plain_text(), Some("abcd"));
    assert_eq!(history.cells().len(), 0);
}

#[test]
fn width_resize_clears_wrapped_flags() {
    let mut screen = new_screen(3, 2, 10);
    parse(&mut screen, b"abcdef");

    screen.resize(TerminalSize { cols: 6, rows: 2 });

    assert!(!screen
        .grid()
        .visible_line(0)
        .expect("first visible line")
        .flags()
        .contains(crate::grid::GridLineFlags::WRAPPED));
}

#[test]
fn width_resize_short_unwrapped_lines_keeps_line_count() {
    let mut screen = new_screen(10, 2, 10);
    parse(&mut screen, b"abc\r\ndef");
    let history_size = screen.history_size();

    screen.resize(TerminalSize { cols: 6, rows: 2 });

    assert_eq!(screen.history_size(), history_size);
    assert_eq!(screen.capture_grid(false).lines, vec!["abc", "def"]);
}

#[test]
fn width_resize_reflows_wrapped_lines_instead_of_truncating() {
    let mut screen = new_screen(5, 16, 10);
    parse(&mut screen, b"PANE1-ABCDE");

    screen.resize(TerminalSize { cols: 1, rows: 16 });

    let capture = screen.capture_transcript(full_range(), GridRenderOptions::default());
    let rendered = String::from_utf8(capture).expect("capture must be UTF-8");
    let lines = rendered.lines().collect::<Vec<_>>();
    assert_eq!(
        &lines[..11],
        &["P", "A", "N", "E", "1", "-", "A", "B", "C", "D", "E"]
    );
}

#[test]
fn width_resize_reflows_plain_ascii_without_materializing_cells() {
    let mut screen = new_screen(4, 3, 20);
    parse(&mut screen, b"abcdefghijkl");

    screen.resize(TerminalSize { cols: 3, rows: 4 });

    let total_lines = screen.history_size() + usize::from(screen.size().rows);
    let compact_lines = (0..total_lines)
        .filter_map(|absolute_y| screen.grid().absolute_line(absolute_y))
        .filter_map(|line| line.plain_text().map(|text| (line, text)))
        .filter(|(_, text)| !text.is_empty())
        .collect::<Vec<_>>();
    assert_eq!(
        compact_lines
            .iter()
            .map(|(_, text)| *text)
            .collect::<Vec<_>>(),
        vec!["abc", "def", "ghi", "jkl"]
    );
    for (line, _) in compact_lines {
        assert_eq!(line.cells().len(), 0);
    }
}

#[test]
fn width_resize_mixed_style_wide_and_wrapped_text_preserves_transcript() {
    let mut screen = new_screen(6, 4, 20);
    parse(
        &mut screen,
        "\x1b[31mred\x1b[0m-表x-abcdef\r\nplain-wrap-line".as_bytes(),
    );

    screen.resize(TerminalSize { cols: 4, rows: 8 });
    assert_no_wide_cell_fragments(&screen);
    screen.resize(TerminalSize { cols: 10, rows: 8 });
    assert_no_wide_cell_fragments(&screen);

    let capture = screen.capture_transcript(
        full_range(),
        GridRenderOptions {
            join_wrapped: true,
            ..GridRenderOptions::default()
        },
    );
    let rendered = String::from_utf8(capture).expect("capture must be UTF-8");

    assert!(
        rendered.contains("red-表x-abcdef"),
        "styled wide logical line must survive resize reflow: {rendered:?}"
    );
    assert!(
        rendered.contains("plain-wrap-line"),
        "plain wrapped logical line must survive resize reflow: {rendered:?}"
    );
}

#[test]
fn writing_at_line_start_breaks_previous_wrapped_line_before_reflow() {
    let mut screen = new_screen(3, 4, 10);
    parse(&mut screen, b"abcdef");
    parse(&mut screen, b"\x1b[2;1HXYZ");

    screen.resize(TerminalSize { cols: 6, rows: 4 });

    let capture = screen.capture_transcript(full_range(), GridRenderOptions::default());
    let rendered = String::from_utf8(capture).expect("capture must be UTF-8");
    let lines = rendered.lines().collect::<Vec<_>>();
    assert_eq!(&lines[..2], &["abc", "XYZ"]);
}

#[test]
fn width_resize_widen_remaps_cursor_after_soft_wrap() {
    let mut screen = new_screen(5, 4, 10);
    parse(&mut screen, b"abcdef");

    screen.resize(TerminalSize { cols: 10, rows: 4 });
    parse(&mut screen, b"X");

    assert_eq!(screen.cursor_position(), (7, 0));
    assert!(!screen.pending_wrap);
    assert_eq!(screen.capture_grid(true).lines[0], "abcdefX");
}

#[test]
fn width_resize_widen_remaps_and_clears_pending_wrap_at_logical_end() {
    let mut screen = new_screen(5, 4, 10);
    parse(&mut screen, b"abcde");
    assert!(screen.pending_wrap);

    screen.resize(TerminalSize { cols: 10, rows: 4 });
    assert_eq!(screen.cursor_position(), (5, 0));
    assert!(!screen.pending_wrap);

    parse(&mut screen, b"X");

    assert_eq!(screen.cursor_position(), (6, 0));
    assert_eq!(screen.capture_grid(true).lines[0], "abcdeX");
}

#[test]
fn width_resize_widen_remaps_middle_cursor_by_logical_column() {
    let mut screen = new_screen(5, 4, 10);
    parse(&mut screen, b"abcdef\x1b[1;4H");

    screen.resize(TerminalSize { cols: 10, rows: 4 });
    parse(&mut screen, b"X");

    assert_eq!(screen.cursor_position(), (4, 0));
    assert_eq!(screen.capture_grid(true).lines[0], "abcXef");
}

#[test]
fn width_resize_narrow_remaps_cursor_into_reflowed_scrollback() {
    let mut screen = new_screen(10, 4, 10);
    parse(&mut screen, b"abcdef");

    screen.resize(TerminalSize { cols: 5, rows: 4 });

    assert_eq!(screen.history_size(), 1);
    assert_eq!(screen.cursor_position(), (1, 0));
    assert!(!screen.pending_wrap);

    parse(&mut screen, b"X");

    assert_eq!(screen.cursor_position(), (2, 0));
    assert_eq!(screen.capture_grid(true).lines[0], "abcdefX");
}

#[test]
fn width_resize_narrow_remaps_pending_wrap_to_new_edge() {
    let mut screen = new_screen(10, 5, 10);
    parse(&mut screen, b"abcdefghij");
    assert!(screen.pending_wrap);

    screen.resize(TerminalSize { cols: 5, rows: 5 });

    assert_eq!(screen.history_size(), 1);
    assert_eq!(screen.cursor_position(), (4, 0));
    assert!(screen.pending_wrap);

    parse(&mut screen, b"X");

    assert_eq!(screen.cursor_position(), (1, 1));
    assert_eq!(screen.capture_grid(true).lines[0], "abcdefghijX");
}

#[test]
fn width_resize_keeps_hard_line_break_separate_from_soft_wraps() {
    let mut screen = new_screen(5, 4, 10);
    parse(&mut screen, b"abcde\r\nfg");

    screen.resize(TerminalSize { cols: 10, rows: 4 });
    parse(&mut screen, b"X");

    let lines = screen.capture_grid(true).lines;
    assert_eq!(&lines[..2], &["abcde", "fgX"]);
    assert_eq!(screen.cursor_position(), (3, 1));
}

#[test]
fn width_resize_remaps_cursor_with_wide_cells() {
    let mut screen = new_screen(5, 4, 10);
    parse(&mut screen, "ab表c".as_bytes());
    assert!(screen.pending_wrap);

    screen.resize(TerminalSize { cols: 10, rows: 4 });
    parse(&mut screen, b"X");

    assert_no_wide_cell_fragments(&screen);
    assert_eq!(screen.cursor_position(), (6, 0));
    assert_eq!(screen.capture_grid(true).lines[0], "ab表cX");
}

#[test]
fn width_resize_maps_cursor_by_consumed_columns_across_wide_wrap_gaps() {
    let mut end_cursor = new_screen(10, 4, 10);
    parse(&mut end_cursor, "abc表d".as_bytes());

    end_cursor.resize(TerminalSize { cols: 4, rows: 4 });
    parse(&mut end_cursor, b"X");

    assert_no_wide_cell_fragments(&end_cursor);
    assert_eq!(end_cursor.cursor_position(), (3, 0));
    assert_eq!(end_cursor.capture_grid(true).lines[0], "abc表dX");

    let mut overwrite_cursor = new_screen(10, 4, 10);
    parse(&mut overwrite_cursor, "abc表d\x1b[1;6H".as_bytes());

    overwrite_cursor.resize(TerminalSize { cols: 4, rows: 4 });
    parse(&mut overwrite_cursor, b"X");

    assert_no_wide_cell_fragments(&overwrite_cursor);
    assert_eq!(overwrite_cursor.cursor_position(), (3, 0));
    assert_eq!(overwrite_cursor.capture_grid(true).lines[0], "abc表X");
}

#[test]
fn width_resize_cycle_does_not_import_wide_placement_gap_as_text() {
    let mut screen = new_screen(20, 3, 100);
    parse(&mut screen, "abc表defghijklmnop".as_bytes());

    screen.resize(TerminalSize { cols: 2, rows: 3 });
    let total_lines = screen.history_size() + usize::from(screen.size().rows);
    assert!(
        (0..total_lines)
            .filter_map(|absolute_y| screen.grid().absolute_line(absolute_y))
            .flat_map(|line| line.cells())
            .any(|cell| cell.is_reflow_gap()),
        "placement metadata must survive compacted history rows"
    );
    screen.resize(TerminalSize { cols: 7, rows: 3 });

    // tmux 3.7b restores this exact logical text after the 20 -> 2 -> 7 cycle;
    // the unused physical cell before the wide glyph is not captured as data.
    assert_no_wide_cell_fragments(&screen);
    assert_eq!(screen.capture_grid(true).lines[0], "abc表defghijklmnop");
    parse(&mut screen, b"X");
    assert_eq!(screen.cursor_position(), (5, 0));
    assert_eq!(screen.capture_grid(true).lines[0], "abc表defghijklmnopX");
}

#[test]
fn width_resize_cycles_preserve_multiple_wide_cells_and_explicit_spaces() {
    for expected in ["ab表cd界efgh", "abc 表def", "abc表 def", "ab 表 cd界 ef"] {
        let mut screen = new_screen(20, 4, 100);
        parse(&mut screen, expected.as_bytes());
        for cols in [3, 5, 7, 4, 20] {
            screen.resize(TerminalSize { cols, rows: 4 });
            assert_no_wide_cell_fragments(&screen);
        }

        assert_eq!(
            screen.capture_grid(true).lines[0],
            expected,
            "resize must discard only physical placement gaps"
        );
    }
}

#[test]
fn width_resize_cycle_preserves_wide_styles_and_pending_wrap() {
    let mut styled = new_screen(20, 4, 100);
    parse(
        &mut styled,
        "\x1b[31mabc表\x1b[32mdefghijklmnop\x1b[0m".as_bytes(),
    );
    styled.resize(TerminalSize { cols: 2, rows: 4 });
    styled.resize(TerminalSize { cols: 20, rows: 4 });

    let capture = styled.capture_transcript(
        full_range(),
        GridRenderOptions {
            join_wrapped: true,
            with_sequences: true,
            ..GridRenderOptions::default()
        },
    );
    let rendered = String::from_utf8(capture).expect("capture must be UTF-8");
    assert!(
        rendered.contains("\x1b[31mabc表\x1b[32mdefghijklmnop"),
        "wide reflow must retain cell styles without importing its placement gap: {rendered:?}"
    );

    let mut pending = new_screen(20, 4, 100);
    parse(&mut pending, "abc表d".as_bytes());
    pending.resize(TerminalSize { cols: 4, rows: 4 });
    pending.resize(TerminalSize { cols: 6, rows: 4 });

    assert_eq!(pending.cursor_position(), (5, 0));
    assert!(pending.pending_wrap);
    parse(&mut pending, b"X");
    assert_eq!(pending.cursor_position(), (1, 1));
    assert_eq!(pending.capture_grid(true).lines[0], "abc表dX");
}

#[test]
fn width_resize_line_edits_materialize_a_wide_placement_gap_as_space() {
    for (label, edit, expected) in [
        ("dch", b"\x1b[2;1H\x1b[P".as_slice(), "ab 表def"),
        ("ich-last-column", b"\x1b[2;2H\x1b[@".as_slice(), "abc表def"),
        (
            "dch-ich-plain",
            b"\x1b[2;1H\x1b[P\x1b[@A".as_slice(),
            "A 表def",
        ),
        (
            "dch-ich-styled",
            b"\x1b[2;1H\x1b[P\x1b[@\x1b[31mA\x1b[0m".as_slice(),
            "A 表def",
        ),
    ] {
        let mut screen = new_screen(20, 10, 100);
        parse(&mut screen, "abc表def\x1b[1;1H".as_bytes());
        screen.resize(TerminalSize { cols: 2, rows: 10 });

        parse(&mut screen, edit);
        screen.resize(TerminalSize { cols: 20, rows: 10 });

        // tmux 3.7b preserves the physical blank as logical data once DCH/ICH
        // explicitly edits the placement-gap row.
        assert_no_wide_cell_fragments(&screen);
        assert!(
            screen
                .capture_grid(true)
                .lines
                .iter()
                .any(|line| line == expected),
            "{label} edit must preserve the measured placement-gap semantics"
        );

        if label == "dch-ich-styled" {
            let capture = screen.capture_transcript(
                full_range(),
                GridRenderOptions {
                    join_wrapped: true,
                    with_sequences: true,
                    ..GridRenderOptions::default()
                },
            );
            let rendered = String::from_utf8(capture).expect("capture must be UTF-8");
            assert!(
                rendered.contains("\x1b[31mA"),
                "styled edit must retain the red cell: {rendered:?}"
            );
        }
    }
}

#[test]
fn insert_character_full_suffix_clears_requested_cells_product_divergence() {
    let mut screen = new_screen(20, 10, 100);
    parse(&mut screen, "abc表def\x1b[1;1H".as_bytes());
    screen.resize(TerminalSize { cols: 2, rows: 10 });

    parse(&mut screen, b"\x1b[2;1H\x1b[2@");
    screen.resize(TerminalSize { cols: 20, rows: 10 });

    // tmux 3.7b treats ICH covering the complete remaining row as a no-op.
    // RMUX keeps the terminal operation meaningful: two blanks are inserted
    // and the previous suffix is truncated before the wrapped continuation.
    assert_eq!(screen.capture_grid(true).lines[0], "ab  表def");
}

#[test]
fn width_resize_derives_pending_wrap_at_the_destination_edge() {
    let mut screen = new_screen(10, 4, 10);
    parse(&mut screen, b"abcde");
    assert!(!screen.pending_wrap);

    screen.resize(TerminalSize { cols: 5, rows: 4 });

    assert_eq!(screen.cursor_position(), (4, 0));
    assert!(screen.pending_wrap);
    parse(&mut screen, b"X");

    assert_eq!(screen.cursor_position(), (1, 1));
    assert_eq!(screen.capture_grid(true).lines[0], "abcdeX");
}

#[test]
fn width_resize_keeps_cursor_content_visible_when_only_empty_tail_rows_move_product_divergence() {
    let mut screen = new_screen(10, 4, 10);
    parse(&mut screen, b"abcdef\x1b[1;3H");

    screen.resize(TerminalSize { cols: 5, rows: 4 });
    parse(&mut screen, b"X");

    // tmux 3.7b keeps "abcde" in history and overwrites the leading "f"
    // after this resize. RMUX keeps the addressed logical cursor row visible
    // by consuming an otherwise empty trailing row.
    assert_eq!(screen.history_size(), 0);
    assert_eq!(screen.cursor_position(), (3, 0));
    assert_eq!(&screen.capture_grid(false).lines[..2], &["abXde", "f"]);
}

#[test]
fn width_resize_preserves_used_rows_below_a_nonbottom_cursor() {
    let mut screen = new_screen(10, 4, 20);
    parse(
        &mut screen,
        b"abcdef\r\nLOWER1\r\nLOWER2\r\nLOWER3\x1b[1;3H",
    );

    screen.resize(TerminalSize { cols: 5, rows: 4 });
    parse(&mut screen, b"X");

    let lines = screen.capture_grid(true).lines;
    for expected in ["abcdef", "LOWER1", "XOWER2", "LOWER3"] {
        assert!(
            lines.iter().any(|line| line == expected),
            "resize must retain used content below the cursor: {lines:?}"
        );
    }
    assert_eq!(screen.cursor_position(), (1, 0));
}

#[test]
fn width_resize_cursor_anchor_respects_zero_and_small_history_limits() {
    let mut no_history = new_screen(10, 3, 0);
    parse(&mut no_history, b"abcdef\x1b[1;3H");
    no_history.resize(TerminalSize { cols: 5, rows: 3 });
    parse(&mut no_history, b"X");

    assert_eq!(no_history.history_size(), 0);
    assert_eq!(&no_history.capture_grid(false).lines[..2], &["abXde", "f"]);

    let mut small_history = new_screen(10, 3, 1);
    parse(&mut small_history, b"abcdefghijklmno");
    small_history.resize(TerminalSize { cols: 5, rows: 3 });
    parse(&mut small_history, b"X");

    assert_eq!(small_history.history_size(), 1);
    assert_eq!(
        small_history.capture_grid(true).lines[0],
        "abcdefghijklmnoX"
    );

    let mut reflow_over_limit = reflow_overflow_screen(1);

    // tmux 3.7b allows width reflow to retain rows beyond history-limit so
    // resizing alone never discards pane contents.
    assert_eq!(reflow_over_limit.history_size(), 9);
    assert_eq!(
        reflow_over_limit.capture_grid(true).lines[0],
        "abcdefghijklmnopqrst"
    );
    parse(&mut reflow_over_limit, b"X");
    assert_eq!(
        reflow_over_limit.history_size(),
        9,
        "new scrolls must rotate preserved reflow overflow without growing it"
    );
    assert_eq!(
        reflow_over_limit.capture_grid(true).lines[0],
        "cdefghijklmnopqrstX"
    );
}

#[test]
fn width_resize_reflow_overflow_round_trips_height_and_explicit_history_mutations() {
    for history_limit in [0, 1, 3] {
        let mut screen = reflow_overflow_screen(history_limit);
        assert_eq!(screen.history_size(), 9);
        assert_eq!(screen.capture_grid(true).lines[0], "abcdefghijklmnopqrst");

        screen.resize(TerminalSize { cols: 2, rows: 5 });
        let expected_grown_history = if history_limit == 0 { 9 } else { 5 };
        let expected_grown_cursor_y = if history_limit == 0 { 0 } else { 4 };
        assert_eq!(screen.history_size(), expected_grown_history);
        assert_eq!(screen.cursor_position(), (1, expected_grown_cursor_y));
        assert_eq!(screen.capture_grid(true).lines[0], "abcdefghijklmnopqrst");

        screen.resize(TerminalSize { cols: 2, rows: 1 });
        assert_eq!(screen.history_size(), 9);
        assert_eq!(screen.cursor_position(), (1, 0));
        assert_eq!(screen.capture_grid(true).lines[0], "abcdefghijklmnopqrst");

        parse(&mut screen, b"X");
        assert_eq!(
            screen.history_size(),
            9,
            "new output must not grow reflow overflow for history-limit={history_limit}"
        );
    }

    for history_limit in [1, 3] {
        let mut direct_scroll = reflow_overflow_screen(history_limit);
        parse(&mut direct_scroll, b"X");
        assert_eq!(direct_scroll.history_size(), 9);
        assert_eq!(
            direct_scroll.capture_grid(true).lines[0],
            "cdefghijklmnopqrstX"
        );

        let mut trimmed = reflow_overflow_screen(history_limit);
        assert!(trimmed.trim_below_cursor());
        assert_eq!(trimmed.history_size(), 9);
        assert_eq!(trimmed.capture_grid(true).lines[0], "abcdefghijklmnopqrst");

        let mut widened = reflow_overflow_screen(history_limit);
        widened.resize(TerminalSize { cols: 20, rows: 1 });
        assert_eq!(widened.history_size(), 0);
        assert_eq!(widened.capture_grid(true).lines[0], "abcdefghijklmnopqrst");

        let mut cleared = reflow_overflow_screen(history_limit);
        cleared.clear_history_and_hyperlinks(false);
        assert_eq!(cleared.history_size(), 0);
        parse(&mut cleared, b"X");
        assert_eq!(
            cleared.history_size(),
            usize::from(history_limit > 0),
            "clear-history must discard the temporary reflow capacity"
        );
    }

    let mut reset_limit = reflow_overflow_screen(1);
    reset_limit.set_history_limit(3);
    assert_eq!(reset_limit.history_limit(), 3);
    assert_eq!(reset_limit.history_size(), 3);
    parse(&mut reset_limit, b"X");
    assert_eq!(reset_limit.history_size(), 3);
}

#[test]
fn zero_history_reflow_overflow_never_becomes_scrollable_history_product_divergence() {
    let mut screen = reflow_overflow_screen(0);
    assert_eq!(screen.history_size(), 9);
    assert_eq!(screen.grid().hscrolled(), 0);

    parse(&mut screen, b"X");

    // tmux 3.7b retains the final old cell while RMUX's bounded pending-wrap
    // representation drops that physical edge row. Both keep the overflow
    // frozen: history-limit=0 must not rotate it or make it height-scrollable.
    assert_eq!(screen.history_size(), 9);
    assert_eq!(screen.grid().hscrolled(), 0);
    assert!(screen.capture_grid(true).lines[0].starts_with("abcdefghijklmnopqr"));

    screen.resize(TerminalSize { cols: 2, rows: 5 });
    assert_eq!(screen.history_size(), 9);
    assert_eq!(screen.cursor_position(), (1, 0));
    assert_eq!(screen.grid().hscrolled(), 0);

    for _ in 0..8 {
        parse(&mut screen, b"\r\nZ");
    }
    assert_eq!(screen.history_size(), 9);
    assert_eq!(screen.grid().hscrolled(), 0);
    screen.resize(TerminalSize { cols: 2, rows: 1 });
    assert_eq!(screen.history_size(), 9);
    assert_eq!(screen.cursor_position(), (1, 0));
    assert_eq!(screen.grid().hscrolled(), 0);
}

#[test]
fn reflow_overflow_capacity_freezes_on_normal_output_after_growth_product_divergence() {
    for history_limit in [1, 3] {
        let mut partial = reflow_overflow_screen(history_limit);
        partial.resize(TerminalSize { cols: 2, rows: 5 });
        assert_eq!(partial.history_size(), 5);
        assert_eq!(partial.grid().hscrolled(), 5);

        for _ in 0..8 {
            parse(&mut partial, b"\r\nZ");
        }
        assert_eq!(
            partial.history_size(),
            5,
            "normal output must freeze the temporary capacity at the retained history"
        );
        assert_eq!(partial.grid().hscrolled(), 5);

        // tmux 3.7b lets this height shrink recreate the original overflow.
        // RMUX deliberately keeps the post-output bound so old capacity cannot
        // be resurrected after the reflow contents have been superseded.
        partial.resize(TerminalSize { cols: 2, rows: 1 });
        assert_eq!(partial.history_size(), 5);
        partial.resize(TerminalSize { cols: 2, rows: 5 });
        assert_eq!(partial.history_size(), 1);
        assert_eq!(partial.cursor_position(), (1, 4));

        let mut fully_pulled = reflow_overflow_screen(history_limit);
        fully_pulled.resize(TerminalSize { cols: 2, rows: 20 });
        assert_eq!(fully_pulled.history_size(), 0);
        assert_eq!(fully_pulled.grid().hscrolled(), 0);

        for _ in 0..30 {
            parse(&mut fully_pulled, b"\r\nZ");
        }
        assert_eq!(fully_pulled.history_size(), history_limit);
        assert_eq!(fully_pulled.grid().hscrolled(), history_limit);

        fully_pulled.resize(TerminalSize { cols: 2, rows: 1 });
        assert_eq!(
            fully_pulled.history_size(),
            history_limit,
            "height shrink after output must not restore consumed reflow capacity"
        );
    }
}

#[test]
fn reflow_overflow_height_shrink_without_output_restores_original_capacity() {
    for history_limit in [1, 3] {
        for grown_rows in [5, 20] {
            let mut screen = reflow_overflow_screen(history_limit);
            screen.resize(TerminalSize {
                cols: 2,
                rows: grown_rows,
            });
            screen.resize(TerminalSize { cols: 2, rows: 1 });

            assert_eq!(screen.history_size(), 9);
            assert_eq!(screen.cursor_position(), (1, 0));
            assert_eq!(screen.capture_grid(true).lines[0], "abcdefghijklmnopqrst");
        }
    }
}

#[test]
fn reflow_overflow_explicit_line_removal_reduces_future_capacity() {
    let mut screen = reflow_overflow_screen(1);
    assert!(screen.delete_absolute_line(0));
    assert_eq!(screen.history_size(), 8);

    parse(&mut screen, b"X");

    assert_eq!(screen.history_size(), 8);
    assert_eq!(screen.grid().hscrolled(), 8);
}

#[test]
fn width_resize_narrow_remaps_cursor_with_wide_cells_without_copying_tmux_gap() {
    let mut screen = new_screen(10, 4, 10);
    parse(&mut screen, "ab表cdef".as_bytes());

    screen.resize(TerminalSize { cols: 5, rows: 4 });
    parse(&mut screen, b"X");

    // tmux 3.7b leaves the spare cell after a wide glyph empty when this
    // narrows to five columns ("ab表" / "cdefX"). RMUX keeps the existing
    // fuller physical reflow while preserving the joined logical text.
    assert_no_wide_cell_fragments(&screen);
    assert_eq!(screen.capture_grid(true).lines[0], "ab表cdefX");
}

#[test]
fn width_resize_remaps_cursor_and_preserves_styles() {
    let mut screen = new_screen(5, 4, 10);
    let mut parser = InputParser::new();
    parser.parse(b"\x1b[31mabcde\x1b[32mf", &mut screen);

    screen.resize(TerminalSize { cols: 10, rows: 4 });
    parser.parse(b"X", &mut screen);

    let capture = screen.capture_transcript(
        full_range(),
        GridRenderOptions {
            join_wrapped: true,
            with_sequences: true,
            ..GridRenderOptions::default()
        },
    );
    let rendered = String::from_utf8(capture).expect("capture must be UTF-8");
    assert!(
        rendered.contains("\x1b[31mabcde\x1b[32mfX"),
        "styled reflow must preserve red/green cells and write X at the logical cursor: {rendered:?}"
    );
}

#[test]
fn alternate_screen_restore_remaps_saved_main_cursor_after_resize() {
    let mut screen = new_screen(5, 4, 10);
    parse(&mut screen, b"abcdef");
    parse(&mut screen, b"\x1b[?1049hALT");

    screen.resize(TerminalSize { cols: 10, rows: 4 });
    parse(&mut screen, b"\x1b[?1049lX");

    assert_eq!(screen.cursor_position(), (7, 0));
    assert_eq!(screen.capture_grid(true).lines[0], "abcdefX");
}

#[test]
fn alternate_screen_restore_remaps_saved_cursor_after_width_and_height_resize() {
    let mut screen = new_screen(5, 4, 10);
    parse(&mut screen, b"abcdef\x1b[?1049hALT");

    screen.resize(TerminalSize { cols: 10, rows: 5 });
    parse(&mut screen, b"\x1b[?1049lX");

    assert_eq!(screen.cursor_position(), (7, 0));
    assert_eq!(screen.capture_grid(true).lines[0], "abcdefX");
}

#[test]
fn alternate_screen_restore_without_saved_cursor_still_restores_resized_main_view() {
    let mut screen = new_screen(5, 4, 10);
    parse(&mut screen, b"abcdef");
    parse(&mut screen, b"\x1b[?1047hALT");

    screen.resize(TerminalSize { cols: 10, rows: 4 });
    parse(&mut screen, b"\x1b[?1047l");

    let lines = screen.capture_grid(true).lines;
    assert_eq!(lines[0], "abcdef");
    assert!(lines.iter().all(|line| !line.contains("ALT")));
}

#[test]
fn alternate_screen_width_resize_preserves_physical_cursor_product_divergence() {
    let mut screen = new_screen(5, 3, 10);
    parse(&mut screen, b"MAIN\r\n12345\x1b[H");
    parse(&mut screen, b"\x1b[?1047h\x1b[Habcdef");
    assert_eq!(screen.cursor_position(), (1, 1));

    screen.resize(TerminalSize { cols: 10, rows: 3 });

    // tmux 3.7b preserves both the physical row split and cursor. RMUX keeps
    // its lossless alternate-screen content reflow, but the cursor remains at
    // the same physical coordinate so a live application can redraw from the
    // position it owned before SIGWINCH.
    assert_eq!(screen.cursor_position(), (1, 1));
    assert!(!screen.pending_wrap);
    parse(&mut screen, b"X");
    assert_eq!(&screen.capture_grid(false).lines[..2], &["abcdef", " X"]);

    parse(&mut screen, b"\x1b[?1047l");
    assert_eq!(screen.cursor_position(), (2, 1));
    parse(&mut screen, b"Y");
    assert_eq!(screen.cursor_position(), (3, 1));
    let lines = screen.capture_grid(false).lines;
    assert_eq!(&lines[..2], &["MAIN", "12Y45"]);
    assert!(lines.iter().all(|line| !line.contains("abcdef")));
    assert!(lines.iter().all(|line| !line.contains('X')));
}

#[test]
fn alternate_screen_narrow_resize_clamps_cursor_with_pending_wrap_product_divergence() {
    let mut screen = new_screen(10, 3, 10);
    parse(&mut screen, b"\x1b[?1047h\x1b[Habcdefgh");
    assert_eq!(screen.cursor_position(), (8, 0));

    screen.resize(TerminalSize { cols: 5, rows: 3 });

    // tmux 3.7b retains an out-of-bounds virtual x=8 and wraps the next
    // printable byte. RMUX represents the same next-write behavior with its
    // bounded cursor plus pending-wrap state.
    assert_eq!(screen.cursor_position(), (4, 0));
    assert!(screen.pending_wrap);
    parse(&mut screen, b"X");
    assert_eq!(&screen.capture_grid(false).lines[..2], &["abcde", "Xgh"]);
}

#[test]
fn alternate_screen_resize_remaps_pending_wrap_by_physical_column_product_divergence() {
    let mut widened = new_screen(5, 3, 10);
    parse(&mut widened, b"\x1b[?1047h\x1b[Habcde");
    assert!(widened.pending_wrap);

    widened.resize(TerminalSize { cols: 10, rows: 3 });

    assert_eq!(widened.cursor_position(), (5, 0));
    assert!(!widened.pending_wrap);
    parse(&mut widened, b"X");
    assert_eq!(widened.capture_grid(false).lines[0], "abcdeX");

    let mut narrowed = new_screen(10, 3, 10);
    parse(&mut narrowed, b"\x1b[?1047h\x1b[Habcdefghij");
    assert!(narrowed.pending_wrap);

    narrowed.resize(TerminalSize { cols: 5, rows: 3 });

    assert_eq!(narrowed.cursor_position(), (4, 0));
    assert!(narrowed.pending_wrap);
    parse(&mut narrowed, b"X");
    assert_eq!(
        &narrowed.capture_grid(false).lines[..2],
        &["abcde", "Xghij"]
    );
}

#[test]
fn alternate_screen_width_and_height_resize_preserves_physical_cursor_product_divergence() {
    let mut screen = new_screen(5, 3, 10);
    parse(&mut screen, b"\x1b[?1047h\x1b[Habcdef");

    screen.resize(TerminalSize { cols: 10, rows: 5 });

    assert_eq!(screen.cursor_position(), (1, 1));
    assert!(!screen.pending_wrap);
    parse(&mut screen, b"X");
    assert_eq!(&screen.capture_grid(false).lines[..2], &["abcdef", " X"]);
}

#[test]
fn alternate_screen_height_shrink_clamps_physical_cursor() {
    let mut screen = new_screen(5, 4, 10);
    parse(&mut screen, b"\x1b[?1047h\x1b[4;4H");
    assert_eq!(screen.cursor_position(), (3, 3));

    screen.resize(TerminalSize { cols: 5, rows: 2 });

    assert_eq!(screen.cursor_position(), (3, 1));
    assert!(!screen.pending_wrap);
    parse(&mut screen, b"X");
    assert_eq!(screen.capture_grid(false).lines[1], "   X");
}

#[test]
fn alternate_screen_wide_reflow_preserves_physical_cursor_without_fragments_product_divergence() {
    let mut screen = new_screen(10, 3, 10);
    parse(&mut screen, "\x1b[?1047h\x1b[Hab表cdef".as_bytes());
    assert_eq!(screen.cursor_position(), (8, 0));

    screen.resize(TerminalSize { cols: 5, rows: 3 });

    assert_eq!(screen.cursor_position(), (4, 0));
    assert!(screen.pending_wrap);
    parse(&mut screen, b"X");
    assert_no_wide_cell_fragments(&screen);
    assert_eq!(&screen.capture_grid(false).lines[..2], &["ab表c", "Xef"]);
}

#[test]
fn alternate_screen_full_viewport_truncates_each_hard_line_before_tail_rows_product_divergence() {
    let mut screen = new_screen(10, 2, 10);
    parse(
        &mut screen,
        b"\x1b[?1047h\x1b[Habcdefghij\x1b[2;1HKLMNOPQRST",
    );

    screen.resize(TerminalSize { cols: 5, rows: 2 });

    // With no spare viewport row, tmux 3.7b keeps the leading columns of
    // every physical hard line rather than letting the first line's reflow
    // displace later rows.
    assert_eq!(screen.capture_grid(false).lines, vec!["abcde", "KLMNO"]);
    assert_eq!(screen.cursor_position(), (4, 1));
    assert!(screen.pending_wrap);
    parse(&mut screen, b"X");
    assert_eq!(screen.capture_grid(false).lines, vec!["KLMNO", "X"]);
}

#[test]
fn alternate_screen_physical_cursor_resize_does_not_change_1049_restore() {
    let mut screen = new_screen(5, 3, 10);
    screen.set_preserve_alternate_screen_cursor(true);
    parse(&mut screen, b"abcdef\x1b[?1049h\x1b[3;4HALT");

    screen.resize(TerminalSize { cols: 10, rows: 5 });
    parse(&mut screen, b"\x1b[?1049lX");

    assert_eq!(screen.cursor_position(), (7, 0));
    assert_eq!(screen.capture_grid(true).lines[0], "abcdefX");
}

#[test]
fn height_growth_keeps_cursor_on_content_when_history_is_pulled_into_view() {
    let mut screen = new_screen(20, 3, 10);
    parse(&mut screen, b"h0\r\nh1\r\np$ echo A0\r\nA0\r\np$ ");

    screen.resize(TerminalSize { cols: 20, rows: 5 });
    parse(&mut screen, b"\rp$ ");

    let capture = screen.capture_transcript(full_range(), GridRenderOptions::default());
    let rendered = String::from_utf8(capture).expect("capture must be UTF-8");
    let lines = rendered.lines().collect::<Vec<_>>();
    assert_eq!(&lines[..5], &["h0", "h1", "p$ echo A0", "A0", "p$"]);
}

#[test]
fn scrollback_lines_are_captured_after_crlf_output() {
    let mut screen = new_screen(8, 2, 10);
    parse(&mut screen, b"one\r\ntwo\r\nthree\r\n");

    assert_eq!(screen.history_size(), 2);
    assert_eq!(
        screen.capture_transcript(full_range(), GridRenderOptions::default()),
        b"one\ntwo\nthree\n\n"
    );
}

#[test]
fn erase_display_moves_used_visible_rows_to_history() {
    let mut screen = new_screen(12, 4, 10);
    parse(&mut screen, b"$ printf\r\n$ clear-x\x1b[H\x1b[2J$");

    assert_eq!(screen.history_size(), 2);
    assert_eq!(
        screen.capture_transcript(full_range(), GridRenderOptions::default()),
        b"$ printf\n$ clear-x\n$\n\n\n\n"
    );
}

#[test]
fn erase_to_end_from_home_moves_used_visible_rows_to_history() {
    let mut screen = new_screen(12, 4, 10);
    parse(&mut screen, b"$ printf\r\n$ clear-x\x1b[H\x1b[J$");

    assert_eq!(screen.history_size(), 2);
    assert_eq!(
        screen.capture_transcript(full_range(), GridRenderOptions::default()),
        b"$ printf\n$ clear-x\n$\n\n\n\n"
    );
}

#[test]
fn independent_transcript_lines_repeat_carried_sgr_state() {
    let mut screen = new_screen(8, 2, 10);
    parse(&mut screen, b"\x1b[48;2;20;20;20mone\r\n   ");

    let lines = screen.capture_transcript_lines_independent(
        full_range(),
        GridRenderOptions {
            with_sequences: true,
            include_empty_cells: true,
            trim_spaces: false,
            ..GridRenderOptions::default()
        },
    );

    assert!(lines[0].starts_with(b"\x1b[48;2;20;20;20m"));
    assert!(lines[1].starts_with(b"\x1b[48;2;20;20;20m"));
}

#[test]
fn insert_and_delete_character_materialize_compact_plain_lines() {
    let mut insert_screen = new_screen(8, 2, 10);
    parse(&mut insert_screen, b"abcd\x1b[1;3H\x1b[@");
    assert_eq!(
        insert_screen.capture_transcript(full_range(), GridRenderOptions::default()),
        b"ab cd\n\n"
    );

    let mut delete_screen = new_screen(8, 2, 10);
    parse(&mut delete_screen, b"abcd\x1b[1;2H\x1b[P");
    assert_eq!(
        delete_screen.capture_transcript(full_range(), GridRenderOptions::default()),
        b"acd\n\n"
    );
}

#[test]
fn alternate_screen_does_not_append_to_history() {
    let mut screen = new_screen(8, 2, 10);
    parse(&mut screen, b"main\n");
    parse(&mut screen, b"\x1b[?1049h");
    parse(&mut screen, b"alt\n");
    parse(&mut screen, b"\x1b[?1049l");

    let captured =
        String::from_utf8(screen.capture_transcript(full_range(), GridRenderOptions::default()))
            .expect("utf8");
    assert!(captured.contains("main"));
    assert!(!captured.contains("alt"));
}

#[test]
fn alternate_screen_entry_preserves_cursor_position() {
    let mut screen = new_screen(12, 5, 10);
    screen.set_preserve_alternate_screen_cursor(true);
    parse(&mut screen, b"main1\r\nmain2\x1b[?1049halt");

    assert_eq!(screen.cursor_position(), (8, 1));
    assert_eq!(
        screen.capture_transcript(full_range(), GridRenderOptions::default()),
        b"\n     alt\n\n\n\n"
    );
}

#[test]
fn history_limit_evicts_oldest_rows_after_crlf_output() {
    let mut screen = new_screen(8, 1, 2);
    parse(&mut screen, b"zero\r\none\r\ntwo\r\nthree\r\n");

    assert_eq!(screen.history_size(), 2);
    assert_eq!(
        screen.capture_transcript(full_range(), GridRenderOptions::default()),
        b"two\nthree\n\n"
    );
}

#[test]
fn joined_capture_merges_wrapped_rows() {
    let mut screen = new_screen(3, 2, 10);
    parse(&mut screen, b"abcdef");

    assert_eq!(screen.capture_grid(true).lines, vec!["abcdef"]);
}

#[test]
fn wide_character_prewrap_preserves_joined_capture() {
    let mut screen = new_screen(10, 3, 10);
    parse(&mut screen, "123456789界Z".as_bytes());

    // tmux 3.7b emits the glyph on the next physical row but keeps both rows
    // in one soft-wrapped logical line.
    assert_eq!(
        &screen.capture_grid(false).lines[..2],
        &["123456789", "界Z"]
    );
    assert!(screen
        .grid()
        .visible_line(0)
        .expect("pre-wrapped row")
        .flags()
        .contains(crate::grid::GridLineFlags::WRAPPED));
    assert_eq!(screen.capture_grid(true).lines[0], "123456789界Z");
    let joined = String::from_utf8(screen.capture_transcript(
        full_range(),
        GridRenderOptions {
            join_wrapped: true,
            ..GridRenderOptions::default()
        },
    ))
    .expect("joined capture must be UTF-8");
    assert!(joined.starts_with("123456789界Z\n"), "{joined:?}");
}

#[test]
fn wide_character_prewrap_reflows_as_one_logical_line() {
    let mut screen = new_screen(10, 4, 20);
    parse(&mut screen, "123456789界Z".as_bytes());

    for cols in [12, 6, 10] {
        screen.resize(TerminalSize { cols, rows: 4 });
        assert_no_wide_cell_fragments(&screen);
        assert_eq!(
            screen.capture_grid(true).lines[0],
            "123456789界Z",
            "pre-wrapped wide text must survive reflow at width {cols}"
        );
    }
}

#[test]
fn wide_character_prewrap_respects_insert_and_no_wrap_edges() {
    for (input, expected) in [
        ("12345678界Z", "12345678界Z"),
        ("123456789\x1b[4h界\x1b[4lZ", "123456789界Z"),
    ] {
        let mut screen = new_screen(10, 3, 10);
        parse(&mut screen, input.as_bytes());

        assert!(screen
            .grid()
            .visible_line(0)
            .expect("wrapped row")
            .flags()
            .contains(crate::grid::GridLineFlags::WRAPPED));
        assert_eq!(screen.capture_grid(true).lines[0], expected);
        assert_no_wide_cell_fragments(&screen);
    }

    let mut occupied_edge = new_screen(10, 3, 10);
    parse(&mut occupied_edge, "abcdefghij\x1b[10G界Z".as_bytes());
    occupied_edge.resize(TerminalSize { cols: 12, rows: 3 });
    assert_eq!(occupied_edge.capture_grid(true).lines[0], "abcdefghij界Z");
    assert_no_wide_cell_fragments(&occupied_edge);

    let mut no_wrap = new_screen(10, 2, 10);
    parse(&mut no_wrap, "\x1b[?7l123456789界Z".as_bytes());
    assert_eq!(no_wrap.capture_grid(false).lines[0], "123456789Z");
    assert!(!no_wrap
        .grid()
        .visible_line(0)
        .expect("unwrapped row")
        .flags()
        .contains(crate::grid::GridLineFlags::WRAPPED));
    assert_no_wide_cell_fragments(&no_wrap);
}

#[test]
fn alternate_screen_restore_preserves_wrapped_rows() {
    let mut screen = new_screen(3, 2, 10);
    parse(&mut screen, b"abcdef");
    parse(&mut screen, b"\x1b[?1049h");
    parse(&mut screen, b"\x1b[?1049l");

    assert!(screen
        .grid()
        .visible_line(0)
        .expect("first visible line")
        .flags()
        .contains(crate::grid::GridLineFlags::WRAPPED));
    assert_eq!(screen.capture_grid(true).lines, vec!["abcdef"]);
}

#[test]
fn alternate_screen_restore_after_width_resize_preserves_history_and_main_view() {
    let mut screen = new_screen(3, 2, 20);
    parse(&mut screen, b"hist0\r\nhist1\r\nabcdef");
    let history_before_alternate = screen.history_size();
    parse(&mut screen, b"\x1b[?1049h");
    parse(&mut screen, b"ALT");
    screen.resize(TerminalSize { cols: 5, rows: 2 });
    assert_eq!(screen.history_size(), history_before_alternate);

    parse(&mut screen, b"\x1b[?1049l");

    assert_eq!(screen.grid().size(), TerminalSize { cols: 5, rows: 2 });
    let lines = screen.capture_grid(true).lines;
    assert!(
        lines.iter().any(|line| line.contains("abcdef")),
        "restored main screen should survive width resize: {lines:?}"
    );
    assert!(
        lines.iter().all(|line| !line.contains("ALT")),
        "alternate-screen content must not leak after restore: {lines:?}"
    );
}

#[test]
fn alternate_screen_resize_preserves_main_history_product_divergence() {
    let mut screen = new_screen(5, 2, 20);
    parse(&mut screen, b"abcdefghijk");
    assert_eq!(screen.history_size(), 1);

    parse(&mut screen, b"\x1b[?1049hALT");
    screen.resize(TerminalSize { cols: 10, rows: 2 });
    assert_eq!(
        screen.history_size(),
        1,
        "alternate resize must not mutate main-screen history"
    );
    parse(&mut screen, b"\x1b[?1049lX");

    // tmux 3.7b retains the same bytes but keeps the old history boundary as
    // a hard split ("abcde" / "fghijkX"). RMUX deliberately reflows the
    // original soft-wrapped logical line after restoring the main screen.
    assert_eq!(screen.history_size(), 0);
    assert_eq!(screen.capture_grid(true).lines[0], "abcdefghijkX");
    assert!(screen
        .capture_grid(true)
        .lines
        .iter()
        .all(|line| !line.contains("ALT")));
}

#[test]
fn insert_and_delete_line_ignore_rows_outside_scroll_region() {
    let mut screen = new_screen(4, 4, 10);
    parse(&mut screen, b"1\r\n2\r\n3\r\n4");
    parse(&mut screen, b"\x1b[2;3r\x1b[1;1H\x1b[L\x1b[M");

    assert_eq!(screen.capture_grid(false).lines, vec!["1", "2", "3", "4"]);
}

#[test]
fn vertical_cursor_commands_respect_scroll_region_boundaries() {
    type CursorCase = (&'static str, u8, &'static [u8], (u32, u32));

    let cases: &[CursorCase] = &[
        // Inside the region, relative vertical movement stops at the margins.
        ("CUU inside", 3, b"\x1b[5A", (2, 1)),
        ("CUD inside", 3, b"\x1b[5B", (2, 3)),
        ("CNL inside", 3, b"\x1b[5E", (0, 3)),
        ("CPL inside", 3, b"\x1b[5F", (0, 1)),
        // The margins themselves remain stable under outward movement.
        ("CUU at top margin", 2, b"\x1b[5A", (2, 1)),
        ("CUD at bottom margin", 4, b"\x1b[5B", (2, 3)),
        ("CPL at top margin", 2, b"\x1b[5F", (0, 1)),
        ("CNL at bottom margin", 4, b"\x1b[5E", (0, 3)),
        // Movement away from a region keeps the full-screen boundary.
        ("CUU above region", 1, b"\x1b[5A", (2, 0)),
        ("CPL above region", 1, b"\x1b[5F", (0, 0)),
        ("CUD below region", 5, b"\x1b[5B", (2, 4)),
        ("CNL below region", 5, b"\x1b[5E", (0, 4)),
        // Movement toward a region may enter it but cannot cross its far margin.
        ("CUD above region", 1, b"\x1b[5B", (2, 3)),
        ("CNL above region", 1, b"\x1b[5E", (0, 3)),
        ("CUU below region", 5, b"\x1b[5A", (2, 1)),
        ("CPL below region", 5, b"\x1b[5F", (0, 1)),
    ];

    for &(name, start_row, command, expected_cursor) in cases {
        let mut screen = new_screen(5, 5, 10);
        parse(&mut screen, b"\x1b[2;4r");
        parse(&mut screen, format!("\x1b[{start_row};3H").as_bytes());
        parse(&mut screen, command);

        assert_eq!(screen.cursor_position(), expected_cursor, "{name}");
    }
}

#[test]
fn osc_8_links_are_applied_to_cells() {
    let mut screen = new_screen(8, 2, 10);
    let mut parser = InputParser::new();
    parser.parse(
        b"\x1b]8;id=link;https://example.com\x1b\\xy\x1b]8;;\x1b\\z",
        &mut screen,
    );

    let line = screen.grid().visible_line(0).expect("first visible line");
    assert_ne!(line.cell(0).expect("x cell").link(), 0);
    assert_ne!(line.cell(1).expect("y cell").link(), 0);
    assert_eq!(line.cell(2).expect("z cell").link(), 0);
}

#[test]
fn default_cell_style_overlay_preserves_application_backgrounds() {
    let mut screen = new_screen(4, 1, 10);
    parse(&mut screen, b"\x1b[44mB\x1b[0mD");

    screen.overlay_style_on_default_cells("fg=green,bg=black");

    let line = screen.grid().visible_line(0).expect("visible line");
    assert_eq!(line.cell(0).expect("explicit cell").fg(), 2);
    assert_eq!(line.cell(0).expect("explicit cell").bg(), 4);
    assert_eq!(line.cell(1).expect("default text").fg(), 2);
    assert_eq!(line.cell(1).expect("default text").bg(), 0);
    assert_eq!(line.cell(2).expect("default blank").fg(), 2);
    assert_eq!(line.cell(2).expect("default blank").bg(), 0);
}

#[test]
fn erase_display_does_not_tint_fully_cleared_rows_with_current_background() {
    let mut screen = new_screen(6, 4, 10);
    parse(&mut screen, b"\x1b[48;5;236mA\x1b[J");

    let current = screen.grid().visible_line(0).expect("current row");
    assert_ne!(
        current.cell(1).expect("current row trailing cell").bg(),
        COLOUR_DEFAULT,
        "ED should preserve BCE on the current line"
    );

    for row in 1..4 {
        let line = screen.grid().visible_line(row).expect("fully cleared row");
        for col in 0..6 {
            let cell = line.cell(col).expect("cleared cell");
            assert_eq!(
                cell.bg(),
                COLOUR_DEFAULT,
                "fully cleared row {row}, col {col} must keep the terminal background"
            );
            assert_eq!(cell.text(), " ", "row {row}, col {col} should be blank");
        }
    }
}

#[test]
fn wide_cells_create_padding_and_overwrite_stale_padding() {
    let mut screen = new_screen(4, 1, 10);
    parse(&mut screen, "表".as_bytes());

    let line = screen.grid().visible_line(0).expect("visible line");
    assert_eq!(line.cell(0).expect("wide cell").width(), 2);
    assert!(line.cell(1).expect("padding cell").is_padding());
    assert_eq!(line.owning_cell_x(1), Some(0));

    parse(&mut screen, b"\rA");
    let line = screen.grid().visible_line(0).expect("visible line");
    assert_eq!(line.cell(0).expect("overwritten cell").text(), "A");
    assert!(!line.cell(1).expect("stale padding cleared").is_padding());
}

#[test]
fn narrow_cells_can_be_replaced_by_wide_cells_with_padding() {
    let mut screen = new_screen(4, 1, 10);
    parse(&mut screen, b"AB");
    parse(&mut screen, "\r表".as_bytes());

    let line = screen.grid().visible_line(0).expect("visible line");
    assert_eq!(line.cell(0).expect("wide cell").text(), "表");
    assert_eq!(line.cell(0).expect("wide cell").width(), 2);
    assert!(line.cell(1).expect("padding cell").is_padding());
    assert_eq!(line.cell(2).expect("untouched cell").text(), " ");
}

#[test]
fn variation_selector_combines_with_optional_force_wide() {
    let mut wide = new_screen(4, 1, 10);
    wide.set_utf8_config(utf8_config(&[], true));
    parse(&mut wide, "❤\u{fe0f}A".as_bytes());

    let wide_line = wide.grid().visible_line(0).expect("wide line");
    assert_eq!(wide_line.cell(0).expect("heart cell").text(), "❤\u{fe0f}");
    assert_eq!(wide_line.cell(0).expect("heart cell").width(), 2);
    assert!(wide_line.cell(1).expect("padding").is_padding());
    assert_eq!(wide_line.cell(2).expect("following text").text(), "A");

    let mut narrow = new_screen(4, 1, 10);
    narrow.set_utf8_config(utf8_config(&[], false));
    parse(&mut narrow, "❤\u{fe0f}A".as_bytes());

    let narrow_line = narrow.grid().visible_line(0).expect("narrow line");
    assert_eq!(narrow_line.cell(0).expect("heart cell").text(), "❤\u{fe0f}");
    assert_eq!(narrow_line.cell(0).expect("heart cell").width(), 1);
    assert!(!narrow_line.cell(1).expect("no padding").is_padding());
    assert_eq!(narrow_line.cell(1).expect("following text").text(), "A");
}

#[test]
fn variation_selector_at_a_narrow_right_edge_keeps_valid_cell_topology() {
    let mut screen = new_screen(1, 1, 10);
    parse(&mut screen, "❤\u{fe0f}".as_bytes());

    let line = screen.grid().visible_line(0).expect("visible line");
    let heart = line.cell(0).expect("heart cell");
    assert_eq!(heart.text(), "❤\u{fe0f}");
    assert_eq!(heart.width(), 1);
    assert!(!heart.is_padding());
    assert_eq!(screen.cursor_position(), (0, 0));
    assert_no_wide_cell_fragments(&screen);
}

#[test]
fn direct_wide_glyph_in_a_one_column_screen_keeps_valid_cell_topology() {
    let mut screen = new_screen(1, 1, 10);
    parse(&mut screen, "界".as_bytes());

    let line = screen.grid().visible_line(0).expect("visible line");
    let glyph = line.cell(0).expect("wide glyph cell");
    assert_eq!(glyph.text(), "界");
    assert_eq!(glyph.width(), 1);
    assert!(!glyph.is_padding());
    assert_eq!(screen.cursor_position(), (0, 0));
    assert_no_wide_cell_fragments(&screen);
}

#[test]
fn variation_selector_promotion_clears_displaced_wide_cell_padding() {
    let mut screen = new_screen(4, 1, 10);
    parse(&mut screen, "A界\r❤\u{fe0f}\x1b[4GC".as_bytes());

    let line = screen.grid().visible_line(0).expect("visible line");
    assert_eq!(line.cell(0).expect("heart owner").text(), "❤\u{fe0f}");
    assert_eq!(line.cell(0).expect("heart owner").width(), 2);
    assert!(line.cell(1).expect("heart padding").is_padding());
    assert_eq!(line.owning_cell_x(1), Some(0));
    assert!(!line
        .cell(2)
        .expect("cleared displaced padding")
        .is_padding());
    assert_eq!(line.cell(2).expect("cleared displaced padding").text(), " ");
    assert_eq!(line.cell(3).expect("positioned suffix").text(), "C");
    assert_eq!(
        screen
            .render_visible_line_independent(
                0,
                GridRenderOptions {
                    trim_spaces: false,
                    ..GridRenderOptions::default()
                }
            )
            .expect("rendered line"),
        "❤\u{fe0f} C".as_bytes()
    );
    assert_no_wide_cell_fragments(&screen);
}

#[test]
fn hangul_jamo_skin_tone_and_flags_combine_into_single_cells() {
    let mut screen = new_screen(8, 1, 10);
    parse(&mut screen, "각 👋🏽 🇨🇭".as_bytes());

    let line = screen.grid().visible_line(0).expect("visible line");
    assert_eq!(line.cell(0).expect("hangul cell").text(), "각");
    assert_eq!(line.cell(0).expect("hangul cell").width(), 2);
    assert_eq!(line.cell(3).expect("emoji cell").text(), "👋🏽");
    assert_eq!(line.cell(3).expect("emoji cell").width(), 2);
    assert_eq!(line.cell(6).expect("flag cell").text(), "🇨🇭");
    assert_eq!(line.cell(6).expect("flag cell").width(), 2);
    assert!(line.cell(7).expect("flag padding").is_padding());
}

#[test]
fn combining_marks_do_not_reach_back_into_previous_wrapped_lines() {
    let mut screen = new_screen(1, 2, 10);
    parse(&mut screen, b"AB");
    <Screen as crate::input::ScreenWriter>::carriage_return(&mut screen);
    parse(&mut screen, "\u{0301}".as_bytes());

    let first = screen.grid().visible_line(0).expect("first line");
    let second = screen.grid().visible_line(1).expect("second line");
    assert_eq!(first.cell(0).expect("first cell").text(), "A");
    assert_eq!(second.cell(0).expect("second cell").text(), "B");
}

#[test]
fn third_regional_indicator_starts_a_new_cell() {
    let mut screen = new_screen(4, 1, 10);
    parse(&mut screen, "🇨🇭🇩".as_bytes());

    let line = screen.grid().visible_line(0).expect("visible line");
    assert_eq!(line.cell(0).expect("flag cell").text(), "🇨🇭");
    assert_eq!(line.cell(0).expect("flag cell").width(), 2);
    assert!(line.cell(1).expect("flag padding").is_padding());
    assert_eq!(line.cell(2).expect("third indicator").text(), "🇩");
    assert_eq!(line.cell(2).expect("third indicator").width(), 1);
}

fn first_line(screen: &Screen) -> String {
    screen
        .capture_grid(false)
        .lines
        .into_iter()
        .next()
        .unwrap_or_default()
}

fn assert_no_wide_cell_fragments(screen: &Screen) {
    for y in 0..screen.grid().sy() {
        let line = screen.grid().visible_line(y).expect("visible line");
        let mut x = 0;
        while x < screen.grid().sx() {
            let cell = line.cell(x).expect("cell exists");
            if cell.is_padding() {
                assert!(
                    line.owning_cell_x(x).is_some(),
                    "padding cell at {x},{y} must have a wide-cell owner"
                );
                x += 1;
                continue;
            }

            let width = u32::from(cell.width());
            if width <= 1 {
                x += 1;
                continue;
            }

            assert!(
                x + width <= screen.grid().sx(),
                "wide cell at {x},{y} must fit in the row"
            );
            for offset in 1..width {
                let padding_x = x + offset;
                assert!(
                    line.cell(padding_x)
                        .expect("wide padding cell exists")
                        .is_padding(),
                    "wide cell at {x},{y} must be followed by padding at {padding_x},{y}"
                );
                assert_eq!(
                    line.owning_cell_x(padding_x),
                    Some(x),
                    "padding at {padding_x},{y} must point back to {x},{y}"
                );
            }
            x += width;
        }
    }
}

#[test]
fn cursor_motion_uses_terminal_columns_for_wide_characters() {
    let mut screen = new_screen(6, 1, 10);
    parse(&mut screen, "表A".as_bytes());
    assert_eq!(screen.cursor_x, 3);

    <Screen as crate::input::ScreenWriter>::cursor_left(&mut screen, 1);
    assert_eq!(screen.cursor_x, 2);

    <Screen as crate::input::ScreenWriter>::cursor_left(&mut screen, 1);
    assert_eq!(screen.cursor_x, 1);

    <Screen as crate::input::ScreenWriter>::cursor_left(&mut screen, 1);
    assert_eq!(screen.cursor_x, 0);

    <Screen as crate::input::ScreenWriter>::cursor_right(&mut screen, 1);
    assert_eq!(screen.cursor_x, 1);

    <Screen as crate::input::ScreenWriter>::cursor_right(&mut screen, 2);
    assert_eq!(screen.cursor_x, 3);
}

#[test]
fn backspace_moves_one_terminal_column_through_wide_characters() {
    let mut screen = new_screen(6, 1, 10);
    parse(&mut screen, "表A".as_bytes());
    assert_eq!((screen.cursor_x, screen.cursor_y), (3, 0));

    <Screen as crate::input::ScreenWriter>::backspace(&mut screen);
    assert_eq!((screen.cursor_x, screen.cursor_y), (2, 0));

    <Screen as crate::input::ScreenWriter>::backspace(&mut screen);
    assert_eq!((screen.cursor_x, screen.cursor_y), (1, 0));

    <Screen as crate::input::ScreenWriter>::backspace(&mut screen);
    assert_eq!((screen.cursor_x, screen.cursor_y), (0, 0));
}

#[test]
fn backspace_wraps_to_previous_line_last_column() {
    let mut screen = new_screen(2, 2, 10);
    parse(&mut screen, "表A".as_bytes());

    <Screen as crate::input::ScreenWriter>::backspace(&mut screen);
    assert_eq!((screen.cursor_x, screen.cursor_y), (0, 1));

    <Screen as crate::input::ScreenWriter>::backspace(&mut screen);
    assert_eq!((screen.cursor_x, screen.cursor_y), (1, 0));

    <Screen as crate::input::ScreenWriter>::backspace(&mut screen);
    assert_eq!((screen.cursor_x, screen.cursor_y), (0, 0));
}

#[test]
fn plain_ascii_run_fallback_is_invariant_to_pty_chunking_across_a_wide_cell() {
    fn prepared_screen() -> Screen {
        let mut screen = new_screen(4, 2, 10);
        parse(&mut screen, "\x1b[2;1H界\x1b[1;3H".as_bytes());
        screen
    }

    let mut single_chunk = prepared_screen();
    parse(&mut single_chunk, b"abcd");

    let mut split_chunks = prepared_screen();
    parse(&mut split_chunks, b"ab");
    parse(&mut split_chunks, b"cd");

    let expected = vec!["  ab".to_owned(), "cd".to_owned()];
    assert_eq!(single_chunk.capture_grid(false).lines, expected);
    assert_eq!(
        split_chunks.capture_grid(false).lines,
        single_chunk.capture_grid(false).lines
    );
    assert_eq!(single_chunk.cursor_position(), (2, 1));
    assert_eq!(split_chunks.cursor_position(), (2, 1));
    assert_no_wide_cell_fragments(&single_chunk);
    assert_no_wide_cell_fragments(&split_chunks);
}

#[test]
fn csi_cursor_backward_uses_columns_for_cjk_text() {
    let mut screen = new_screen(16, 1, 10);
    parse(&mut screen, "你好世界\x1b[2D".as_bytes());

    assert_eq!(first_line(&screen), "你好世界");
    assert_eq!(screen.cursor_position(), (6, 0));
    assert_no_wide_cell_fragments(&screen);
}

#[test]
fn insert_mode_shifts_ascii_and_reset_restores_overwrite() {
    let mut screen = new_screen(10, 1, 10);
    parse(&mut screen, b"abcdef\x1b[1;3H\x1b[4hXY\x1b[4lZ");

    assert_eq!(first_line(&screen), "abXYZdef");
    assert_eq!(screen.cursor_position(), (5, 0));
}

#[test]
fn insert_mode_preserves_shifted_styles() {
    let mut screen = new_screen(10, 1, 10);
    parse(
        &mut screen,
        b"\x1b[31mabcdef\x1b[0m\x1b[1;3H\x1b[34m\x1b[4hX\x1b[4l\x1b[0m",
    );

    assert_eq!(first_line(&screen), "abXcdef");
    let line = screen.grid().visible_line(0).expect("visible line");
    assert_eq!(line.cell(0).expect("red a").fg(), 1);
    assert_eq!(line.cell(1).expect("red b").fg(), 1);
    assert_eq!(line.cell(2).expect("blue X").fg(), 4);
    for x in 3..7 {
        assert_eq!(line.cell(x).expect("shifted red cell").fg(), 1);
    }
}

#[test]
fn insert_mode_shifts_for_wide_characters_without_padding_fragments() {
    let mut screen = new_screen(10, 1, 10);
    parse(&mut screen, "abcdef\x1b[1;3H\x1b[4h界\x1b[4l".as_bytes());

    assert_eq!(first_line(&screen), "ab界cdef");
    assert_eq!(screen.cursor_position(), (4, 0));
    assert_no_wide_cell_fragments(&screen);

    let mut shifted = new_screen(10, 1, 10);
    parse(&mut shifted, "ab界cd\x1b[1;3H\x1b[4hX\x1b[4l".as_bytes());
    assert_eq!(first_line(&shifted), "abX界cd");
    assert_no_wide_cell_fragments(&shifted);
}

#[test]
fn insert_character_on_wide_padding_uses_the_next_logical_boundary_product_divergence() {
    let mut screen = new_screen(12, 1, 10);
    parse(&mut screen, "ab界cd\x1b[1;4H\x1b[@X".as_bytes());

    // tmux 3.7b keeps its cursor on the padding column and renders an
    // overlapping cell plus the inserted blank ("ab界X cd"). RMUX keeps a
    // valid owner/padding pair and advances to the next logical boundary.
    assert_eq!(first_line(&screen), "ab界Xcd");
    assert_eq!(screen.cursor_position(), (5, 0));
    assert_no_wide_cell_fragments(&screen);
}

#[test]
fn insert_mode_on_wide_padding_preserves_valid_cells_product_divergence() {
    let mut ascii = new_screen(12, 1, 10);
    parse(&mut ascii, "ab界cd\x1b[1;4H\x1b[4hX\x1b[4l".as_bytes());
    assert_eq!(first_line(&ascii), "ab界Xcd");
    assert_eq!(ascii.cursor_position(), (5, 0));
    assert_no_wide_cell_fragments(&ascii);

    let mut wide = new_screen(12, 1, 10);
    parse(&mut wide, "ab界cd\x1b[1;4H\x1b[4h好\x1b[4l".as_bytes());
    // tmux 3.7b leaves an extra blank after the inserted wide glyph because
    // it retains an overlapping padding cursor. RMUX deliberately keeps the
    // compact logical row while preserving both wide owner/padding pairs.
    assert_eq!(first_line(&wide), "ab界好cd");
    assert_eq!(wide.cursor_position(), (6, 0));
    assert_no_wide_cell_fragments(&wide);

    let mut adjacent_wide = new_screen(12, 1, 10);
    parse(
        &mut adjacent_wide,
        "ab界好cd\x1b[1;4H\x1b[4hX\x1b[4l".as_bytes(),
    );
    assert_eq!(first_line(&adjacent_wide), "ab界X好cd");
    assert_eq!(adjacent_wide.cursor_position(), (5, 0));
    assert_no_wide_cell_fragments(&adjacent_wide);
}

#[test]
fn insert_mode_on_wide_padding_handles_edges_product_divergence() {
    let mut no_wrap = new_screen(12, 1, 10);
    parse(
        &mut no_wrap,
        "ab界cd\x1b[1;4H\x1b[?7l\x1b[4hX\x1b[4l".as_bytes(),
    );
    assert_eq!(first_line(&no_wrap), "ab界Xcd");
    assert_eq!(no_wrap.cursor_position(), (5, 0));
    assert_no_wide_cell_fragments(&no_wrap);

    let mut edge_wrap = new_screen(8, 2, 10);
    parse(
        &mut edge_wrap,
        "abcdef界\x1b[1;8H\x1b[4hX\x1b[4l".as_bytes(),
    );
    assert_eq!(edge_wrap.capture_grid(false).lines, vec!["abcdef界", "X"]);
    assert_eq!(edge_wrap.cursor_position(), (1, 1));
    assert_no_wide_cell_fragments(&edge_wrap);

    let mut edge_no_wrap = new_screen(8, 1, 10);
    parse(
        &mut edge_no_wrap,
        "abcdef界\x1b[1;8H\x1b[?7l\x1b[4hX\x1b[4l".as_bytes(),
    );
    assert_eq!(first_line(&edge_no_wrap), "abcdef界");
    assert_eq!(edge_no_wrap.cursor_position(), (7, 0));
    assert_no_wide_cell_fragments(&edge_no_wrap);

    let mut scroll_region = new_screen(8, 4, 10);
    parse(
        &mut scroll_region,
        "\x1b[1;1H11111111\x1b[2;1H22222222\x1b[3;1Habcdef界\
         \x1b[4;1H44444444\x1b[2;3r\x1b[3;8H\x1b[4hX\x1b[4l"
            .as_bytes(),
    );
    assert_eq!(
        scroll_region.capture_grid(false).lines,
        vec!["11111111", "abcdef界", "X", "44444444"]
    );
    assert_eq!(scroll_region.cursor_position(), (1, 2));
    assert_no_wide_cell_fragments(&scroll_region);
}

#[test]
fn insert_mode_on_styled_wide_padding_preserves_shifted_cell_styles() {
    let mut screen = new_screen(12, 1, 10);
    parse(
        &mut screen,
        "\x1b[31mab界cd\x1b[0m\x1b[1;4H\x1b[34m\x1b[4hX\x1b[4l\x1b[0m".as_bytes(),
    );

    assert_eq!(first_line(&screen), "ab界Xcd");
    let line = screen.grid().visible_line(0).expect("visible line");
    for x in [0, 1, 2, 3, 5, 6] {
        assert_eq!(line.cell(x).expect("shifted red cell").fg(), 1);
    }
    assert_eq!(line.cell(4).expect("blue inserted cell").fg(), 4);
    assert_no_wide_cell_fragments(&screen);
}

#[test]
fn insert_mode_matches_tmux_at_the_right_edge() {
    let mut ascii = new_screen(8, 2, 10);
    parse(&mut ascii, b"abcdefgh\x1b[1;8H\x1b[4hX");
    assert_eq!(first_line(&ascii), "abcdefgX");
    parse(&mut ascii, b"\x1b[4lY");
    assert_eq!(ascii.capture_grid(false).lines, vec!["abcdefgX", "Y"]);

    let mut wide_wrap = new_screen(8, 3, 10);
    parse(
        &mut wide_wrap,
        "\x1b[1;1Habcdefgh\x1b[2;1H12345678\x1b[1;8H\x1b[4h界\x1b[4l".as_bytes(),
    );
    let wrapped_lines = wide_wrap.capture_grid(false).lines;
    assert_eq!(wrapped_lines.first().map(String::as_str), Some("abcdefg"));
    assert_eq!(wrapped_lines.get(1).map(String::as_str), Some("界345678"));
    assert_no_wide_cell_fragments(&wide_wrap);

    let mut no_wrap = new_screen(8, 1, 10);
    parse(
        &mut no_wrap,
        "abcdefgh\x1b[1;8H\x1b[?7l\x1b[4h界\x1b[4l".as_bytes(),
    );
    assert_eq!(first_line(&no_wrap), "abcdefgh");
    assert_no_wide_cell_fragments(&no_wrap);
}

#[test]
fn bash_style_backspace_deletes_one_cjk_character() {
    let mut screen = new_screen(16, 1, 10);
    parse(&mut screen, "你好世界\x08\x08  \x08\x08".as_bytes());

    assert_eq!(first_line(&screen), "你好世");
    assert_eq!(screen.cursor_position(), (6, 0));
    assert_no_wide_cell_fragments(&screen);
}

#[test]
fn delete_character_removes_cjk_columns_without_padding_orphans() {
    let mut screen = new_screen(16, 1, 10);
    parse(&mut screen, "你好世界\x1b[7G\x1b[2P".as_bytes());

    assert_eq!(first_line(&screen), "你好世");
    assert_eq!(screen.cursor_position(), (6, 0));
    assert_no_wide_cell_fragments(&screen);
}

#[test]
fn erase_character_clears_cjk_columns_without_padding_orphans() {
    let mut screen = new_screen(16, 1, 10);
    parse(&mut screen, "你好世界\x1b[7G\x1b[2X".as_bytes());

    assert_eq!(first_line(&screen), "你好世");
    assert_eq!(screen.cursor_position(), (6, 0));
    assert_no_wide_cell_fragments(&screen);
}

#[test]
fn insert_character_shifts_whole_cjk_cells() {
    let mut screen = new_screen(16, 1, 10);
    parse(&mut screen, "你好世界\x1b[7G\x1b[2@".as_bytes());

    assert_eq!(first_line(&screen), "你好世  界");
    assert_eq!(screen.cursor_position(), (6, 0));
    assert_no_wide_cell_fragments(&screen);
}

#[test]
fn writing_on_wide_padding_clears_owner_cell() {
    let mut screen = new_screen(6, 1, 10);
    parse(&mut screen, "表\x08A".as_bytes());

    assert_eq!(first_line(&screen), " A");
    assert_eq!(screen.cursor_position(), (2, 0));
    assert_no_wide_cell_fragments(&screen);
}
