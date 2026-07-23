use super::*;
use rmux_core::{input::InputParser, Screen};

const SIZE: TerminalSize = TerminalSize { cols: 20, rows: 5 };

async fn replace_contents(handler: &RequestHandler, target: &PaneTarget) {
    let transcript = {
        let state = handler.state.lock().await;
        state.transcript_handle(target).expect("session transcript")
    };
    let history_limit = transcript
        .lock()
        .expect("pane transcript mutex")
        .history_limit();
    let mut screen = Screen::new(SIZE, history_limit);
    let mut parser = InputParser::new();
    parser.parse(
        b"zero one two three\r\nalpha beta gamma\r\nomega sigma tau\r\n",
        &mut screen,
    );
    transcript
        .lock()
        .expect("pane transcript mutex")
        .set_screen_for_test(screen);
}

async fn fixture(
    name: &str,
) -> (
    RequestHandler,
    rmux_proto::SessionName,
    PaneTarget,
    PaneId,
    u32,
) {
    let handler = RequestHandler::new();
    let session = session_name(name);
    create_quiet_input_session(&handler, &session).await;
    let target = PaneTarget::new(session.clone(), 0);
    replace_contents(&handler, &target).await;

    for (option, value) in [(OptionName::ModeKeys, "vi"), (OptionName::Mouse, "on")] {
        assert!(matches!(
            handler
                .handle(Request::SetOption(SetOptionRequest {
                    scope: ScopeSelector::Global,
                    option,
                    value: value.to_owned(),
                    mode: SetOptionMode::Replace,
                }))
                .await,
            Response::SetOption(_)
        ));
    }

    let requester_pid = std::process::id();
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, session.clone(), control_tx)
        .await;
    let _control_drain =
        spawn_accounted_attach_control_drain(&handler, requester_pid, control_rx).await;

    assert!(matches!(
        handler
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
            .await,
        Response::CopyMode(_)
    ));

    let pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&session)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0))
            .map(rmux_core::Pane::id)
            .expect("pane exists")
    };
    (handler, session, target, pane_id, requester_pid)
}

async fn reset_keyboard_cursor(handler: &RequestHandler, target: &PaneTarget) {
    for command in [
        "clear-selection",
        "history-top",
        "start-of-line",
        "cursor-right",
    ] {
        handler
            .execute_copy_mode_command(std::process::id(), target.clone(), command, &[], 1)
            .await
            .unwrap_or_else(|error| panic!("{command} succeeds: {error}"));
    }
}

async fn summary(
    handler: &RequestHandler,
    session: &rmux_proto::SessionName,
    pane_id: PaneId,
) -> crate::copy_mode::CopyModeSummary {
    let state = handler.state.lock().await;
    state
        .pane_copy_mode_summary(session, pane_id)
        .expect("copy-mode summary")
}

async fn install_cached_mouse_event(
    handler: &RequestHandler,
    target: &PaneTarget,
    pane_id: PaneId,
    requester_pid: u32,
) {
    let window_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(target.session_name())
            .and_then(|session| session.window_at(target.window_index()))
            .map(rmux_core::Window::id)
            .expect("window exists")
    };
    let mut active_attach = handler.active_attach.lock().await;
    let active = active_attach
        .by_pid
        .get_mut(&requester_pid)
        .expect("attached client exists");
    active.mouse.current_event = Some(AttachedMouseEvent {
        raw: MouseForwardEvent {
            b: 0,
            lb: 0,
            x: 12,
            y: 3,
            lx: 12,
            ly: 3,
            sgr_b: 0,
            sgr_type: 'M',
            ignore: false,
        },
        session_id: 0,
        window_id: Some(window_id.as_u32()),
        pane_id: Some(pane_id),
        pane_target: Some(target.clone()),
        location: MouseLocation::Pane,
        status_at: None,
        status_lines: 0,
        ignore: false,
    });
}

async fn install_scrollback(handler: &RequestHandler, target: &PaneTarget) {
    let transcript = {
        let state = handler.state.lock().await;
        state.transcript_handle(target).expect("session transcript")
    };
    let history_limit = transcript
        .lock()
        .expect("pane transcript mutex")
        .history_limit();
    let mut screen = Screen::new(SIZE, history_limit);
    let mut parser = InputParser::new();
    let contents = (0..80)
        .map(|line| format!("line {line:02}\r\n"))
        .collect::<String>();
    parser.parse(contents.as_bytes(), &mut screen);
    transcript
        .lock()
        .expect("pane transcript mutex")
        .set_screen_for_test(screen);
    assert!(matches!(
        handler
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
            .await,
        Response::CopyMode(_)
    ));
}

async fn send_copy_command(
    handler: &RequestHandler,
    target: &PaneTarget,
    command: &str,
) -> Response {
    handler
        .handle(Request::SendKeysExt(SendKeysExtRequest {
            target: Some(target.clone()),
            keys: vec![command.to_owned()],
            expand_formats: false,
            hex: false,
            literal: false,
            dispatch_key_table: false,
            copy_mode_command: true,
            forward_mouse_event: false,
            reset_terminal: false,
            repeat_count: None,
        }))
        .await
}

