// Issue #180: keys must resolve through the key tables like every other key.
// Two hardcoded shims used to answer first and swallow the user binding:
//
//   * `step03_prefix_binding` mapped prefix Up/Down/Left/Right/n/p onto
//     next/previous pane and next/previous window, overriding the binding that
//     had already been resolved from the prefix table.
//   * `attached_copy_mode_input_action` mapped arrows *and* Space / Enter / q /
//     Escape / `/` / `?` / n / N onto fixed copy-mode commands before the
//     copy-mode / copy-mode-vi tables were consulted on the attached
//     live-input path.
//
// tmux 3.7b, measured through a real PTY client, honours a user binding on
// every one of those keys in both tables and makes an unbound one completely
// inert -- there is no hardcoded fallback in front of, or behind, the table.
// Both shims are therefore gone and the tables are the sole authority, which
// is what the detached `send-keys -X` path already did.
//
// These tests pin the dispatch layer: the resolved binding wins, the defaults
// still work when the user binds nothing, and the neighbouring tables plus the
// modified arrows behave exactly as before.

use super::*;

const PROBE: &str = "@probe180";

async fn bind(handler: &RequestHandler, table: &str, key: &str, command: &[&str]) {
    let response = handler
        .handle(Request::BindKey(Box::new(BindKeyRequest {
            table_name: table.to_owned(),
            key: key.to_owned(),
            note: None,
            repeat: false,
            command: Some(command.iter().map(|part| (*part).to_owned()).collect()),
        })))
        .await;
    assert!(matches!(response, Response::BindKey(_)), "{response:?}");
}

async fn bind_repeating(handler: &RequestHandler, table: &str, key: &str, command: &[&str]) {
    let response = handler
        .handle(Request::BindKey(Box::new(BindKeyRequest {
            table_name: table.to_owned(),
            key: key.to_owned(),
            note: None,
            repeat: true,
            command: Some(command.iter().map(|part| (*part).to_owned()).collect()),
        })))
        .await;
    assert!(matches!(response, Response::BindKey(_)), "{response:?}");
}

async fn unbind(handler: &RequestHandler, table: &str, key: &str) {
    let response = handler
        .handle(Request::UnbindKey(UnbindKeyRequest {
            table_name: table.to_owned(),
            key: Some(key.to_owned()),
            all: false,
            quiet: false,
        }))
        .await;
    assert!(matches!(response, Response::UnbindKey(_)), "{response:?}");
}

/// Read a user option back the way the issue reporter did (`show-options -gv`).
async fn probe_value(handler: &RequestHandler, name: &str) -> String {
    let response = handler
        .handle(Request::ShowOptions(rmux_proto::ShowOptionsRequest {
            scope: rmux_proto::OptionScopeSelector::SessionGlobal,
            name: Some(name.to_owned()),
            value_only: true,
            include_inherited: false,
            quiet: true,
            include_hooks: false,
        }))
        .await;
    let Response::ShowOptions(response) = response else {
        panic!("expected show-options response, got {response:?}");
    };
    String::from_utf8(response.command_output().stdout().to_vec())
        .expect("option value is utf-8")
        .trim()
        .to_owned()
}

/// Dispatch a key through the client's key table, exactly like `send-keys -K`.
async fn dispatch_key_table(handler: &RequestHandler, target: &PaneTarget, key: &str) {
    let response = handler
        .handle(Request::SendKeysExt(SendKeysExtRequest {
            target: Some(target.clone()),
            keys: vec![key.to_owned()],
            expand_formats: false,
            hex: false,
            literal: false,
            dispatch_key_table: true,
            copy_mode_command: false,
            forward_mouse_event: false,
            reset_terminal: false,
            repeat_count: None,
        }))
        .await;
    assert!(matches!(response, Response::SendKeys(_)), "{response:?}");
}

async fn attach(
    handler: &RequestHandler,
    requester_pid: u32,
    session: &rmux_proto::SessionName,
) -> mpsc::UnboundedReceiver<crate::pane_io::AttachControl> {
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, session.clone(), control_tx)
        .await;
    control_rx
}

async fn enter_copy_mode(handler: &RequestHandler, target: &PaneTarget) {
    let response = handler
        .handle(Request::CopyMode(CopyModeRequest {
            target: Some(target.clone()),
            page_down: false,
            exit_on_scroll: false,
            hide_position: false,
            mouse_drag_start: false,
            cancel_mode: false,
            scrollbar_scroll: false,
            source: None,
            page_up: false,
        }))
        .await;
    assert!(matches!(response, Response::CopyMode(_)), "{response:?}");
}

async fn set_mode_keys(handler: &RequestHandler, session: &rmux_proto::SessionName, value: &str) {
    let response = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Window(WindowTarget::with_window(session.clone(), 0)),
            option: OptionName::ModeKeys,
            value: value.to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(response, Response::SetOption(_)), "{response:?}");
}

/// Read the copy cursor row through `#{copy_cursor_y}`, the same surface the
/// issue reporter used from the command line.
async fn copy_cursor_y(handler: &RequestHandler, target: &PaneTarget) -> u32 {
    let listed = handler
        .handle(Request::ListPanes(Box::new(ListPanesRequest {
            target: target.session_name().clone(),
            format: Some("#{copy_cursor_y}".to_owned()),
            filter: None,
            sort_order: None,
            reversed: false,
            target_window_index: None,
        })))
        .await;
    let output = listed
        .command_output()
        .expect("list-panes returns command output");
    String::from_utf8_lossy(output.stdout())
        .lines()
        .next()
        .expect("a pane row is rendered")
        .trim()
        .parse()
        .expect("copy_cursor_y is numeric")
}

/// Read how far the copy-mode view is scrolled back.
async fn scroll_position(handler: &RequestHandler, target: &PaneTarget) -> u32 {
    let listed = handler
        .handle(Request::ListPanes(Box::new(ListPanesRequest {
            target: target.session_name().clone(),
            format: Some("#{scroll_position}".to_owned()),
            filter: None,
            sort_order: None,
            reversed: false,
            target_window_index: None,
        })))
        .await;
    let output = listed
        .command_output()
        .expect("list-panes returns command output");
    String::from_utf8_lossy(output.stdout())
        .lines()
        .next()
        .expect("a pane row is rendered")
        .trim()
        .parse()
        .expect("scroll_position is numeric")
}

