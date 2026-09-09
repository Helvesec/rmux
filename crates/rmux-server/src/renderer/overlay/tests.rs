use super::{
    render_menu_overlay, render_popup_overlay, resolve_overlay_rect, status_line_layout,
    MenuRenderItem, MenuRenderSpec, OverlayMousePosition, OverlayPositionContext, OverlayRect,
    PopupContent, PopupRenderSpec,
};
use crate::format_runtime::RuntimeFormatContext;
use rmux_core::{BoxLines, OptionStore, Session, Style};
use rmux_proto::{OptionName, ScopeSelector, SessionName, SetOptionMode, TerminalSize};

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

fn session_with_windows() -> Session {
    let mut session = Session::new(session_name("alpha"), TerminalSize { cols: 80, rows: 24 });
    session
        .create_window(TerminalSize { cols: 80, rows: 24 })
        .expect("window create succeeds");
    session
}

#[test]
fn overlay_position_resolves_tmux_shorthand_positions() {
    let mut session = Session::new(session_name("alpha"), TerminalSize { cols: 80, rows: 24 });
    session.split_active_pane().expect("split succeeds");
    let pane = session
        .window()
        .active_pane()
        .expect("active pane")
        .geometry();
    let runtime =
        RuntimeFormatContext::new(rmux_core::formats::FormatContext::from_session(&session))
            .with_session(&session)
            .with_window(session.active_window_index(), session.window());
    let rect = resolve_overlay_rect(
        runtime,
        OverlayPositionContext {
            client_size: TerminalSize { cols: 80, rows: 24 },
            pane: Some(pane),
            mouse: Some(OverlayMousePosition { x: 10, y: 7 }),
            status_at: Some(23),
            status_lines: 1,
            window_status_x: Some(22),
        },
        Some("M"),
        Some("W"),
        20,
        10,
    )
    .expect("position resolves");
    assert_eq!(rect.x, 0);
    assert_eq!(rect.y, 13);
}

#[test]
fn menu_overlay_renders_separators_and_right_aligned_shortcuts() {
    let frame = String::from_utf8(render_menu_overlay(&MenuRenderSpec {
        rect: OverlayRect {
            x: 5,
            y: 3,
            width: 18,
            height: 6,
        },
        title: "Menu".to_owned(),
        style: Style::default(),
        selected_style: Style::parse("reverse").expect("style parses"),
        border_style: Style::default(),
        border_lines: BoxLines::Single,
        items: vec![
            MenuRenderItem {
                label: "First".to_owned(),
                shortcut: Some("(f)".to_owned()),
                separator: false,
                selected: false,
            },
            MenuRenderItem {
                label: String::new(),
                shortcut: None,
                separator: true,
                selected: false,
            },
            MenuRenderItem {
                label: "Second".to_owned(),
                shortcut: Some("(s)".to_owned()),
                separator: false,
                selected: true,
            },
        ],
    }))
    .expect("utf-8 frame");
    assert!(frame.contains("┌"));
    assert!(frame.contains("\u{1b}[6;6H\u{1b}[0m├────────────────┤"));
    assert!(frame.contains("First"));
    assert!(frame.contains("(f)"));
    assert!(frame.contains("Second"));
    assert!(frame.contains("\u{1b}[7;7H\u{1b}[0;7m                "));
}

#[test]
fn menu_overlay_titles_honour_inline_alignment_directives() {
    let frame = String::from_utf8(render_menu_overlay(&MenuRenderSpec {
        rect: OverlayRect {
            x: 5,
            y: 3,
            width: 18,
            height: 4,
        },
        title: "#[align=centre]Menu".to_owned(),
        style: Style::default(),
        selected_style: Style::parse("reverse").expect("style parses"),
        border_style: Style::default(),
        border_lines: BoxLines::Single,
        items: vec![],
    }))
    .expect("utf-8 frame");
    assert!(!frame.contains("align=centre"));
    assert!(frame.contains("\u{1b}[4;8H"));
    assert!(frame.contains("Menu"));
}

#[test]
fn menu_overlay_items_render_inline_styles_without_leaking_clause_text() {
    let frame = String::from_utf8(render_menu_overlay(&MenuRenderSpec {
        rect: OverlayRect {
            x: 1,
            y: 1,
            width: 16,
            height: 4,
        },
        title: "Menu".to_owned(),
        style: Style::default(),
        selected_style: Style::parse("reverse").expect("style parses"),
        border_style: Style::default(),
        border_lines: BoxLines::Single,
        items: vec![MenuRenderItem {
            label: "#[fg=red]Hot#[default]Key".to_owned(),
            shortcut: None,
            separator: false,
            selected: false,
        }],
    }))
    .expect("utf-8 frame");
    assert!(!frame.contains("fg=red"));
    assert!(frame.contains("\u{1b}[31mHot"));
    assert!(frame.contains("Key"));
}

