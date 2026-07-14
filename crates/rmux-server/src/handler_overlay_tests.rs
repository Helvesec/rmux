use super::super::{
    overlay_support::{AttachedOverlayInput, ClientOverlayState},
    RequestHandler,
};
use super::session_name;
use crate::input_keys::MAX_SGR_MOUSE_FRAME_BYTES;
use crate::mouse::{layout_for_session, StatusRangeType};
use crate::pane_io::AttachControl;
use rmux_proto::request::RefreshClientRequest;
use rmux_proto::{
    BindKeyRequest, CapturePaneRequest, NewSessionExtRequest, NewSessionRequest, PaneTarget,
    Request, Response, ScopeSelector, SessionName, SetOptionMode, Target, TerminalSize,
    WindowTarget, DEFAULT_MAX_FRAME_LENGTH,
};
use rmux_proto::{OptionName, SetOptionRequest};
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

async fn enable_mouse(handler: &RequestHandler) {
    let response = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::Mouse,
            value: "on".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(response, Response::SetOption(_)));
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

#[tokio::test]
async fn display_menu_renders_mouse_word_and_line_from_clicked_pane() {
    let handler = RequestHandler::new();
    let alpha = session_name("menu-mouse-word");
    let requester_pid = std::process::id();
    let mut control_rx = create_quiet_attached_session(&handler, &alpha, requester_pid).await;
    enable_mouse(&handler).await;

    {
        let mut state = handler.state.lock().await;
        state
            .append_bytes_to_pane_transcript_for_test(&alpha, 0, 0, b"alpha beta gamma")
            .expect("transcript append succeeds");
    }

    handler
        .handle_attached_live_input_for_test(requester_pid, &sgr_mouse(0, 6, 0))
        .await
        .expect("mouse input records current mouse event");
    run_overlay_command(
        &handler,
        requester_pid,
        r##"display-menu -xM -yM -T Menu "#{mouse_word}:#{mouse_line}" w "display-message word""##,
    )
    .await;

    let frame = next_overlay_frame(&mut control_rx).await;
    let rendered = String::from_utf8(frame.frame).expect("menu frame is utf-8");
    assert!(
        rendered.contains("beta:alpha beta gamma"),
        "menu should render mouse formats from clicked pane, got {rendered:?}"
    );
}

#[tokio::test]
async fn display_menu_single_character_shortcut_executes_item() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha-shortcut");
    let requester_pid = std::process::id();
    let mut control_rx = create_attached_session(&handler, &alpha, requester_pid).await;

    run_overlay_command(
        &handler,
        requester_pid,
        r#"display-menu -xM -yM -T Menu "First" "f" { display-message first }"#,
    )
    .await;
    let _ = next_overlay_frame(&mut control_rx).await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"f")
        .await
        .expect("menu shortcut");

    let active_attach = handler.active_attach.lock().await;
    let active = active_attach
        .by_pid
        .get(&requester_pid)
        .expect("attached client");
    assert!(active.overlay.is_none());
}

#[tokio::test]
async fn display_menu_hyphen_prefixed_label_with_command_is_actionable() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha-hyphen-menu");
    let requester_pid = std::process::id();
    let mut control_rx = create_attached_session(&handler, &alpha, requester_pid).await;

    run_overlay_command(
        &handler,
        requester_pid,
        r#"display-menu -xM -yM -T Menu -- "-Danger" "d" { display-message danger }"#,
    )
    .await;
    let frame = next_overlay_frame(&mut control_rx).await;
    let rendered = String::from_utf8(frame.frame).expect("menu frame is utf-8");
    assert!(
        rendered.contains("-Danger"),
        "hyphen-prefixed label should render as an actionable item, got {rendered:?}"
    );

    handler
        .handle_attached_live_input_for_test(requester_pid, b"d")
        .await
        .expect("menu shortcut");

    let active_attach = handler.active_attach.lock().await;
    let active = active_attach
        .by_pid
        .get(&requester_pid)
        .expect("attached client");
    assert!(active.overlay.is_none());
}