/// Fill the pane with scrollback so the copy cursor has somewhere to move.
async fn fill_transcript(handler: &RequestHandler, target: &PaneTarget) {
    let transcript = {
        let state = handler.state.lock().await;
        state
            .transcript_handle(target)
            .expect("pane transcript must exist")
    };
    let size = TerminalSize { cols: 80, rows: 24 };
    let history_limit = transcript.lock().history_limit();
    let mut screen = rmux_core::Screen::new(size, history_limit);
    let mut parser = rmux_core::input::InputParser::new();
    let mut content = Vec::new();
    for line in 0..40u32 {
        content.extend_from_slice(format!("line {line}\r\n").as_bytes());
    }
    parser.parse(&content, &mut screen);
    transcript.lock().set_screen_for_test(screen);
}

// --------------------------------------------------------------------------
// prefix table
// --------------------------------------------------------------------------

#[tokio::test]
async fn prefix_table_arrow_bindings_run_the_user_command() {
    for (key, expected) in [
        ("Up", "HIT-Up"),
        ("Down", "HIT-Down"),
        ("Left", "HIT-Left"),
        ("Right", "HIT-Right"),
    ] {
        let handler = RequestHandler::new();
        let alpha = session_name("alpha");
        let target = PaneTarget::new(alpha.clone(), 0);
        let requester_pid = std::process::id();

        create_send_keys_test_session(&handler, &alpha).await;
        let _control_rx = attach(&handler, requester_pid, &alpha).await;
        bind(
            &handler,
            "prefix",
            key,
            &["set-option", "-g", PROBE, expected],
        )
        .await;

        dispatch_key_table(&handler, &target, "C-b").await;
        dispatch_key_table(&handler, &target, key).await;

        assert_eq!(
            probe_value(&handler, PROBE).await,
            expected,
            "prefix {key} must run its user binding"
        );
    }
}

#[tokio::test]
async fn prefix_table_window_navigation_keys_run_the_user_command() {
    // `n` and `p` were captured by the same shim as the arrows, so the issue is
    // wider than the four arrow keys the report named.
    for (key, expected) in [("n", "HIT-n"), ("p", "HIT-p")] {
        let handler = RequestHandler::new();
        let alpha = session_name("alpha");
        let target = PaneTarget::new(alpha.clone(), 0);
        let requester_pid = std::process::id();

        create_send_keys_test_session(&handler, &alpha).await;
        let _control_rx = attach(&handler, requester_pid, &alpha).await;
        bind(
            &handler,
            "prefix",
            key,
            &["set-option", "-g", PROBE, expected],
        )
        .await;

        dispatch_key_table(&handler, &target, "C-b").await;
        dispatch_key_table(&handler, &target, key).await;

        assert_eq!(
            probe_value(&handler, PROBE).await,
            expected,
            "prefix {key} must run its user binding"
        );
    }
}

#[tokio::test]
async fn prefix_table_non_arrow_control_binding_is_unchanged() {
    // A key the shim never touched, to prove the dispatch path itself is sound.
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let target = PaneTarget::new(alpha.clone(), 0);
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    let _control_rx = attach(&handler, requester_pid, &alpha).await;
    bind(
        &handler,
        "prefix",
        "z",
        &["set-option", "-g", PROBE, "HIT-z"],
    )
    .await;

    dispatch_key_table(&handler, &target, "C-b").await;
    dispatch_key_table(&handler, &target, "z").await;

    assert_eq!(probe_value(&handler, PROBE).await, "HIT-z");
}

#[tokio::test]
async fn prefix_table_modified_arrow_bindings_still_resolve() {
    for (key, expected) in [("M-Up", "HIT-M-Up"), ("C-Left", "HIT-C-Left")] {
        let handler = RequestHandler::new();
        let alpha = session_name("alpha");
        let target = PaneTarget::new(alpha.clone(), 0);
        let requester_pid = std::process::id();

        create_send_keys_test_session(&handler, &alpha).await;
        let _control_rx = attach(&handler, requester_pid, &alpha).await;
        bind(
            &handler,
            "prefix",
            key,
            &["set-option", "-g", PROBE, expected],
        )
        .await;

        dispatch_key_table(&handler, &target, "C-b").await;
        dispatch_key_table(&handler, &target, key).await;

        assert_eq!(probe_value(&handler, PROBE).await, expected);
    }
}

#[tokio::test]
async fn prefix_table_default_arrow_selects_the_pane_in_that_direction() {
    // The default binding is `select-pane -U`, not "previous pane by index".
    // tmux 3.7b measured on the same layout selects pane 0 here; the removed
    // shim selected pane 1.
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let target = PaneTarget::new(alpha.clone(), 0);
    let requester_pid = std::process::id();

    create_quiet_input_session(&handler, &alpha).await;
    let _control_rx = attach(&handler, requester_pid, &alpha).await;

    let split = handler
        .handle(Request::SplitWindowExt(Box::new(
            rmux_proto::SplitWindowExtRequest {
                target: SplitWindowTarget::Pane(target.clone()),
                direction: SplitDirection::Vertical,
                before: false,
                environment: None,
                command: Some(quiet_pane_command()),
                process_command: None,
                start_directory: None,
                keep_alive_on_exit: None,
                detached: false,
                size: None,
                preserve_zoom: false,
                full_size: false,
                stdin_payload: None,
            },
        )))
        .await;
    let Response::SplitWindow(split) = split else {
        panic!("expected split-window response: {split:?}");
    };
    handler
        .wait_for_pane_startup_to_finish_for_test(&split.pane)
        .await;

    let split = handler
        .handle(Request::SplitWindowExt(Box::new(
            rmux_proto::SplitWindowExtRequest {
                target: SplitWindowTarget::Pane(PaneTarget::new(alpha.clone(), 1)),
                direction: SplitDirection::Horizontal,
                before: false,
                environment: None,
                command: Some(quiet_pane_command()),
                process_command: None,
                start_directory: None,
                keep_alive_on_exit: None,
                detached: false,
                size: None,
                preserve_zoom: false,
                full_size: false,
                stdin_payload: None,
            },
        )))
        .await;
    let Response::SplitWindow(split) = split else {
        panic!("expected split-window response: {split:?}");
    };
    handler
        .wait_for_pane_startup_to_finish_for_test(&split.pane)
        .await;

    let selected = handler
        .handle(Request::SelectPane(Box::new(SelectPaneRequest {
            target: PaneTarget::new(alpha.clone(), 2),
            title: None,
            style: None,
            input_disabled: None,
            preserve_zoom: false,
        })))
        .await;
    assert!(matches!(selected, Response::SelectPane(_)), "{selected:?}");

    dispatch_key_table(&handler, &PaneTarget::new(alpha.clone(), 2), "C-b").await;
    dispatch_key_table(&handler, &PaneTarget::new(alpha.clone(), 2), "Up").await;

    let active = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&alpha)
            .expect("session exists")
            .window_at(0)
            .expect("window exists")
            .active_pane_index()
    };
    assert_eq!(
        active, 0,
        "prefix Up must select the pane above (select-pane -U), not the previous index"
    );
}

