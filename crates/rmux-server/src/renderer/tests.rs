use super::{
    border_cells, parse_standalone_style, render, status_bar_runs, style_sgr_bytes, BorderStyle,
};
use rmux_core::{OptionStore, Session, Style, Utf8Config};
use rmux_proto::{
    OptionName, ResizePaneAdjustment, ScopeSelector, SessionName, SetOptionMode, SplitDirection,
    TerminalSize, WindowTarget,
};

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

fn session_with_three_panes() -> Session {
    let mut session = Session::new(session_name("alpha"), TerminalSize { cols: 80, rows: 24 });
    session.split_active_pane().expect("first split succeeds");
    session.split_pane(1).expect("second split succeeds");
    session
}

fn border_style(value: Option<&str>) -> Style {
    parse_standalone_style(value)
}

#[test]
fn rendered_pane_line_truncates_to_pane_width_without_counting_sgr() {
    let utf8 = Utf8Config::default();
    let clipped = String::from_utf8(super::truncate_rendered_pane_line(
        b"\x1b[31mabcdef",
        3,
        &utf8,
    ))
    .expect("utf8");

    assert_eq!(clipped, "\x1b[31mabc");

    let clipped_wide = String::from_utf8(super::truncate_rendered_pane_line(
        "表ab".as_bytes(),
        3,
        &utf8,
    ))
    .expect("utf8");
    assert_eq!(clipped_wide, "表a");
}

fn has_cell(cells: &[super::BorderCell], x: u16, y: u16, glyph: char) -> bool {
    cells
        .iter()
        .any(|cell| cell.x == x && cell.y == y && cell.glyph == glyph)
}

fn has_styled_cell(
    cells: &[super::BorderCell],
    x: u16,
    y: u16,
    glyph: char,
    style: &BorderStyle,
) -> bool {
    cells
        .iter()
        .any(|cell| cell.x == x && cell.y == y && cell.glyph == glyph && &cell.style == style)
}

#[test]
fn style_parser_maps_supported_forms_to_exact_ansi_bytes() {
    assert_eq!(style_sgr_bytes(&border_style(None), false), b"\x1b[0m");
    assert_eq!(
        style_sgr_bytes(&border_style(Some("default")), false),
        b"\x1b[0m"
    );
    assert_eq!(
        style_sgr_bytes(&border_style(Some("colour214")), false),
        b"\x1b[38;5;214m"
    );

    for (value, sgr) in [
        ("black", b"\x1b[30m".as_slice()),
        ("red", b"\x1b[31m".as_slice()),
        ("green", b"\x1b[32m".as_slice()),
        ("yellow", b"\x1b[33m".as_slice()),
        ("blue", b"\x1b[34m".as_slice()),
        (concat!("mag", "enta"), b"\x1b[35m".as_slice()),
        ("cyan", b"\x1b[36m".as_slice()),
        ("white", b"\x1b[37m".as_slice()),
        ("brightblack", b"\x1b[90m".as_slice()),
        ("brightred", b"\x1b[91m".as_slice()),
        ("brightgreen", b"\x1b[92m".as_slice()),
        ("brightyellow", b"\x1b[93m".as_slice()),
        ("brightblue", b"\x1b[94m".as_slice()),
        (concat!("bright", "mag", "enta"), b"\x1b[95m".as_slice()),
        ("brightcyan", b"\x1b[96m".as_slice()),
        ("brightwhite", b"\x1b[97m".as_slice()),
    ] {
        assert_eq!(style_sgr_bytes(&border_style(Some(value)), false), sgr);
    }

    assert_eq!(
        style_sgr_bytes(&parse_standalone_style(Some("fg=red")), false),
        b"\x1b[31m"
    );
    assert_eq!(
        style_sgr_bytes(
            &parse_standalone_style(Some("bg=green,fg=black,bold,reverse")),
            false,
        ),
        b"\x1b[0;1;7;30;42m"
    );
    assert_eq!(
        style_sgr_bytes(
            &parse_standalone_style(Some("fg=colour214,bg=brightblue")),
            false
        ),
        b"\x1b[0;38;5;214;104m"
    );
}

