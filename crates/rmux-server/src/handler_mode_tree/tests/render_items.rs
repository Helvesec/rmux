use super::*;

#[test]
fn choose_tree_uses_window_local_visible_pane_indices_in_rows_and_previews() {
    let mut state = HandlerState::default();
    let session_name = SessionName::new("visible-indices").expect("valid session name");
    state
        .sessions
        .create_session(session_name.clone(), TerminalSize { cols: 80, rows: 24 })
        .expect("session creation succeeds");
    state
        .sessions
        .session_mut(&session_name)
        .expect("session exists")
        .split_active_pane()
        .expect("split succeeds");
    state
        .options
        .set(
            ScopeSelector::Window(rmux_proto::WindowTarget::with_window(
                session_name.clone(),
                0,
            )),
            OptionName::PaneBaseIndex,
            "10".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("window-local pane-base-index set succeeds");

    let session = state
        .sessions
        .session(&session_name)
        .expect("session exists");
    let window = session.window_at(0).expect("window exists");
    let pane = window.pane(0).expect("pane exists");
    let mode = test_mode(20);
    let row = super::super::mode_tree_tree_build::render_tree_pane_line(
        &mode, &state, session, 0, 0, window, pane,
    );
    assert!(
        row.0 == "10" || row.0.starts_with("10: "),
        "choose-tree row should expose pane index 10, got {:?}",
        row.0
    );

    let item = ModeTreeItem {
        id: "window".to_owned(),
        parent: None,
        children: Vec::new(),
        depth: 1,
        line: String::new(),
        search_text: String::new(),
        preview: Vec::new(),
        no_tag: false,
        action: ModeTreeAction::TreeTarget {
            session_name,
            session_id: session.id(),
            window_index: Some(0),
            window_id: None,
            window_occurrence_id: None,
            pane_index: None,
            pane_id: None,
            pane_output_generation: None,
        },
    };
    let preview = mode_tree_preview_lines(&state, &mode, &item, 60, 7, &Utf8Config::default());
    assert!(
        preview.iter().any(|line| line.contains(" 10 ")),
        "choose-tree preview should label the first pane 10, got {preview:?}"
    );
    assert!(
        preview.iter().any(|line| line.contains(" 11 ")),
        "choose-tree preview should label the second pane 11, got {preview:?}"
    );
}

#[test]
fn render_visible_item_hides_single_pane_branch_marker_in_window_tree() {
    let state = HandlerState::default();
    let utf8 = Utf8Config::default();
    let mut mode = test_mode(10);
    mode.key_format.clear();
    mode.tree_depth = TreeDepth::Window;
    let item = ModeTreeItem {
        id: "window".to_owned(),
        parent: Some("session".to_owned()),
        children: vec!["pane".to_owned()],
        depth: 1,
        line: "1: shell".to_owned(),
        search_text: String::new(),
        preview: Vec::new(),
        no_tag: false,
        action: ModeTreeAction::None,
    };
    let build = ModeTreeBuild {
        items: BTreeMap::from([
            (
                "session".to_owned(),
                ModeTreeItem {
                    id: "session".to_owned(),
                    parent: None,
                    children: vec!["window".to_owned()],
                    depth: 0,
                    line: String::new(),
                    search_text: String::new(),
                    preview: Vec::new(),
                    no_tag: false,
                    action: ModeTreeAction::None,
                },
            ),
            (item.id.clone(), item.clone()),
            (
                "pane".to_owned(),
                ModeTreeItem {
                    id: "pane".to_owned(),
                    parent: Some("window".to_owned()),
                    children: Vec::new(),
                    depth: 2,
                    line: String::new(),
                    search_text: String::new(),
                    preview: Vec::new(),
                    no_tag: false,
                    action: ModeTreeAction::None,
                },
            ),
        ]),
        roots: vec!["session".to_owned()],
        order: vec!["session".to_owned(), "window".to_owned(), "pane".to_owned()],
        visible: vec!["session".to_owned(), "window".to_owned()],
        no_matches: false,
    };

    let rendered = render_visible_item(&state, &mode, &build, &item, 1, 0, &utf8);

    assert_eq!(rendered, "└─>   1: shell");
}

#[test]
fn render_visible_item_keeps_branch_marker_for_multi_pane_window_tree_item() {
    let state = HandlerState::default();
    let utf8 = Utf8Config::default();
    let mut mode = test_mode(10);
    mode.key_format.clear();
    mode.tree_depth = TreeDepth::Window;
    let item = ModeTreeItem {
        id: "window".to_owned(),
        parent: Some("session".to_owned()),
        children: vec!["pane0".to_owned(), "pane1".to_owned()],
        depth: 1,
        line: "0: shell*".to_owned(),
        search_text: String::new(),
        preview: Vec::new(),
        no_tag: false,
        action: ModeTreeAction::None,
    };
    let build = ModeTreeBuild {
        items: BTreeMap::from([
            (
                "session".to_owned(),
                ModeTreeItem {
                    id: "session".to_owned(),
                    parent: None,
                    children: vec!["window".to_owned()],
                    depth: 0,
                    line: String::new(),
                    search_text: String::new(),
                    preview: Vec::new(),
                    no_tag: false,
                    action: ModeTreeAction::None,
                },
            ),
            (item.id.clone(), item.clone()),
        ]),
        roots: vec!["session".to_owned()],
        order: vec!["session".to_owned(), "window".to_owned()],
        visible: vec!["session".to_owned(), "window".to_owned()],
        no_matches: false,
    };

    let rendered = render_visible_item(&state, &mode, &build, &item, 1, 0, &utf8);

    assert_eq!(rendered, "└─> + 0: shell*");
}

#[test]
fn render_visible_item_omits_extra_leaf_padding_for_flat_pane_lists() {
    let state = HandlerState::default();
    let utf8 = Utf8Config::default();
    let mut mode = test_mode(10);
    mode.key_format.clear();
    mode.tree_depth = TreeDepth::Pane;
    let item = ModeTreeItem {
        id: "pane0".to_owned(),
        parent: Some("window".to_owned()),
        children: Vec::new(),
        depth: 2,
        line: "0: bash".to_owned(),
        search_text: String::new(),
        preview: Vec::new(),
        no_tag: false,
        action: ModeTreeAction::None,
    };
    let build = ModeTreeBuild {
        items: BTreeMap::from([
            (
                "session0".to_owned(),
                ModeTreeItem {
                    id: "session0".to_owned(),
                    parent: None,
                    children: vec!["window".to_owned()],
                    depth: 0,
                    line: String::new(),
                    search_text: String::new(),
                    preview: Vec::new(),
                    no_tag: false,
                    action: ModeTreeAction::None,
                },
            ),
            (
                "session1".to_owned(),
                ModeTreeItem {
                    id: "session1".to_owned(),
                    parent: None,
                    children: Vec::new(),
                    depth: 0,
                    line: String::new(),
                    search_text: String::new(),
                    preview: Vec::new(),
                    no_tag: false,
                    action: ModeTreeAction::None,
                },
            ),
            (
                "window".to_owned(),
                ModeTreeItem {
                    id: "window".to_owned(),
                    parent: Some("session0".to_owned()),
                    children: vec!["pane0".to_owned(), "pane1".to_owned()],
                    depth: 1,
                    line: String::new(),
                    search_text: String::new(),
                    preview: Vec::new(),
                    no_tag: false,
                    action: ModeTreeAction::None,
                },
            ),
            (item.id.clone(), item.clone()),
            (
                "pane1".to_owned(),
                ModeTreeItem {
                    id: "pane1".to_owned(),
                    parent: Some("window".to_owned()),
                    children: Vec::new(),
                    depth: 2,
                    line: "1: bash".to_owned(),
                    search_text: String::new(),
                    preview: Vec::new(),
                    no_tag: false,
                    action: ModeTreeAction::None,
                },
            ),
        ]),
        roots: vec!["session0".to_owned(), "session1".to_owned()],
        order: vec![
            "session0".to_owned(),
            "window".to_owned(),
            "pane0".to_owned(),
            "pane1".to_owned(),
            "session1".to_owned(),
        ],
        visible: vec![
            "session0".to_owned(),
            "window".to_owned(),
            "pane0".to_owned(),
            "pane1".to_owned(),
            "session1".to_owned(),
        ],
        no_matches: false,
    };

    let rendered = render_visible_item(&state, &mode, &build, &item, 2, 0, &utf8);

    assert_eq!(rendered, "│   ├─> 0: bash");
}

#[test]
fn default_key_format_renders_meta_shortcuts_without_tail_junk() {
    let state = HandlerState::default();
    let utf8 = Utf8Config::default();
    let mode = test_mode(40);
    let build = flat_build(&["line10", "line36"]);

    let line10 = build.items.get("line10").expect("line 10 item exists");
    let rendered = render_visible_item(&state, &mode, &build, line10, 10, 6, &utf8);

    assert!(
        rendered.starts_with("(M-a) "),
        "line 10 should render the M-a shortcut, got {rendered:?}"
    );
    assert!(
        !rendered.contains("1,M-a"),
        "line 10 must not leak the false branch tail, got {rendered:?}"
    );

    let line36 = build.items.get("line36").expect("line 36 item exists");
    let rendered = render_visible_item(&state, &mode, &build, line36, 36, 6, &utf8);
    assert!(
        !rendered.starts_with('('),
        "line 36 has no tmux shortcut, got {rendered:?}"
    );
}

#[test]
fn render_mode_tree_overlay_keeps_cursor_hidden_while_active() {
    let mut state = HandlerState::default();
    state
        .sessions
        .create_session(
            SessionName::new("test").expect("valid session"),
            rmux_proto::TerminalSize { cols: 80, rows: 24 },
        )
        .expect("session create succeeds");
    let mut mode = test_mode(10);
    mode.selected_id = Some("root".to_owned());
    let build = flat_build(&["root"]);

    let frame = render_mode_tree_overlay(&state, &mode, &build);

    let rendered = String::from_utf8_lossy(&frame);
    assert!(rendered.contains("\u{1b}[?25l"));
    assert!(!rendered.contains("\u{1b}[?25h"));
}

#[test]
fn render_mode_tree_overlay_uses_scrollbar_content_geometry_on_both_sides() {
    for (position, list_cursor, box_cursor) in [
        ("right", b"\x1b[1;1H".as_slice(), b"\x1b[3;1H".as_slice()),
        ("left", b"\x1b[1;4H".as_slice(), b"\x1b[3;4H".as_slice()),
    ] {
        let mut state = HandlerState::default();
        let session_name = SessionName::new("test").expect("valid session");
        state
            .sessions
            .create_session(
                session_name.clone(),
                rmux_proto::TerminalSize { cols: 20, rows: 8 },
            )
            .expect("session create succeeds");
        state
            .options
            .set(
                ScopeSelector::Session(session_name.clone()),
                OptionName::Status,
                "off".to_owned(),
                SetOptionMode::Replace,
            )
            .expect("status option");
        let target = rmux_proto::WindowTarget::with_window(session_name, 0);
        for (option, value) in [
            (OptionName::PaneScrollbars, "on"),
            (OptionName::PaneScrollbarsPosition, position),
            (OptionName::PaneScrollbarsStyle, "width=2,pad=1"),
        ] {
            state
                .options
                .set(
                    ScopeSelector::Window(target.clone()),
                    option,
                    value.to_owned(),
                    SetOptionMode::Replace,
                )
                .expect("scrollbar option");
        }
        let mut mode = test_mode(8);
        mode.key_format.clear();
        mode.preview_mode = PreviewMode::Big;
        mode.selected_id = Some("12345678\tABCDEFGHjkl".to_owned());
        let build = flat_build(&["12345678\tABCDEFGHjkl"]);

        let frame = render_mode_tree_overlay(&state, &mode, &build);

        assert!(
            frame
                .windows(list_cursor.len())
                .any(|run| run == list_cursor),
            "{position}: list must start at the content origin"
        );
        assert!(
            frame.windows(box_cursor.len()).any(|run| run == box_cursor),
            "{position}: preview box must use the same content origin"
        );
        assert!(
            !frame.windows(b"jkl".len()).any(|run| run == b"jkl"),
            "{position}: list text must be clipped to 17 content cells"
        );
        assert!(
            !frame.contains(&b'\t'),
            "{position}: a literal tab must not escape the content geometry"
        );
        assert!(
            frame
                .windows(b"12345678 ABCDEFGH".len())
                .any(|run| run == b"12345678 ABCDEFGH"),
            "{position}: tabs must be sanitized before width clipping"
        );
        assert!(frame.ends_with(b"\x1b[0m\x1b[u"));
    }
}

#[test]
fn render_mode_tree_overlay_uses_compact_plain_lines() {
    let mut state = HandlerState::default();
    state
        .sessions
        .create_session(
            SessionName::new("test").expect("valid session"),
            rmux_proto::TerminalSize { cols: 80, rows: 24 },
        )
        .expect("session create succeeds");
    let mut mode = test_mode(10);
    mode.selected_id = Some("root".to_owned());
    let build = flat_build(&["root"]);

    let frame = render_mode_tree_overlay(&state, &mode, &build);

    assert!(
        frame.len() < 2_000,
        "plain choose-tree overlay should stay below common tty output queues, got {} bytes",
        frame.len()
    );
}