#[tokio::test]
async fn prefix_table_unbound_arrow_stays_inert() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let target = PaneTarget::new(alpha.clone(), 0);
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    let _control_rx = attach(&handler, requester_pid, &alpha).await;
    unbind(&handler, "prefix", "Up").await;

    dispatch_key_table(&handler, &target, "C-b").await;
    dispatch_key_table(&handler, &target, "Up").await;

    // The prefix table is left, and nothing ran.
    let active_attach = handler.active_attach.lock().await;
    let active = active_attach
        .by_pid
        .get(&requester_pid)
        .expect("attached client remains registered");
    assert_eq!(active.key_table_name, None);
}

async fn client_key_table(handler: &RequestHandler, requester_pid: u32) -> Option<String> {
    let active_attach = handler.active_attach.lock().await;
    active_attach
        .by_pid
        .get(&requester_pid)
        .expect("attached client remains registered")
        .key_table_name
        .clone()
}

#[tokio::test]
async fn prefix_table_arrow_repeat_semantics_come_from_the_binding() {
    // The stock prefix arrows are `bind -r`, so after one `prefix + Up` the
    // client stays in the prefix table and a bare Up repeats the motion. The
    // removed shim ran its own action *after* this bookkeeping, so the repeat
    // flag now has to come from the binding that actually executes.
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_quiet_input_session(&handler, &alpha).await;
    let _control_rx = attach(&handler, requester_pid, &alpha).await;

    // Stack three panes vertically: 0 on top, then 1, then 2.
    for source in [0u32, 1] {
        let split = handler
            .handle(Request::SplitWindowExt(Box::new(
                rmux_proto::SplitWindowExtRequest {
                    target: SplitWindowTarget::Pane(PaneTarget::new(alpha.clone(), source)),
                    direction: SplitDirection::Vertical,
                    before: false,
                    environment: None,
                    command: Some(quiet_pane_command()),
                    process_command: None,
                    start_directory: None,
                    keep_alive_on_exit: None,
                    detached: false,
                    size: None,
                    preserve_zoom: false,
                    full_size: false,
                    stdin_payload: None,
                },
            )))
            .await;
        let Response::SplitWindow(split) = split else {
            panic!("expected split-window response: {split:?}");
        };
        handler
            .wait_for_pane_startup_to_finish_for_test(&split.pane)
            .await;
    }

    let bottom = PaneTarget::new(alpha.clone(), 2);
    let selected = handler
        .handle(Request::SelectPane(Box::new(SelectPaneRequest {
            target: bottom.clone(),
            title: None,
            style: None,
            input_disabled: None,
            preserve_zoom: false,
        })))
        .await;
    assert!(matches!(selected, Response::SelectPane(_)), "{selected:?}");

    dispatch_key_table(&handler, &bottom, "C-b").await;
    dispatch_key_table(&handler, &bottom, "Up").await;
    assert_eq!(
        active_pane_index(&handler, &alpha).await,
        1,
        "prefix Up must select the pane above"
    );
    assert_eq!(
        client_key_table(&handler, requester_pid).await.as_deref(),
        Some("prefix"),
        "a repeating binding keeps the client in the prefix table"
    );

    // No second prefix: the repeat window carries the bare arrow.
    dispatch_key_table(&handler, &PaneTarget::new(alpha.clone(), 1), "Up").await;
    assert_eq!(
        active_pane_index(&handler, &alpha).await,
        0,
        "the repeat window must run select-pane -U again"
    );
}

#[tokio::test]
async fn prefix_table_non_repeating_arrow_leaves_the_prefix_table() {
    // The mirror image: a user binding without `-r` must drop back to the root
    // table immediately, which is only observable now that the real binding
    // decides instead of the shim.
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let target = PaneTarget::new(alpha.clone(), 0);
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    let _control_rx = attach(&handler, requester_pid, &alpha).await;
    bind(
        &handler,
        "prefix",
        "Up",
        &["set-option", "-g", PROBE, "HIT-once"],
    )
    .await;

    dispatch_key_table(&handler, &target, "C-b").await;
    dispatch_key_table(&handler, &target, "Up").await;

    assert_eq!(probe_value(&handler, PROBE).await, "HIT-once");
    assert_eq!(
        client_key_table(&handler, requester_pid).await,
        None,
        "a non-repeating binding must return to the default table"
    );
}

