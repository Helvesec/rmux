use super::*;
use rmux_core::{input::InputParser, Screen};

async fn replace_transcript_contents(
    handler: &RequestHandler,
    target: &PaneTarget,
    size: TerminalSize,
    content: &[u8],
) {
    handler
        .wait_for_pane_startup_to_finish_for_test(target)
        .await;
    let transcript = {
        let state = handler.state.lock().await;
        state
            .transcript_handle(target)
            .expect("session transcript must exist")
    };
    let history_limit = transcript
        .lock()
        .expect("pane transcript mutex must not be poisoned")
        .history_limit();
    let mut screen = Screen::new(size, history_limit);
    let mut parser = InputParser::new();
    parser.parse(content, &mut screen);
    transcript
        .lock()
        .expect("pane transcript mutex must not be poisoned")
        .set_screen_for_test(screen);
}

#[tokio::test]
async fn copy_mode_mouse_drag_start_anchors_on_press_cell() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();
    let target = PaneTarget::new(alpha.clone(), 0);

    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: Some(TerminalSize { cols: 20, rows: 5 }),
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let (window_id, pane_id) = {
        let state = handler.state.lock().await;
        let session = state.sessions.session(&alpha).expect("session exists");
        let window = session.window_at(0).expect("window exists");
        let pane = window.pane(0).expect("pane exists");
        (window.id(), pane.id())
    };

    {
        let mut active_attach = handler.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get_mut(&requester_pid)
            .expect("attached client exists");
        active.mouse.current_event = Some(AttachedMouseEvent {
            raw: MouseForwardEvent {
                b: 32,
                lb: 0,
                x: 6,
                y: 1,
                lx: 1,
                ly: 1,
                sgr_b: 32,
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

    let response = handler
        .handle(Request::CopyMode(CopyModeRequest {
            target: Some(target),
            page_down: false,
            exit_on_scroll: false,
            hide_position: false,
            mouse_drag_start: true,
            cancel_mode: false,
            scrollbar_scroll: false,
            source: None,
            page_up: false,
        }))
        .await;
    assert!(matches!(response, Response::CopyMode(_)));

    let summary = {
        let state = handler.state.lock().await;
        state
            .pane_copy_mode_summary(&alpha, pane_id)
            .expect("copy mode summary")
    };
    let selection_start = summary
        .selection_start
        .expect("copy-mode -M should set a selection anchor");
    assert_eq!(
        selection_start.x, 1,
        "copy-mode -M must anchor at the press cell, not the first drag cell"
    );
}

#[cfg(unix)]
fn quiet_copy_mode_fixture_command() -> Vec<String> {
    vec!["/bin/sh".to_owned(), "-c".to_owned(), "sleep 60".to_owned()]
}

#[cfg(windows)]
fn quiet_copy_mode_fixture_command() -> Vec<String> {
    let system_root =
        std::env::var_os("SystemRoot").unwrap_or_else(|| std::ffi::OsString::from(r"C:\Windows"));
    let cmd = std::path::PathBuf::from(system_root)
        .join("System32")
        .join("cmd.exe");
    vec![
        cmd.to_string_lossy().into_owned(),
        "/d".to_owned(),
        "/q".to_owned(),
        "/c".to_owned(),
        "ping -n 120 127.0.0.1 >NUL".to_owned(),
    ]
}

#[tokio::test]
async fn copy_mode_single_motion_drag_copies_from_press_to_motion_cell() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();
    let target = PaneTarget::new(alpha.clone(), 0);
    let size = TerminalSize { cols: 20, rows: 5 };

    let created = handler
        .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
            session_name: Some(alpha.clone()),
            working_directory: None,
            detached: true,
            size: Some(size),
            environment: None,
            group_target: None,
            attach_if_exists: false,
            detach_other_clients: false,
            kill_other_clients: false,
            flags: None,
            window_name: None,
            print_session_info: false,
            print_format: None,
            command: Some(quiet_copy_mode_fixture_command()),
            process_command: None,
            client_environment: None,
            skip_environment_update: false,
        })))
        .await;
    assert!(matches!(created, Response::NewSession(_)));
    replace_transcript_contents(&handler, &target, size, b"ABCDEF\r\n").await;

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let (window_id, pane_id) = {
        let state = handler.state.lock().await;
        let session = state.sessions.session(&alpha).expect("session exists");
        let window = session.window_at(0).expect("window exists");
        let pane = window.pane(0).expect("pane exists");
        (window.id(), pane.id())
    };

    {
        let mut active_attach = handler.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get_mut(&requester_pid)
            .expect("attached client exists");
        active.mouse.current_event = Some(AttachedMouseEvent {
            raw: MouseForwardEvent {
                b: 32,
                lb: 0,
                x: 1,
                y: 0,
                lx: 0,
                ly: 0,
                sgr_b: 32,
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

    let response = handler
        .handle(Request::CopyMode(CopyModeRequest {
            target: Some(target.clone()),
            page_down: false,
            exit_on_scroll: false,
            hide_position: false,
            mouse_drag_start: true,
            cancel_mode: false,
            scrollbar_scroll: false,
            source: None,
            page_up: false,
        }))
        .await;
    assert!(matches!(response, Response::CopyMode(_)));

    let copied = handler
        .handle(Request::SendKeysExt(SendKeysExtRequest {
            target: Some(target),
            keys: vec!["copy-selection".to_owned()],
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
    assert!(matches!(
        copied,
        Response::SendKeys(SendKeysResponse { key_count: 1 })
    ));

    let shown = handler
        .handle(Request::ShowBuffer(ShowBufferRequest { name: None }))
        .await;
    let Response::ShowBuffer(response) = shown else {
        panic!("expected show-buffer response, got {shown:?}");
    };
    assert_eq!(
        response.command_output().stdout(),
        b"A",
        "a quick one-motion mouse drag from A to B should copy A like tmux"
    );
}

#[tokio::test]
async fn copy_mode_mouse_entry_uses_left_scrollbar_content_origin() {
    let handler = RequestHandler::new();
    let alpha = session_name("copy-mode-left-scrollbar");
    let requester_pid = std::process::id();
    let target = PaneTarget::new(alpha.clone(), 0);
    let created = handler
        .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
            session_name: Some(alpha.clone()),
            working_directory: None,
            detached: true,
            size: Some(TerminalSize { cols: 20, rows: 5 }),
            environment: None,
            group_target: None,
            attach_if_exists: false,
            detach_other_clients: false,
            kill_other_clients: false,
            flags: None,
            window_name: None,
            print_session_info: false,
            print_format: None,
            command: Some(quiet_copy_mode_fixture_command()),
            process_command: None,
            client_environment: None,
            skip_environment_update: false,
        })))
        .await;
    assert!(matches!(created, Response::NewSession(_)));
    for (option, value) in [
        (OptionName::PaneScrollbars, "on"),
        (OptionName::PaneScrollbarsPosition, "left"),
        (OptionName::PaneScrollbarsStyle, "width=2,pad=1"),
    ] {
        assert!(matches!(
            handler
                .handle(Request::SetOption(SetOptionRequest {
                    scope: ScopeSelector::Window(WindowTarget::with_window(alpha.clone(), 0)),
                    option,
                    value: value.to_owned(),
                    mode: SetOptionMode::Replace,
                }))
                .await,
            Response::SetOption(_)
        ));
    }
    replace_transcript_contents(
        &handler,
        &target,
        TerminalSize { cols: 17, rows: 4 },
        b"ABCDEF\r\n",
    )
    .await;

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    let (window_id, pane_id) = {
        let state = handler.state.lock().await;
        let window = state
            .sessions
            .session(&alpha)
            .expect("session exists")
            .window();
        (window.id(), window.pane(0).expect("pane exists").id())
    };
    {
        let mut active_attach = handler.active_attach.lock().await;
        active_attach
            .by_pid
            .get_mut(&requester_pid)
            .expect("attached client exists")
            .mouse
            .current_event = Some(AttachedMouseEvent {
            raw: MouseForwardEvent {
                b: 32,
                lb: 0,
                // The two track cells and one pad cell precede content.
                x: 4,
                y: 0,
                lx: 3,
                ly: 0,
                sgr_b: 32,
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

    let entered = handler
        .handle(Request::CopyMode(CopyModeRequest {
            target: Some(target.clone()),
            page_down: false,
            exit_on_scroll: false,
            hide_position: false,
            mouse_drag_start: true,
            cancel_mode: false,
            scrollbar_scroll: false,
            source: None,
            page_up: false,
        }))
        .await;
    assert!(matches!(entered, Response::CopyMode(_)));
    let copied = handler
        .handle(Request::SendKeysExt(SendKeysExtRequest {
            target: Some(target),
            keys: vec!["copy-selection".to_owned()],
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
    assert!(matches!(copied, Response::SendKeys(_)));
    let shown = handler
        .handle(Request::ShowBuffer(ShowBufferRequest { name: None }))
        .await;
    let Response::ShowBuffer(response) = shown else {
        panic!("expected show-buffer response, got {shown:?}");
    };
    assert_eq!(
        response.command_output().stdout(),
        b"A",
        "the first content cell after a left scrollbar must remain copy-mode x=0"
    );
}
