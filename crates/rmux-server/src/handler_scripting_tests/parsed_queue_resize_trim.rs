use super::*;
use crate::input_keys::MouseForwardEvent;
use crate::mouse::{AttachedMouseEvent, MouseLocation};
use crate::pane_terminals::PaneCaptureRequest;
use rmux_core::{GridRenderOptions, PaneId, ScreenCaptureRange};

#[tokio::test]
async fn parsed_queue_resize_pane_trim_flag_trims_below_cursor() {
    let handler = RequestHandler::new();
    let session = session_name("resize-trim");
    let target = PaneTarget::with_window(session.clone(), 0, 0);
    create_test_session(
        &handler,
        session.clone(),
        TerminalSize { cols: 10, rows: 5 },
    )
    .await;
    {
        let mut state = handler.state.lock().await;
        state
            .append_bytes_to_pane_transcript_for_test(
                &session,
                0,
                0,
                b"01\r\n02\r\n03\r\n04\r\n05\r\n06\r\n07\r\n08\r\n09\r\n10\x1b[3;1H",
            )
            .expect("transcript append succeeds");
    }

    execute(&handler, "resize-pane -T -t resize-trim:0.0").await;

    let captured = {
        let state = handler.state.lock().await;
        state
            .capture_transcript(
                &target,
                PaneCaptureRequest {
                    range: ScreenCaptureRange {
                        start_is_absolute: true,
                        end_is_absolute: true,
                        ..ScreenCaptureRange::default()
                    },
                    options: GridRenderOptions::default(),
                    alternate: false,
                    use_mode_screen: false,
                    pending_input: false,
                    quiet: false,
                    escape_pending: false,
                },
            )
            .expect("capture succeeds")
    };
    let captured = String::from_utf8(captured).expect("capture is utf-8");
    assert_eq!(
        captured.lines().collect::<Vec<_>>(),
        vec!["01", "02", "03", "04", "05", "06", "07", "08"]
    );
}

#[tokio::test]
async fn parsed_queue_resize_pane_trim_flag_takes_precedence_over_size_flags() {
    let handler = RequestHandler::new();
    let session = session_name("resize-trim-size");
    create_test_session(
        &handler,
        session.clone(),
        TerminalSize { cols: 80, rows: 24 },
    )
    .await;
    execute(&handler, "split-window -v -t resize-trim-size:0.0").await;

    let before = pane_height(&handler, &session, 0).await;
    execute(&handler, "resize-pane -T -y 5 -t resize-trim-size:0.0").await;
    let after = pane_height(&handler, &session, 0).await;

    assert_eq!(after, before);
}

#[tokio::test]
async fn parsed_queue_resize_pane_zoom_takes_precedence_over_other_adjustments() {
    let handler = RequestHandler::new();
    let session = session_name("resize-zoom-precedence");
    create_test_session(
        &handler,
        session.clone(),
        TerminalSize { cols: 80, rows: 24 },
    )
    .await;
    execute(&handler, "split-window -h -t resize-zoom-precedence:0.0").await;

    execute(&handler, "resize-pane -R -Z -t resize-zoom-precedence:0.0").await;

    let state = handler.state.lock().await;
    let window = state
        .sessions
        .session(&session)
        .expect("session exists")
        .window_at(0)
        .expect("window exists");
    assert!(window.is_zoomed());
}

#[tokio::test]
async fn parsed_queue_resize_pane_repeated_directions_follow_tmux_priority() {
    let handler = RequestHandler::new();
    let session = session_name("resize-priority");
    create_test_session(
        &handler,
        session.clone(),
        TerminalSize { cols: 80, rows: 24 },
    )
    .await;
    execute(&handler, "split-window -h -t resize-priority:0.0").await;

    execute(&handler, "resize-pane -L -R -t resize-priority:0.0").await;

    assert_eq!(
        pane_sizes(&handler, &session).await,
        vec![(39, 24), (40, 24)]
    );
}

#[tokio::test]
async fn parsed_queue_resize_pane_trailing_adjustment_after_target_matches_tmux() {
    let handler = RequestHandler::new();
    let session = session_name("resize-trailing-adjustment");
    create_test_session(
        &handler,
        session.clone(),
        TerminalSize { cols: 80, rows: 24 },
    )
    .await;
    execute(
        &handler,
        "split-window -h -t resize-trailing-adjustment:0.0",
    )
    .await;

    execute(
        &handler,
        "resize-pane -R -L -t resize-trailing-adjustment:0.0 3",
    )
    .await;

    assert_eq!(
        pane_sizes(&handler, &session).await,
        vec![(37, 24), (42, 24)]
    );
}