#[tokio::test]
async fn prefix_table_repeating_user_binding_repeats() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let target = PaneTarget::new(alpha.clone(), 0);
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    let _control_rx = attach(&handler, requester_pid, &alpha).await;
    bind_repeating(
        &handler,
        "prefix",
        "Right",
        &["set-option", "-g", PROBE, "HIT-repeat"],
    )
    .await;

    dispatch_key_table(&handler, &target, "C-b").await;
    dispatch_key_table(&handler, &target, "Right").await;
    assert_eq!(probe_value(&handler, PROBE).await, "HIT-repeat");
    assert_eq!(
        client_key_table(&handler, requester_pid).await.as_deref(),
        Some("prefix"),
        "`bind -r` must hold the prefix table open"
    );

    // No second prefix: the repeat window must accept the bare key and hold
    // the table open again.
    dispatch_key_table(&handler, &target, "Right").await;
    assert_eq!(
        client_key_table(&handler, requester_pid).await.as_deref(),
        Some("prefix"),
        "the repeat window must run the user binding again"
    );
}

async fn active_pane_index(handler: &RequestHandler, session: &rmux_proto::SessionName) -> u32 {
    let state = handler.state.lock().await;
    state
        .sessions
        .session(session)
        .expect("session exists")
        .window_at(0)
        .expect("window exists")
        .active_pane_index()
}

// --------------------------------------------------------------------------
// copy-mode / copy-mode-vi, attached live input
// --------------------------------------------------------------------------

/// Type raw terminal bytes into the attached client, the path a real keyboard
/// takes. `\x1b[A` and friends are what Windows Terminal delivers for arrows.
async fn type_live(handler: &RequestHandler, requester_pid: u32, bytes: &[u8]) {
    handler
        .handle_attached_live_input_for_test(requester_pid, bytes)
        .await
        .expect("attached live input succeeds");
}

const ARROW_SEQUENCES: [(&str, &[u8]); 4] = [
    ("Up", b"\x1b[A"),
    ("Down", b"\x1b[B"),
    ("Right", b"\x1b[C"),
    ("Left", b"\x1b[D"),
];

async fn copy_mode_arrow_binding_case(mode_keys: &str, table: &str, key: &str, sequence: &[u8]) {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let target = PaneTarget::new(alpha.clone(), 0);
    let requester_pid = std::process::id();

    create_quiet_input_session(&handler, &alpha).await;
    let _control_rx = attach(&handler, requester_pid, &alpha).await;
    set_mode_keys(&handler, &alpha, mode_keys).await;
    fill_transcript(&handler, &target).await;

    let expected = format!("HIT-{key}");
    bind(
        &handler,
        table,
        key,
        &["set-option", "-g", PROBE, &expected],
    )
    .await;
    enter_copy_mode(&handler, &target).await;
    let before = copy_cursor_y(&handler, &target).await;

    type_live(&handler, requester_pid, sequence).await;

    assert_eq!(
        probe_value(&handler, PROBE).await,
        expected,
        "{table} {key} must run its user binding"
    );
    assert_eq!(
        copy_cursor_y(&handler, &target).await,
        before,
        "{table} {key} must not also move the copy cursor"
    );
}

#[tokio::test]
async fn copy_mode_arrow_bindings_run_the_user_command() {
    for (key, sequence) in ARROW_SEQUENCES {
        copy_mode_arrow_binding_case("emacs", "copy-mode", key, sequence).await;
    }
}

#[tokio::test]
async fn copy_mode_vi_arrow_bindings_run_the_user_command() {
    for (key, sequence) in ARROW_SEQUENCES {
        copy_mode_arrow_binding_case("vi", "copy-mode-vi", key, sequence).await;
    }
}

#[tokio::test]
async fn copy_mode_default_arrows_still_move_the_copy_cursor() {
    for (mode_keys, table) in [("emacs", "copy-mode"), ("vi", "copy-mode-vi")] {
        let handler = RequestHandler::new();
        let alpha = session_name("alpha");
        let target = PaneTarget::new(alpha.clone(), 0);
        let requester_pid = std::process::id();

        create_quiet_input_session(&handler, &alpha).await;
        let _control_rx = attach(&handler, requester_pid, &alpha).await;
        set_mode_keys(&handler, &alpha, mode_keys).await;
        fill_transcript(&handler, &target).await;
        enter_copy_mode(&handler, &target).await;

        let before = copy_cursor_y(&handler, &target).await;
        assert!(before > 0, "{table} needs room to move up");
        type_live(&handler, requester_pid, b"\x1b[A").await;
        let after_up = copy_cursor_y(&handler, &target).await;
        assert_eq!(
            after_up,
            before - 1,
            "{table} default Up must still move the copy cursor up"
        );

        type_live(&handler, requester_pid, b"\x1b[B").await;
        assert_eq!(
            copy_cursor_y(&handler, &target).await,
            before,
            "{table} default Down must still move the copy cursor down"
        );
    }
}

#[tokio::test]
async fn copy_mode_unbound_arrow_stays_inert() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let target = PaneTarget::new(alpha.clone(), 0);
    let requester_pid = std::process::id();

    create_quiet_input_session(&handler, &alpha).await;
    let _control_rx = attach(&handler, requester_pid, &alpha).await;
    set_mode_keys(&handler, &alpha, "emacs").await;
    fill_transcript(&handler, &target).await;
    unbind(&handler, "copy-mode", "Up").await;
    enter_copy_mode(&handler, &target).await;

    let before = copy_cursor_y(&handler, &target).await;
    type_live(&handler, requester_pid, b"\x1b[A").await;

    assert_eq!(
        copy_cursor_y(&handler, &target).await,
        before,
        "an unbound copy-mode Up must do nothing"
    );
}

