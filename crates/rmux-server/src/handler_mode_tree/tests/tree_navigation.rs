use super::*;

#[test]
fn collapse_or_parent_moves_to_parent_when_already_collapsed() {
    let mut items = BTreeMap::new();
    items.insert(
        "parent".to_owned(),
        ModeTreeItem {
            id: "parent".to_owned(),
            parent: None,
            children: vec!["child".to_owned()],
            depth: 0,
            line: String::new(),
            search_text: String::new(),
            preview: Vec::new(),
            no_tag: false,
            action: ModeTreeAction::None,
        },
    );
    items.insert(
        "child".to_owned(),
        ModeTreeItem {
            id: "child".to_owned(),
            parent: Some("parent".to_owned()),
            children: Vec::new(),
            depth: 1,
            line: String::new(),
            search_text: String::new(),
            preview: Vec::new(),
            no_tag: false,
            action: ModeTreeAction::None,
        },
    );
    let build = ModeTreeBuild {
        items,
        roots: vec!["parent".to_owned()],
        order: vec!["parent".to_owned(), "child".to_owned()],
        visible: vec!["parent".to_owned(), "child".to_owned()],
        no_matches: false,
    };
    let mut mode = test_mode(10);
    mode.selected_id = Some("child".to_owned());
    collapse_or_parent(&mut mode, &build);
    assert_eq!(mode.selected_id.as_deref(), Some("parent"));
}

#[test]
fn expand_or_child_expands_then_enters() {
    let mut items = BTreeMap::new();
    items.insert(
        "parent".to_owned(),
        ModeTreeItem {
            id: "parent".to_owned(),
            parent: None,
            children: vec!["child".to_owned()],
            depth: 0,
            line: String::new(),
            search_text: String::new(),
            preview: Vec::new(),
            no_tag: false,
            action: ModeTreeAction::None,
        },
    );
    items.insert(
        "child".to_owned(),
        ModeTreeItem {
            id: "child".to_owned(),
            parent: Some("parent".to_owned()),
            children: Vec::new(),
            depth: 1,
            line: String::new(),
            search_text: String::new(),
            preview: Vec::new(),
            no_tag: false,
            action: ModeTreeAction::None,
        },
    );
    let build = ModeTreeBuild {
        items,
        roots: vec!["parent".to_owned()],
        order: vec!["parent".to_owned(), "child".to_owned()],
        visible: vec!["parent".to_owned(), "child".to_owned()],
        no_matches: false,
    };
    let mut mode = test_mode(10);
    mode.selected_id = Some("parent".to_owned());
    // First right-arrow: expand
    expand_or_child(&mut mode, &build);
    assert!(mode.expanded.contains("parent"));
    assert_eq!(mode.selected_id.as_deref(), Some("parent"));
    // Second right-arrow: enter child
    expand_or_child(&mut mode, &build);
    assert_eq!(mode.selected_id.as_deref(), Some("child"));
}

#[test]
fn stable_order_reverses_primary_but_not_tiebreaker() {
    // When primary keys are equal, tiebreaker should NOT be reversed.
    let result = stable_order(Ordering::Equal, true, "a", "b");
    assert_eq!(result, Ordering::Less, "tiebreaker keeps natural order");

    // When primary keys differ, they should be reversed.
    let result = stable_order(Ordering::Less, true, "a", "b");
    assert_eq!(result, Ordering::Greater, "primary is reversed");
}

#[test]
fn expand_or_child_skips_ghost_children() {
    let mut items = BTreeMap::new();
    items.insert(
        "parent".to_owned(),
        ModeTreeItem {
            id: "parent".to_owned(),
            parent: None,
            // "ghost" is listed as a child but not in items
            children: vec!["ghost".to_owned()],
            depth: 0,
            line: String::new(),
            search_text: String::new(),
            preview: Vec::new(),
            no_tag: false,
            action: ModeTreeAction::None,
        },
    );
    let build = ModeTreeBuild {
        items,
        roots: vec!["parent".to_owned()],
        order: vec!["parent".to_owned()],
        visible: vec!["parent".to_owned()],
        no_matches: false,
    };
    let mut mode = test_mode(10);
    mode.selected_id = Some("parent".to_owned());
    mode.expanded.insert("parent".to_owned());
    // Expanding into ghost: selected should stay on parent (no valid child)
    expand_or_child(&mut mode, &build);
    assert_eq!(mode.selected_id.as_deref(), Some("parent"));
}

