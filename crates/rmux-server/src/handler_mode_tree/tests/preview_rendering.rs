use super::*;

#[test]
fn l12_tree_preview_unicode_is_not_always_empty() {
    use rmux_core::input::GridAttr;
    use rmux_proto::OptionScopeSelector;

    type PreviewVector<'a> = (&'a str, usize, u16, bool, &'a [&'a str], Option<usize>);

    // Frozen from tmux 3.7b on Linux, with a 20x24 source and a (width+4)x24
    // chooser, status off and empty preview labels. Each vector was measured
    // with the session, window and pane selected. Empty strings are wide-cell
    // continuations; e + combining acute remains one cell (pyte reports NFC).
    let vectors: &[PreviewVector<'_>] = &[
        (
            "界e\u{301}ABCDEFGHijklmnop",
            8,
            4,
            true,
            &[" ", "e\u{301}", "A", "B", "C", "D", "E", "F"],
            Some(2),
        ),
        (
            "界e\u{301}ABCDEFGHijklmnop",
            7,
            4,
            true,
            &[" ", "e\u{301}", "A", "B", "C", "D", "E"],
            Some(2),
        ),
        (
            "界e\u{301}ABCDEFGHijklmnop",
            9,
            4,
            true,
            &["界", "", "e\u{301}", "A", "B", "C", "D", "E", "F"],
            Some(3),
        ),
        (
            "0123456789abcdef",
            8,
            6,
            true,
            &["3", "4", "5", "6", "7", "8", "9", "a"],
            Some(2),
        ),
        (
            "abcdef界Z",
            7,
            1,
            true,
            &["a", "b", "c", "d", "e", "f", " "],
            Some(0),
        ),
        (
            "e\u{301}界🙂Z",
            8,
            1,
            true,
            &["e\u{301}", "界", "", "🙂", "", "Z", " ", " "],
            Some(0),
        ),
        (
            "界e\u{301}ABCDEFGHijklmnop",
            8,
            4,
            false,
            &["界", "", "e\u{301}", "A", "B", "C", "D", "E"],
            None,
        ),
    ];
    for &(text, width, cursor, visible, expected, reverse) in vectors {
        let mut state = HandlerState::default();
        let source = SessionName::new("source").unwrap();
        let host = SessionName::new("host").unwrap();
        let source_size = TerminalSize { cols: 20, rows: 24 };
        let host_size = TerminalSize {
            cols: width as u16 + 4,
            rows: 24,
        };
        state
            .sessions
            .create_session(source.clone(), source_size)
            .unwrap();
        state
            .sessions
            .create_session(host.clone(), host_size)
            .unwrap();
        let pane_id = state
            .sessions
            .session(&source)
            .unwrap()
            .active_pane()
            .unwrap()
            .id();
        state.g6_install_pane_transcript(&source, 0, pane_id, source_size);
        let bytes = format!(
            "{text}\x1b[1;{cursor}H\x1b[?25{}",
            if visible { 'h' } else { 'l' }
        );
        state
            .append_bytes_to_pane_transcript_for_test(&source, 0, 0, bytes.as_bytes())
            .unwrap();
        for (scope, name, value) in [
            (OptionScopeSelector::SessionGlobal, "status", "off"),
            (
                OptionScopeSelector::WindowGlobal,
                "tree-mode-preview-format",
                "",
            ),
            (OptionScopeSelector::WindowGlobal, "mode-style", "reverse"),
        ] {
            state
                .options
                .set_by_name(
                    scope,
                    name,
                    Some(value.into()),
                    SetOptionMode::Replace,
                    false,
                    false,
                    false,
                )
                .unwrap();
        }
        state.ensure_live_window_link_occurrences();
        let mut mode = test_mode(12);
        mode.session_name = host.clone();
        mode.session_id = state.sessions.session(&host).unwrap().id();
        mode.preview_mode = PreviewMode::Normal;
        let build =
            super::super::mode_tree_tree_build::build_tree_items(&mode, &state, &[]).unwrap();
        mode.expanded.extend(build.items.keys().cloned());
        let build =
            super::super::mode_tree_tree_build::build_tree_items(&mode, &state, &[]).unwrap();
        for depth in 0..=2 {
            let item = build.items.values().find(|item| item.depth == depth && matches!(
                &item.action, ModeTreeAction::TreeTarget { session_name, .. } if session_name == &source
            )).unwrap();
            mode.selected_id = Some(item.id.clone());
            let selected_row = build.visible.iter().position(|id| id == &item.id).unwrap();
            let frame = render_mode_tree_overlay(&state, &mode, &build);
            let mut screen = Screen::new(host_size, 0);
            InputParser::new().parse(&frame, &mut screen);
            let preview = screen.absolute_line_view(13).unwrap();
            let cells = &preview.cells()[2..width + 2];
            assert!(
                cells.iter().any(|cell| !cell.text().trim().is_empty()),
                "always-empty preview canary: {text:?}, width={width}, depth={depth}"
            );
            for (x, (cell, want)) in cells.iter().zip(expected).enumerate() {
                assert_eq!(
                    if cell.is_padding() { "" } else { cell.text() },
                    *want,
                    "text={text:?}, width={width}, depth={depth}, column={x}"
                );
                assert_eq!(cell.is_padding(), want.is_empty());
                if !want.is_empty() {
                    let columns = if matches!(*want, "界" | "🙂") {
                        2
                    } else {
                        1
                    };
                    assert_eq!(cell.width(), columns, "grapheme occupies oracle columns");
                }
                assert_eq!(
                    (cell.attr() & GridAttr::REVERSE) != 0,
                    reverse == Some(x),
                    "cursor: text={text:?}, width={width}, depth={depth}, column={x}"
                );
            }
            for row in 0..12 {
                let line = screen.absolute_line_view(row).unwrap();
                assert_eq!(
                    (line.cells()[0].attr() & GridAttr::REVERSE) != 0,
                    row == selected_row,
                    "selection must stay on the represented item at depth {depth}"
                );
            }
        }
    }
}