#[tokio::test]
async fn copy_mode_modified_arrows_still_resolve_from_the_table() {
    // Modified arrows never reached the removed shim (decode_prompt_key keeps
    // their modifiers in a KeyName), and they must keep resolving from the
    // table. `\x1b[1;3A` / `\x1b[1;5A` are xterm M-Up / C-Up.
    for (name, sequence) in [
        ("M-Up", b"\x1b[1;3A".as_slice()),
        ("C-Up", b"\x1b[1;5A".as_slice()),
        ("M-Down", b"\x1b[1;3B".as_slice()),
        ("C-Down", b"\x1b[1;5B".as_slice()),
    ] {
        let handler = RequestHandler::new();
        let alpha = session_name("alpha");
        let target = PaneTarget::new(alpha.clone(), 0);
        let requester_pid = std::process::id();

        create_quiet_input_session(&handler, &alpha).await;
        let _control_rx = attach(&handler, requester_pid, &alpha).await;
        set_mode_keys(&handler, &alpha, "emacs").await;
        fill_transcript(&handler, &target).await;

        let expected = format!("HIT-{name}");
        bind(
            &handler,
            "copy-mode",
            name,
            &["set-option", "-g", PROBE, &expected],
        )
        .await;
        enter_copy_mode(&handler, &target).await;

        type_live(&handler, requester_pid, sequence).await;

        assert_eq!(
            probe_value(&handler, PROBE).await,
            expected,
            "copy-mode {name} must run its user binding"
        );
    }
}

#[tokio::test]
async fn copy_mode_default_modified_arrow_still_scrolls_a_half_page() {
    // The stock `bind -Tcopy-mode M-Up { send -X halfpage-up }` must survive the
    // arrow removal. halfpage-up scrolls the view, so measure scroll_position.
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let target = PaneTarget::new(alpha.clone(), 0);
    let requester_pid = std::process::id();

    create_quiet_input_session(&handler, &alpha).await;
    let _control_rx = attach(&handler, requester_pid, &alpha).await;
    set_mode_keys(&handler, &alpha, "emacs").await;
    fill_transcript(&handler, &target).await;
    enter_copy_mode(&handler, &target).await;

    let before = scroll_position(&handler, &target).await;
    type_live(&handler, requester_pid, b"\x1b[1;3A").await;
    let after = scroll_position(&handler, &target).await;
    assert!(
        after > before + 1,
        "default M-Up must still half-page up: {before} -> {after}"
    );
}

#[tokio::test]
async fn copy_mode_user_binding_replaces_the_default_motion() {
    // Rebinding Down to cursor-up inverts the motion. Under the removed shim
    // Down always ran cursor-down, so this is the causal check for #180.
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let target = PaneTarget::new(alpha.clone(), 0);
    let requester_pid = std::process::id();

    create_quiet_input_session(&handler, &alpha).await;
    let _control_rx = attach(&handler, requester_pid, &alpha).await;
    set_mode_keys(&handler, &alpha, "emacs").await;
    fill_transcript(&handler, &target).await;
    bind(&handler, "copy-mode", "Down", &["send", "-X", "cursor-up"]).await;
    enter_copy_mode(&handler, &target).await;

    let before = copy_cursor_y(&handler, &target).await;
    assert!(before > 0, "the copy cursor needs room to move up");
    type_live(&handler, requester_pid, b"\x1b[B").await;

    assert_eq!(
        copy_cursor_y(&handler, &target).await,
        before - 1,
        "a rebound copy-mode Down must run cursor-up, not the default cursor-down"
    );
}

// --------------------------------------------------------------------------
// neighbouring tables must not shift
// --------------------------------------------------------------------------

#[tokio::test]
async fn root_table_arrow_binding_still_resolves() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let target = PaneTarget::new(alpha.clone(), 0);
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    let _control_rx = attach(&handler, requester_pid, &alpha).await;
    bind(
        &handler,
        "root",
        "Up",
        &["set-option", "-g", PROBE, "HIT-root"],
    )
    .await;

    dispatch_key_table(&handler, &target, "Up").await;

    assert_eq!(probe_value(&handler, PROBE).await, "HIT-root");
}

#[tokio::test]
async fn copy_mode_default_q_still_cancels_through_the_table() {
    // `q` used to cancel through the shim; it must keep cancelling, now via
    // `bind -Tcopy-mode q { send -X cancel }` (tmux 3.7b: same binding).
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let target = PaneTarget::new(alpha.clone(), 0);
    let requester_pid = std::process::id();

    create_quiet_input_session(&handler, &alpha).await;
    let _control_rx = attach(&handler, requester_pid, &alpha).await;
    set_mode_keys(&handler, &alpha, "emacs").await;
    fill_transcript(&handler, &target).await;
    enter_copy_mode(&handler, &target).await;

    type_live(&handler, requester_pid, b"q").await;

    assert!(
        !in_copy_mode(&handler, &target).await,
        "q must still cancel copy mode"
    );
}

// --------------------------------------------------------------------------
// copy-mode / copy-mode-vi: every key the shim used to answer, not just arrows
// --------------------------------------------------------------------------

/// Is the pane still showing the copy-mode overlay?
async fn in_copy_mode(handler: &RequestHandler, target: &PaneTarget) -> bool {
    let state = handler.state.lock().await;
    let transcript = state
        .transcript_handle(target)
        .expect("pane transcript must exist");
    let in_mode = transcript.lock().copy_mode_state().is_some();
    in_mode
}

/// Read `#{selection_present}`, which discriminates begin-selection from the
/// page-down that tmux actually binds to emacs Space.
async fn selection_present(handler: &RequestHandler, target: &PaneTarget) -> String {
    let listed = handler
        .handle(Request::ListPanes(Box::new(ListPanesRequest {
            target: target.session_name().clone(),
            format: Some("#{selection_present}".to_owned()),
            filter: None,
            sort_order: None,
            reversed: false,
            target_window_index: None,
        })))
        .await;
    let output = listed
        .command_output()
        .expect("list-panes returns command output");
    String::from_utf8_lossy(output.stdout())
        .lines()
        .next()
        .expect("a pane row is rendered")
        .trim()
        .to_owned()
}

/// The keys `attached_copy_mode_input_action` answered before the tables.
/// tmux 3.7b honours a user binding on every one of them (16/16 measured).
const EMACS_SHIM_KEYS: [(&str, &[u8]); 5] = [
    ("Space", b" "),
    ("Enter", b"\r"),
    ("q", b"q"),
    ("n", b"n"),
    ("N", b"N"),
];

const VI_SHIM_KEYS: [(&str, &[u8]); 7] = [
    ("Space", b" "),
    ("Enter", b"\r"),
    ("q", b"q"),
    ("n", b"n"),
    ("N", b"N"),
    ("/", b"/"),
    ("?", b"?"),
];