async fn next_overlay_frame(
    control_rx: &mut mpsc::UnboundedReceiver<AttachControl>,
) -> crate::pane_io::OverlayFrame {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    loop {
        let now = tokio::time::Instant::now();
        assert!(now < deadline, "overlay control message arrives");
        match timeout(deadline - now, control_rx.recv())
            .await
            .expect("overlay control message arrives")
        {
            Some(AttachControl::Overlay(frame)) => return frame,
            Some(AttachControl::Refresh | AttachControl::Switch(_)) => continue,
            other => panic!("expected overlay frame, got {other:?}"),
        }
    }
}

fn full_refresh_client_request(target_pid: u32) -> RefreshClientRequest {
    RefreshClientRequest {
        target_client: Some(target_pid.to_string()),
        adjustment: None,
        clear_pan: false,
        pan_left: false,
        pan_right: false,
        pan_up: false,
        pan_down: false,
        status_only: false,
        clipboard_query: false,
        flags: None,
        flags_alias: None,
        subscriptions: Vec::new(),
        subscriptions_format: Vec::new(),
        control_size: None,
        colour_report: None,
    }
}

async fn refresh_client_overlay_frame(
    handler: &RequestHandler,
    requester_pid: u32,
    control_rx: &mut mpsc::UnboundedReceiver<AttachControl>,
) -> crate::pane_io::OverlayFrame {
    let response = handler
        .dispatch(
            requester_pid,
            Request::RefreshClient(Box::new(full_refresh_client_request(requester_pid))),
        )
        .await
        .response;
    assert!(
        matches!(response, Response::RefreshClient(_)),
        "refresh-client should succeed, got {response:?}"
    );

    refresh_overlay_frame_after_base_switch(control_rx).await
}

