use rmux_core::{input::InputParser, OptionStore, Screen, Session};
use rmux_proto::{OptionName, ScopeSelector, SessionName, SetOptionMode, TerminalSize};

use crate::copy_mode::{CopyModeOverlayRange, CopyModeRenderOverlays, CopyModeRenderSnapshot};

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

fn screen_with(bytes: &[u8], size: TerminalSize) -> Screen {
    let mut screen = Screen::new(size, 100);
    let mut parser = InputParser::new();
    parser.parse(bytes, &mut screen);
    screen
}

fn visible_colours(screen: &Screen, cols: usize) -> Vec<(i32, i32)> {
    let mut colours = Vec::new();
    assert!(screen.visit_visible_line_cells(0, cols, |cell| {
        colours.push((cell.fg(), cell.bg()));
    }));
    colours
}

fn line_number_options(mode: &str) -> OptionStore {
    let mut options = OptionStore::new();
    options
        .set(
            ScopeSelector::Global,
            OptionName::CopyModeLineNumbers,
            mode.to_owned(),
            SetOptionMode::Replace,
        )
        .expect("copy-mode line-number option set succeeds");
    options
}

fn line_number_snapshot(
    screen: Screen,
    history_size: usize,
    scroll_position: usize,
) -> CopyModeRenderSnapshot {
    CopyModeRenderSnapshot {
        screen,
        overlays: CopyModeRenderOverlays::default(),
        history_size,
        scroll_position,
        alternate_on: false,
        line_numbers_enabled: true,
    }
}

#[test]
fn copy_mode_overlays_follow_tmux_37b_priority_and_mark_the_full_line() {
    let size = TerminalSize { cols: 8, rows: 2 };
    let session = Session::new(session_name("alpha"), size);
    let pane = session.window().pane(0).expect("pane exists");
    let mut screen = screen_with(b"abcdefgh", size);
    screen.mark_selected_row_range(0, 4, 4);
    let overlays = CopyModeRenderOverlays {
        mark: Some(CopyModeOverlayRange {
            row: 0,
            start_x: 0,
            end_x: 7,
        }),
        matches: vec![CopyModeOverlayRange {
            row: 0,
            start_x: 1,
            end_x: 5,
        }],
        current_match: Some(CopyModeOverlayRange {
            row: 0,
            start_x: 3,
            end_x: 5,
        }),
    };

    let styled = super::pane_screen::styled_copy_mode_pane_screen(
        &session,
        &OptionStore::new(),
        pane,
        &screen,
        &overlays,
    );
    let colours = visible_colours(&styled, 8);

    assert_eq!(colours[0], (0, 1), "mark style starts at column zero");
    assert_eq!(colours[1], (0, 6), "regular match overrides mark");
    assert_eq!(colours[3], (0, 5), "current match overrides regular match");
    assert_eq!(colours[4], (0, 3), "selection is the final overlay");
    assert_eq!(colours[5], (0, 5), "current match covers its full range");
    assert_eq!(colours[7], (0, 1), "mark style covers the complete line");
}

#[test]
fn copy_mode_overlay_options_resolve_runtime_templates_and_explicit_styles() {
    let size = TerminalSize { cols: 4, rows: 2 };
    let session = Session::new(session_name("alpha"), size);
    let pane = session.window().pane(0).expect("pane exists");
    let screen = screen_with(b"abcd", size);
    let mut options = OptionStore::new();
    for (option, value) in [
        (OptionName::ModeStyle, "bg=blue,fg=white"),
        (OptionName::CopyModeMarkStyle, "#{E:mode-style}"),
        (OptionName::CopyModeMatchStyle, "bg=green,fg=white"),
        (OptionName::CopyModeCurrentMatchStyle, "bg=yellow,fg=black"),
    ] {
        options
            .set(
                ScopeSelector::Global,
                option,
                value.to_owned(),
                SetOptionMode::Replace,
            )
            .expect("copy-mode style option set succeeds");
    }
    let overlays = CopyModeRenderOverlays {
        mark: Some(CopyModeOverlayRange {
            row: 0,
            start_x: 0,
            end_x: 3,
        }),
        matches: vec![CopyModeOverlayRange {
            row: 0,
            start_x: 1,
            end_x: 2,
        }],
        current_match: Some(CopyModeOverlayRange {
            row: 0,
            start_x: 2,
            end_x: 2,
        }),
    };

    let styled = super::pane_screen::styled_copy_mode_pane_screen(
        &session, &options, pane, &screen, &overlays,
    );
    let colours = visible_colours(&styled, 4);

    assert_eq!(colours[0], (7, 4), "mark expands mode-style at runtime");
    assert_eq!(colours[1], (7, 2), "match style option is consumed");
    assert_eq!(
        colours[2],
        (0, 3),
        "current-match style option overrides match"
    );
    assert_eq!(colours[3], (7, 4), "mark resumes after the match");
}