#[test]
fn move_selection_resets_preview_scroll() {
    let build = flat_build(&["a", "b", "c"]);
    let mut mode = test_mode(10);
    mode.selected_id = Some("a".to_owned());
    mode.preview_scroll = 5;
    move_selection(&mut mode, &build, 1, true);
    assert_eq!(mode.selected_id.as_deref(), Some("b"));
    assert_eq!(
        mode.preview_scroll, 0,
        "preview_scroll reset on selection change"
    );
}

#[test]
fn move_selection_preserves_preview_scroll_when_at_boundary() {
    let build = flat_build(&["a", "b", "c"]);
    let mut mode = test_mode(10);
    mode.selected_id = Some("c".to_owned());
    mode.preview_scroll = 3;
    // Page-style movement past the end should not change selection.
    move_selection(&mut mode, &build, 1, false);
    assert_eq!(mode.selected_id.as_deref(), Some("c"));
    assert_eq!(
        mode.preview_scroll, 3,
        "preview_scroll unchanged when selection doesn't change"
    );
}

#[test]
fn search_resets_preview_scroll_on_match() {
    let build = flat_build(&["alpha", "beta", "gamma"]);
    let mut mode = test_mode(10);
    mode.selected_id = Some("alpha".to_owned());
    mode.preview_scroll = 7;
    mode.search = Some(SearchState {
        value: "gamma".to_owned(),
        direction: SearchDirection::Forward,
    });
    repeat_search(&mut mode, &build, false);
    assert_eq!(mode.selected_id.as_deref(), Some("gamma"));
    assert_eq!(mode.preview_scroll, 0);
}

#[test]
fn parse_choose_tree_with_template_and_flags() {
    let parsed = CommandParser::new()
        .parse_one_group("choose-tree -sZ display-message")
        .expect("parses");
    let mode = RequestHandler::parse_mode_tree_queue_command(parsed.commands()[0].clone())
        .expect("ok")
        .expect("recognized");
    assert_eq!(mode.tree_depth, TreeDepth::Session);
    assert!(mode.zoom);
    assert_eq!(mode.template.as_deref(), Some("display-message"));
}

#[test]
fn parse_choose_tree_preserves_single_argv_template_for_runtime_reparse() {
    for template in [
        "switch-client -t target",
        "switch-client -t '%%'",
        "switch-client -Zt '%%'",
        "switch-client -t '#{?#{==:#{session_name},target},%%,missing}'",
        "switch-client -t '%%' ; display-message -p selected",
    ] {
        let parsed = CommandParser::new()
            .parse_arguments(["choose-tree", template])
            .expect("parses");
        let mode = RequestHandler::parse_mode_tree_queue_command(parsed.commands()[0].clone())
            .expect("ok")
            .expect("recognized");
        assert_eq!(mode.template.as_deref(), Some(template));
    }
}

#[test]
fn parse_choose_tree_preserves_quoted_source_template_for_runtime_reparse() {
    let template = "switch-client -t '%%' ; display-message -p selected";
    let parsed = CommandParser::new()
        .parse_one_group(&format!("choose-tree {template:?}"))
        .expect("parses");
    let mode = RequestHandler::parse_mode_tree_queue_command(parsed.commands()[0].clone())
        .expect("ok")
        .expect("recognized");
    assert_eq!(mode.template.as_deref(), Some(template));
}

#[test]
fn parse_choose_tree_zw_preserves_trailing_direct_command_arguments_from_argv() {
    let parsed = CommandParser::new()
        .parse_arguments(["choose-tree", "-Zw", "set-buffer", "-b", "chosen", "%%"])
        .expect("parses");
    let mode = RequestHandler::parse_mode_tree_queue_command(parsed.commands()[0].clone())
        .expect("ok")
        .expect("recognized");
    assert_eq!(mode.tree_depth, TreeDepth::Window);
    assert!(mode.zoom);
    assert_eq!(
        mode.template.as_deref(),
        Some("set-buffer -b chosen \"%%\"")
    );
}

#[test]
fn parse_choose_tree_double_dash_separates_template() {
    let parsed = CommandParser::new()
        .parse_one_group("choose-tree -- -not-a-flag")
        .expect("parses");
    let mode = RequestHandler::parse_mode_tree_queue_command(parsed.commands()[0].clone())
        .expect("ok")
        .expect("recognized");
    assert_eq!(mode.template.as_deref(), Some("-not-a-flag"));
}

