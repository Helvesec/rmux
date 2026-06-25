use super::super::{overlay_support::ClientOverlayState, RequestHandler};
use super::session_name;
use crate::mouse::{layout_for_session, StatusRangeType};
use crate::pane_io::AttachControl;
use rmux_proto::{
    BindKeyRequest, CapturePaneRequest, NewSessionExtRequest, NewSessionRequest, PaneTarget,
    Request, Response, SessionName, Target, TerminalSize, WindowTarget, DEFAULT_MAX_FRAME_LENGTH,
};
use tokio::sync::mpsc;
use tokio::time::{timeout, Duration};

async fn create_attached_session(
    handler: &RequestHandler,
    name: &SessionName,
    requester_pid: u32,
) -> mpsc::UnboundedReceiver<AttachControl> {
    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: name.clone(),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));

    let (control_tx, control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, name.clone(), control_tx)
        .await;
    control_rx
}

async fn create_quiet_attached_session(
    handler: &RequestHandler,
    name: &SessionName,
    requester_pid: u32,
) -> mpsc::UnboundedReceiver<AttachControl> {
    let response = handler
        .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
            session_name: Some(name.clone()),
            working_directory: None,
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
            group_target: None,
            attach_if_exists: false,
            detach_other_clients: false,
            kill_other_clients: false,
            flags: None,
            window_name: None,
            print_session_info: false,
            print_format: None,
            command: Some(quiet_overlay_command()),
            process_command: None,
            client_environment: None,
            skip_environment_update: false,
        })))
        .await;
    assert!(
        matches!(response, Response::NewSession(_)),
        "quiet overlay test session should be created, got {response:?}"
    );

    let (control_tx, control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, name.clone(), control_tx)
        .await;
    control_rx
}

#[cfg(windows)]
fn quiet_overlay_command() -> Vec<String> {
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

#[cfg(unix)]
fn quiet_overlay_command() -> Vec<String> {
    ["/bin/sh", "-c", "sleep 60"]
        .into_iter()
        .map(str::to_owned)
        .collect()
}

async fn run_overlay_command(handler: &RequestHandler, requester_pid: u32, command: &str) {
    let parsed = handler
        .parse_control_commands(command)
        .await
        .expect("overlay command parses");
    let result = handler
        .execute_parsed_commands_for_test(requester_pid, parsed)
        .await
        .expect("overlay command executes");
    assert!(result.stdout().is_empty());
}

#[tokio::test]
async fn display_menu_accepts_parsed_command_list_items_from_queue() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();
    let mut control_rx = create_attached_session(&handler, &alpha, requester_pid).await;

    run_overlay_command(
        &handler,
        requester_pid,
        r#"display-menu -xM -yM -T Menu "First" "f" { display-message first }"#,
    )
    .await;

    let frame = next_overlay_frame(&mut control_rx).await;
    let rendered = String::from_utf8(frame.frame).expect("menu frame is utf-8");
    assert!(rendered.contains("First"));
}

async fn next_overlay_frame(
    control_rx: &mut mpsc::UnboundedReceiver<AttachControl>,
) -> crate::pane_io::OverlayFrame {
    match timeout(Duration::from_secs(1), control_rx.recv())
        .await
        .expect("overlay control message arrives")
    {
        Some(AttachControl::Overlay(frame)) => frame,
        other => panic!("expected overlay frame, got {other:?}"),
    }
}

async fn capture_pane_print(handler: &RequestHandler, target: PaneTarget) -> String {
    let response = handler
        .handle(Request::CapturePane(Box::new(CapturePaneRequest {
            target,
            start: None,
            end: None,
            print: true,
            buffer_name: None,
            alternate: false,
            escape_ansi: false,
            escape_sequences: false,
            join_wrapped: false,
            use_mode_screen: false,
            preserve_trailing_spaces: false,
            do_not_trim_spaces: false,
            pending_input: false,
            quiet: false,
            start_is_absolute: false,
            end_is_absolute: false,
        })))
        .await;
    let Response::CapturePane(response) = response else {
        panic!("expected capture-pane response, got {response:?}");
    };
    let output = response
        .output
        .expect("capture-pane -p should return command output");
    String::from_utf8(output.stdout().to_vec()).expect("capture-pane stdout is utf-8")
}

fn sgr_mouse(button: u16, x: u16, y: u16) -> Vec<u8> {
    format!(
        "\x1b[<{button};{};{}M",
        x.saturating_add(1),
        y.saturating_add(1)
    )
    .into_bytes()
}