#[test]
fn rendered_copy_mode_frame_paints_marked_trailing_cells() {
    let size = TerminalSize { cols: 6, rows: 2 };
    let session = Session::new(session_name("alpha"), size);
    let pane = session.window().pane(0).expect("pane exists");
    let snapshot = CopyModeRenderSnapshot {
        screen: screen_with(b"x", size),
        history_size: 0,
        scroll_position: 0,
        alternate_on: false,
        line_numbers_enabled: false,
        overlays: CopyModeRenderOverlays {
            mark: Some(CopyModeOverlayRange {
                row: 0,
                start_x: 0,
                end_x: 5,
            }),
            ..CopyModeRenderOverlays::default()
        },
    };

    let frame = super::pane_screen::render_copy_mode_pane_screen(
        &session,
        &OptionStore::new(),
        pane,
        &snapshot,
    );

    assert!(
        frame
            .windows(b"x     ".len())
            .any(|window| window == b"x     "),
        "mark rendering must paint trailing cells instead of clearing them: {frame:?}"
    );
}

#[test]
fn copy_mode_line_number_gutter_uses_tmux_default_and_current_styles() {
    let size = TerminalSize { cols: 20, rows: 3 };
    let session = Session::new(session_name("alpha"), size);
    let pane = session.window().pane(0).expect("pane exists");
    let snapshot = line_number_snapshot(screen_with(b"alpha\r\nbeta", size), 0, 0);

    let frame = String::from_utf8(super::pane_screen::render_copy_mode_pane_screen(
        &session,
        &line_number_options("absolute"),
        pane,
        &snapshot,
    ))
    .expect("copy-mode frame is utf-8");

    assert!(
        frame.contains("\u{1b}[0;2;37m  1 \u{1b}[0malpha"),
        "non-current numbers should use fg=white,dim: {frame:?}"
    );
    assert!(
        frame.contains("\u{1b}[33m  2 \u{1b}[0mbeta"),
        "the cursor row should use fg=yellow: {frame:?}"
    );
}

#[test]
fn copy_mode_line_number_gutter_is_inside_the_scrollbar_content_geometry() {
    let size = TerminalSize { cols: 20, rows: 3 };
    let session = Session::new(session_name("alpha"), size);
    let pane = session.window().pane(0).expect("pane exists");
    let mut options = line_number_options("absolute");
    options
        .set(
            ScopeSelector::Global,
            OptionName::PaneScrollbars,
            "on".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("pane scrollbar option set succeeds");
    let snapshot = line_number_snapshot(screen_with(b"content", size), 23, 23);

    let frame = String::from_utf8(super::pane_screen::render_copy_mode_pane_screen(
        &session, &options, pane, &snapshot,
    ))
    .expect("copy-mode frame is utf-8");

    assert!(
        frame.contains("\u{1b}[33m  1 \u{1b}[0mcontent"),
        "the gutter should precede pane content: {frame:?}"
    );
    assert!(
        frame.contains("\u{1b}[1;20H"),
        "the right scrollbar should remain in physical column 20: {frame:?}"
    );
}

#[test]
fn copy_mode_line_number_gutter_marks_a_cursor_beyond_reduced_content() {
    let size = TerminalSize { cols: 8, rows: 2 };
    let session = Session::new(session_name("alpha"), size);
    let pane = session.window().pane(0).expect("pane exists");
    let snapshot = line_number_snapshot(screen_with(b"\x1b[1;8H", size), 0, 0);

    let frame = String::from_utf8(super::pane_screen::render_copy_mode_pane_screen(
        &session,
        &line_number_options("absolute"),
        pane,
        &snapshot,
    ))
    .expect("copy-mode frame is utf-8");

    assert!(
        frame.contains("\u{1b}[1;8H\u{1b}[0m$\u{1b}[0m"),
        "tmux marks a cursor clipped by the gutter with '$': {frame:?}"
    );
}
