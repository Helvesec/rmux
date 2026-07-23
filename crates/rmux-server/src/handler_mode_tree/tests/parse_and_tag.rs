use super::*;

#[test]
fn parse_choose_buffer_command_preserves_preview_and_sort_flags() {
    let parsed = CommandParser::new()
        .parse_one_group("choose-buffer -NN -O size")
        .expect("choose-buffer parses");
    let mode = RequestHandler::parse_mode_tree_queue_command(
        parsed
            .commands()
            .first()
            .cloned()
            .expect("single parsed choose-buffer command"),
    )
    .expect("mode-tree command parses")
    .expect("mode-tree command recognized");

    assert!(matches!(mode.kind, ModeTreeKind::Buffer));
    assert!(matches!(mode.preview_mode, PreviewMode::Big));
    assert_eq!(mode.sort_order, Some(SortOrder::Size));
}

#[test]
fn tag_all_descends_through_no_tag_headers() {
    let mut mode = ModeTreeClientState {
        origin: test_origin(),
        kind: ModeTreeKind::Customize,
        session_name: SessionName::new("alpha").expect("valid session"),
        session_id: rmux_proto::SessionId::new(1),
        host_pane: None,
        host_identity: None,
        host_transcript: None,
        preview_mode: PreviewMode::Normal,
        row_format: None,
        filter_format: None,
        filter_text: None,
        key_format: DEFAULT_KEY_FORMAT.to_owned(),
        template: None,
        search: None,
        tagged: BTreeSet::new(),
        expanded: BTreeSet::new(),
        selected_id: None,
        scroll: 0,
        preview_scroll: 0,
        sort_order: None,
        order_seq: Vec::new(),
        reversed: false,
        tree_depth: TreeDepth::Pane,
        show_all_group_members: false,
        auto_accept: false,
        zoom_restore: None,
        last_list_rows: 20,
    };
    let build = ModeTreeBuild {
        items: BTreeMap::from([
            (
                "root".to_owned(),
                ModeTreeItem {
                    id: "root".to_owned(),
                    parent: None,
                    children: vec!["header".to_owned()],
                    depth: 0,
                    line: String::new(),
                    search_text: String::new(),
                    preview: Vec::new(),
                    no_tag: true,
                    action: ModeTreeAction::None,
                },
            ),
            (
                "header".to_owned(),
                ModeTreeItem {
                    id: "header".to_owned(),
                    parent: Some("root".to_owned()),
                    children: vec!["leaf".to_owned()],
                    depth: 1,
                    line: String::new(),
                    search_text: String::new(),
                    preview: Vec::new(),
                    no_tag: true,
                    action: ModeTreeAction::None,
                },
            ),
            (
                "leaf".to_owned(),
                ModeTreeItem {
                    id: "leaf".to_owned(),
                    parent: Some("header".to_owned()),
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
        roots: vec!["root".to_owned()],
        order: vec!["root".to_owned(), "header".to_owned(), "leaf".to_owned()],
        visible: vec!["root".to_owned(), "header".to_owned(), "leaf".to_owned()],
        no_matches: false,
    };

    tag_all(&mut mode, &build);

    assert!(mode.tagged.contains("leaf"));
    assert!(!mode.tagged.contains("root"));
    assert!(!mode.tagged.contains("header"));
}
