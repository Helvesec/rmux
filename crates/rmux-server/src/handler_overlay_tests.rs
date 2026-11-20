use super::super::input_capture::RawPaneInputProbe;
use super::super::{
    overlay_support::{AttachedOverlayInput, ClientOverlayState},
    RequestHandler,
};
use super::session_name;
use crate::input_keys::MAX_SGR_MOUSE_FRAME_BYTES;
use crate::mouse::{layout_for_session, StatusRangeType};
use crate::pane_io::AttachControl;
use rmux_core::kitty_keyboard::FRAME_LIMIT;
use rmux_proto::request::RefreshClientRequest;
use rmux_proto::{
    BindKeyRequest, CapturePaneRequest, ListSessionsRequest, NewSessionExtRequest,
    NewSessionRequest, PaneTarget, Request, Response, ScopeSelector, SessionName, SetBufferRequest,
    SetOptionMode, Target, TerminalSize, WindowTarget,
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
    crate::session_fixture::assert_new_session(&created);

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

#[cfg(unix)]
fn quiet_popup_shell_command() -> &'static str {
    "sleep 60"
}

#[cfg(windows)]
fn quiet_popup_shell_command() -> &'static str {
    "ping -n 120 127.0.0.1 >NUL"
}

#[cfg(unix)]
fn ready_popup_shell_command() -> &'static str {
    "printf RMUX_POPUP_READY; sleep 60"
}