#[tokio::test]
async fn display_menu_keyboard_navigation_wraps_around_separators() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();
    let mut control_rx = create_attached_session(&handler, &alpha, requester_pid).await;

    run_overlay_command(
        &handler,
        requester_pid,
        r#"display-menu -T Menu "First" "f" "display-message first" "" "" "" "Second" "s" "display-message second""#,
    )
    .await;

    let frame = next_overlay_frame(&mut control_rx).await;
    assert!(frame.persistent);
    let rendered = String::from_utf8(frame.frame).expect("menu frame is utf-8");
    assert!(rendered.contains("First"));
    assert!(rendered.contains("Second"));

    {
        let active_attach = handler.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get(&requester_pid)
            .expect("attached client");
        let Some(ClientOverlayState::Menu(menu)) = active.overlay.as_ref() else {
            panic!("expected a root menu overlay");
        };
        assert_eq!(menu.choice, Some(0));
    }

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x0e")
        .await
        .expect("menu navigation");
    {
        let active_attach = handler.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get(&requester_pid)
            .expect("attached client");
        let Some(ClientOverlayState::Menu(menu)) = active.overlay.as_ref() else {
            panic!("expected a root menu overlay");
        };
        assert_eq!(menu.choice, Some(2));
    }

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x0e")
        .await
        .expect("menu wrap");
    {
        let active_attach = handler.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get(&requester_pid)
            .expect("attached client");
        let Some(ClientOverlayState::Menu(menu)) = active.overlay.as_ref() else {
            panic!("expected a root menu overlay");
        };
        assert_eq!(menu.choice, Some(0));
    }

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\r")
        .await
        .expect("menu choose");
    let active_attach = handler.active_attach.lock().await;
    let active = active_attach
        .by_pid
        .get(&requester_pid)
        .expect("attached client");
    assert!(active.overlay.is_none());
}

#[tokio::test]
async fn display_menu_unterminated_sgr_mouse_input_is_bounded() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();
    let mut control_rx = create_attached_session(&handler, &alpha, requester_pid).await;

    run_overlay_command(
        &handler,
        requester_pid,
        r#"display-menu -T Menu "First" "f" "display-message first""#,
    )
    .await;
    let _ = next_overlay_frame(&mut control_rx).await;

    let mut pending_input = Vec::new();
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"\x1b[<")
        .await
        .expect("partial menu mouse is retained");
    assert_eq!(pending_input, b"\x1b[<");

    let overflow = vec![b'9'; DEFAULT_MAX_FRAME_LENGTH - pending_input.len() + 1];
    let err = handler
        .handle_attached_live_input(requester_pid, &mut pending_input, &overflow)
        .await
        .expect_err("unterminated menu mouse should be bounded");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    assert!(err.to_string().contains("menu overlay mouse"));
    assert!(pending_input.is_empty());
}

#[tokio::test]
async fn display_menu_partial_utf8_input_is_retained_and_recovered() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();
    let mut control_rx = create_quiet_attached_session(&handler, &alpha, requester_pid).await;
    let target = PaneTarget::new(alpha.clone(), 0);
    let before_capture = capture_pane_print(&handler, target.clone()).await;

    run_overlay_command(
        &handler,
        requester_pid,
        r#"display-menu -T Menu "First" "f" "display-message first""#,
    )
    .await;
    let _ = next_overlay_frame(&mut control_rx).await;

    let mut pending_input = Vec::new();
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, &[0xe6])
        .await
        .expect("partial menu UTF-8 is retained");
    assert_eq!(
        pending_input,
        vec![0xe6],
        "menu overlay should retain only the partial UTF-8 fragment"
    );
    assert_eq!(
        capture_pane_print(&handler, target.clone()).await,
        before_capture,
        "partial menu prompt UTF-8 must not leak to the pane"
    );

    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"\x97\xa5")
        .await
        .expect("completed menu UTF-8 is handled");
    assert!(
        pending_input.is_empty(),
        "completed menu UTF-8 input should leave no retained bytes"
    );
    let active_attach = handler.active_attach.lock().await;
    let active = active_attach
        .by_pid
        .get(&requester_pid)
        .expect("attached client");
    assert!(
        matches!(active.overlay.as_ref(), Some(ClientOverlayState::Menu(_))),
        "completed non-matching menu input should not leave retained bytes or collapse the menu"
    );
}

#[tokio::test]
async fn display_menu_extended_key_partial_is_bounded_without_pane_leak() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();
    let mut control_rx = create_quiet_attached_session(&handler, &alpha, requester_pid).await;
    let target = PaneTarget::new(alpha.clone(), 0);
    let before_capture = capture_pane_print(&handler, target.clone()).await;

    run_overlay_command(
        &handler,
        requester_pid,
        r#"display-menu -T Menu "First" "f" "display-message first""#,
    )
    .await;
    let _ = next_overlay_frame(&mut control_rx).await;

    let mut pending_input = Vec::new();
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"\x1b[27;2;65")
        .await
        .expect("partial menu extended key is retained");
    assert_eq!(pending_input, b"\x1b[27;2;65");
    assert_eq!(
        capture_pane_print(&handler, target.clone()).await,
        before_capture,
        "partial menu prompt extended key must not leak to the pane"
    );

    let oversized = vec![b'9'; DEFAULT_MAX_FRAME_LENGTH - pending_input.len() + 1];
    let err = handler
        .handle_attached_live_input(requester_pid, &mut pending_input, &oversized)
        .await
        .expect_err("oversized partial menu extended key should be bounded");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    assert!(err.to_string().contains("menu overlay prompt input"));
    assert!(
        pending_input.is_empty(),
        "overflowing menu prompt input should be cleared after rejection"
    );
    assert_eq!(
        capture_pane_print(&handler, target.clone()).await,
        before_capture,
        "rejected oversized menu prompt input must not leak to the pane"
    );

    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"9")
        .await
        .expect("menu remains usable after partial-input rejection");
    assert!(pending_input.is_empty());
    assert_eq!(
        capture_pane_print(&handler, target).await,
        before_capture,
        "post-rejection menu input must not leak to the pane"
    );
}