async fn copy_mode_key_binding_case(mode_keys: &str, table: &str, key: &str, sequence: &[u8]) {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let target = PaneTarget::new(alpha.clone(), 0);
    let requester_pid = std::process::id();

    create_quiet_input_session(&handler, &alpha).await;
    let _control_rx = attach(&handler, requester_pid, &alpha).await;
    set_mode_keys(&handler, &alpha, mode_keys).await;
    fill_transcript(&handler, &target).await;

    let expected = format!("HIT-{key}");
    bind(
        &handler,
        table,
        key,
        &["set-option", "-g", PROBE, &expected],
    )
    .await;
    enter_copy_mode(&handler, &target).await;

    type_live(&handler, requester_pid, sequence).await;

    assert_eq!(
        probe_value(&handler, PROBE).await,
        expected,
        "{table} {key} must run its user binding"
    );
    assert!(
        in_copy_mode(&handler, &target).await,
        "{table} {key} must not also run the built-in action it replaced"
    );
}

#[tokio::test]
async fn copy_mode_shim_keys_run_the_user_command() {
    for (key, sequence) in EMACS_SHIM_KEYS {
        copy_mode_key_binding_case("emacs", "copy-mode", key, sequence).await;
    }
}

#[tokio::test]
async fn copy_mode_vi_shim_keys_run_the_user_command() {
    for (key, sequence) in VI_SHIM_KEYS {
        copy_mode_key_binding_case("vi", "copy-mode-vi", key, sequence).await;
    }
}

#[tokio::test]
async fn copy_mode_escape_binding_wins_on_the_bare_escape_path() {
    // A lone ESC never decodes into a key, so it reaches copy mode through the
    // escape-timeout flush rather than through the decoder. That second entry
    // point had its own copy of the shim.
    for (mode_keys, table) in [("emacs", "copy-mode"), ("vi", "copy-mode-vi")] {
        let handler = RequestHandler::new();
        let alpha = session_name("alpha");
        let target = PaneTarget::new(alpha.clone(), 0);
        let requester_pid = std::process::id();

        create_quiet_input_session(&handler, &alpha).await;
        let _control_rx = attach(&handler, requester_pid, &alpha).await;
        set_mode_keys(&handler, &alpha, mode_keys).await;
        fill_transcript(&handler, &target).await;
        bind(
            &handler,
            table,
            "Escape",
            &["set-option", "-g", PROBE, "HIT-Escape"],
        )
        .await;
        enter_copy_mode(&handler, &target).await;

        let mut pending_input = Vec::new();
        handler
            .handle_attached_live_input(requester_pid, &mut pending_input, b"\x1b")
            .await
            .expect("Escape prefix is retained until escape-time expires");
        assert_eq!(pending_input, b"\x1b");
        let forwarded = handler
            .flush_attached_pending_escape_input(requester_pid, &mut pending_input)
            .await
            .expect("Escape flush reaches copy mode");

        assert!(!forwarded, "{table} Escape must not leak to the pane");
        assert_eq!(
            probe_value(&handler, PROBE).await,
            "HIT-Escape",
            "{table} Escape must run its user binding"
        );
        assert!(
            in_copy_mode(&handler, &target).await,
            "{table} Escape must not also cancel copy mode"
        );
    }
}

#[tokio::test]
async fn copy_mode_unbound_shim_keys_stay_inert() {
    // tmux 3.7b: unbinding any of these leaves the key completely inert. There
    // is no hardcoded action waiting behind the table.
    for (mode_keys, table, key, sequence) in [
        ("emacs", "copy-mode", "q", b"q".as_slice()),
        ("emacs", "copy-mode", "Space", b" ".as_slice()),
        ("vi", "copy-mode-vi", "q", b"q".as_slice()),
        ("vi", "copy-mode-vi", "Enter", b"\r".as_slice()),
    ] {
        let handler = RequestHandler::new();
        let alpha = session_name("alpha");
        let target = PaneTarget::new(alpha.clone(), 0);
        let requester_pid = std::process::id();

        create_quiet_input_session(&handler, &alpha).await;
        let _control_rx = attach(&handler, requester_pid, &alpha).await;
        set_mode_keys(&handler, &alpha, mode_keys).await;
        fill_transcript(&handler, &target).await;
        unbind(&handler, table, key).await;
        enter_copy_mode(&handler, &target).await;
        let before = copy_cursor_y(&handler, &target).await;

        type_live(&handler, requester_pid, sequence).await;

        assert!(
            in_copy_mode(&handler, &target).await,
            "an unbound {table} {key} must not cancel copy mode"
        );
        assert_eq!(
            copy_cursor_y(&handler, &target).await,
            before,
            "an unbound {table} {key} must not move the copy cursor"
        );
        assert_eq!(
            selection_present(&handler, &target).await,
            "0",
            "an unbound {table} {key} must not start a selection"
        );
    }
}

#[tokio::test]
async fn copy_mode_space_defaults_match_the_measured_tmux_tables() {
    // tmux 3.7b binds emacs Space to page-down and vi Space to begin-selection;
    // C-Space is the emacs begin-selection. The shim answered begin-selection
    // for both flavours, which is what this pins against.
    for (mode_keys, expected_selection) in [("emacs", "0"), ("vi", "1")] {
        let handler = RequestHandler::new();
        let alpha = session_name("alpha");
        let target = PaneTarget::new(alpha.clone(), 0);
        let requester_pid = std::process::id();

        create_quiet_input_session(&handler, &alpha).await;
        let _control_rx = attach(&handler, requester_pid, &alpha).await;
        set_mode_keys(&handler, &alpha, mode_keys).await;
        fill_transcript(&handler, &target).await;
        enter_copy_mode(&handler, &target).await;

        type_live(&handler, requester_pid, b" ").await;

        assert_eq!(
            selection_present(&handler, &target).await,
            expected_selection,
            "{mode_keys} Space must follow the default table, not the removed shim"
        );
    }
}