#[tokio::test]
async fn keyboard_and_cli_copy_commands_ignore_cached_mouse_position() {
    // Oracle tmux 3.7b: both direct `send-keys -X` and an attached keyboard
    // binding preserve the keyboard copy cursor even after a mouse event.
    let (handler, session, target, pane_id, requester_pid) = fixture("mouse-cache-keyboard").await;
    install_cached_mouse_event(&handler, &target, pane_id, requester_pid).await;

    reset_keyboard_cursor(&handler, &target).await;
    let before = summary(&handler, &session, pane_id).await;
    assert_eq!((before.cursor_x, before.cursor_y), (1, 0));
    let response = send_copy_command(&handler, &target, "begin-selection").await;
    assert_eq!(
        response,
        Response::SendKeys(SendKeysResponse { key_count: 1 })
    );
    let direct = summary(&handler, &session, pane_id).await;
    assert_eq!((direct.cursor_x, direct.cursor_y), (1, 0));
    assert_eq!(
        direct
            .selection_start
            .map(|position| (position.x, position.y)),
        Some((1, 0))
    );

    for (label, dispatch_key_table) in [("plain send-keys", false), ("send-keys -K", true)] {
        reset_keyboard_cursor(&handler, &target).await;
        let response = handler
            .handle(Request::SendKeysExt(SendKeysExtRequest {
                target: Some(target.clone()),
                keys: vec!["Space".to_owned()],
                expand_formats: false,
                hex: false,
                literal: false,
                dispatch_key_table,
                copy_mode_command: false,
                forward_mouse_event: false,
                reset_terminal: false,
                repeat_count: None,
            }))
            .await;
        assert_eq!(
            response,
            Response::SendKeys(SendKeysResponse { key_count: 1 }),
            "{label} dispatches one key"
        );
        let injected = summary(&handler, &session, pane_id).await;
        assert_eq!(
            (injected.cursor_x, injected.cursor_y),
            (1, 0),
            "{label} must ignore the cached mouse coordinate"
        );
        assert_eq!(
            injected
                .selection_start
                .map(|position| (position.x, position.y)),
            Some((1, 0)),
            "{label} must anchor at the keyboard cursor"
        );
    }

    reset_keyboard_cursor(&handler, &target).await;
    handler
        .handle_attached_live_input_for_test(requester_pid, b" ")
        .await
        .expect("attached keyboard begin-selection");
    let keyboard = summary(&handler, &session, pane_id).await;
    assert_eq!((keyboard.cursor_x, keyboard.cursor_y), (1, 0));
    assert_eq!(
        keyboard
            .selection_start
            .map(|position| (position.x, position.y)),
        Some((1, 0))
    );

    reset_keyboard_cursor(&handler, &target).await;
    handler
        .handle_attached_live_input_for_test(requester_pid, b"V")
        .await
        .expect("attached keyboard select-line");
    let line = summary(&handler, &session, pane_id).await;
    assert_eq!(line.selection_start.map(|position| position.y), Some(0));

    reset_keyboard_cursor(&handler, &target).await;
    assert!(matches!(
        send_copy_command(&handler, &target, "select-word").await,
        Response::SendKeys(SendKeysResponse { key_count: 1 })
    ));
    let word = summary(&handler, &session, pane_id).await;
    assert_eq!(word.selection_start.map(|position| position.y), Some(0));
}

#[tokio::test]
async fn non_mouse_scroll_to_mouse_is_a_safe_noop_product_divergence() {
    // Oracle tmux 3.7b: invoking scroll-to-mouse from CLI or a keyboard binding
    // dereferences a missing mouse event and kills the server. RMUX deliberately
    // keeps this non-mouse invocation as a safe no-op.
    let (handler, session, target, pane_id, requester_pid) = fixture("scroll-without-origin").await;
    install_scrollback(&handler, &target).await;
    install_cached_mouse_event(&handler, &target, pane_id, requester_pid).await;
    {
        let mut active_attach = handler.active_attach.lock().await;
        active_attach
            .by_pid
            .get_mut(&requester_pid)
            .expect("attached client exists")
            .mouse
            .slider_mpos = 1;
    }
    handler
        .execute_copy_mode_command(requester_pid, target.clone(), "history-bottom", &[], 1)
        .await
        .expect("history-bottom succeeds");
    let before = summary(&handler, &session, pane_id).await;
    assert!(before.history_size > 0);
    assert_eq!(before.scroll_position, 0);

    assert!(matches!(
        send_copy_command(&handler, &target, "scroll-to-mouse").await,
        Response::SendKeys(SendKeysResponse { key_count: 1 })
    ));
    let after = summary(&handler, &session, pane_id).await;
    assert_eq!(after.scroll_position, 0);
}

#[tokio::test]
async fn live_mouse_binding_uses_its_originating_event() {
    // Oracle tmux 3.7b: a MouseDrag1Pane begin-selection starts at the press
    // cell and ends at the current drag cell.
    let (handler, session, target, pane_id, requester_pid) = fixture("mouse-origin-binding").await;
    reset_keyboard_cursor(&handler, &target).await;
    assert!(matches!(
        handler
            .handle(Request::BindKey(Box::new(BindKeyRequest {
                table_name: "copy-mode-vi".to_owned(),
                key: "MouseDrag1Pane".to_owned(),
                note: Some("issue-125-origin".to_owned()),
                repeat: false,
                command: Some(vec![
                    "send-keys".to_owned(),
                    "-X".to_owned(),
                    "begin-selection".to_owned(),
                ]),
            })))
            .await,
        Response::BindKey(_)
    ));

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x1b[<0;5;2M")
        .await
        .expect("live mouse press");

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x1b[<32;10;3M")
        .await
        .expect("live mouse drag binding");
    let dragged = summary(&handler, &session, pane_id).await;
    assert_eq!((dragged.cursor_x, dragged.cursor_y), (9, 2));
    assert_eq!(
        dragged
            .selection_start
            .map(|position| (position.x, position.y)),
        Some((4, 1)),
        "mouse drag must use the press cell as its selection anchor"
    );
    assert_eq!(
        dragged
            .selection_end
            .map(|position| (position.x, position.y)),
        Some((9, 2))
    );
}