#[tokio::test]
async fn popup_right_click_opens_nested_menu_and_escape_closes_layers() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();
    let mut control_rx = create_attached_session(&handler, &alpha, requester_pid).await;

    run_overlay_command(
        &handler,
        requester_pid,
        r#"display-popup -N -T Popup -w 20 -h 6 -x C -y C"#,
    )
    .await;

    let frame = next_overlay_frame(&mut control_rx).await;
    assert!(frame.persistent);
    let rendered = String::from_utf8(frame.frame).expect("popup frame is utf-8");
    assert!(rendered.contains("Popup"));

    let rect = {
        let active_attach = handler.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get(&requester_pid)
            .expect("attached client");
        let Some(ClientOverlayState::Popup(popup)) = active.overlay.as_ref() else {
            panic!("expected popup overlay");
        };
        popup.rect
    };

    handler
        .handle_attached_live_input_for_test(requester_pid, &sgr_mouse(2, rect.x, rect.y))
        .await
        .expect("popup menu mouse");
    {
        let active_attach = handler.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get(&requester_pid)
            .expect("attached client");
        let Some(ClientOverlayState::Popup(popup)) = active.overlay.as_ref() else {
            panic!("expected popup overlay");
        };
        assert!(popup.nested_menu.is_some());
    }

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x1b")
        .await
        .expect("close nested menu");
    {
        let active_attach = handler.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get(&requester_pid)
            .expect("attached client");
        let Some(ClientOverlayState::Popup(popup)) = active.overlay.as_ref() else {
            panic!("expected popup overlay");
        };
        assert!(popup.nested_menu.is_none());
    }

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x1b")
        .await
        .expect("close popup");
    let active_attach = handler.active_attach.lock().await;
    let active = active_attach
        .by_pid
        .get(&requester_pid)
        .expect("attached client");
    assert!(active.overlay.is_none());
}

#[tokio::test]
async fn status_right_click_routes_window_menu_to_clicked_window_target() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();
    let mut control_rx = create_attached_session(&handler, &alpha, requester_pid).await;
    let rebound = handler
        .handle(Request::BindKey(Box::new(BindKeyRequest {
            table_name: "root".to_owned(),
            key: "MouseDown3Status".to_owned(),
            note: Some("overlay-status-menu".to_owned()),
            repeat: false,
            command: Some(vec![
                "display-menu".to_owned(),
                "-x".to_owned(),
                "W".to_owned(),
                "-y".to_owned(),
                "W".to_owned(),
                "-T".to_owned(),
                "#{window_index}:#{window_name}".to_owned(),
                "Inspect".to_owned(),
                "i".to_owned(),
                "display-message inspect".to_owned(),
            ]),
        })))
        .await;
    assert!(matches!(rebound, Response::BindKey(_)));

    let (click_x, click_y) = {
        let state = handler.state.lock().await;
        let layout = layout_for_session(&state, &alpha, 1).expect("mouse layout");
        let status = layout.status.as_ref().expect("status layout");
        let range = status
            .ranges
            .iter()
            .find(|range| matches!(range.kind, StatusRangeType::Window(_)))
            .expect("window status range");
        (
            *range.x.start(),
            layout.status_at.expect("status line position"),
        )
    };

    handler
        .handle_attached_live_input_for_test(requester_pid, &sgr_mouse(2, click_x, click_y))
        .await
        .expect("status mouse input");

    let frame = next_overlay_frame(&mut control_rx).await;
    assert!(frame.persistent);
    let rendered = String::from_utf8(frame.frame).expect("window menu frame is utf-8");
    assert!(rendered.contains("Inspect"));

    let active_attach = handler.active_attach.lock().await;
    let active = active_attach
        .by_pid
        .get(&requester_pid)
        .expect("attached client");
    let Some(ClientOverlayState::Menu(menu)) = active.overlay.as_ref() else {
        panic!("expected a status menu overlay");
    };
    assert_eq!(
        menu.current_target,
        Target::Window(WindowTarget::with_window(alpha, 0))
    );
}