#[test]
fn customize_mode_rejects_extra_argument() {
    let parsed = CommandParser::new()
        .parse_one_group("customize-mode unexpected")
        .expect("parses");
    let err = RequestHandler::parse_mode_tree_queue_command(parsed.commands()[0].clone());
    assert!(err.is_err());
}

async fn attached_choose_tree(
    label: &str,
    attach_pid_offset: u32,
    windows: u32,
) -> (
    RequestHandler,
    u32,
    mpsc::UnboundedReceiver<crate::pane_io::AttachControl>,
) {
    let handler = RequestHandler::new();
    let session_name = SessionName::new(label).expect("valid session");
    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session_name.clone(),
                detached: true,
                size: Some(TerminalSize {
                    cols: 100,
                    rows: 30,
                }),
                environment: None,
            }))
            .await,
    );
    for window_index in 1..windows {
        assert!(matches!(
            handler
                .handle(Request::NewWindow(Box::new(rmux_proto::NewWindowRequest {
                    target: session_name.clone(),
                    name: None,
                    detached: true,
                    environment: None,
                    command: None,
                    process_command: None,
                    start_directory: None,
                    target_window_index: Some(window_index),
                    insert_at_target: false,
                })))
                .await,
            Response::NewWindow(_)
        ));
    }
    let attach_pid = std::process::id().saturating_add(attach_pid_offset);
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(attach_pid, session_name, control_tx)
        .await;
    let parsed = CommandParser::new()
        .parse_arguments(["choose-tree"])
        .expect("choose-tree parses");
    let command = RequestHandler::parse_mode_tree_queue_command(parsed.commands()[0].clone())
        .expect("mode-tree command parses")
        .expect("mode-tree command recognized");
    handler
        .execute_queued_mode_tree(
            attach_pid,
            command,
            &QueueExecutionContext::without_caller_cwd(),
        )
        .await
        .expect("choose-tree opens");
    (handler, attach_pid, control_rx)
}

/// R3509 M1: tree-mode ignores a key it does not bind, as tmux 3.7b's
/// `mode_tree_key` does; only Escape (and q) closes it.
#[tokio::test]
async fn choose_tree_ignores_unbound_special_and_modified_keys() {
    let (handler, attach_pid, _control_rx) =
        attached_choose_tree("tree-unbound-keys", 2395, 1).await;
    for bytes in [
        &b"\x1b[5~"[..],
        b"\x1b[6~",
        b"\x1b[2~",
        b"\x1b[15~",
        b"\x1b[24~",
        b"\x1b[H",
        b"\x1b[6;2~",
        b"\x1b[3;5~",
        b"\x1b[15;2~",
        b"\x1b[15;3~",
        b"\x1b[1;2P",
        b"\x1b[3~",
        b"\x1b[B",
        b"\x1b[1;5A",
        b"\x1b[1;5H",
        b"\x1bOP",
    ] {
        handler
            .handle_attached_live_input_for_test(attach_pid, bytes)
            .await
            .expect("tree-mode input is handled");
        assert!(
            handler.mode_tree_active(attach_pid).await,
            "{:?} must leave choose-tree open",
            String::from_utf8_lossy(bytes)
        );
    }

    let (handler, attach_pid, _control_rx) =
        attached_choose_tree("tree-lone-escape", 2397, 1).await;
    let mut pending_input = Vec::new();
    handler
        .handle_attached_live_input(attach_pid, &mut pending_input, b"\x1b")
        .await
        .expect("lone Escape is retained");
    handler
        .flush_attached_pending_escape_input(attach_pid, &mut pending_input)
        .await
        .expect("escape-time flush");
    assert!(
        !handler.mode_tree_active(attach_pid).await,
        "a lone Escape still closes choose-tree"
    );
}

/// R3530 (1): the escape time does not make an Escape of input that only
/// starts with one. tmux 3.7b reads `ESC [` as M-[ and `ESC O` as M-O, which
/// tree-mode does not bind, so choose-tree stays open.
#[tokio::test]
async fn choose_tree_keeps_an_unfinished_escape_key_open_at_the_escape_time() {
    for (offset, retained) in [(2442, &b"\x1b["[..]), (2443, b"\x1bO")] {
        let (handler, attach_pid, _control_rx) =
            attached_choose_tree("tree-expired-escape", offset, 1).await;
        let mut pending_input = Vec::new();
        handler
            .handle_attached_live_input(attach_pid, &mut pending_input, retained)
            .await
            .expect("unfinished key input");
        assert_eq!(pending_input, retained, "the unfinished key waits");
        handler
            .flush_attached_pending_escape_input(attach_pid, &mut pending_input)
            .await
            .expect("escape-time flush");
        assert!(pending_input.is_empty(), "the escape time empties it");
        assert!(
            handler.mode_tree_active(attach_pid).await,
            "{:?} must leave choose-tree open",
            String::from_utf8_lossy(retained)
        );
    }
}