#[tokio::test]
async fn copy_mode_emacs_c_space_begins_the_selection() {
    // The emacs begin-selection key tmux actually ships, so removing Space's
    // hardcoded begin-selection does not leave emacs users without one.
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let target = PaneTarget::new(alpha.clone(), 0);
    let requester_pid = std::process::id();

    create_quiet_input_session(&handler, &alpha).await;
    let _control_rx = attach(&handler, requester_pid, &alpha).await;
    set_mode_keys(&handler, &alpha, "emacs").await;
    fill_transcript(&handler, &target).await;
    enter_copy_mode(&handler, &target).await;

    type_live(&handler, requester_pid, b"\x00").await;

    assert_eq!(
        selection_present(&handler, &target).await,
        "1",
        "emacs C-Space must begin the selection"
    );
}

#[tokio::test]
async fn copy_mode_vi_escape_clears_the_selection_without_leaving_copy_mode() {
    // tmux 3.7b: `bind -Tcopy-mode-vi Escape { send -X clear-selection }`. The
    // shim ran `cancel-or-clear-selection`, which exits copy mode when nothing
    // is selected -- measured as a divergence against the oracle.
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let target = PaneTarget::new(alpha.clone(), 0);
    let requester_pid = std::process::id();

    create_quiet_input_session(&handler, &alpha).await;
    let _control_rx = attach(&handler, requester_pid, &alpha).await;
    set_mode_keys(&handler, &alpha, "vi").await;
    fill_transcript(&handler, &target).await;
    enter_copy_mode(&handler, &target).await;

    let mut pending_input = Vec::new();
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"\x1b")
        .await
        .expect("Escape prefix is retained until escape-time expires");
    handler
        .flush_attached_pending_escape_input(requester_pid, &mut pending_input)
        .await
        .expect("Escape flush reaches copy mode");

    assert!(
        in_copy_mode(&handler, &target).await,
        "vi Escape must clear the selection and stay in copy mode"
    );
}

#[tokio::test]
async fn copy_mode_emacs_enter_is_unbound_like_tmux() {
    // tmux 3.7b's emacs copy-mode table has no Enter entry, and neither does
    // rmux's. The shim invented `copy-selection-no-clear` for it.
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let target = PaneTarget::new(alpha.clone(), 0);
    let requester_pid = std::process::id();

    create_quiet_input_session(&handler, &alpha).await;
    let _control_rx = attach(&handler, requester_pid, &alpha).await;
    set_mode_keys(&handler, &alpha, "emacs").await;
    fill_transcript(&handler, &target).await;
    enter_copy_mode(&handler, &target).await;
    type_live(&handler, requester_pid, b"\x00").await;
    type_live(&handler, requester_pid, b"\x1b[C").await;
    assert_eq!(
        selection_present(&handler, &target).await,
        "1",
        "test setup needs a live emacs selection"
    );

    type_live(&handler, requester_pid, b"\r").await;

    assert!(
        in_copy_mode(&handler, &target).await,
        "emacs Enter is unbound in tmux 3.7b and must not copy-and-stay either"
    );
    assert_eq!(
        selection_present(&handler, &target).await,
        "1",
        "emacs Enter must leave the selection untouched"
    );
}

#[tokio::test]
async fn read_only_client_keys_reach_neither_the_table_nor_copy_mode() {
    // The read-only gate sits ahead of the whole live-key path. Removing the
    // copy-mode shim must not have moved any key in front of it.
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let target = PaneTarget::new(alpha.clone(), 0);
    let requester_pid = std::process::id();

    create_quiet_input_session(&handler, &alpha).await;
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    {
        let mut active_attach = handler.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get_mut(&requester_pid)
            .expect("read-only attach is active");
        active.can_write = false;
        active.flags = active.flags.with_read_only();
    }
    set_mode_keys(&handler, &alpha, "emacs").await;
    fill_transcript(&handler, &target).await;
    bind(
        &handler,
        "copy-mode",
        "q",
        &["set-option", "-g", PROBE, "HIT-readonly"],
    )
    .await;
    enter_copy_mode(&handler, &target).await;
    let before = copy_cursor_y(&handler, &target).await;

    type_live(&handler, requester_pid, b"q").await;
    type_live(&handler, requester_pid, b"\x1b[A").await;

    assert_eq!(
        probe_value(&handler, PROBE).await,
        "",
        "a read-only client must not run copy-mode bindings"
    );
    assert!(
        in_copy_mode(&handler, &target).await,
        "a read-only client must not cancel copy mode"
    );
    assert_eq!(
        copy_cursor_y(&handler, &target).await,
        before,
        "a read-only client must not move the copy cursor"
    );
}

#[tokio::test]
async fn prefix_table_wins_over_copy_mode_for_keys_the_shim_ate() {
    // Pressing the prefix inside copy mode must reach the prefix table. The
    // shim answered `q` first, so `bind -T prefix q` could never fire from a
    // pane that happened to be in copy mode.
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let target = PaneTarget::new(alpha.clone(), 0);
    let requester_pid = std::process::id();

    create_quiet_input_session(&handler, &alpha).await;
    let _control_rx = attach(&handler, requester_pid, &alpha).await;
    set_mode_keys(&handler, &alpha, "emacs").await;
    fill_transcript(&handler, &target).await;
    bind(
        &handler,
        "prefix",
        "q",
        &["set-option", "-g", PROBE, "HIT-prefix-q"],
    )
    .await;
    enter_copy_mode(&handler, &target).await;

    type_live(&handler, requester_pid, b"\x02").await;
    type_live(&handler, requester_pid, b"q").await;

    assert_eq!(
        probe_value(&handler, PROBE).await,
        "HIT-prefix-q",
        "prefix q must win over copy mode's built-in q"
    );
    assert!(
        in_copy_mode(&handler, &target).await,
        "the prefix binding must not also cancel copy mode"
    );
}

// --------------------------------------------------------------------------
// sibling entry paths that must not have moved
// --------------------------------------------------------------------------

