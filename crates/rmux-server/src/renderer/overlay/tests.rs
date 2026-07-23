use super::{
    render_menu_overlay, render_popup_overlay, resolve_overlay_rect, status_line_layout,
    MenuRenderItem, MenuRenderSpec, OverlayMousePosition, OverlayPositionContext, OverlayRect,
    PopupRenderSpec,
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
        content_lines: vec!["body".to_owned()],
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
        content_lines: vec!["#[fg=green]body#[default]".to_owned()],
    }))
    .expect("utf-8 frame");
    assert!(!frame.contains("fg=green"));
    assert!(frame.contains("\u{1b}[32mbody"));
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
            content_lines: vec!["body".to_owned()],
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