#[test]
fn sessions_without_visible_borders_emit_status_only_when_enabled() {
    let session = Session::new(session_name("alpha"), TerminalSize { cols: 80, rows: 24 });
    assert!(border_cells(
        session.window(),
        session.active_pane_index(),
        Style::default(),
        Style::default()
    )
    .is_empty());
    let default_frame =
        String::from_utf8(render(&session, &OptionStore::new())).expect("status frame is utf-8");
    assert!(default_frame.contains("[alpha]"));
    assert!(!default_frame.contains('┬'));

    let mut status_off = OptionStore::new();
    status_off
        .set(
            ScopeSelector::Session(session.name().clone()),
            OptionName::Status,
            "off".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("status off succeeds");
    assert!(render(&session, &status_off).is_empty());

    let mut narrow = Session::new(session_name("narrow"), TerminalSize { cols: 3, rows: 2 });
    narrow.split_active_pane().expect("split succeeds");
    narrow.resize_terminal(TerminalSize { cols: 1, rows: 2 });
    assert!(!render(&narrow, &OptionStore::new()).is_empty());

    let mut zero_height = Session::new(session_name("flat"), TerminalSize { cols: 80, rows: 3 });
    zero_height
        .split_active_pane_with_direction(SplitDirection::Horizontal)
        .expect("split succeeds");
    zero_height.resize_terminal(TerminalSize { cols: 80, rows: 0 });
    assert!(render(&zero_height, &OptionStore::new()).is_empty());
}

#[test]
fn zoomed_sessions_clear_before_redrawing_active_pane() {
    let mut session = Session::new(session_name("alpha"), TerminalSize { cols: 80, rows: 24 });
    session.split_active_pane().expect("split succeeds");
    session
        .resize_pane(0, ResizePaneAdjustment::Zoom)
        .expect("zoom succeeds");

    let frame = render(&session, &OptionStore::new());
    assert!(
        frame.starts_with(b"\x1b[0m\x1b[H\x1b[2J"),
        "zoom repaint must clear stale non-active pane cells before drawing"
    );
}

#[test]
fn two_pane_sessions_render_the_main_vertical_border_column_and_exact_frame_bytes() {
    let mut session = Session::new(session_name("alpha"), TerminalSize { cols: 4, rows: 2 });
    session.split_active_pane().expect("split succeeds");
    let cells = border_cells(
        session.window(),
        session.active_pane_index(),
        border_style(Some("red")),
        border_style(Some("red")),
    );

    assert!(has_cell(&cells, 2, 0, '│'));
    assert!(has_cell(&cells, 2, 1, '│'));
    assert_eq!(cells.len(), 2);
    assert_eq!(
        super::render_cells(&cells),
        b"\x1b[s\x1b[0m\x1b[1;3H\x1b[31m\xe2\x94\x82\x1b[2;3H\xe2\x94\x82\x1b[0m\x1b[u"
    );
}

#[test]
fn two_pane_sessions_colour_only_the_active_half_of_the_shared_border() {
    let mut session = Session::new(session_name("alpha"), TerminalSize { cols: 10, rows: 4 });
    session.split_active_pane().expect("split succeeds");
    let inactive = border_style(Some("blue"));
    let active = border_style(Some("red"));
    let cells = border_cells(
        session.window(),
        session.active_pane_index(),
        inactive.clone(),
        active.clone(),
    );

    assert!(has_styled_cell(&cells, 5, 0, '│', &inactive));
    assert!(has_styled_cell(&cells, 5, 1, '│', &inactive));
    assert!(has_styled_cell(&cells, 5, 3, '│', &active));
}

#[test]
fn three_pane_sessions_render_full_height_vertical_dividers() {
    let session = session_with_three_panes();
    let cells = border_cells(
        session.window(),
        session.active_pane_index(),
        Style::default(),
        Style::default(),
    );

    assert!(has_cell(&cells, 40, 0, '│'));
    assert!(has_cell(&cells, 40, 12, '│'));
    assert!(has_cell(&cells, 60, 0, '│'));
    assert!(has_cell(&cells, 60, 12, '│'));
    assert!(has_cell(&cells, 60, 23, '│'));
}

#[test]
fn four_pane_sessions_keep_vertical_splits_as_full_height_bars() {
    let mut session = session_with_three_panes();
    session.split_pane(2).expect("third split succeeds");
    let cells = border_cells(
        session.window(),
        session.active_pane_index(),
        Style::default(),
        Style::default(),
    );

    assert_eq!(
        cells.iter().filter(|cell| cell.glyph == '┬').count(),
        0,
        "parallel vertical splits should not sprout top tees at the screen edge"
    );
    assert_eq!(
        cells.iter().filter(|cell| cell.glyph == '┴').count(),
        0,
        "parallel vertical splits should not sprout bottom tees above the status line"
    );
}

#[test]
fn lower_vertical_split_joins_top_bottom_border_with_a_top_tee() {
    let mut session = Session::new(session_name("alpha"), TerminalSize { cols: 80, rows: 24 });
    let bottom = session
        .split_active_pane_with_direction(SplitDirection::Horizontal)
        .expect("horizontal split succeeds");
    session
        .split_pane_with_direction(bottom, SplitDirection::Vertical)
        .expect("vertical split succeeds");
    let cells = border_cells(
        session.window(),
        session.active_pane_index(),
        Style::default(),
        Style::default(),
    );

    let top_geometry = session
        .window()
        .pane(0)
        .expect("top pane exists")
        .geometry();
    let lower_left_geometry = session
        .window()
        .pane(bottom)
        .expect("lower-left pane exists")
        .geometry();
    let junction_x = lower_left_geometry
        .x()
        .saturating_add(lower_left_geometry.cols());
    let junction_y = top_geometry.y().saturating_add(top_geometry.rows());

    assert!(has_cell(&cells, junction_x, junction_y, '┬'));
    assert!(!has_cell(&cells, junction_x, junction_y, '┼'));
    assert!(!has_cell(
        &cells,
        junction_x,
        junction_y.saturating_sub(1),
        '│'
    ));
}

#[test]
fn active_and_inactive_styles_follow_the_active_pane_border_segments() {
    let mut session = session_with_three_panes();
    session.select_pane(0).expect("pane selection succeeds");
    let active = border_style(Some("red"));
    let inactive = border_style(Some("blue"));
    let cells = border_cells(
        session.window(),
        session.active_pane_index(),
        inactive.clone(),
        active.clone(),
    );

    assert!(has_styled_cell(&cells, 40, 18, '│', &active));
    assert!(has_styled_cell(&cells, 60, 6, '│', &inactive));
    assert!(has_styled_cell(&cells, 60, 23, '│', &inactive));
    assert!(has_styled_cell(&cells, 40, 23, '│', &active));
    assert!(!cells.iter().any(|cell| cell.y == 12 && cell.glyph == '─'));
}

#[test]
fn renderer_uses_session_option_resolution_and_renders_status_when_enabled() {
    let mut session = session_with_three_panes();
    session.select_pane(0).expect("pane selection succeeds");
    let session_name = session.name().clone();
    let window = WindowTarget::with_window(session_name.clone(), 0);
    let mut options = OptionStore::new();
    for (scope, option, value) in [
        (ScopeSelector::Global, OptionName::PaneBorderStyle, "blue"),
        (
            ScopeSelector::Window(window.clone()),
            OptionName::PaneBorderStyle,
            "yellow",
        ),
        (
            ScopeSelector::Window(window),
            OptionName::PaneActiveBorderStyle,
            "colour196",
        ),
        (
            ScopeSelector::Session(session_name.clone()),
            OptionName::Status,
            "off",
        ),
        (
            ScopeSelector::Session(session_name.clone()),
            OptionName::StatusLeft,
            "status #{session_name}",
        ),
    ] {
        options
            .set(scope, option, value.to_owned(), SetOptionMode::Replace)
            .expect("option set succeeds");
    }

    let frame = render(&session, &options);
    let frame_text = String::from_utf8_lossy(&frame);

    assert!(frame_text.contains("\u{1b}[33m"));
    assert!(frame_text.contains("\u{1b}[38;5;196m"));
    assert!(frame_text.contains('│'));
    assert!(!frame_text.contains('┬'));
    assert!(!frame_text.contains('┴'));
    assert!(!frame_text.contains("status"));

    options
        .set(
            ScopeSelector::Session(session_name),
            OptionName::Status,
            "on".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("status on succeeds");
    let status_frame = render(&session, &options);
    let status_text = String::from_utf8_lossy(&status_frame);
    assert!(status_text.contains("status al"));
    assert!(status_text.contains("\u{1b}[24;1H"));
}

#[test]
fn top_status_reserves_the_first_row_and_offsets_border_cells() {
    let session = session_with_three_panes();
    let session_name = session.name().clone();
    let mut options = OptionStore::new();
    options
        .set(
            ScopeSelector::Session(session_name),
            OptionName::StatusPosition,
            "top".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("status-position top succeeds");

    let frame = String::from_utf8(render(&session, &options)).expect("frame is utf-8");

    assert!(frame.contains("\u{1b}[1;1H"));
    assert!(frame.contains("\u{1b}[2;41H"));
    assert!(!frame.contains("\u{1b}[1;41H┬"));
}

#[test]
fn status_window_list_uses_expanded_truncation_justify_and_raw_flags() {
    let mut session = Session::new(session_name("alpha"), TerminalSize { cols: 20, rows: 4 });
    session
        .insert_window_with_initial_pane(1, TerminalSize { cols: 20, rows: 4 })
        .expect("window 1 insert succeeds");
    session
        .insert_window_with_initial_pane(2, TerminalSize { cols: 20, rows: 4 })
        .expect("window 2 insert succeeds");
    session.select_window(2).expect("window 2 select succeeds");
    session.select_window(1).expect("window 1 select succeeds");
    let mut options = OptionStore::new();

    for (option, value) in [
        (OptionName::StatusStyle, "default"),
        (OptionName::StatusLeft, "L#{session_name}LONG"),
        (OptionName::StatusLeftLength, "4"),
        (OptionName::StatusRight, "R#{session_windows}"),
        (OptionName::StatusRightLength, "2"),
        (OptionName::StatusJustify, "right"),
        (
            OptionName::WindowStatusFormat,
            "#{window_index}#{window_raw_flags}",
        ),
        (
            OptionName::WindowStatusCurrentFormat,
            "#{window_index}#{window_raw_flags}",
        ),
    ] {
        options
            .set(
                ScopeSelector::Global,
                option,
                value.to_owned(),
                SetOptionMode::Replace,
            )
            .expect("status option set succeeds");
    }

    let frame = String::from_utf8(render(&session, &options)).expect("frame is utf-8");

    assert!(frame.contains("Lalp"), "{frame}");
    assert!(frame.contains("1*"), "{frame}");
    assert!(frame.contains("R3"), "{frame}");
}

#[test]
fn status_fill_applies_background_when_text_background_is_default() {
    let session = Session::new(session_name("alpha"), TerminalSize { cols: 8, rows: 2 });
    let mut options = OptionStore::new();

    for (option, value) in [
        (OptionName::StatusStyle, "fill=blue"),
        (OptionName::StatusLeft, "X"),
        (OptionName::StatusRight, ""),
        (OptionName::WindowStatusFormat, ""),
        (OptionName::WindowStatusCurrentFormat, ""),
    ] {
        options
            .set(
                ScopeSelector::Global,
                option,
                value.to_owned(),
                SetOptionMode::Replace,
            )
            .expect("status option set succeeds");
    }

    let frame = String::from_utf8(render(&session, &options)).expect("frame is utf-8");
    assert!(frame.contains("\u{1b}[44m"));
}

#[test]
fn status_only_render_starts_from_a_reset_sgr_state() {
    let session = Session::new(session_name("alpha"), TerminalSize { cols: 8, rows: 2 });

    let frame = String::from_utf8(render(&session, &OptionStore::new())).expect("frame is utf-8");
    assert!(frame.starts_with("\u{1b}[s\u{1b}[0m"));
}

#[test]
fn prompt_status_render_positions_cursor_on_the_input_cell() {
    let session = Session::new(session_name("alpha"), TerminalSize { cols: 20, rows: 4 });
    let prompt = super::RenderedPrompt {
        prompt: "rename-window ".to_owned(),
        input: String::new(),
        cursor: 0,
        command_prompt: false,
    };

    let frame = String::from_utf8(super::render_with_attached_count_and_prompt(
        &session,
        &OptionStore::new(),
        1,
        Some(&prompt),
    ))
    .expect("frame is utf-8");

    assert!(
        frame.ends_with("\u{1b}[4;15H"),
        "prompt cursor should land after the prompt label, got {frame:?}"
    );
}

#[test]
fn border_render_starts_from_a_reset_sgr_state() {
    let mut session = Session::new(session_name("alpha"), TerminalSize { cols: 8, rows: 4 });
    session.split_active_pane().expect("split succeeds");

    let frame = String::from_utf8(render(&session, &OptionStore::new())).expect("frame is utf-8");
    assert!(frame.starts_with("\u{1b}[s\u{1b}[0m"));
}

#[test]
fn status_bar_runs_include_session_attached_in_status_context() {
    let session = Session::new(session_name("alpha"), TerminalSize { cols: 4, rows: 2 });
    let mut options = OptionStore::new();

    for (option, value) in [
        (OptionName::StatusLeft, "#{session_attached}"),
        (OptionName::StatusRight, ""),
        (OptionName::WindowStatusFormat, ""),
        (OptionName::WindowStatusCurrentFormat, ""),
    ] {
        options
            .set(
                ScopeSelector::Global,
                option,
                value.to_owned(),
                SetOptionMode::Replace,
            )
            .expect("status option set succeeds");
    }

    let rendered_with_attach = status_bar_runs(&session, &options, 4, 1)
        .into_iter()
        .map(|run| run.text)
        .collect::<String>();
    let rendered_without_attach = status_bar_runs(&session, &options, 4, 0)
        .into_iter()
        .map(|run| run.text)
        .collect::<String>();

    assert_eq!(rendered_with_attach, "1   ");
    assert_eq!(rendered_without_attach, "0   ");
}

#[test]
fn status_message_text_cannot_emit_control_characters_into_the_status_row() {
    let session = Session::new(session_name("alpha"), TerminalSize { cols: 20, rows: 4 });
    let frame = String::from_utf8(super::render_status_message(
        &session,
        &OptionStore::new(),
        "hi\nthere\t\x1b[31m",
    ))
    .expect("status message frame is utf-8");

    assert!(!frame.contains('\n'));
    assert!(!frame.contains('\t'));
    assert!(frame.contains("hi there  [31m"));
}

#[test]
fn status_message_renders_default_message_style_from_message_format() {
    let session = Session::new(session_name("alpha"), TerminalSize { cols: 20, rows: 4 });
    let frame = String::from_utf8(super::render_status_message(
        &session,
        &OptionStore::new(),
        "No next window",
    ))
    .expect("status message frame is utf-8");

    assert!(
        frame.contains("\x1b[0;30;43m") || frame.contains("\x1b[30;43m"),
        "default message-format should expand message-style inside the style clause, got {frame:?}"
    );
}

#[test]
fn status_message_style_fills_the_full_status_line() {
    let session = Session::new(session_name("alpha"), TerminalSize { cols: 20, rows: 4 });
    let frame = String::from_utf8(super::render_status_message(
        &session,
        &OptionStore::new(),
        "No next window",
    ))
    .expect("status message frame is utf-8");

    assert!(
        frame.contains("\x1b[0;30;43mNo next window      \x1b[0m")
            || frame.contains("\x1b[30;43mNo next window      \x1b[0m"),
        "message-style should fill the whole status row, got {frame:?}"
    );
}

#[test]
fn status_message_truncates_by_display_width_instead_of_scalar_count() {
    let session = Session::new(session_name("alpha"), TerminalSize { cols: 3, rows: 4 });
    let frame = String::from_utf8(super::render_status_message(
        &session,
        &OptionStore::new(),
        "表ab",
    ))
    .expect("status message frame is utf-8");

    assert!(frame.contains("表a"));
    assert!(!frame.contains("表ab"));
}

#[test]
fn status_bar_spacing_uses_display_width_for_cjk_and_emoji() {
    let session = Session::new(session_name("alpha"), TerminalSize { cols: 6, rows: 4 });
    let mut options = OptionStore::new();

    for (option, value) in [
        (OptionName::StatusLeft, "表A"),
        (OptionName::StatusRight, "🇨🇭"),
        (OptionName::WindowStatusFormat, ""),
        (OptionName::WindowStatusCurrentFormat, ""),
    ] {
        options
            .set(
                ScopeSelector::Global,
                option,
                value.to_owned(),
                SetOptionMode::Replace,
            )
            .expect("status option set succeeds");
    }

    let rendered = status_bar_runs(&session, &options, 6, 0)
        .into_iter()
        .map(|run| run.text)
        .collect::<String>();

    assert_eq!(rendered, "表A 🇨🇭");
}

#[test]
fn pane_active_border_style_conditionals_are_runtime_expanded() {
    let mut session = Session::new(session_name("alpha"), TerminalSize { cols: 10, rows: 4 });
    session.split_active_pane().expect("split succeeds");
    let session_name = session.name().clone();
    let window = WindowTarget::with_window(session_name.clone(), 0);
    let mut options = OptionStore::new();

    options
        .set(
            ScopeSelector::Window(window.clone()),
            OptionName::PaneBorderStyle,
            "green".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("inactive border style set succeeds");
    options
        .set(
            ScopeSelector::Window(window),
            OptionName::PaneActiveBorderStyle,
            "#{?pane_active,red,blue}".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("active border style set succeeds");

    let frame = render(&session, &options);
    let frame_text = String::from_utf8_lossy(&frame);

    assert!(frame_text.contains("\u{1b}[32m"));
    assert!(frame_text.contains("\u{1b}[31m"));
    assert!(!frame_text.contains("\u{1b}[34m"));
}