/// SS3 arrows, i.e. what a terminal sends in application cursor mode.
const SS3_ARROW_SEQUENCES: [(&str, &[u8]); 4] = [
    ("Up", b"\x1bOA"),
    ("Down", b"\x1bOB"),
    ("Right", b"\x1bOC"),
    ("Left", b"\x1bOD"),
];

#[tokio::test]
async fn copy_mode_ss3_arrows_resolve_from_the_table() {
    // `decode_ss3_key` is a separate decoding branch from `decode_csi_key`.
    // The shim sat behind both, so both have to keep reaching the table.
    for (key, sequence) in SS3_ARROW_SEQUENCES {
        copy_mode_arrow_binding_case("emacs", "copy-mode", key, sequence).await;
    }
}

#[tokio::test]
async fn copy_mode_default_ss3_arrows_still_move_the_copy_cursor() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let target = PaneTarget::new(alpha.clone(), 0);
    let requester_pid = std::process::id();

    create_quiet_input_session(&handler, &alpha).await;
    let _control_rx = attach(&handler, requester_pid, &alpha).await;
    set_mode_keys(&handler, &alpha, "emacs").await;
    fill_transcript(&handler, &target).await;
    enter_copy_mode(&handler, &target).await;

    let before = copy_cursor_y(&handler, &target).await;
    assert!(before > 0, "the copy cursor needs room to move up");
    type_live(&handler, requester_pid, b"\x1bOA").await;

    assert_eq!(
        copy_cursor_y(&handler, &target).await,
        before - 1,
        "the default SS3 Up must still run cursor-up"
    );
}

#[tokio::test]
async fn detached_send_keys_x_ignores_the_key_tables() {
    // `send-keys -X` names a copy-mode command directly and never consults a
    // key table. Rebinding the key that would normally produce that command
    // must not change what `-X` does.
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let target = PaneTarget::new(alpha.clone(), 0);

    create_quiet_input_session(&handler, &alpha).await;
    set_mode_keys(&handler, &alpha, "emacs").await;
    fill_transcript(&handler, &target).await;
    bind(
        &handler,
        "copy-mode",
        "Up",
        &["set-option", "-g", PROBE, "HIT-Up"],
    )
    .await;
    enter_copy_mode(&handler, &target).await;

    let before = copy_cursor_y(&handler, &target).await;
    assert!(before > 0, "the copy cursor needs room to move up");
    let response = handler
        .handle(Request::SendKeysExt(SendKeysExtRequest {
            target: Some(target.clone()),
            keys: vec!["cursor-up".to_owned()],
            expand_formats: false,
            hex: false,
            literal: false,
            dispatch_key_table: false,
            copy_mode_command: true,
            forward_mouse_event: false,
            reset_terminal: false,
            repeat_count: None,
        }))
        .await;
    assert!(matches!(response, Response::SendKeys(_)), "{response:?}");

    assert_eq!(
        copy_cursor_y(&handler, &target).await,
        before - 1,
        "send-keys -X cursor-up must still move the copy cursor"
    );
    assert_eq!(
        probe_value(&handler, PROBE).await,
        "",
        "send-keys -X must not run the copy-mode key binding"
    );
}

/// Read `#{pane_mode}` for the first pane of the session.
async fn pane_mode(handler: &RequestHandler, target: &PaneTarget) -> String {
    let listed = handler
        .handle(Request::ListPanes(Box::new(ListPanesRequest {
            target: target.session_name().clone(),
            format: Some("#{pane_mode}".to_owned()),
            filter: None,
            sort_order: None,
            reversed: false,
            target_window_index: None,
        })))
        .await;
    let output = listed
        .command_output()
        .expect("list-panes returns command output");
    // An out-of-mode pane renders an empty `#{pane_mode}`, so an absent row and
    // an empty row mean the same thing here.
    String::from_utf8_lossy(output.stdout())
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_owned()
}

#[tokio::test]
async fn mode_tree_overlay_still_wins_over_the_copy_mode_table() {
    // The mode-tree branch sits ahead of the pane-target lookup in
    // `handle_attached_live_key_inner`. Removing the copy-mode shim must not
    // have moved any key in front of it.
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let target = PaneTarget::new(alpha.clone(), 0);
    let requester_pid = std::process::id();

    create_quiet_input_session(&handler, &alpha).await;
    let _control_rx = attach(&handler, requester_pid, &alpha).await;
    bind(
        &handler,
        "copy-mode",
        "q",
        &["set-option", "-g", PROBE, "HIT-copy-mode-q"],
    )
    .await;

    let commands = handler
        .parse_control_commands("choose-tree -Zw")
        .await
        .expect("choose-tree parses");
    handler
        .execute_parsed_commands_for_test(requester_pid, commands)
        .await
        .expect("choose-tree activates mode-tree");
    assert_eq!(pane_mode(&handler, &target).await, "tree-mode");

    type_live(&handler, requester_pid, b"q").await;

    assert_eq!(
        pane_mode(&handler, &target).await,
        "",
        "q must close the mode tree, not fall through to a key table"
    );
    assert_eq!(
        probe_value(&handler, PROBE).await,
        "",
        "the copy-mode binding must not fire while the mode tree is open"
    );
}

#[tokio::test]
async fn root_escape_binding_wins_on_the_bare_escape_path() {
    let handler = RequestHandler::new();
    let alpha = session_name("root-escape");
    let requester_pid = std::process::id();
    create_quiet_input_session(&handler, &alpha).await;
    let _control_rx = attach(&handler, requester_pid, &alpha).await;
    bind(
        &handler,
        "root",
        "Escape",
        &["set-option", "-g", PROBE, "HIT-Escape"],
    )
    .await;
    let mut pending_input = Vec::new();
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"\x1b")
        .await
        .expect("retain Escape");
    assert_eq!(pending_input, b"\x1b");
    assert!(!handler
        .flush_attached_pending_escape_input(requester_pid, &mut pending_input)
        .await
        .expect("dispatch completed Escape"));
    assert_eq!(probe_value(&handler, PROBE).await, "HIT-Escape");
    assert!(pending_input.is_empty());
}