#[test]
fn tree_preview_layout_matches_tmux_gutters_when_not_all_items_fit() {
    let layout = tree_preview_layout(5, 2, 60).expect("layout");
    assert_eq!(layout.start, 1);
    assert_eq!(layout.end, 3);
    assert!(layout.left);
    assert!(layout.right);
    assert_eq!(layout.each, 27);
    assert_eq!(layout.remaining, 0);
}

#[test]
fn render_tree_columns_preview_draws_real_columns_and_label_boxes() {
    let utf8 = Utf8Config::default();
    let inactive = Style::parse("fg=blue").expect("style parses");
    let active = Style::parse("fg=red").expect("style parses");
    let lines = render_tree_columns_preview(
        vec![
            PreviewColumn {
                label: PreviewLabel {
                    text: "0:alpha".into(),
                    style: active,
                },
                lines: vec!["left".to_owned(); 7],
            },
            PreviewColumn {
                label: PreviewLabel {
                    text: "1:beta".into(),
                    style: inactive,
                },
                lines: vec!["right".to_owned(); 7],
            },
        ],
        0,
        60,
        7,
        &utf8,
    );

    assert_eq!(lines.len(), 7);
    assert!(lines.iter().any(|line| line.contains("left")));
    assert!(lines.iter().any(|line| line.contains("right")));
    assert!(lines.iter().any(|line| line.contains('│')));
    assert!(lines.iter().any(|line| line.contains("0:alpha")));
    assert!(lines.iter().any(|line| line.contains("1:beta")));
    assert!(
        lines.iter().any(|line| line.contains("#[fg=red]0:alpha")),
        "active preview label should carry display-panes-active-colour"
    );
}

#[test]
fn preview_lines_for_screen_preserves_cell_colours() {
    let mut screen = Screen::new(TerminalSize { cols: 20, rows: 5 }, 0);
    let mut parser = InputParser::new();
    parser.parse(b"\x1b[32mRMUXHOST\x1b[0m\r\n", &mut screen);

    let lines = preview_lines_for_screen(&screen, 3, 20, &Utf8Config::default());
    assert!(
        lines
            .iter()
            .any(|line| line.contains("#[fg=green]RMUXHOST#[default]")),
        "screen preview should preserve shell prompt colours"
    );
}

