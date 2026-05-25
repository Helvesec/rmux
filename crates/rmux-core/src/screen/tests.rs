use super::{Screen, MAX_TERMINAL_PASSTHROUGH_EVENTS};
use crate::input::InputParser;
use crate::terminal_passthrough::MAX_TERMINAL_PASSTHROUGH_PAYLOAD_BYTES;
use crate::{GridRenderOptions, OptionStore, ScreenCaptureRange, Utf8Config};
use rmux_proto::{OptionName, ScopeSelector, SetOptionMode, TerminalSize};

fn parse(screen: &mut Screen, bytes: &[u8]) {
    let mut parser = InputParser::new();
    parser.parse(bytes, screen);
}

fn new_screen(cols: u16, rows: u16, history: usize) -> Screen {
    Screen::new(TerminalSize { cols, rows }, history)
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
fn insert_and_delete_line_ignore_rows_outside_scroll_region() {
    let mut screen = new_screen(4, 4, 10);
    parse(&mut screen, b"1\r\n2\r\n3\r\n4");
    parse(&mut screen, b"\x1b[2;3r\x1b[1;1H\x1b[L\x1b[M");

    assert_eq!(screen.capture_grid(false).lines, vec!["1", "2", "3", "4"]);
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

#[test]
fn cursor_motion_skips_padding_cells_for_wide_characters() {
    let mut screen = new_screen(4, 1, 10);
    parse(&mut screen, "表A".as_bytes());

    <Screen as crate::input::ScreenWriter>::cursor_left(&mut screen, 1);
    assert_eq!(screen.cursor_x, 2);

    <Screen as crate::input::ScreenWriter>::cursor_left(&mut screen, 1);
    assert_eq!(screen.cursor_x, 0);

    <Screen as crate::input::ScreenWriter>::cursor_right(&mut screen, 1);
    assert_eq!(screen.cursor_x, 2);

    screen.cursor_x = 1;
    <Screen as crate::input::ScreenWriter>::cursor_right(&mut screen, 1);
    assert_eq!(screen.cursor_x, 2);
}

#[test]
fn backspace_steps_over_wide_characters_and_wrapped_padding() {
    let mut screen = new_screen(2, 2, 10);
    parse(&mut screen, "表A".as_bytes());

    <Screen as crate::input::ScreenWriter>::backspace(&mut screen);
    assert_eq!((screen.cursor_x, screen.cursor_y), (0, 1));

    <Screen as crate::input::ScreenWriter>::backspace(&mut screen);
    assert_eq!((screen.cursor_x, screen.cursor_y), (0, 0));
}