#[test]
fn popup_overlay_titles_honour_inline_alignment_directives() {
    let frame = String::from_utf8(render_popup_overlay(&PopupRenderSpec {
        rect: OverlayRect {
            x: 2,
            y: 1,
            width: 12,
            height: 4,
        },
        title: "#[align=right]Popup".to_owned(),
        style: Style::default(),
        border_style: Style::default(),
        border_lines: BoxLines::Single,
        content: PopupContent::Text(vec!["body".to_owned()]),
    }))
    .expect("utf-8 frame");
    assert!(!frame.contains("align=right"));
    assert!(frame.contains("\u{1b}[2;5H"));
    assert!(frame.contains("Popup"));
}

#[test]
fn popup_overlay_content_renders_inline_styles_without_clause_text() {
    let frame = String::from_utf8(render_popup_overlay(&PopupRenderSpec {
        rect: OverlayRect {
            x: 0,
            y: 0,
            width: 14,
            height: 4,
        },
        title: "Popup".to_owned(),
        style: Style::default(),
        border_style: Style::default(),
        border_lines: BoxLines::Single,
        content: PopupContent::Text(vec!["#[fg=green]body#[default]".to_owned()]),
    }))
    .expect("utf-8 frame");
    assert!(!frame.contains("fg=green"));
    assert!(frame.contains("\u{1b}[32mbody"));
}

fn surface_popup_frame(width: u16, rows: Vec<Vec<u8>>) -> String {
    String::from_utf8(render_popup_overlay(&PopupRenderSpec {
        rect: OverlayRect {
            x: 0,
            y: 0,
            width,
            height: 4,
        },
        title: "Popup".to_owned(),
        style: Style::default(),
        border_style: Style::default(),
        border_lines: BoxLines::Single,
        content: PopupContent::Surface(rows),
    }))
    .expect("utf-8 frame")
}

#[test]
fn popup_overlay_surface_rows_keep_process_colours_and_attributes() {
    // A popup hosts terminal output: the SGR the process emitted must reach the
    // client verbatim instead of being flattened into plain text (issue #181).
    for sequence in [
        "\u{1b}[31m",             // basic 16-colour
        "\u{1b}[38;5;208m",       // indexed 256-colour
        "\u{1b}[38;2;10;200;30m", // truecolour
        "\u{1b}[1m",              // bold
        "\u{1b}[4m",              // underline
        "\u{1b}[7m",              // reverse
    ] {
        let row = format!("{sequence}body\u{1b}[0m").into_bytes();
        let frame = surface_popup_frame(14, vec![row]);
        assert!(
            frame.contains(&format!("{sequence}body")),
            "surface row must keep {sequence:?}: {frame:?}"
        );
    }
}

#[test]
fn popup_overlay_surface_rows_are_never_format_expanded() {
    // Process output is not a status template: a literal `#[fg=red]` must stay
    // visible text, and a control byte must not be scrubbed into a space the
    // way the static-text path scrubs it.
    let frame = surface_popup_frame(20, vec![b"#[fg=red]x".to_vec()]);
    assert!(
        frame.contains("#[fg=red]x"),
        "literal clause text must survive: {frame:?}"
    );
    assert!(
        !frame.contains("\u{1b}[31m"),
        "literal clause text must not be applied as a style: {frame:?}"
    );
}

#[test]
fn popup_overlay_surface_rows_clip_without_splitting_escape_sequences() {
    // The inner width is 3 here, so the row is clipped mid-content. Clipping
    // must drop visible columns, never half of an SGR sequence.
    let frame = surface_popup_frame(
        5,
        vec![b"\x1b[31mABCDEFGH\x1b[0m".to_vec(), b"second".to_vec()],
    );
    assert!(frame.contains("\u{1b}[31mABC"), "clipped row: {frame:?}");
    assert!(!frame.contains('D'), "columns past the width: {frame:?}");
    assert!(
        frame.contains("sec"),
        "second row is clipped too: {frame:?}"
    );
    assert!(!frame.contains("second"), "second row overflow: {frame:?}");
}

#[test]
fn popup_overlay_surface_rows_stop_at_the_content_height() {
    let rows = (0..8)
        .map(|index| format!("row{index}").into_bytes())
        .collect::<Vec<_>>();
    let frame = surface_popup_frame(12, rows);
    assert!(frame.contains("row0"), "first row drawn: {frame:?}");
    assert!(frame.contains("row1"), "second row drawn: {frame:?}");
    assert!(
        !frame.contains("row2"),
        "a 4-row popup with a border has two content rows: {frame:?}"
    );
}

#[test]
fn popup_overlay_surface_rows_reset_around_each_row() {
    let frame = surface_popup_frame(14, vec![b"\x1b[41mred".to_vec()]);
    let row = frame
        .split("\u{1b}7")
        .find(|chunk| chunk.contains("\u{1b}[41mred"))
        .expect("surface row is emitted");
    assert!(
        row.starts_with("\u{1b}[0m"),
        "a row must start from a known state: {row:?}"
    );
    assert!(
        row.ends_with("\u{1b}[0m\u{1b}8"),
        "a row must not leak its attributes into the frame: {row:?}"
    );
}