/// S2452: Escape typed twice in one read is M-Escape for tmux 3.7b, which
/// tree-mode does not bind; with `[` after it, the escape time makes it
/// M-Escape and `[`. choose-tree stays open either way.
#[tokio::test]
async fn choose_tree_reads_escape_escape_in_one_read_as_meta_escape() {
    for (offset, read) in [(2452, &b"\x1b\x1b"[..]), (2453, b"\x1b\x1b[")] {
        let (handler, attach_pid, _control_rx) =
            attached_choose_tree("tree-escape-escape", offset, 1).await;
        let mut pending_input = Vec::new();
        handler
            .handle_attached_live_input(attach_pid, &mut pending_input, read)
            .await
            .expect("escape input");
        handler
            .flush_attached_pending_escape_input(attach_pid, &mut pending_input)
            .await
            .expect("escape-time flush");
        assert!(pending_input.is_empty(), "the escape time empties it");
        assert!(
            handler.mode_tree_active(attach_pid).await,
            "{read:?} must leave choose-tree open"
        );
    }
}

#[test]
fn page_keys_move_by_the_list_height_and_stop_at_the_ends() {
    let ids: Vec<String> = (0..41).map(|index| format!("item-{index}")).collect();
    let build = flat_build(&ids.iter().map(String::as_str).collect::<Vec<_>>());
    let mut mode = test_mode(18);
    mode.selected_id = Some("item-1".to_owned());

    move_selection_by_page(&mut mode, &build, true);
    assert_eq!(mode.selected_id.as_deref(), Some("item-19"));
    move_selection_by_page(&mut mode, &build, true);
    assert_eq!(mode.selected_id.as_deref(), Some("item-37"));
    move_selection_by_page(&mut mode, &build, true);
    assert_eq!(mode.selected_id.as_deref(), Some("item-40"), "no wrap");
    move_selection_by_page(&mut mode, &build, false);
    assert_eq!(mode.selected_id.as_deref(), Some("item-22"));
    mode.selected_id = Some("item-3".to_owned());
    move_selection_by_page(&mut mode, &build, false);
    assert_eq!(mode.selected_id.as_deref(), Some("item-0"), "no wrap");
}

async fn selected_tree_position(
    handler: &RequestHandler,
    attach_pid: u32,
) -> (usize, usize, usize) {
    let mut mode = handler
        .active_attach
        .lock()
        .await
        .by_pid
        .get(&attach_pid)
        .and_then(|active| active.mode_tree.clone())
        .expect("choose-tree is open");
    let build = handler
        .build_mode_tree(&mut mode, attach_pid)
        .await
        .expect("choose-tree builds");
    let position = mode
        .selected_id
        .as_ref()
        .and_then(|id| build.visible.iter().position(|visible| visible == id))
        .expect("the selection is visible");
    (position, mode.last_list_rows, build.visible.len())
}

/// R3509 M1: PgDn and PgUp from the terminal page through choose-tree by the
/// height of its list, as tmux 3.7b tree-mode's NPage and PPage do.
#[tokio::test]
async fn choose_tree_page_keys_from_the_terminal_page_the_list() {
    let (handler, attach_pid, _control_rx) = attached_choose_tree("tree-page-keys", 2396, 40).await;
    let (start, rows, len) = selected_tree_position(&handler, attach_pid).await;
    assert!(
        rows > 1 && start + rows < len,
        "start {start}, rows {rows}, len {len}"
    );

    handler
        .handle_attached_live_input_for_test(attach_pid, b"\x1b[6~")
        .await
        .expect("PgDn is handled");
    assert_eq!(
        selected_tree_position(&handler, attach_pid).await.0,
        start + rows
    );

    handler
        .handle_attached_live_input_for_test(attach_pid, b"\x1b[5~")
        .await
        .expect("PgUp is handled");
    assert_eq!(selected_tree_position(&handler, attach_pid).await.0, start);
    assert!(handler.mode_tree_active(attach_pid).await);
}