async fn refresh_overlay_frame_after_base_switch(
    control_rx: &mut mpsc::UnboundedReceiver<AttachControl>,
) -> crate::pane_io::OverlayFrame {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    let mut saw_switch = false;
    loop {
        let now = tokio::time::Instant::now();
        assert!(now < deadline, "refresh-client overlay replay arrives");
        match timeout(deadline - now, control_rx.recv())
            .await
            .expect("refresh-client control arrives")
        {
            Some(AttachControl::Switch(_)) => saw_switch = true,
            Some(AttachControl::Overlay(frame)) => {
                assert!(saw_switch, "base refresh must precede the overlay replay");
                return frame;
            }
            Some(AttachControl::Refresh) => {}
            other => panic!("expected refresh-client switch and overlay, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn refresh_client_replays_active_menu_after_base_switch() {
    let handler = RequestHandler::new();
    let alpha = session_name("refresh-client-menu-overlay");
    let requester_pid = std::process::id();
    let mut control_rx = create_attached_session(&handler, &alpha, requester_pid).await;

    run_overlay_command(
        &handler,
        requester_pid,
        r#"display-menu -T Menu "First" "f" "display-message first""#,
    )
    .await;
    let _ = next_overlay_frame(&mut control_rx).await;

    let frame = refresh_client_overlay_frame(&handler, requester_pid, &mut control_rx).await;
    assert!(frame.persistent);
    let rendered = String::from_utf8(frame.frame).expect("menu frame is utf-8");
    assert!(rendered.contains("First"));

    handler
        .handle_attached_live_input_for_test(requester_pid, b"f")
        .await
        .expect("replayed menu remains interactive");
    let active_attach = handler.active_attach.lock().await;
    assert!(
        active_attach.by_pid[&requester_pid].overlay.is_none(),
        "menu action should dismiss the active overlay after refresh-client"
    );
}

#[tokio::test]
async fn refresh_client_replays_active_popup_after_base_switch() {
    let handler = RequestHandler::new();
    let alpha = session_name("refresh-client-popup-overlay");
    let requester_pid = std::process::id();
    let mut control_rx = create_attached_session(&handler, &alpha, requester_pid).await;

    run_overlay_command(
        &handler,
        requester_pid,
        r#"display-popup -N -T Popup -w 20 -h 6 -x C -y C"#,
    )
    .await;
    let _ = next_overlay_frame(&mut control_rx).await;

    let frame = refresh_client_overlay_frame(&handler, requester_pid, &mut control_rx).await;
    assert!(frame.persistent);
    let rendered = String::from_utf8(frame.frame).expect("popup frame is utf-8");
    assert!(rendered.contains("Popup"));

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x03")
        .await
        .expect("replayed popup remains interactive");
    let active_attach = handler.active_attach.lock().await;
    assert!(
        active_attach.by_pid[&requester_pid].overlay.is_none(),
        "popup control input should dismiss the active overlay after refresh-client"
    );
}

#[tokio::test]
async fn cancelling_prompt_replays_the_menu_it_temporarily_hid() {
    let handler = RequestHandler::new();
    let alpha = session_name("prompt-cancel-menu-overlay");
    let requester_pid = std::process::id();
    let mut control_rx = create_quiet_attached_session(&handler, &alpha, requester_pid).await;

    run_overlay_command(
        &handler,
        requester_pid,
        r#"display-menu -T Menu "First" "f" "display-message first""#,
    )
    .await;
    let _ = next_overlay_frame(&mut control_rx).await;
    run_overlay_command(&handler, requester_pid, "command-prompt -b -p Prompt").await;
    assert!(handler
        .attached_prompt_render(requester_pid)
        .await
        .is_some());
    tokio::time::sleep(Duration::from_millis(100)).await;
    while control_rx.try_recv().is_ok() {}

    handler.clear_prompt_for_attach(requester_pid).await;
    let expected_render_generation =
        handler.active_attach.lock().await.by_pid[&requester_pid].render_generation;

    let frame = refresh_overlay_frame_after_base_switch(&mut control_rx).await;
    assert_eq!(frame.render_generation, expected_render_generation);
    assert!(frame.persistent);
    let rendered = String::from_utf8(frame.frame).expect("menu frame is utf-8");
    assert!(
        rendered.contains("First"),
        "prompt cancellation must restore the still-active menu, got {rendered:?}"
    );
    handler
        .handle_attached_live_input_for_test(requester_pid, b"f")
        .await
        .expect("restored menu remains interactive");
    assert!(handler.active_attach.lock().await.by_pid[&requester_pid]
        .overlay
        .is_none());
}

#[tokio::test]
async fn session_identity_refresh_keeps_the_underlying_menu_hidden_by_a_prompt() {
    let handler = RequestHandler::new();
    let alpha = session_name("identity-refresh-prompt-menu");
    let requester_pid = std::process::id();
    let mut control_rx = create_quiet_attached_session(&handler, &alpha, requester_pid).await;

    run_overlay_command(
        &handler,
        requester_pid,
        r#"display-menu -T Menu "First" "f" "display-message first""#,
    )
    .await;
    let _ = next_overlay_frame(&mut control_rx).await;
    run_overlay_command(&handler, requester_pid, "command-prompt -b -p Prompt").await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    while control_rx.try_recv().is_ok() {}
    let session_id = handler.active_attach.lock().await.by_pid[&requester_pid].session_id;

    handler
        .refresh_attached_session_for_session_identity(&alpha, session_id)
        .await;

    let mut saw_prompt = false;
    while let Ok(control) = control_rx.try_recv() {
        match control {
            AttachControl::Switch(target) => {
                let rendered = String::from_utf8_lossy(&target.render_frame);
                saw_prompt |= rendered.contains("Prompt");
            }
            AttachControl::Overlay(frame) => {
                assert!(
                    !String::from_utf8_lossy(&frame.frame).contains("First"),
                    "an active prompt must keep the underlying menu hidden"
                );
            }
            _ => {}
        }
    }
    assert!(
        saw_prompt,
        "identity refresh should repaint the active prompt"
    );
    handler.clear_prompt_for_attach(requester_pid).await;
}

#[tokio::test]
async fn stale_same_pid_popup_callbacks_cannot_mutate_replacement_popup() {
    let handler = RequestHandler::new();
    let alpha = session_name("popup-callback-identity");
    let requester_pid = 920_041;
    let mut old_control_rx = create_attached_session(&handler, &alpha, requester_pid).await;

    run_overlay_command(
        &handler,
        requester_pid,
        r#"display-popup -N -E -T Old -w 20 -h 6 -x C -y C"#,
    )
    .await;
    let _ = next_overlay_frame(&mut old_control_rx).await;
    let (old_identity, old_popup_id) = {
        let active_attach = handler.active_attach.lock().await;
        let active = &active_attach.by_pid[&requester_pid];
        (
            active.identity(requester_pid),
            active.overlay.as_ref().expect("old popup active").id(),
        )
    };

    let (replacement_tx, mut replacement_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, alpha.clone(), replacement_tx)
        .await;
    run_overlay_command(
        &handler,
        requester_pid,
        r#"display-popup -N -E -T Replacement -w 20 -h 6 -x C -y C"#,
    )
    .await;
    let replacement_frame = next_overlay_frame(&mut replacement_rx).await;
    assert!(
        String::from_utf8_lossy(&replacement_frame.frame).contains("Replacement"),
        "replacement popup must be active before old callbacks run"
    );
    let replacement_popup_id = {
        let active_attach = handler.active_attach.lock().await;
        active_attach.by_pid[&requester_pid]
            .overlay
            .as_ref()
            .expect("replacement popup active")
            .id()
    };
    assert_eq!(
        old_popup_id, replacement_popup_id,
        "the regression requires the per-registration popup id collision"
    );

    handler
        .popup_reader_tick(old_identity, old_popup_id)
        .await
        .expect("stale reader callback is ignored");
    handler
        .popup_job_finished(old_identity, old_popup_id, 0)
        .await
        .expect("stale waiter callback is ignored");

    let active_attach = handler.active_attach.lock().await;
    let active = &active_attach.by_pid[&requester_pid];
    assert!(
        matches!(active.overlay, Some(ClientOverlayState::Popup(_))),
        "old reader/waiter callbacks must not refresh or close B's colliding popup"
    );
    assert_eq!(active.overlay.as_ref().map(ClientOverlayState::id), Some(1));
}

#[tokio::test]
async fn rename_session_rekeys_menu_and_popup_targets_before_they_handle_input() {
    let handler = RequestHandler::new();
    let alpha = session_name("overlay-rename-alpha");
    let beta = session_name("overlay-rename-beta");
    let gamma = session_name("overlay-rename-gamma");
    let requester_pid = std::process::id();
    let mut control_rx = create_attached_session(&handler, &alpha, requester_pid).await;

    run_overlay_command(
        &handler,
        requester_pid,
        r#"display-menu -T Menu "First" "f" "display-message first""#,
    )
    .await;
    let _ = next_overlay_frame(&mut control_rx).await;
    let response = handler
        .handle(Request::RenameSession(rmux_proto::RenameSessionRequest {
            target: alpha,
            new_name: beta.clone(),
        }))
        .await;
    assert!(
        matches!(response, Response::RenameSession(_)),
        "{response:?}"
    );
    {
        let active_attach = handler.active_attach.lock().await;
        let Some(ClientOverlayState::Menu(menu)) =
            active_attach.by_pid[&requester_pid].overlay.as_ref()
        else {
            panic!("menu remains active across rename");
        };
        assert_eq!(menu.current_target.session_name(), &beta);
    }
    handler
        .handle_attached_live_input_for_test(requester_pid, b"f")
        .await
        .expect("renamed menu target remains actionable");

    run_overlay_command(
        &handler,
        requester_pid,
        r#"display-popup -N -T Popup -w 20 -h 6 -x C -y C"#,
    )
    .await;
    let _ = next_overlay_frame(&mut control_rx).await;
    let response = handler
        .handle(Request::RenameSession(rmux_proto::RenameSessionRequest {
            target: beta,
            new_name: gamma.clone(),
        }))
        .await;
    assert!(
        matches!(response, Response::RenameSession(_)),
        "{response:?}"
    );
    {
        let active_attach = handler.active_attach.lock().await;
        let Some(ClientOverlayState::Popup(popup)) =
            active_attach.by_pid[&requester_pid].overlay.as_ref()
        else {
            panic!("popup remains active across rename");
        };
        assert_eq!(popup.current_target.session_name(), &gamma);
    }
    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x03")
        .await
        .expect("renamed popup target remains interactive");
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
            include_format: false,
            hyperlinks: false,
            line_numbers: false,
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
        r#"display-menu -T Menu "First" "f" "display-message first" "" "Second" "s" "display-message second""#,
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

    let mut malformed = b"\x1b[<".to_vec();
    malformed.resize(MAX_SGR_MOUSE_FRAME_BYTES, b'9');
    malformed.push(b'\r');
    let mut pending_input = Vec::new();
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, &malformed)
        .await
        .expect("bounded malformed menu mouse is discarded as one batch");
    assert!(pending_input.is_empty());

    {
        let active_attach = handler.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get(&requester_pid)
            .expect("attached client");
        assert!(
            matches!(active.overlay.as_ref(), Some(ClientOverlayState::Menu(_))),
            "the same-batch Enter tail must not choose a menu item"
        );
    }

    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"\x1b")
        .await
        .expect("a later Escape is retained for its ambiguity deadline");
    handler
        .flush_attached_pending_escape_input(requester_pid, &mut pending_input)
        .await
        .expect("the later Escape still closes the menu at its deadline");
    assert!(pending_input.is_empty());
    let active_attach = handler.active_attach.lock().await;
    let active = active_attach
        .by_pid
        .get(&requester_pid)
        .expect("attached client");
    assert!(active.overlay.is_none());
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

    let mut pending_input = Vec::new();
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"\x1b")
        .await
        .expect("retain nested menu Escape");
    handler
        .flush_attached_pending_escape_input(requester_pid, &mut pending_input)
        .await
        .expect("close nested menu after escape-time");
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
        .handle_attached_live_input(requester_pid, &mut pending_input, b"\x1b")
        .await
        .expect("retain popup Escape");
    handler
        .flush_attached_pending_escape_input(requester_pid, &mut pending_input)
        .await
        .expect("close popup after escape-time");
    let active_attach = handler.active_attach.lock().await;
    let active = active_attach
        .by_pid
        .get(&requester_pid)
        .expect("attached client");
    assert!(active.overlay.is_none());
}