#[tokio::test]
async fn parsed_queue_resize_pane_composes_absolute_then_relative_like_tmux() {
    let handler = RequestHandler::new();
    let session = session_name("resize-compose");
    create_test_session(
        &handler,
        session.clone(),
        TerminalSize { cols: 80, rows: 24 },
    )
    .await;
    execute(&handler, "split-window -h -t resize-compose:0.0").await;

    execute(&handler, "resize-pane -x 30 -R -t resize-compose:0.0").await;

    assert_eq!(
        pane_sizes(&handler, &session).await,
        vec![(31, 24), (48, 24)]
    );
}

#[tokio::test]
async fn parsed_queue_resize_pane_mouse_flag_is_noop_without_mouse_context() {
    let handler = RequestHandler::new();
    let session = session_name("resize-mouse-noop");
    create_test_session(
        &handler,
        session.clone(),
        TerminalSize { cols: 80, rows: 24 },
    )
    .await;
    execute(&handler, "split-window -h -t resize-mouse-noop:0.0").await;

    let before = pane_sizes(&handler, &session).await;
    execute(&handler, "resize-pane -M -t resize-mouse-noop:0.0").await;
    let after = pane_sizes(&handler, &session).await;

    assert_eq!(after, before);
}

#[tokio::test]
async fn parsed_queue_resize_pane_mouse_flag_resizes_from_border_context() {
    let handler = RequestHandler::new();
    let session = session_name("resize-mouse-border");
    let target = PaneTarget::with_window(session.clone(), 0, 0);
    create_test_session(
        &handler,
        session.clone(),
        TerminalSize { cols: 80, rows: 24 },
    )
    .await;
    execute(&handler, "split-window -h -t resize-mouse-border:0.0").await;

    let before = pane_sizes(&handler, &session).await;
    let border_x = before.first().expect("first pane").0.saturating_add(5);
    let mouse_event = AttachedMouseEvent {
        raw: MouseForwardEvent {
            b: 32,
            lb: 32,
            x: border_x,
            y: 0,
            lx: border_x.saturating_sub(1),
            ly: 0,
            sgr_b: 32,
            sgr_type: 'M',
            ignore: false,
        },
        session_id: 1,
        window_id: Some(1),
        pane_id: Some(PaneId::new(0)),
        pane_target: Some(target.clone()),
        location: MouseLocation::Border,
        status_at: None,
        status_lines: 0,
        ignore: false,
    };
    let parsed = CommandParser::new()
        .parse("resize-pane -M")
        .expect("command parses");

    handler
        .execute_parsed_commands(
            std::process::id(),
            parsed,
            QueueExecutionContext::without_caller_cwd()
                .with_current_target(Some(Target::Pane(target.clone())))
                .with_mouse_target(Some(Target::Pane(target.clone())))
                .with_mouse_event(Some(mouse_event.clone())),
        )
        .await
        .expect("mouse resize executes");

    let after = pane_sizes(&handler, &session).await;
    assert!(
        after[0].0 > before[0].0,
        "first pane should grow after mouse border resize: before={before:?} after={after:?}"
    );
}

#[tokio::test]
async fn parsed_queue_resize_pane_mouse_flag_shrinks_horizontal_split_from_border_context() {
    let handler = RequestHandler::new();
    let session = session_name("resize-mouse-border-shrink-h");
    let target = PaneTarget::with_window(session.clone(), 0, 0);
    create_test_session(
        &handler,
        session.clone(),
        TerminalSize { cols: 80, rows: 24 },
    )
    .await;
    execute(
        &handler,
        "split-window -h -t resize-mouse-border-shrink-h:0.0",
    )
    .await;

    let before = pane_sizes(&handler, &session).await;
    let border_x = before.first().expect("first pane").0;
    let mouse_event = border_mouse_event(
        target.clone(),
        PaneId::new(0),
        border_x.saturating_sub(4),
        0,
        border_x,
        0,
    );
    let parsed = CommandParser::new()
        .parse("resize-pane -M")
        .expect("command parses");

    handler
        .execute_parsed_commands(
            std::process::id(),
            parsed,
            QueueExecutionContext::without_caller_cwd()
                .with_current_target(Some(Target::Pane(target.clone())))
                .with_mouse_target(Some(Target::Pane(target.clone())))
                .with_mouse_event(Some(mouse_event)),
        )
        .await
        .expect("mouse resize executes");

    let after = pane_sizes(&handler, &session).await;
    assert!(
        after[0].0 < before[0].0,
        "first pane should shrink after dragging the vertical border left: before={before:?} after={after:?}"
    );
}

