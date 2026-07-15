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
    BindKeyRequest, CapturePaneRequest, ListSessionsRequest, NewSessionExtRequest,
    NewSessionRequest, PaneTarget, Request, Response, ScopeSelector, SessionName, SetBufferRequest,
    SetOptionMode, Target, TerminalSize, WindowTarget, DEFAULT_MAX_FRAME_LENGTH,
};
use rmux_proto::{OptionName, SetOptionRequest};
use std::sync::{Arc, Condvar, Mutex as StdMutex};
use tokio::sync::mpsc;
use tokio::time::{timeout, Duration};

struct BlockingPopupIoRelease {
    state: Arc<(StdMutex<bool>, Condvar)>,
}

impl BlockingPopupIoRelease {
    fn new() -> Self {
        let state = Arc::new((StdMutex::new(false), Condvar::new()));
        let watchdog_state = Arc::clone(&state);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(3));
            let (released, released_cv) = &*watchdog_state;
            *released.lock().expect("popup I/O watchdog release") = true;
            released_cv.notify_all();
        });
        Self { state }
    }

    fn callback_state(&self) -> Arc<(StdMutex<bool>, Condvar)> {
        Arc::clone(&self.state)
    }

    fn release(&self) {
        let (released, released_cv) = &*self.state;
        *released.lock().expect("popup I/O release") = true;
        released_cv.notify_all();
    }
}

impl Drop for BlockingPopupIoRelease {
    fn drop(&mut self) {
        self.release();
    }
}

struct PopupJobCleanup {
    terminate: Option<Box<dyn FnOnce() + Send>>,
}

impl PopupJobCleanup {
    fn new(terminate: impl FnOnce() + Send + 'static) -> Self {
        Self {
            terminate: Some(Box::new(terminate)),
        }
    }
}