#[test]
fn popup_overlay_uses_every_box_line_variant() {
    for (lines, corner) in [
        (BoxLines::Single, '┌'),
        (BoxLines::Double, '╔'),
        (BoxLines::Heavy, '┏'),
        (BoxLines::Simple, '+'),
        (BoxLines::Rounded, '╭'),
        (BoxLines::Padded, ' '),
    ] {
        let frame = String::from_utf8(render_popup_overlay(&PopupRenderSpec {
            rect: OverlayRect {
                x: 0,
                y: 0,
                width: 10,
                height: 4,
            },
            title: "Popup".to_owned(),
            style: Style::default(),
            border_style: Style::default(),
            border_lines: lines,
            content: PopupContent::Text(vec!["body".to_owned()]),
        }))
        .expect("utf-8 frame");
        assert!(frame.contains(corner));
    }
}

#[test]
fn status_layout_marks_left_window_and_right_ranges() {
    let session = session_with_windows();
    let mut options = OptionStore::new();
    options
        .set(
            ScopeSelector::Session(session.name().clone()),
            OptionName::StatusLeft,
            "[left]".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("left option set");
    options
        .set(
            ScopeSelector::Session(session.name().clone()),
            OptionName::StatusRight,
            "[right]".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("right option set");

    let layout = status_line_layout(&session, &options, 0, None, None).expect("layout exists");
    assert!(layout
        .ranges
        .iter()
        .any(|range| matches!(range.kind, crate::status_ranges::StatusRangeType::Left)));
    assert!(layout
        .ranges
        .iter()
        .any(|range| matches!(range.kind, crate::status_ranges::StatusRangeType::Window(_))));
    assert!(layout
        .ranges
        .iter()
        .any(|range| matches!(range.kind, crate::status_ranges::StatusRangeType::Right)));
}

#[test]
fn status_layout_tracks_inline_range_changes_inside_status_left() {
    let session = session_with_windows();
    let mut options = OptionStore::new();
    options
        .set(
            ScopeSelector::Session(session.name().clone()),
            OptionName::StatusLeft,
            "A#[range=control|7]B".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("left option set");
    options
        .set(
            ScopeSelector::Session(session.name().clone()),
            OptionName::StatusRight,
            String::new(),
            SetOptionMode::Replace,
        )
        .expect("right option set");
    options
        .set(
            ScopeSelector::Session(session.name().clone()),
            OptionName::StatusLeftLength,
            "32".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("left length set");

    let layout = status_line_layout(&session, &options, 0, None, None).expect("layout exists");

    assert!(layout.ranges.iter().any(|range| matches!(
        range.kind,
        crate::status_ranges::StatusRangeType::Left
    ) && range.x == (0..=0)));
    assert!(layout.ranges.iter().any(|range| matches!(
        range.kind,
        crate::status_ranges::StatusRangeType::Control(7)
    ) && range.x == (1..=1)));
}

#[test]
fn popup_surface_paints_final_rows_without_a_blank_pass() {
    let spec = PopupRenderSpec {
        rect: OverlayRect {
            x: 2,
            y: 1,
            width: 8,
            height: 2,
        },
        title: String::new(),
        style: Style::default(),
        border_style: Style::default(),
        border_lines: BoxLines::None,
        content: PopupContent::Surface(vec![b"abcdefgh".to_vec(), b"12345678".to_vec()]),
    };
    let frame = render_popup_overlay(&spec);
    let frame = String::from_utf8(frame).unwrap();
    // There must be no intermediate all-blank surface, even on terminals
    // which paint the stream before the entire frame has arrived.
    assert!(!frame.contains("        "));
    assert_eq!(frame.matches("\x1b[2;3H").count(), 1);
    assert_eq!(frame.matches("\x1b[3;3H").count(), 1);
    assert!(frame.contains("abcdefgh"));
    assert!(frame.contains("12345678"));
}

#[test]
fn popup_surface_short_and_missing_rows_erase_old_cells() {
    use rmux_core::{input::InputParser, Screen};
    let size = TerminalSize { cols: 12, rows: 5 };
    let mut screen = Screen::new(size, 0);
    let mut parser = InputParser::new();
    parser.parse(b"\x1b[2;3HXXXXXXXX\x1b[3;3HYYYYYYYY", &mut screen);
    let spec = PopupRenderSpec {
        rect: OverlayRect {
            x: 2,
            y: 1,
            width: 8,
            height: 2,
        },
        title: String::new(),
        style: Style::default(),
        border_style: Style::default(),
        border_lines: BoxLines::None,
        content: PopupContent::Surface(vec!["界e\u{301}".as_bytes().to_vec()]),
    };
    parser.parse(&render_popup_overlay(&spec), &mut screen);
    let options = rmux_core::GridRenderOptions {
        trim_spaces: false,
        include_empty_cells: true,
        ..Default::default()
    };
    let first = screen
        .render_visible_line_independent_with_default_style(1, options, &Style::default())
        .unwrap();
    let second = screen
        .render_visible_line_independent_with_default_style(2, options, &Style::default())
        .unwrap();
    assert!(!first.contains(&b'X'), "{first:?}");
    assert!(!second.contains(&b'Y'), "{second:?}");
    assert!(String::from_utf8(first).unwrap().contains("界e\u{301}"));
}