#[tokio::test]
async fn nested_popup_menu_close_reroutes_same_chunk_tail_to_popup() {
    let handler = RequestHandler::new();
    let alpha = session_name("popup-menu-tail");
    let requester_pid = std::process::id();
    let _control_rx = create_attached_session(&handler, &alpha, requester_pid).await;

    run_overlay_command(
        &handler,
        requester_pid,
        r#"display-popup -N -T Popup -w 20 -h 6 -x C -y C"#,
    )
    .await;

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

    let mut pending_input = Vec::new();
    let outcome = handler
        .handle_attached_overlay_input(requester_pid, &mut pending_input, b"\x03TAIL")
        .await
        .expect("nested menu closes");
    assert_eq!(outcome, AttachedOverlayInput::Reroute(b"TAIL".to_vec()));
    assert!(pending_input.is_empty());

    let active_attach = handler.active_attach.lock().await;
    let active = active_attach
        .by_pid
        .get(&requester_pid)
        .expect("attached client");
    let Some(ClientOverlayState::Popup(popup)) = active.overlay.as_ref() else {
        panic!("popup should remain active");
    };
    assert!(popup.nested_menu.is_none());
}

#[tokio::test]
async fn status_right_click_routes_window_menu_to_clicked_window_target() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();
    let mut control_rx = create_attached_session(&handler, &alpha, requester_pid).await;
    enable_mouse(&handler).await;
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