#[test]
fn preview_vertical_offset_follows_tmux_cursor_bias() {
    let mut screen = Screen::new(TerminalSize { cols: 20, rows: 10 }, 0);
    let mut parser = InputParser::new();
    parser.parse(b"1\r\n2\r\n3\r\n4\r\n5\r\n6\r\n7\r\n", &mut screen);

    assert_eq!(preview_vertical_offset(&screen, 4), 6);
}

#[test]
fn preview_horizontal_offset_follows_tmux_cursor_bias() {
    let mut screen = Screen::new(TerminalSize { cols: 20, rows: 3 }, 0);
    let mut parser = InputParser::new();
    parser.parse(b"0123456789abcdef", &mut screen);

    assert_eq!(preview_horizontal_offset(&screen, 8), 12);
}

#[test]
fn preview_lines_for_screen_uses_tmux_horizontal_cursor_window() {
    let mut screen = Screen::new(TerminalSize { cols: 20, rows: 3 }, 0);
    let mut parser = InputParser::new();
    parser.parse(b"0123456789abcdef", &mut screen);

    let lines = preview_lines_for_screen(&screen, 2, 8, &Utf8Config::default());

    assert!(
        lines[0].contains("cdef"),
        "preview should follow tmux and show the tail near the cursor, got {:?}",
        lines[0]
    );
    assert!(
        !lines[0].contains("0123"),
        "preview should not stay pinned to the far left once the cursor is to the right, got {:?}",
        lines[0]
    );
}

#[test]
fn render_preview_segment_row_pads_empty_preview_rows_to_full_width() {
    let column = PreviewColumn {
        label: PreviewLabel {
            text: "0".into(),
            style: Style::default(),
        },
        lines: vec![String::new()],
    };

    let rendered = render_preview_segment_row(&column, 0, 8, 5, &Utf8Config::default());

    assert_eq!(rendered, "        ");
}

#[test]
fn render_preview_segment_row_preserves_formatted_preview_lines() {
    let column = PreviewColumn {
        label: PreviewLabel {
            text: "0".into(),
            style: Style::default(),
        },
        lines: vec!["#[fg=green]abcdef#[default]".to_owned()],
    };

    let rendered = render_preview_segment_row(&column, 0, 6, 5, &Utf8Config::default());

    assert_eq!(rendered, "#[fg=green]abcdef#[default]");
}

#[test]
fn preview_lines_for_screen_marks_cursor_cell_reverse_like_tmux() {
    let mut screen = Screen::new(TerminalSize { cols: 20, rows: 3 }, 0);
    let mut parser = InputParser::new();
    parser.parse(b"prompt> ", &mut screen);

    let lines = preview_lines_for_screen(&screen, 1, 10, &Utf8Config::default());

    assert!(
        lines[0].contains("reverse"),
        "preview should reverse the cursor cell like tmux, got {:?}",
        lines[0]
    );
}

#[test]
fn preview_lines_for_screen_shows_top_content_instead_of_blank_tail() {
    let mut screen = Screen::new(TerminalSize { cols: 20, rows: 10 }, 0);
    let mut parser = InputParser::new();
    parser.parse(b"top\r\n", &mut screen);

    let lines = preview_lines_for_screen(&screen, 4, 20, &Utf8Config::default());

    assert!(
        lines.iter().any(|line| line.contains("top")),
        "cursor-anchored preview should keep top content visible"
    );
}

#[test]
fn mode_tree_list_rows_disables_preview_when_terminal_is_too_small() {
    assert_eq!(mode_tree_list_rows(3, 20, PreviewMode::Normal), 3);
    assert_eq!(mode_tree_list_rows(5, 20, PreviewMode::Normal), 5);
}

#[test]
fn mode_tree_list_rows_uses_all_rows_when_preview_is_off() {
    assert_eq!(mode_tree_list_rows(100, 20, PreviewMode::Off), 100);
    assert_eq!(mode_tree_list_rows(0, 20, PreviewMode::Off), 0);
}
