use super::*;

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