#[tokio::test]
async fn parsed_queue_resize_pane_mouse_flag_shrinks_vertical_split_from_border_context() {
    let handler = RequestHandler::new();
    let session = session_name("resize-mouse-border-shrink-v");
    let target = PaneTarget::with_window(session.clone(), 0, 0);
    create_test_session(
        &handler,
        session.clone(),
        TerminalSize { cols: 80, rows: 24 },
    )
    .await;
    execute(
        &handler,
        "split-window -v -t resize-mouse-border-shrink-v:0.0",
    )
    .await;

    let before = pane_sizes(&handler, &session).await;
    let border_y = before.first().expect("first pane").1;
    let mouse_event = border_mouse_event(
        target.clone(),
        PaneId::new(0),
        0,
        border_y.saturating_sub(3),
        0,
        border_y,
    );
    let parsed = CommandParser::new()
        .parse("resize-pane -M")
        .expect("command parses");

    handler
        .execute_parsed_commands(
            std::process::id(),
            parsed,
            QueueExecutionContext::without_caller_cwd()
                .with_current_target(Some(Target::Pane(target.clone())))
                .with_mouse_target(Some(Target::Pane(target.clone())))
                .with_mouse_event(Some(mouse_event)),
        )
        .await
        .expect("mouse resize executes");

    let after = pane_sizes(&handler, &session).await;
    assert!(
        after[0].1 < before[0].1,
        "first pane should shrink after dragging the horizontal border up: before={before:?} after={after:?}"
    );
}

#[tokio::test]
async fn parsed_queue_mouse_resize_survives_prior_command_in_pipeline() {
    let handler = RequestHandler::new();
    let session = session_name("resize-mouse-pipeline");
    let target = PaneTarget::with_window(session.clone(), 0, 0);
    create_test_session(
        &handler,
        session.clone(),
        TerminalSize { cols: 80, rows: 24 },
    )
    .await;
    execute(&handler, "split-window -h -t resize-mouse-pipeline:0.0").await;

    let before = pane_sizes(&handler, &session).await;
    let border_x = before.first().expect("first pane").0.saturating_add(5);
    let mouse_event = AttachedMouseEvent {
        raw: MouseForwardEvent {
            b: 32,
            lb: 32,
            x: border_x,
            y: 0,
            lx: border_x.saturating_sub(1),
            ly: 0,
            sgr_b: 32,
            sgr_type: 'M',
            ignore: false,
        },
        session_id: 1,
        window_id: Some(1),
        pane_id: Some(PaneId::new(0)),
        pane_target: Some(target.clone()),
        location: MouseLocation::Border,
        status_at: None,
        status_lines: 0,
        ignore: false,
    };
    let parsed = CommandParser::new()
        .parse("display-message dragged ; resize-pane -M")
        .expect("command pipeline parses");

    handler
        .execute_parsed_commands(
            std::process::id(),
            parsed,
            QueueExecutionContext::without_caller_cwd()
                .with_current_target(Some(Target::Pane(target.clone())))
                .with_mouse_target(Some(Target::Pane(target)))
                .with_mouse_event(Some(mouse_event)),
        )
        .await
        .expect("mouse resize pipeline executes");

    let after = pane_sizes(&handler, &session).await;
    assert!(
        after[0].0 > before[0].0,
        "mouse_event must survive earlier pipeline commands: before={before:?} after={after:?}"
    );
}