#[cfg(windows)]
fn ready_popup_shell_command() -> &'static str {
    "echo RMUX_POPUP_READY & ping -n 120 127.0.0.1 >NUL"
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
async fn display_menu_masks_and_restores_native_pane_focus() {
    let handler = RequestHandler::new();
    let alpha = session_name("focus-menu-mask");
    let requester_pid = std::process::id();
    let mut control_rx = create_quiet_attached_session(&handler, &alpha, requester_pid).await;
    let target = PaneTarget::new(alpha.clone(), 0);
    {
        let mut state = handler.state.lock().await;
        state
            .append_bytes_to_pane_transcript_for_test(&alpha, 0, 0, b"\x1b[?1004h")
            .expect("pane enables focus reporting");
        state.start_pane_input_capture_for_test(&target);
    }
    run_overlay_command(
        &handler,
        requester_pid,
        r#"display-menu -xM -yM -T Menu "Close" "f" { display-message closed }"#,
    )
    .await;
    let _ = next_overlay_frame(&mut control_rx).await;
    {
        let state = handler.state.lock().await;
        assert_eq!(
            state.pane_input_capture_for_test(&target),
            Some(b"\x1b[O".to_vec())
        );
    }
    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x1b[O")
        .await
        .expect("menu passes focus-out to the attach controller");
    {
        let active_attach = handler.active_attach.lock().await;
        let active = &active_attach.by_pid[&requester_pid];
        assert!(!active.reported_focused);
        assert!(
            active.overlay.is_some(),
            "focus-out must not close the menu"
        );
    }
    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x1b[I")
        .await
        .expect("menu passes focus-in to the attach controller");
    {
        let active_attach = handler.active_attach.lock().await;
        let active = &active_attach.by_pid[&requester_pid];
        assert!(active.reported_focused);
        assert!(active.overlay.is_some(), "focus-in must not close the menu");
    }
    handler
        .clear_interactive_overlay(requester_pid, true)
        .await
        .expect("menu action closes the masking surface");
    {
        let state = handler.state.lock().await;
        assert_eq!(
            state.pane_input_capture_for_test(&target),
            Some(b"\x1b[O\x1b[I".to_vec())
        );
    }
}

#[tokio::test]
async fn display_popup_masks_and_restores_native_pane_focus() {
    let handler = RequestHandler::new();
    let alpha = session_name("focus-popup-mask");
    let requester_pid = std::process::id();
    let mut control_rx = create_quiet_attached_session(&handler, &alpha, requester_pid).await;
    let target = PaneTarget::new(alpha.clone(), 0);
    {
        let mut state = handler.state.lock().await;
        state
            .append_bytes_to_pane_transcript_for_test(&alpha, 0, 0, b"\x1b[?1004h")
            .expect("pane enables focus reporting");
        state.start_pane_input_capture_for_test(&target);
    }

    run_overlay_command(
        &handler,
        requester_pid,
        r#"display-popup -N -T Popup -w 20 -h 6 -x C -y C"#,
    )
    .await;
    let _ = next_overlay_frame(&mut control_rx).await;
    run_overlay_command(&handler, requester_pid, "display-popup -C").await;

    let state = handler.state.lock().await;
    assert_eq!(
        state.pane_input_capture_for_test(&target),
        Some(b"\x1b[O\x1b[I".to_vec())
    );
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

async fn assert_menu_shortcut_writes(label: &str, shortcut: &str, input: &[u8], expected: &[u8]) {
    let handler = RequestHandler::new();
    let session = session_name(label);
    let requester_pid = std::process::id();
    let mut control_rx = create_quiet_attached_session(&handler, &session, requester_pid).await;
    // The overlay takes and returns focus; this fixture measures only what the
    // menu writes to the pane, so the pane must not report those edges.
    handler
        .disable_pane_focus_reports_for_test(&PaneTarget::new(session.clone(), 0))
        .await;
    let capture = RawPaneInputProbe::start(&handler, &session, label, expected.len()).await;
    let command = format!("display-menu -T Menu Action {shortcut:?} {{ send-keys -l {label:?} }}");

    run_overlay_command(&handler, requester_pid, &command).await;
    let _ = next_overlay_frame(&mut control_rx).await;
    let input_handler = handler.clone();
    let input = input.to_vec();
    tokio::spawn(async move {
        input_handler
            .handle_attached_live_input_for_test(requester_pid, &input)
            .await
    })
    .await
    .expect("menu shortcut task joins")
    .expect("menu shortcut input is handled");

    capture.finish(&handler, &session).await;
    capture.assert_contents(&handler, expected).await;
    assert!(handler.active_attach.lock().await.by_pid[&requester_pid]
        .overlay
        .is_none());
}

#[tokio::test]
async fn display_menu_modify_other_keys_printables_execute_shortcuts() {
    for (label, shortcut, input) in [
        ("mok-menu-shift-a", "A", b"\x1b[27;2;65~".as_slice()),
        ("mok-menu-plain-nine", "9", b"\x1b[27;1;57~".as_slice()),
        ("mok-menu-raw-a", "A", b"A".as_slice()),
        ("mok-menu-kitty-a", "A", b"\x1b[65u".as_slice()),
    ] {
        assert_menu_shortcut_writes(label, shortcut, input, label.as_bytes()).await;
    }
}

async fn assert_prompt_input_writes(label: &str, input: &[u8], expected: &[u8]) {
    let handler = RequestHandler::new();
    let session = session_name(label);
    let requester_pid = std::process::id();
    let _control_rx = create_quiet_attached_session(&handler, &session, requester_pid).await;
    let capture = RawPaneInputProbe::start(&handler, &session, label, expected.len()).await;

    let parsed = handler
        .parse_control_commands(r#"command-prompt -p Prompt { send-keys -l 'P:%%' }"#)
        .await
        .expect("prompt command parses");
    let prompt_handler = handler.clone();
    let prompt = tokio::spawn(async move {
        prompt_handler
            .execute_parsed_commands_for_test(requester_pid, parsed)
            .await
    });
    timeout(Duration::from_secs(1), async {
        while !handler.prompt_active(requester_pid).await {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("prompt becomes active");
    handler
        .handle_attached_live_input_for_test(requester_pid, input)
        .await
        .expect("prompt input is handled");
    handler
        .handle_attached_live_input_for_test(requester_pid, b"\r")
        .await
        .expect("prompt submission is handled");
    prompt
        .await
        .expect("prompt task joins")
        .expect("prompt command executes");

    capture.finish(&handler, &session).await;
    capture.assert_contents(&handler, expected).await;
    assert!(!handler.prompt_active(requester_pid).await);
}

#[tokio::test]
async fn command_prompt_modify_other_keys_printables_insert_text() {
    for (label, input, expected) in [
        (
            "mok-prompt-shift-a",
            b"\x1b[27;2;65~".as_slice(),
            b"P:A".as_slice(),
        ),
        (
            "mok-prompt-unicode",
            b"\x1b[27;1;233~".as_slice(),
            "P:é".as_bytes(),
        ),
        ("mok-prompt-raw-a", b"A".as_slice(), b"P:A".as_slice()),
        (
            "mok-prompt-kitty-a",
            b"\x1b[65u".as_slice(),
            b"P:A".as_slice(),
        ),
    ] {
        assert_prompt_input_writes(label, input, expected).await;
    }
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
            .map(AttachControl::materialize_received)
        {
            Some(AttachControl::Overlay(frame)) => return frame,
            Some(AttachControl::Refresh | AttachControl::Switch(_)) => continue,
            other => panic!("expected overlay frame, got {other:?}"),
        }
    }
}

async fn next_overlay_frame_containing(
    control_rx: &mut mpsc::UnboundedReceiver<AttachControl>,
    marker: &[u8],
) -> crate::pane_io::OverlayFrame {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    loop {
        let now = tokio::time::Instant::now();
        assert!(now < deadline, "popup readiness marker arrives");
        match timeout(deadline - now, control_rx.recv())
            .await
            .expect("popup readiness marker arrives")
            .map(AttachControl::materialize_received)
        {
            Some(AttachControl::Overlay(frame))
                if frame
                    .frame
                    .windows(marker.len())
                    .any(|window| window == marker) =>
            {
                return frame;
            }
            Some(AttachControl::Overlay(_) | AttachControl::Refresh | AttachControl::Switch(_)) => {
            }
            other => panic!("expected popup readiness frame, got {other:?}"),
        }
    }
}

/// Only a popup job's reader repaints through the popup coalescing slot
/// (`send_popup_overlay`). A real job (ConPTY under Windows) writes its modes,
/// title and repaint whenever its terminal emits them, so a read waiting for an
/// ordinary overlay (a replay or a clear) must not take such a repaint for it.
fn is_popup_reader_repaint(control: &AttachControl) -> bool {
    matches!(control, AttachControl::CoalescedPopupOverlay(_))
}

/// The next overlay frame that is not a popup job's reader repaint.
async fn next_ordinary_overlay_frame(
    control_rx: &mut mpsc::UnboundedReceiver<AttachControl>,
) -> crate::pane_io::OverlayFrame {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    loop {
        let now = tokio::time::Instant::now();
        assert!(now < deadline, "ordinary overlay control message arrives");
        let control = timeout(deadline - now, control_rx.recv())
            .await
            .expect("ordinary overlay control message arrives");
        if control.as_ref().is_some_and(is_popup_reader_repaint) {
            continue;
        }
        match control.map(AttachControl::materialize_received) {
            Some(AttachControl::Overlay(frame)) => return frame,
            Some(AttachControl::Refresh | AttachControl::Switch(_)) => {}
            other => panic!("expected ordinary overlay frame, got {other:?}"),
        }
    }
}

/// The first overlay frame stamped after `overlay_generation`: an older one was
/// published before the step that set it.
async fn next_overlay_frame_after(
    control_rx: &mut mpsc::UnboundedReceiver<AttachControl>,
    overlay_generation: u64,
) -> crate::pane_io::OverlayFrame {
    loop {
        let frame = next_overlay_frame(control_rx).await;
        if frame.overlay_generation > overlay_generation {
            return frame;
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
        let control = timeout(deadline - now, control_rx.recv())
            .await
            .expect("refresh-client control arrives");
        if control.as_ref().is_some_and(is_popup_reader_repaint) {
            continue;
        }
        match control.map(AttachControl::materialize_received) {
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
    let initial = next_overlay_frame(&mut control_rx).await;
    assert!(
        initial.frame.ends_with(b"\x1b[?25l"),
        "a no-job popup must hide the unrelated pane cursor"
    );

    let frame = refresh_client_overlay_frame(&handler, requester_pid, &mut control_rx).await;
    assert!(frame.persistent);
    assert!(
        frame.frame.ends_with(b"\x1b[?25l"),
        "the replayed popup frame must retain cursor ownership"
    );
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
        match control.materialize_received() {
            AttachControl::Switch(target) => {
                let target = target.into_target();
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
async fn popup_repaint_backpressure_does_not_detach_attached_client() {
    let handler = RequestHandler::new();
    let fixture = handler.clone();
    let mut exercise = tokio::spawn(async move {
        let handler = fixture;
        let alpha = session_name("popup-repaint-backpressure");
        let requester_pid = 920_216;
        let mut control_rx = create_quiet_attached_session(&handler, &alpha, requester_pid).await;

        // Record only the child and PTY created by this private handler fixture.
        #[cfg(unix)]
        {
            let state = handler.state.lock().await;
            let pid = state.pane_pid_in_window(&alpha, 0, 0).unwrap();
            let tty = state.pane_tty_path_in_window(&alpha, 0, 0).unwrap();
            eprintln!(
                "popup quiet fixture: parent={} pid={pid} pgid={pid} tty={}",
                std::process::id(),
                tty.display()
            );
        }

        run_overlay_command(
            &handler,
            requester_pid,
            r#"display-popup -N -T Backpressure -w 100% -h 100% -x C -y C"#,
        )
        .await;

        let (identity, popup_id, surface, backlog, closing) = {
            let active_attach = handler.active_attach.lock().await;
            let active = &active_attach.by_pid[&requester_pid];
            let Some(ClientOverlayState::Popup(popup)) = active.overlay.as_ref() else {
                panic!("expected popup overlay");
            };
            assert!(
                popup.no_job && popup.job.is_none(),
                "-N never spawns a popup job"
            );
            (
                active.identity(requester_pid),
                popup.id,
                Arc::clone(&popup.surface),
                Arc::clone(&active.control_backlog),
                Arc::clone(&active.closing),
            )
        };
        while let Ok(control) = control_rx.try_recv() {
            crate::pane_io::release_attach_control_backlog(
                &backlog,
                control.received_backlog_units(),
            );
        }
        assert_eq!(backlog.load(std::sync::atomic::Ordering::Acquire), 0);

        let repaint_count = super::super::attach_support::ATTACH_CONTROL_BACKLOG_LIMIT * 2;
        let (done, baseline) = {
            let surface = surface.lock().expect("popup surface");
            (
                Arc::clone(&surface.flush_done_for_test),
                surface.production_counts_for_test(),
            )
        };
        for marker in 0..repaint_count {
            // Earlier acknowledged renders still exercise real queue replacement
            // under pressure. Hold only the last production worker to force the
            // schedule that made an immediate try_recv drain race final delivery.
            let gate = (marker + 1 == repaint_count).then(|| {
                let gate = Arc::new(tokio::sync::Semaphore::new(0));
                surface.lock().expect("popup surface").flush_gate_for_test =
                    Some(Arc::clone(&gate));
                gate
            });
            surface
                .lock()
                .expect("popup surface")
                .append_for_test(format!("\x1b[Hpopup-final-{marker:03}").as_bytes());
            handler
                .popup_reader_tick(identity, popup_id)
                .await
                .expect("popup repaint succeeds");
            if let Some(gate) = gate {
                tokio::task::yield_now().await;
                let counts = surface
                    .lock()
                    .expect("popup surface")
                    .production_counts_for_test();
                assert_eq!(counts, (baseline.0 + marker, baseline.1 + marker));
                assert_eq!(backlog.load(std::sync::atomic::Ordering::Acquire), 1);
                tokio::select! {
                    biased;
                    _ = done.notified() => panic!("held final worker must not acknowledge publication"),
                    _ = std::future::ready(()) => {}
                }
                eprintln!(
                    "popup delayed final: acknowledged={marker} final_pending=true backlog=1"
                );
                gate.add_permits(1);
            }
            timeout(Duration::from_secs(2), done.notified())
                .await
                .expect("actual reader publication completes before draining");
        }
        let counts = surface
            .lock()
            .expect("popup surface")
            .production_counts_for_test();
        assert_eq!(
            counts,
            (baseline.0 + repaint_count, baseline.1 + repaint_count)
        );

        assert!(
            !closing.load(std::sync::atomic::Ordering::SeqCst),
            "popup repaint pressure must not close the attached client"
        );
        let active_attach = handler.active_attach.lock().await;
        assert!(active_attach.by_pid.contains_key(&requester_pid));
        assert!(
            backlog.load(std::sync::atomic::Ordering::Acquire)
                < super::super::attach_support::ATTACH_CONTROL_BACKLOG_LIMIT,
            "replacement-safe popup repaints must stay below the overload limit"
        );
        drop(active_attach);

        let final_marker = format!("popup-final-{:03}", repaint_count - 1);
        let mut overlay_frames = Vec::new();
        let mut saw_detach = false;
        while let Ok(control) = control_rx.try_recv() {
            crate::pane_io::release_attach_control_backlog(
                &backlog,
                control.received_backlog_units(),
            );
            let control = control.materialize_received();
            match control {
                AttachControl::Overlay(frame) => overlay_frames.push(frame),
                AttachControl::Detach => saw_detach = true,
                other => panic!("unexpected popup repaint control: {other:?}"),
            }
        }
        assert!(!saw_detach, "popup repaint pressure must not queue Detach");
        assert_eq!(
            overlay_frames.len(),
            1,
            "only the latest repaint is retained"
        );
        assert!(
            String::from_utf8_lossy(&overlay_frames[0].frame).contains(&final_marker),
            "the retained repaint must contain the popup's final surface"
        );
        assert_eq!(backlog.load(std::sync::atomic::Ordering::Acquire), 1);
        drop(overlay_frames);
        assert_eq!(backlog.load(std::sync::atomic::Ordering::Acquire), 0);
        eprintln!(
            "popup backpressure final: repaints={repaint_count} latest={final_marker} no_detach=true retained=1 released=0"
        );
    });
    // Keep the owning handler outside the task so panic/timeout cannot skip
    // teardown. Abort and join first, releasing any state lock before cleanup.
    let outcome = timeout(Duration::from_secs(15), &mut exercise).await;
    if outcome.is_err() {
        exercise.abort();
        let _ = exercise.await;
    }
    handler.state.lock().await.shutdown_terminals_for_test();
    eprintln!("popup quiet fixture: owned terminal teardown completed");
    outcome
        .expect("backpressure fixture is bounded")
        .expect("backpressure assertions pass");
}

#[tokio::test]
async fn a_second_popup_keeps_a_job_backed_popup_and_its_child() {
    let handler = RequestHandler::new();
    let alpha = session_name("popup-replacement-terminates-child");
    let requester_pid = std::process::id();
    let mut control_rx = create_attached_session(&handler, &alpha, requester_pid).await;

    run_overlay_command(
        &handler,
        requester_pid,
        &format!(
            r#"display-popup -T Old -w 20 -h 6 -x C -y C "{}""#,
            ready_popup_shell_command()
        ),
    )
    .await;
    let _ = next_overlay_frame_containing(&mut control_rx, b"RMUX_POPUP_READY").await;
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

    // Measured on tmux 3.7b: a second display-popup for a client that already shows a
    // popup succeeds without opening, and the first popup and its job stay in place.
    run_overlay_command(
        &handler,
        requester_pid,
        r#"display-popup -N -E -T Second -w 20 -h 6 -x C -y C"#,
    )
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        old_job.child_is_running_for_test(),
        "the first popup's job keeps running"
    );
    {
        let active_attach = handler.active_attach.lock().await;
        let Some(ClientOverlayState::Popup(popup)) =
            active_attach.by_pid[&requester_pid].overlay.as_ref()
        else {
            panic!("the first popup stays on the client");
        };
        assert_eq!(popup.title, "Old");
        assert!(popup.job.is_some(), "the first popup keeps its job");
    }

    handler
        .clear_interactive_overlay(requester_pid, true)
        .await
        .expect("first popup cleanup");
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

/// Feeds `output` to the popup's terminal surface the way its child process
/// would, then returns the overlay frame the attached client receives.
async fn popup_surface_frame(
    handler: &RequestHandler,
    requester_pid: u32,
    control_rx: &mut mpsc::UnboundedReceiver<AttachControl>,
    output: &[u8],
) -> String {
    {
        let mut active_attach = handler.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get_mut(&requester_pid)
            .expect("attached client");
        let Some(ClientOverlayState::Popup(popup)) = active.overlay.as_mut() else {
            panic!("expected popup overlay");
        };
        popup
            .surface
            .lock()
            .expect("popup surface")
            .append_for_test(output);
    }
    let frame = refresh_client_overlay_frame(handler, requester_pid, control_rx).await;
    String::from_utf8(frame.frame).expect("popup frame is utf-8")
}

/// Feeds `output` to a detached popup surface the way its job's reader would,
/// and returns the first popup frame rendered after `output` was applied.
async fn popup_reader_surface_frame(
    handler: &RequestHandler,
    requester_pid: u32,
    control_rx: &mut mpsc::UnboundedReceiver<AttachControl>,
    _detached: &DetachedPopupJob,
    output: &[u8],
) -> crate::pane_io::OverlayFrame {
    let (identity, popup_id, injected_generation) = {
        let mut active_attach = handler.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get_mut(&requester_pid)
            .expect("attached client");
        let identity = active.identity(requester_pid);
        let Some(ClientOverlayState::Popup(popup)) = active.overlay.as_mut() else {
            panic!("expected popup overlay");
        };
        popup
            .surface
            .lock()
            .expect("popup surface")
            .append_for_test(output);
        let popup_id = popup.id;
        // A popup repaint rendered before `output` now fails its generation
        // check and renders again, so every frame stamped after this one
        // shows `output`, whatever the job's reader published meanwhile.
        active.overlay_generation = active.overlay_generation.saturating_add(1);
        (identity, popup_id, active.overlay_generation)
    };
    handler
        .popup_reader_tick(identity, popup_id)
        .await
        .expect("popup reader refresh succeeds");
    next_overlay_frame_after(control_rx, injected_generation).await
}

/// A popup whose surface only the test writes to. Under ConPTY the job's own
/// terminal writes its startup modes, title and repaint whenever Windows emits
/// them, so its reader may publish a popup-only refresh or move the surface
/// cursor between two steps of a test. The job keeps writing to its original
/// surface, which `write_job_surface` reaches to replay such a write anywhere.
struct DetachedPopupJob {
    write_job_surface: JobSurfaceWriter,
}

/// Writes to the surface a popup job's reader keeps.
type JobSurfaceWriter = Box<dyn Fn(&[u8]) + Send + Sync>;

/// Gives the popup a blank surface of the same size that no job reader writes
/// to.
async fn detach_popup_surface_from_job(
    handler: &RequestHandler,
    requester_pid: u32,
) -> DetachedPopupJob {
    let mut active_attach = handler.active_attach.lock().await;
    let active = active_attach
        .by_pid
        .get_mut(&requester_pid)
        .expect("attached client");
    let Some(ClientOverlayState::Popup(popup)) = active.overlay.as_mut() else {
        panic!("expected popup overlay");
    };
    let detached = popup
        .surface
        .lock()
        .expect("popup surface")
        .detached_for_test();
    let job_surface = std::mem::replace(&mut popup.surface, Arc::new(StdMutex::new(detached)));
    DetachedPopupJob {
        write_job_surface: Box::new(move |bytes| {
            job_surface
                .lock()
                .expect("job popup surface")
                .append_for_test(bytes);
        }),
    }
}

/// What a ConPTY job writes late around a repaint: it hides the cursor, sets
/// its title and homes, and may show the cursor again only in a later read.
const LATE_CONPTY_JOB_WRITE: &[u8] = b"\x1b[?25l\x1b]0;job\x07\x1b[H";

/// Replays a late job write the way the job's reader delivers it, and waits
/// until the popup refresh it causes has been published to the client.
async fn publish_late_job_write(
    handler: &RequestHandler,
    requester_pid: u32,
    job: &DetachedPopupJob,
    output: &[u8],
) {
    let (identity, popup_id, surface) = {
        let mut active_attach = handler.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get_mut(&requester_pid)
            .expect("attached client");
        let identity = active.identity(requester_pid);
        let Some(ClientOverlayState::Popup(popup)) = active.overlay.as_mut() else {
            panic!("expected popup overlay");
        };
        (identity, popup.id, Arc::clone(&popup.surface))
    };
    let published = || {
        surface
            .lock()
            .expect("popup surface")
            .production_counts_for_test()
            .1
    };
    let before = published();
    (job.write_job_surface)(output);
    handler
        .popup_reader_tick(identity, popup_id)
        .await
        .expect("popup reader refresh succeeds");
    timeout(Duration::from_secs(1), async {
        while published() == before {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("the job reader refresh is published");
}

/// Replays a late job write, then the base refresh the pane's own late ConPTY
/// output causes: that ordinary control closes the popup coalescing slot, so
/// the job's repaint stays queued ahead of the next popup frame.
async fn publish_late_job_write_then_pane_refresh(
    handler: &RequestHandler,
    requester_pid: u32,
    job: &DetachedPopupJob,
) {
    publish_late_job_write(handler, requester_pid, job, LATE_CONPTY_JOB_WRITE).await;
    let active_attach = handler.active_attach.lock().await;
    active_attach.by_pid[&requester_pid]
        .control_tx
        .send(AttachControl::Refresh)
        .expect("pane refresh is queued");
}

fn expected_popup_cursor_suffix(
    rect: crate::renderer::OverlayRect,
    bordered: bool,
    cursor_x: u16,
    cursor_y: u16,
    visible: bool,
) -> Vec<u8> {
    let border_offset = if bordered { 1 } else { 0 };
    format!(
        "\x1b[{};{}H\x1b[?25{}",
        rect.y
            .saturating_add(border_offset)
            .saturating_add(cursor_y)
            .saturating_add(1),
        rect.x
            .saturating_add(border_offset)
            .saturating_add(cursor_x)
            .saturating_add(1),
        if visible { 'h' } else { 'l' }
    )
    .into_bytes()
}

fn final_cursor_visibility(frame: &[u8]) -> Option<bool> {
    let frame = String::from_utf8_lossy(frame);
    match (frame.rfind("\u{1b}[?25h"), frame.rfind("\u{1b}[?25l")) {
        (Some(show), Some(hide)) => Some(show > hide),
        (Some(_), None) => Some(true),
        (None, Some(_)) => Some(false),
        (None, None) => None,
    }
}

#[tokio::test]
async fn live_popup_projects_bordered_cursor_visibility_move_resize_replay_and_clear() {
    let handler = RequestHandler::new();
    let alpha = session_name("popup-live-cursor");
    let requester_pid = std::process::id();
    let mut control_rx = create_attached_session(&handler, &alpha, requester_pid).await;
    let command = format!(
        r#"display-popup -T Cursor -w 20 -h 6 -x C -y C "{}""#,
        ready_popup_shell_command()
    );

    run_overlay_command(&handler, requester_pid, &command).await;
    let _ = next_overlay_frame_containing(&mut control_rx, b"RMUX_POPUP_READY").await;
    let (rect, _popup_cleanup) =
        install_popup_test_writer(&handler, requester_pid, false, |_| Ok(())).await;
    let job = detach_popup_surface_from_job(&handler, requester_pid).await;

    // Each read below follows a late job write, as one may under ConPTY.
    publish_late_job_write_then_pane_refresh(&handler, requester_pid, &job).await;
    let visible = popup_reader_surface_frame(
        &handler,
        requester_pid,
        &mut control_rx,
        &job,
        b"\x1b[2J\x1b[3;5H",
    )
    .await;
    let expected = expected_popup_cursor_suffix(rect, true, 4, 2, true);
    assert!(
        visible.frame.ends_with(&expected),
        "bordered popup cursor should follow its live surface: {:?}",
        String::from_utf8_lossy(&visible.frame)
    );

    publish_late_job_write(&handler, requester_pid, &job, LATE_CONPTY_JOB_WRITE).await;
    let replay = refresh_client_overlay_frame(&handler, requester_pid, &mut control_rx).await;
    assert!(
        replay.frame.ends_with(&expected),
        "refresh replay should retain the popup cursor projection"
    );

    let hidden = popup_reader_surface_frame(
        &handler,
        requester_pid,
        &mut control_rx,
        &job,
        b"\x1b[?25l\x1b[4;6H",
    )
    .await;
    let (identity, popup_id, active_render_generation, active_overlay_generation, job_present) = {
        let active_attach = handler.active_attach.lock().await;
        let active = &active_attach.by_pid[&requester_pid];
        let Some(ClientOverlayState::Popup(popup)) = active.overlay.as_ref() else {
            panic!("expected cursor popup");
        };
        (
            active.identity(requester_pid),
            popup.id,
            active.render_generation,
            active.overlay_generation,
            popup.job.is_some(),
        )
    };
    assert!(
        hidden
            .frame
            .ends_with(&expected_popup_cursor_suffix(rect, true, 5, 3, false)),
        "a child DECTCEM hide should be projected with its cursor position; identity={identity:?}, popup_id={popup_id}, active_render_generation={active_render_generation}, active_overlay_generation={active_overlay_generation}, received_render_generation={}, received_overlay_generation={}, popup_job_present={job_present}, frame={:?}",
        hidden.render_generation,
        hidden.overlay_generation,
        String::from_utf8_lossy(&hidden.frame)
    );

    let moved_rect = {
        let mut active_attach = handler.active_attach.lock().await;
        let Some(ClientOverlayState::Popup(popup)) = active_attach
            .by_pid
            .get_mut(&requester_pid)
            .and_then(|active| active.overlay.as_mut())
        else {
            panic!("expected popup overlay");
        };
        popup.rect.x = popup.rect.x.saturating_add(3);
        popup.rect.y = popup.rect.y.saturating_add(2);
        popup.rect
    };
    let moved = popup_reader_surface_frame(
        &handler,
        requester_pid,
        &mut control_rx,
        &job,
        b"\x1b[?25h\x1b[2;3H",
    )
    .await;
    assert!(
        moved
            .frame
            .ends_with(&expected_popup_cursor_suffix(moved_rect, true, 2, 1, true)),
        "a moved popup should translate from its current content origin"
    );

    let resized_rect = {
        let mut active_attach = handler.active_attach.lock().await;
        let Some(ClientOverlayState::Popup(popup)) = active_attach
            .by_pid
            .get_mut(&requester_pid)
            .and_then(|active| active.overlay.as_mut())
        else {
            panic!("expected popup overlay");
        };
        popup.rect.width = 8;
        popup.rect.height = 5;
        popup.rect
    };
    let resized = popup_reader_surface_frame(
        &handler,
        requester_pid,
        &mut control_rx,
        &job,
        b"\x1b[999;999H",
    )
    .await;
    assert!(
        resized.frame.ends_with(&expected_popup_cursor_suffix(
            resized_rect,
            true,
            5,
            2,
            true
        )),
        "a resized popup should clamp the projected cursor to drawable content"
    );

    publish_late_job_write(&handler, requester_pid, &job, LATE_CONPTY_JOB_WRITE).await;
    handler
        .clear_interactive_overlay(requester_pid, true)
        .await
        .expect("popup clear succeeds");
    let clear = next_ordinary_overlay_frame(&mut control_rx).await;
    assert!(
        clear.frame.is_empty(),
        "popup clear publishes an empty frame"
    );

    let response = handler
        .dispatch(
            requester_pid,
            Request::RefreshClient(Box::new(full_refresh_client_request(requester_pid))),
        )
        .await
        .response;
    assert!(matches!(response, Response::RefreshClient(_)));
    let base_frame = loop {
        match timeout(Duration::from_secs(1), control_rx.recv())
            .await
            .expect("base redraw arrives")
        {
            Some(AttachControl::Switch(target)) => break target.into_target().render_frame,
            Some(AttachControl::Refresh | AttachControl::Overlay(_)) => {}
            other => panic!("expected base redraw after popup clear, got {other:?}"),
        }
    };
    assert_eq!(
        final_cursor_visibility(&base_frame),
        Some(true),
        "the existing base redraw should restore the visible pane cursor"
    );
}

#[tokio::test]
async fn live_borderless_popup_projects_cursor_without_border_offset() {
    let handler = RequestHandler::new();
    let alpha = session_name("popup-borderless-cursor");
    let requester_pid = std::process::id();
    let mut control_rx = create_attached_session(&handler, &alpha, requester_pid).await;
    let command = format!(
        r#"display-popup -B -T Cursor -w 20 -h 6 -x C -y C "{}""#,
        quiet_popup_shell_command()
    );

    run_overlay_command(&handler, requester_pid, &command).await;
    let _ = next_overlay_frame(&mut control_rx).await;
    let (rect, _popup_cleanup) =
        install_popup_test_writer(&handler, requester_pid, false, |_| Ok(())).await;
    let job = detach_popup_surface_from_job(&handler, requester_pid).await;
    publish_late_job_write_then_pane_refresh(&handler, requester_pid, &job).await;
    let frame = popup_reader_surface_frame(
        &handler,
        requester_pid,
        &mut control_rx,
        &job,
        b"\x1b[2J\x1b[3;5H",
    )
    .await;

    assert!(
        frame
            .frame
            .ends_with(&expected_popup_cursor_suffix(rect, false, 4, 2, true)),
        "-B should not add a border offset to the popup cursor"
    );
}

#[tokio::test]
async fn popup_cursor_preserves_exited_surface_and_hides_nested_menu_or_zero_size() {
    let handler = RequestHandler::new();
    let alpha = session_name("popup-hidden-cursor-states");
    let requester_pid = std::process::id();
    let mut control_rx = create_attached_session(&handler, &alpha, requester_pid).await;
    let command = format!(
        r#"display-popup -T Cursor -w 20 -h 6 -x C -y C "{}""#,
        quiet_popup_shell_command()
    );

    run_overlay_command(&handler, requester_pid, &command).await;
    let _ = next_overlay_frame(&mut control_rx).await;
    let (_, _popup_cleanup) =
        install_popup_test_writer(&handler, requester_pid, false, |_| Ok(())).await;
    let job_write = detach_popup_surface_from_job(&handler, requester_pid).await;
    let _ = popup_reader_surface_frame(
        &handler,
        requester_pid,
        &mut control_rx,
        &job_write,
        b"\x1b[2J\x1b[3;5H",
    )
    .await;
    // A ConPTY job writes its title and repaint late; here that popup-only
    // refresh is queued before the menu opens, as one was in Windows CI.
    publish_late_job_write(
        &handler,
        requester_pid,
        &job_write,
        b"\x1b]0;job\x07\x1b[?25h\x1b[H",
    )
    .await;

    run_overlay_command(
        &handler,
        requester_pid,
        r#"display-menu -T Menu "First" "f" { display-message first }"#,
    )
    .await;
    let nested = next_overlay_frame_containing(&mut control_rx, b"First").await;
    assert!(
        nested.frame.ends_with(b"\x1b[?25l"),
        "a nested menu should hide the popup cursor after composing both layers"
    );

    let (zero_sized, exited, rect) = {
        let mut active_attach = handler.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get_mut(&requester_pid)
            .expect("attached client");
        let Some(ClientOverlayState::Popup(popup)) = active.overlay.as_mut() else {
            panic!("expected popup overlay");
        };
        popup.nested_menu = None;
        let original_rect = popup.rect;
        popup.rect.width = 2;
        popup.rect.height = 2;
        let zero_sized = active
            .overlay
            .as_ref()
            .expect("popup remains active")
            .render();
        let Some(ClientOverlayState::Popup(popup)) = active.overlay.as_mut() else {
            panic!("expected popup overlay");
        };
        popup.rect = original_rect;
        let job = popup.job.take().expect("live popup job");
        let exited = active
            .overlay
            .as_ref()
            .expect("retained exited popup")
            .render();
        let Some(ClientOverlayState::Popup(popup)) = active.overlay.as_mut() else {
            panic!("expected popup overlay");
        };
        popup.job = Some(job);
        (zero_sized, exited, original_rect)
    };
    assert!(
        zero_sized.ends_with(b"\x1b[?25l"),
        "zero-sized popup content should hide the cursor"
    );
    assert!(
        exited.ends_with(&expected_popup_cursor_suffix(rect, true, 4, 2, true)),
        "tmux3.7b retains the final surface cursor after the popup job exits"
    );
}

#[tokio::test]
async fn display_popup_preserves_process_colours_and_attributes() {
    // Issue #181: popup content was captured as plain text and then drawn
    // through the status format pipeline, so every SGR attribute a process
    // emitted was discarded before the client ever saw it.
    let handler = RequestHandler::new();
    let alpha = session_name("popup-process-sgr");
    let requester_pid = std::process::id();
    let mut control_rx = create_attached_session(&handler, &alpha, requester_pid).await;

    run_overlay_command(
        &handler,
        requester_pid,
        r#"display-popup -N -T Colours -w 40 -h 12 -x C -y C"#,
    )
    .await;
    let _ = next_overlay_frame(&mut control_rx).await;

    let frame = popup_surface_frame(
        &handler,
        requester_pid,
        &mut control_rx,
        b"\x1b[31mRED\x1b[0m\r\n\
          \x1b[38;5;208mIDX\x1b[0m\r\n\
          \x1b[38;2;10;200;30mTRUE\x1b[0m\r\n\
          \x1b[1mBOLD\x1b[0m \x1b[4mUNDER\x1b[0m \x1b[7mREV\x1b[0m\r\n\
          plain\r\n",
    )
    .await;

    for (sequence, text) in [
        ("\u{1b}[31m", "RED"),
        ("\u{1b}[38;5;208m", "IDX"),
        ("\u{1b}[38;2;10;200;30m", "TRUE"),
        ("\u{1b}[1m", "BOLD"),
        ("\u{1b}[4m", "UNDER"),
        ("\u{1b}[7m", "REV"),
    ] {
        assert!(
            frame.contains(&format!("{sequence}{text}")),
            "popup content must keep {sequence:?} before {text:?}: {frame:?}"
        );
    }
    assert!(
        frame.contains("plain"),
        "unstyled popup content still renders: {frame:?}"
    );
}

#[tokio::test]
async fn display_popup_resets_process_attributes_at_their_boundaries() {
    let handler = RequestHandler::new();
    let alpha = session_name("popup-process-sgr-reset");
    let requester_pid = std::process::id();
    let mut control_rx = create_attached_session(&handler, &alpha, requester_pid).await;

    run_overlay_command(
        &handler,
        requester_pid,
        r#"display-popup -N -T Reset -w 30 -h 8 -x C -y C"#,
    )
    .await;
    let _ = next_overlay_frame(&mut control_rx).await;

    let frame = popup_surface_frame(
        &handler,
        requester_pid,
        &mut control_rx,
        b"\x1b[31mRED\x1b[0mTAIL\r\n",
    )
    .await;

    let row = frame
        .split("\u{1b}7")
        .find(|chunk| chunk.contains("RED"))
        .expect("styled popup row is emitted");
    let tail = row.split("RED").nth(1).expect("text after the styled run");
    assert!(
        tail.starts_with('\u{1b}'),
        "the colour must be closed before the unstyled tail: {row:?}"
    );
    assert!(
        !tail
            .split("TAIL")
            .next()
            .expect("prefix before the tail text")
            .contains("31"),
        "the unstyled tail must not stay red: {row:?}"
    );
}

#[tokio::test]
async fn display_popup_style_only_paints_cells_the_process_left_unstyled() {
    let handler = RequestHandler::new();
    let alpha = session_name("popup-style-defaults");
    let requester_pid = std::process::id();
    let mut control_rx = create_attached_session(&handler, &alpha, requester_pid).await;

    run_overlay_command(
        &handler,
        requester_pid,
        r#"display-popup -N -T Styled -s bg=blue -w 30 -h 8 -x C -y C"#,
    )
    .await;
    let _ = next_overlay_frame(&mut control_rx).await;

    let frame = popup_surface_frame(
        &handler,
        requester_pid,
        &mut control_rx,
        b"\x1b[41mRED\x1b[0m plain\r\n",
    )
    .await;

    // Asserted on the content row itself, not on the whole frame: the popup
    // background fill would otherwise satisfy a frame-wide check on its own.
    let row = frame
        .split("\u{1b}7")
        .find(|chunk| chunk.contains("RED"))
        .expect("styled popup row is emitted");
    assert!(
        row.contains("\u{1b}[41mRED"),
        "-s must not override a background the process chose: {row:?}"
    );
    let tail = row.split("RED").nth(1).expect("text after the styled run");
    assert!(
        tail.contains("44"),
        "-s must still colour the cells the process left unstyled: {row:?}"
    );
}

#[tokio::test]
async fn display_popup_keeps_wide_characters_whole_when_clipping() {
    let handler = RequestHandler::new();
    let alpha = session_name("popup-unicode-clip");
    let requester_pid = std::process::id();
    let mut control_rx = create_attached_session(&handler, &alpha, requester_pid).await;

    run_overlay_command(
        &handler,
        requester_pid,
        r#"display-popup -N -T Unicode -w 10 -h 6 -x C -y C"#,
    )
    .await;
    let _ = next_overlay_frame(&mut control_rx).await;

    let frame = popup_surface_frame(
        &handler,
        requester_pid,
        &mut control_rx,
        "\u{1b}[32m日本語テキスト\u{1b}[0m\r\n".as_bytes(),
    )
    .await;

    assert!(
        frame.contains("\u{1b}[32m日本"),
        "wide characters keep their colour: {frame:?}"
    );
    let row = frame
        .split("\u{1b}7")
        .find(|chunk| chunk.contains('日'))
        .expect("unicode popup row is emitted");
    let painted = row.matches('日').count()
        + row.matches('本').count()
        + row.matches('語').count()
        + row.matches('テ').count()
        + row.matches('キ').count()
        + row.matches('ス').count()
        + row.matches('ト').count();
    assert!(
        painted <= 4,
        "an 8-column content area holds at most four wide cells: {row:?}"
    );
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
    enum Resolution {
        Final,
        Interruption,
        Expiry,
    }

    for (label, resolution) in [
        ("menu-kitty-bound-final", Resolution::Final),
        ("menu-kitty-bound-interruption", Resolution::Interruption),
        ("menu-kitty-bound-expiry", Resolution::Expiry),
    ] {
        let handler = RequestHandler::new();
        let session = session_name(label);
        let requester_pid = std::process::id();
        let mut control_rx = create_quiet_attached_session(&handler, &session, requester_pid).await;
        // The overlay takes and returns focus; this fixture measures only what
        // the menu writes to the pane, so the pane must not report those edges.
        handler
            .disable_pane_focus_reports_for_test(&PaneTarget::new(session.clone(), 0))
            .await;
        let capture = RawPaneInputProbe::start(&handler, &session, label, b"NINE".len()).await;

        run_overlay_command(
            &handler,
            requester_pid,
            r#"display-menu -T Menu "Nine" "9" "send-keys -l NINE""#,
        )
        .await;
        let _ = next_overlay_frame(&mut control_rx).await;

        let mut exact_limit = b"\x1b[27;2;65".to_vec();
        exact_limit.resize(FRAME_LIMIT, b'9');
        let mut pending_input = Vec::new();
        handler
            .handle_attached_live_input(requester_pid, &mut pending_input, &exact_limit)
            .await
            .expect("an exact-limit Kitty candidate is retained");
        assert_eq!(pending_input, exact_limit, "exact 4096-byte retention");

        handler
            .handle_attached_live_input(requester_pid, &mut pending_input, b"9")
            .await
            .expect("the first over-limit byte is quarantined");
        assert_eq!(pending_input.len(), FRAME_LIMIT + 1);
        assert_eq!(&pending_input[..FRAME_LIMIT], exact_limit);
        assert_eq!(pending_input[FRAME_LIMIT], b'0', "adapter sentinel");

        let saturated = pending_input.clone();
        handler
            .handle_attached_live_input(
                requester_pid,
                &mut pending_input,
                &vec![b'9'; FRAME_LIMIT * 2],
            )
            .await
            .expect("a saturated continuation remains quarantined");
        assert_eq!(
            pending_input, saturated,
            "continuation must retain only 4096 bytes plus one adapter sentinel"
        );
        assert!(matches!(
            handler.active_attach.lock().await.by_pid[&requester_pid]
                .overlay
                .as_ref(),
            Some(ClientOverlayState::Menu(_))
        ));

        match resolution {
            Resolution::Final => handler
                .handle_attached_live_input(requester_pid, &mut pending_input, b"~")
                .await
                .expect("the final rejects the saturated candidate"),
            Resolution::Interruption => handler
                .handle_attached_live_input(requester_pid, &mut pending_input, b"\x01")
                .await
                .expect("a control byte interrupts the saturated candidate"),
            Resolution::Expiry => {
                handler
                    .flush_attached_pending_escape_input(requester_pid, &mut pending_input)
                    .await
                    .expect("expiry abandons the saturated candidate");
            }
        }
        assert!(pending_input.is_empty(), "{label} clears retained input");
        assert!(matches!(
            handler.active_attach.lock().await.by_pid[&requester_pid]
                .overlay
                .as_ref(),
            Some(ClientOverlayState::Menu(_))
        ));

        handler
            .handle_attached_live_input(requester_pid, &mut pending_input, b"9")
            .await
            .expect("the next menu shortcut remains usable");
        assert!(pending_input.is_empty());
        capture.finish(&handler, &session).await;
        capture.assert_contents(&handler, b"NINE").await;
        assert!(handler.active_attach.lock().await.by_pid[&requester_pid]
            .overlay
            .is_none());
    }
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

#[path = "handler_overlay_tests/popup_burst.rs"]
mod popup_burst;

#[tokio::test]
async fn popup_status_ticks_respect_covered_rows_and_resume_after_close() {
    for (position, lines, popup, blocked) in [
        ("bottom", "on", "-h 100% -y 0", true),
        ("bottom", "on", "-h 99% -y 0", false),
        ("top", "2", "-h 8 -y 0", true),
        ("top", "2", "-h 8 -y 12", false),
    ] {
        let handler = RequestHandler::new();
        let name = session_name("popup-status-coverage");
        let pid = std::process::id();
        let mut rx = create_quiet_attached_session(&handler, &name, pid).await;
        run_overlay_command(&handler, pid, &format!(
            "set -g status {lines}; set -g status-position {position}; set -g status-interval 3600; set -g status-left STATUS-COVERAGE"
        )).await;
        run_overlay_command(
            &handler,
            pid,
            &format!("display-popup -N -B -w 100% -x 0 {popup}"),
        )
        .await;
        let _ = next_overlay_frame(&mut rx).await;
        while rx.try_recv().is_ok() {}
        handler
            .refresh_attached_client_status(pid, &name)
            .await
            .expect("status tick");
        let mut writes = Vec::new();
        while let Ok(control) = rx.try_recv() {
            if let AttachControl::Write(bytes) = control {
                writes.extend(bytes);
            }
        }
        assert_eq!(writes.is_empty(), blocked, "{position} {lines} {popup}");
        run_overlay_command(&handler, pid, "display-popup -C").await;
        while rx.try_recv().is_ok() {}
        handler
            .refresh_attached_client_status(pid, &name)
            .await
            .expect("status after close");
        let mut writes = Vec::new();
        while let Ok(control) = rx.try_recv() {
            if let AttachControl::Write(bytes) = control {
                writes.extend(bytes);
            }
        }
        assert!(!writes.is_empty(), "status resumes after close");
    }
}