impl Drop for PopupJobCleanup {
    fn drop(&mut self) {
        if let Some(terminate) = self.terminate.take() {
            terminate();
        }
    }
}

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
async fn replacing_a_job_backed_popup_terminates_its_child() {
    let handler = RequestHandler::new();
    let alpha = session_name("popup-replacement-terminates-child");
    let requester_pid = std::process::id();
    let mut control_rx = create_attached_session(&handler, &alpha, requester_pid).await;

    run_overlay_command(
        &handler,
        requester_pid,
        r#"display-popup -T Old -w 20 -h 6 -x C -y C"#,
    )
    .await;
    let _ = next_overlay_frame(&mut control_rx).await;
    let old_job = {
        let active_attach = handler.active_attach.lock().await;
        let Some(ClientOverlayState::Popup(popup)) =
            active_attach.by_pid[&requester_pid].overlay.as_ref()
        else {
            panic!("expected old popup");
        };
        popup.job.clone().expect("old popup should own a job")
    };
    let cleanup_job = old_job.clone();
    let _cleanup = PopupJobCleanup::new(move || cleanup_job.terminate());
    assert!(
        old_job.child_is_running_for_test(),
        "the regression requires a live old popup process"
    );

    run_overlay_command(
        &handler,
        requester_pid,
        r#"display-popup -N -E -T Replacement -w 20 -h 6 -x C -y C"#,
    )
    .await;
    let replacement_frame = next_overlay_frame(&mut control_rx).await;
    assert!(
        String::from_utf8_lossy(&replacement_frame.frame).contains("Replacement"),
        "replacement popup should remain active"
    );

    timeout(Duration::from_secs(2), async {
        while old_job.child_is_running_for_test() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("replaced popup child should be terminated and reaped");

    handler
        .clear_interactive_overlay(requester_pid, true)
        .await
        .expect("replacement popup cleanup");
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

async fn install_popup_test_writer<F>(
    handler: &RequestHandler,
    requester_pid: u32,
    enable_mouse_forwarding: bool,
    writer: F,
) -> (crate::renderer::OverlayRect, PopupJobCleanup)
where
    F: Fn(Vec<u8>) -> std::io::Result<()> + Send + Sync + 'static,
{
    let mut active_attach = handler.active_attach.lock().await;
    let active = active_attach
        .by_pid
        .get_mut(&requester_pid)
        .expect("attached client");
    let Some(ClientOverlayState::Popup(popup)) = active.overlay.as_mut() else {
        panic!("expected popup overlay");
    };
    let test_job = popup
        .job
        .as_ref()
        .expect("expected job-backed popup")
        .with_test_writer(writer);
    let cleanup_job = test_job.clone();
    let cleanup = PopupJobCleanup::new(move || cleanup_job.terminate());
    popup.job = Some(test_job);
    if enable_mouse_forwarding {
        popup
            .surface
            .lock()
            .expect("popup surface")
            .append_for_test(b"\x1b[?1000h\x1b[?1006h");
    }
    (popup.rect, cleanup)
}

async fn install_popup_test_resize<F>(
    handler: &RequestHandler,
    requester_pid: u32,
    resize: F,
) -> (crate::renderer::OverlayRect, PopupJobCleanup)
where
    F: Fn(TerminalSize) -> std::io::Result<()> + Send + Sync + 'static,
{
    let mut active_attach = handler.active_attach.lock().await;
    let active = active_attach
        .by_pid
        .get_mut(&requester_pid)
        .expect("attached client");
    let Some(ClientOverlayState::Popup(popup)) = active.overlay.as_mut() else {
        panic!("expected popup overlay");
    };
    let test_job = popup
        .job
        .as_ref()
        .expect("expected job-backed popup")
        .with_test_resize(resize);
    let cleanup_job = test_job.clone();
    let cleanup = PopupJobCleanup::new(move || cleanup_job.terminate());
    popup.job = Some(test_job);
    (popup.rect, cleanup)
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
async fn blocked_popup_key_write_times_out_without_stalling_attach_or_server() {
    let handler = RequestHandler::new();
    let alpha = session_name("popup-blocking-writer");
    let requester_pid = std::process::id();
    let mut control_rx = create_attached_session(&handler, &alpha, requester_pid).await;

    run_overlay_command(
        &handler,
        requester_pid,
        r#"display-popup -T Popup -w 20 -h 6 -x C -y C"#,
    )
    .await;
    let _ = next_overlay_frame(&mut control_rx).await;

    let writes = Arc::new(StdMutex::new(Vec::<Vec<u8>>::new()));
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let started_tx = Arc::new(StdMutex::new(Some(started_tx)));
    let release = BlockingPopupIoRelease::new();
    let writer_writes = Arc::clone(&writes);
    let writer_started = Arc::clone(&started_tx);
    let writer_release = release.callback_state();
    let (_, _popup_cleanup) =
        install_popup_test_writer(&handler, requester_pid, false, move |bytes| {
            writer_writes.lock().expect("recorded writes").push(bytes);
            if let Some(started_tx) = writer_started.lock().expect("writer start").take() {
                let _ = started_tx.send(());
            }
            let (released, released_cv) = &*writer_release;
            let mut released = released.lock().expect("writer release");
            while !*released {
                released = released_cv.wait(released).expect("writer release wait");
            }
            Ok(())
        })
        .await;

    let input_handler = handler.clone();
    let input_task = tokio::spawn(async move {
        input_handler
            .handle_attached_live_input_for_test(requester_pid, b"k")
            .await
    });
    let writer_started = timeout(Duration::from_secs(2), started_rx).await;
    let progress = timeout(
        Duration::from_millis(500),
        handler.handle(Request::ListSessions(ListSessionsRequest {
            format: None,
            filter: None,
            sort_order: None,
            reversed: false,
        })),
    )
    .await;

    writer_started
        .expect("popup writer should receive the key")
        .expect("popup writer start signal should remain connected");
    let input_result = timeout(Duration::from_secs(1), input_task)
        .await
        .expect("popup key input should finish at the I/O deadline")
        .expect("popup key input task should not panic");
    input_result.expect("popup key timeout should not disconnect the attached client");
    {
        let active_attach = handler.active_attach.lock().await;
        assert!(
            active_attach.by_pid[&requester_pid].overlay.is_none(),
            "the blocked popup should be retired after its I/O deadline"
        );
    }
    release.release();
    assert!(
        matches!(&progress, Ok(Response::ListSessions(_))),
        "a blocked popup writer must not prevent unrelated server work: {progress:?}"
    );
    assert_eq!(
        *writes.lock().expect("recorded writes"),
        vec![b"k".to_vec()]
    );
}

#[tokio::test]
async fn popup_mouse_forward_uses_nonblocking_io_queue() {
    let handler = RequestHandler::new();
    let alpha = session_name("popup-mouse-writer");
    let requester_pid = std::process::id();
    let mut control_rx = create_attached_session(&handler, &alpha, requester_pid).await;

    run_overlay_command(
        &handler,
        requester_pid,
        r#"display-popup -T Popup -w 20 -h 6 -x C -y C"#,
    )
    .await;
    let _ = next_overlay_frame(&mut control_rx).await;

    let writes = Arc::new(StdMutex::new(Vec::<Vec<u8>>::new()));
    let writer_writes = Arc::clone(&writes);
    let (rect, _popup_cleanup) =
        install_popup_test_writer(&handler, requester_pid, true, move |bytes| {
            writer_writes.lock().expect("recorded writes").push(bytes);
            Ok(())
        })
        .await;

    handler
        .handle_attached_live_input_for_test(
            requester_pid,
            &sgr_mouse(0, rect.x.saturating_add(1), rect.y.saturating_add(1)),
        )
        .await
        .expect("popup mouse input");

    let recorded = writes.lock().expect("recorded writes").clone();
    handler
        .clear_interactive_overlay(requester_pid, true)
        .await
        .expect("popup cleanup");
    assert_eq!(recorded, vec![b"\x1b[<0;1;1M".to_vec()]);
}

#[tokio::test]
async fn popup_menu_paste_uses_nonblocking_io_queue() {
    let handler = RequestHandler::new();
    let alpha = session_name("popup-paste-writer");
    let requester_pid = std::process::id();
    let mut control_rx = create_attached_session(&handler, &alpha, requester_pid).await;
    let set_buffer = handler
        .handle(Request::SetBuffer(Box::new(SetBufferRequest {
            name: None,
            content: b"paste-data".to_vec(),
            append: false,
            new_name: None,
            set_clipboard: false,
            target_client: None,
        })))
        .await;
    assert!(matches!(set_buffer, Response::SetBuffer(_)));

    run_overlay_command(
        &handler,
        requester_pid,
        r#"display-popup -T Popup -w 20 -h 6 -x C -y C"#,
    )
    .await;
    let _ = next_overlay_frame(&mut control_rx).await;

    let writes = Arc::new(StdMutex::new(Vec::<Vec<u8>>::new()));
    let writer_writes = Arc::clone(&writes);
    let (rect, _popup_cleanup) =
        install_popup_test_writer(&handler, requester_pid, false, move |bytes| {
            writer_writes.lock().expect("recorded writes").push(bytes);
            Ok(())
        })
        .await;

    handler
        .handle_attached_live_input_for_test(requester_pid, &sgr_mouse(2, rect.x, rect.y))
        .await
        .expect("open popup menu");
    handler
        .handle_attached_live_input_for_test(requester_pid, b"p")
        .await
        .expect("paste from popup menu");

    let recorded = writes.lock().expect("recorded writes").clone();
    handler
        .clear_interactive_overlay(requester_pid, true)
        .await
        .expect("popup cleanup");
    assert_eq!(recorded, vec![b"paste-data".to_vec()]);
}

#[tokio::test]
async fn popup_attached_resize_releases_attach_lock_while_pty_resize_blocks() {
    let handler = RequestHandler::new();
    let alpha = session_name("popup-resize-writer");
    let requester_pid = std::process::id();
    let mut control_rx = create_attached_session(&handler, &alpha, requester_pid).await;

    run_overlay_command(
        &handler,
        requester_pid,
        r#"display-popup -T Popup -w 20 -h 6 -x C -y C"#,
    )
    .await;
    let _ = next_overlay_frame(&mut control_rx).await;

    let resized = Arc::new(StdMutex::new(Vec::<TerminalSize>::new()));
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let started_tx = Arc::new(StdMutex::new(Some(started_tx)));
    let release = BlockingPopupIoRelease::new();
    let callback_resized = Arc::clone(&resized);
    let callback_started = Arc::clone(&started_tx);
    let callback_release = release.callback_state();
    let (_, _popup_cleanup) = install_popup_test_resize(&handler, requester_pid, move |size| {
        callback_resized
            .lock()
            .expect("recorded popup resizes")
            .push(size);
        if let Some(started_tx) = callback_started.lock().expect("resize start").take() {
            let _ = started_tx.send(());
        }
        let (released, released_cv) = &*callback_release;
        let mut released = released.lock().expect("resize release");
        while !*released {
            released = released_cv.wait(released).expect("resize release wait");
        }
        Ok(())
    })
    .await;

    let resize_handler = handler.clone();
    let resize_task = tokio::spawn(async move {
        resize_handler
            .handle_attached_resize(requester_pid, TerminalSize { cols: 10, rows: 5 })
            .await
    });
    let resize_started = timeout(Duration::from_secs(2), started_rx).await;

    let attach_lock_progress = timeout(Duration::from_millis(500), async {
        let _active_attach = handler.active_attach.lock().await;
    })
    .await;

    release.release();
    resize_started
        .expect("popup PTY resize should start")
        .expect("popup PTY resize start signal should remain connected");
    attach_lock_progress
        .expect("a blocked popup PTY resize must not retain the active-attach lock");
    timeout(Duration::from_secs(2), resize_task)
        .await
        .expect("attached resize should finish after PTY resize release")
        .expect("attached resize task should not panic")
        .expect("attached resize should succeed");

    let recorded = resized.lock().expect("recorded popup resizes").clone();
    handler
        .clear_interactive_overlay(requester_pid, true)
        .await
        .expect("popup cleanup");
    assert_eq!(recorded, vec![TerminalSize { cols: 8, rows: 3 }]);
}

#[tokio::test]
async fn popup_menu_resize_releases_attach_lock_while_pty_resize_blocks() {
    let handler = RequestHandler::new();
    let alpha = session_name("popup-menu-resize-writer");
    let requester_pid = std::process::id();
    let mut control_rx = create_attached_session(&handler, &alpha, requester_pid).await;
    run_overlay_command(
        &handler,
        requester_pid,
        r#"display-popup -T Popup -w 20 -h 6 -x C -y C"#,
    )
    .await;
    let _ = next_overlay_frame(&mut control_rx).await;

    let resized = Arc::new(StdMutex::new(Vec::<TerminalSize>::new()));
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let started_tx = Arc::new(StdMutex::new(Some(started_tx)));
    let release = BlockingPopupIoRelease::new();
    let callback_resized = Arc::clone(&resized);
    let callback_started = Arc::clone(&started_tx);
    let callback_release = release.callback_state();
    let (rect, _popup_cleanup) = install_popup_test_resize(&handler, requester_pid, move |size| {
        callback_resized
            .lock()
            .expect("recorded popup resizes")
            .push(size);
        if let Some(started_tx) = callback_started.lock().expect("resize start").take() {
            let _ = started_tx.send(());
        }
        let (released, released_cv) = &*callback_release;
        let mut released = released.lock().expect("resize release");
        while !*released {
            released = released_cv.wait(released).expect("resize release wait");
        }
        Ok(())
    })
    .await;

    handler
        .handle_attached_live_input_for_test(requester_pid, &sgr_mouse(2, rect.x, rect.y))
        .await
        .expect("open popup menu");
    let menu_handler = handler.clone();
    let menu_task = tokio::spawn(async move {
        menu_handler
            .handle_attached_live_input_for_test(requester_pid, b"F")
            .await
    });
    let resize_started = timeout(Duration::from_secs(2), started_rx).await;
    let attach_lock_progress = timeout(Duration::from_millis(500), async {
        let _active_attach = handler.active_attach.lock().await;
    })
    .await;
    release.release();
    let menu_result = timeout(Duration::from_secs(2), menu_task).await;
    let recorded = resized.lock().expect("recorded popup resizes").clone();
    handler
        .clear_interactive_overlay(requester_pid, true)
        .await
        .expect("popup cleanup");

    resize_started
        .expect("popup menu PTY resize should start")
        .expect("popup menu resize start signal should remain connected");
    attach_lock_progress
        .expect("blocked popup menu PTY resize must not retain the active-attach lock");
    menu_result
        .expect("popup menu resize should finish after release")
        .expect("popup menu resize task should not panic")
        .expect("popup menu resize should succeed");
    assert_eq!(recorded, vec![TerminalSize { cols: 78, rows: 22 }]);
}

#[tokio::test]
async fn popup_drag_resize_releases_attach_lock_while_pty_resize_blocks() {
    let handler = RequestHandler::new();
    let alpha = session_name("popup-drag-resize-writer");
    let requester_pid = std::process::id();
    let mut control_rx = create_attached_session(&handler, &alpha, requester_pid).await;
    run_overlay_command(
        &handler,
        requester_pid,
        r#"display-popup -T Popup -w 20 -h 6 -x C -y C"#,
    )
    .await;
    let _ = next_overlay_frame(&mut control_rx).await;

    let resized = Arc::new(StdMutex::new(Vec::<TerminalSize>::new()));
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let started_tx = Arc::new(StdMutex::new(Some(started_tx)));
    let release = BlockingPopupIoRelease::new();
    let callback_resized = Arc::clone(&resized);
    let callback_started = Arc::clone(&started_tx);
    let callback_release = release.callback_state();
    let (rect, _popup_cleanup) = install_popup_test_resize(&handler, requester_pid, move |size| {
        callback_resized
            .lock()
            .expect("recorded popup resizes")
            .push(size);
        if let Some(started_tx) = callback_started.lock().expect("resize start").take() {
            let _ = started_tx.send(());
        }
        let (released, released_cv) = &*callback_release;
        let mut released = released.lock().expect("resize release");
        while !*released {
            released = released_cv.wait(released).expect("resize release wait");
        }
        Ok(())
    })
    .await;
    {
        let mut active_attach = handler.active_attach.lock().await;
        let Some(ClientOverlayState::Popup(popup)) = active_attach
            .by_pid
            .get_mut(&requester_pid)
            .and_then(|active| active.overlay.as_mut())
        else {
            panic!("expected popup overlay");
        };
        popup.begin_resize_for_test();
    }

    let drag_handler = handler.clone();
    let drag = sgr_mouse(34, rect.x.saturating_add(10), rect.y.saturating_add(3));
    let drag_task = tokio::spawn(async move {
        drag_handler
            .handle_attached_live_input_for_test(requester_pid, &drag)
            .await
    });
    let resize_started = timeout(Duration::from_secs(2), started_rx).await;
    let attach_lock_progress = timeout(Duration::from_millis(500), async {
        let _active_attach = handler.active_attach.lock().await;
    })
    .await;
    release.release();
    let drag_result = timeout(Duration::from_secs(2), drag_task).await;
    let recorded = resized.lock().expect("recorded popup resizes").clone();
    handler
        .clear_interactive_overlay(requester_pid, true)
        .await
        .expect("popup cleanup");

    resize_started
        .expect("popup drag PTY resize should start")
        .expect("popup drag resize start signal should remain connected");
    attach_lock_progress
        .expect("blocked popup drag PTY resize must not retain the active-attach lock");
    drag_result
        .expect("popup drag resize should finish after release")
        .expect("popup drag resize task should not panic")
        .expect("popup drag resize should succeed");
    assert_eq!(recorded, vec![TerminalSize { cols: 9, rows: 2 }]);
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