#[tokio::test]
async fn parsed_queue_mouse_resize_can_recover_attached_current_mouse_event() {
    let handler = RequestHandler::new();
    let session = session_name("resize-mouse-fallback");
    let target = PaneTarget::with_window(session.clone(), 0, 0);
    let requester_pid = std::process::id();
    create_test_session(
        &handler,
        session.clone(),
        TerminalSize { cols: 80, rows: 24 },
    )
    .await;
    execute(&handler, "split-window -h -t resize-mouse-fallback:0.0").await;
    let (control_tx, _control_rx) = tokio::sync::mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, session.clone(), control_tx)
        .await;

    let before = pane_sizes(&handler, &session).await;
    let border_x = before.first().expect("first pane").0.saturating_add(5);
    let mouse_event = AttachedMouseEvent {
        raw: MouseForwardEvent {
            b: 32,
            lb: 32,
            x: border_x,
            y: 0,
            lx: border_x.saturating_sub(1),
            ly: 0,
            sgr_b: 32,
            sgr_type: 'M',
            ignore: false,
        },
        session_id: 1,
        window_id: Some(1),
        pane_id: Some(PaneId::new(0)),
        pane_target: Some(target.clone()),
        location: MouseLocation::Border,
        status_at: None,
        status_lines: 0,
        ignore: false,
    };
    {
        let mut active_attach = handler.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get_mut(&requester_pid)
            .expect("attach is registered");
        active.mouse.current_event = Some(mouse_event);
    }
    let parsed = CommandParser::new()
        .parse("display-message dragged ; resize-pane -M")
        .expect("command pipeline parses");

    handler
        .execute_parsed_commands(
            requester_pid,
            parsed,
            QueueExecutionContext::without_caller_cwd()
                .with_current_target(Some(Target::Pane(target.clone())))
                .with_mouse_target(Some(Target::Pane(target))),
        )
        .await
        .expect("mouse resize fallback executes");

    let after = pane_sizes(&handler, &session).await;
    assert!(
        after[0].0 > before[0].0,
        "resize-pane -M should recover the active attach mouse event when queue context was truncated: before={before:?} after={after:?}"
    );
}

async fn create_test_session(handler: &RequestHandler, session: SessionName, size: TerminalSize) {
    let response = handler
        .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
            session_name: Some(session),
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
            command: Some(quiet_resize_test_command()),
            process_command: None,
            client_environment: None,
            skip_environment_update: false,
        })))
        .await;
    assert!(
        matches!(response, Response::NewSession(_)),
        "resize test session should be created, got {response:?}"
    );
}

#[cfg(unix)]
fn quiet_resize_test_command() -> Vec<String> {
    vec!["/bin/sh".to_owned(), "-c".to_owned(), "sleep 60".to_owned()]
}

#[cfg(windows)]
fn quiet_resize_test_command() -> Vec<String> {
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

async fn execute(handler: &RequestHandler, command: &str) {
    let parsed = CommandParser::new().parse(command).expect("command parses");
    handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .unwrap_or_else(|error| panic!("{command} should execute: {error}"));
}

async fn pane_height(handler: &RequestHandler, session: &SessionName, pane_index: u32) -> u16 {
    let state = handler.state.lock().await;
    state
        .sessions
        .session(session)
        .expect("session exists")
        .window_at(0)
        .expect("window exists")
        .pane(pane_index)
        .expect("pane exists")
        .geometry()
        .rows()
}

async fn pane_sizes(handler: &RequestHandler, session: &SessionName) -> Vec<(u16, u16)> {
    let state = handler.state.lock().await;
    state
        .sessions
        .session(session)
        .expect("session exists")
        .window_at(0)
        .expect("window exists")
        .panes()
        .iter()
        .map(|pane| {
            let geometry = pane.geometry();
            (geometry.cols(), geometry.rows())
        })
        .collect()
}

fn border_mouse_event(
    target: PaneTarget,
    pane_id: PaneId,
    x: u16,
    y: u16,
    start_x: u16,
    start_y: u16,
) -> AttachedMouseEvent {
    AttachedMouseEvent {
        raw: MouseForwardEvent {
            b: 32,
            lb: 0,
            x,
            y,
            lx: start_x,
            ly: start_y,
            sgr_b: 32,
            sgr_type: 'M',
            ignore: false,
        },
        session_id: 1,
        window_id: Some(1),
        pane_id: Some(pane_id),
        pane_target: Some(target),
        location: MouseLocation::Border,
        status_at: None,
        status_lines: 0,
        ignore: false,
    }
}
