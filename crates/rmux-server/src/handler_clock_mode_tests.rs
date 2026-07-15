use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use super::RequestHandler;
use crate::control::{ControlModeUpgrade, ControlServerEvent, CONTROL_SERVER_EVENT_CAPACITY};
use crate::pane_io::AttachControl;
use rmux_core::{
    input::{mode, InputParser},
    GridRenderOptions, Screen, ScreenCaptureRange,
};
use rmux_proto::request::NewSessionExtRequest;
use rmux_proto::{
    ClockModeRequest, ControlMode, HookLifecycle, HookName, ListPanesRequest, OptionName,
    PaneTarget, Request, Response, ScopeSelector, SessionName, SetHookRequest, SetOptionMode,
    SetOptionRequest, ShowBufferRequest, TerminalSize, WindowTarget,
};
use tokio::sync::{broadcast, mpsc};
use tokio::time::{timeout, Duration};

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

async fn create_session(handler: &RequestHandler, name: &str, size: TerminalSize) -> PaneTarget {
    let session_name = session_name(name);
    let ready_marker = "RCREADY";
    let response = handler
        .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
            session_name: Some(session_name.clone()),
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
            command: Some(quiet_clock_command(ready_marker)),
            process_command: None,
            client_environment: None,
            skip_environment_update: false,
        })))
        .await;
    assert!(matches!(response, Response::NewSession(_)));
    let target = PaneTarget::with_window(session_name, 0, 0);
    wait_for_transcript_containing(
        handler,
        &target,
        ready_marker,
        "quiet clock fixture should reach a stable frame",
    )
    .await;
    replace_transcript_contents(handler, &target, size, b"").await;
    target
}

#[cfg(windows)]
fn quiet_clock_command(marker: &str) -> Vec<String> {
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
        format!("echo {marker} & ping -n 120 127.0.0.1 >NUL"),
    ]
}

#[cfg(unix)]
fn quiet_clock_command(marker: &str) -> Vec<String> {
    vec![
        "/bin/sh".to_owned(),
        "-c".to_owned(),
        format!("printf '{marker}\\n'; sleep 60"),
    ]
}

async fn wait_for_transcript_containing(
    handler: &RequestHandler,
    target: &PaneTarget,
    needle: &str,
    context: &str,
) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let capture = capture_transcript(handler, target).await;
        if capture.contains(needle) {
            return;
        }

        assert!(
            tokio::time::Instant::now() < deadline,
            "{context}, got {capture:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn capture_transcript(handler: &RequestHandler, target: &PaneTarget) -> String {
    let transcript = {
        let state = handler.state.lock().await;
        state
            .transcript_handle(target)
            .expect("pane transcript exists")
    };
    let capture = transcript
        .lock()
        .expect("pane transcript mutex must not be poisoned")
        .capture_main(ScreenCaptureRange::default(), GridRenderOptions::default());
    String::from_utf8_lossy(&capture).into_owned()
}

async fn replace_transcript_contents(
    handler: &RequestHandler,
    target: &PaneTarget,
    size: TerminalSize,
    content: &[u8],
) {
    let transcript = {
        let state = handler.state.lock().await;
        state
            .transcript_handle(target)
            .expect("pane transcript exists")
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

async fn dispatch_as(handler: &RequestHandler, requester_pid: u32, request: Request) -> Response {
    let mut lifecycle_events = handler.subscribe_lifecycle_events();
    let outcome = handler.dispatch(requester_pid, request).await;
    drain_lifecycle_hooks(handler, &mut lifecycle_events).await;
    outcome.response
}

async fn drain_lifecycle_hooks(
    handler: &RequestHandler,
    events: &mut broadcast::Receiver<super::QueuedLifecycleEvent>,
) {
    loop {
        match events.try_recv() {
            Ok(event) => handler.dispatch_lifecycle_hook(event).await,
            Err(broadcast::error::TryRecvError::Empty | broadcast::error::TryRecvError::Closed) => {
                break
            }
            Err(broadcast::error::TryRecvError::Lagged(skipped)) => {
                panic!("lifecycle events lagged during test: {skipped}");
            }
        }
    }
}

async fn register_control_client(
    handler: &RequestHandler,
    requester_pid: u32,
    session_name: SessionName,
) -> mpsc::Receiver<ControlServerEvent> {
    let (event_tx, event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let _control_id = handler
        .register_control_with_closing(
            requester_pid,
            ControlModeUpgrade {
                initial_command_count: 0,
                mode: ControlMode::Plain,
                terminal_context: crate::outer_terminal::OuterTerminalContext::default()
                    .with_client_terminal(&rmux_proto::ClientTerminalContext {
                        terminal_features: Vec::new(),
                        utf8: true,
                    }),
            },
            event_tx,
            Arc::new(AtomicBool::new(false)),
        )
        .await;
    handler
        .set_control_session(requester_pid, Some(session_name))
        .await
        .expect("control session set succeeds");
    event_rx
}

fn drain_control_notifications(rx: &mut mpsc::Receiver<ControlServerEvent>) -> Vec<String> {
    let mut lines = Vec::new();
    loop {
        match rx.try_recv() {
            Ok(ControlServerEvent::Notification(line)) => lines.push(line),
            Ok(
                ControlServerEvent::SessionChanged(_)
                | ControlServerEvent::SessionChangedAt { .. }
                | ControlServerEvent::Refresh,
            ) => {}
            Ok(ControlServerEvent::Exit(reason)) => {
                panic!("unexpected control exit: {reason:?}");
            }
            Err(mpsc::error::TryRecvError::Empty | mpsc::error::TryRecvError::Disconnected) => {
                break;
            }
        }
    }
    lines
}

async fn pane_id(handler: &RequestHandler, target: &PaneTarget) -> u32 {
    let state = handler.state.lock().await;
    state
        .sessions
        .session(target.session_name())
        .and_then(|session| session.window_at(target.window_index()))
        .and_then(|window| window.pane(target.pane_index()))
        .expect("pane exists")
        .id()
        .as_u32()
}

async fn list_panes_text(handler: &RequestHandler, target: &PaneTarget, format: &str) -> String {
    let response = handler
        .handle(Request::ListPanes(Box::new(ListPanesRequest {
            target: target.session_name().clone(),
            format: Some(format.to_owned()),
            filter: None,
            sort_order: None,
            reversed: false,
            target_window_index: None,
        })))
        .await;
    let output = response
        .command_output()
        .expect("list-panes returns command output");
    String::from_utf8_lossy(output.stdout()).into_owned()
}

async fn next_overlay(
    control_rx: &mut mpsc::UnboundedReceiver<AttachControl>,
) -> crate::pane_io::OverlayFrame {
    loop {
        match control_rx.recv().await {
            Some(AttachControl::Overlay(frame)) if frame.frame.is_empty() => {}
            Some(AttachControl::Overlay(frame)) => return frame,
            Some(AttachControl::AdvancePersistentOverlayState(_)) => {}
            Some(AttachControl::Switch(_)) => {}
            Some(AttachControl::Refresh) => {}
            Some(AttachControl::InteractiveInput) => {}
            Some(AttachControl::Detach) => panic!("unexpected detach"),
            Some(AttachControl::Exited) => panic!("unexpected exited"),
            Some(AttachControl::DetachKill) => panic!("unexpected detach kill"),
            Some(AttachControl::DetachExecShellCommand(_)) => panic!("unexpected detach exec"),
            Some(AttachControl::Write(_)) => {}
            Some(AttachControl::LockShellCommand(_)) => {}
            Some(AttachControl::Suspend) => panic!("unexpected suspend"),
            None => panic!("attach control closed"),
        }
    }
}

async fn next_transient_overlay(
    control_rx: &mut mpsc::UnboundedReceiver<AttachControl>,
) -> crate::pane_io::OverlayFrame {
    loop {
        let frame = next_overlay(control_rx).await;
        if !frame.persistent {
            return frame;
        }
    }
}

async fn next_transient_overlay_matching(
    control_rx: &mut mpsc::UnboundedReceiver<AttachControl>,
    description: &str,
    mut matches: impl FnMut(&str) -> bool,
) -> crate::pane_io::OverlayFrame {
    let mut seen = Vec::new();
    let result = timeout(Duration::from_secs(2), async {
        loop {
            let frame = next_transient_overlay(control_rx).await;
            let text = String::from_utf8_lossy(&frame.frame);
            if matches(&text) {
                return frame;
            }
            seen.push(text.into_owned());
        }
    })
    .await;
    match result {
        Ok(frame) => frame,
        Err(_) => panic!("timed out waiting for {description}; seen frames: {seen:?}"),
    }
}

#[tokio::test]
async fn clock_mode_overlay_uses_window_options_for_fallback_rendering() {
    let handler = RequestHandler::new();
    let target = create_session(&handler, "alpha", TerminalSize { cols: 11, rows: 5 }).await;
    let session = target.session_name().clone();
    let requester_pid = std::process::id();
    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, session.clone(), control_tx)
        .await;

    assert!(matches!(
        handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Window(WindowTarget::new(session.clone())),
                option: OptionName::ClockModeColour,
                value: "red".to_owned(),
                mode: SetOptionMode::Replace,
            }))
            .await,
        Response::SetOption(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Window(WindowTarget::new(session.clone())),
                option: OptionName::ClockModeStyle,
                value: "12".to_owned(),
                mode: SetOptionMode::Replace,
            }))
            .await,
        Response::SetOption(_)
    ));

    let response = handler
        .handle(Request::ClockMode(ClockModeRequest {
            target: Some(target.clone()),
        }))
        .await;
    assert_eq!(
        response,
        Response::ClockMode(rmux_proto::ClockModeResponse {
            target: target.clone(),
            active: true,
        })
    );

    let overlay = next_overlay(&mut control_rx).await;
    let frame = String::from_utf8(overlay.frame).expect("overlay is utf-8");
    assert!(overlay.persistent);
    assert!(frame.contains("\u{1b}[?25l"));
    assert!(frame.contains("\u{1b}[31m"));
    assert!(frame.contains("AM") || frame.contains("PM"));
}

#[tokio::test]
async fn refresh_client_replays_active_clock_after_base_switch() {
    let handler = RequestHandler::new();
    let target = create_session(
        &handler,
        "refresh-client-clock",
        TerminalSize { cols: 20, rows: 8 },
    )
    .await;
    let requester_pid = std::process::id();
    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, target.session_name().clone(), control_tx)
        .await;
    assert!(matches!(
        handler
            .handle(Request::ClockMode(ClockModeRequest {
                target: Some(target),
            }))
            .await,
        Response::ClockMode(_)
    ));
    let _ = next_overlay(&mut control_rx).await;
    while control_rx.try_recv().is_ok() {}

    let response = handler
        .handle(Request::RefreshClient(Box::new(
            rmux_proto::request::RefreshClientRequest {
                target_client: None,
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
            },
        )))
        .await;
    assert!(
        matches!(response, Response::RefreshClient(_)),
        "{response:?}"
    );

    let mut saw_switch = false;
    let mut replayed_clock = None;
    while let Ok(control) = control_rx.try_recv() {
        match control {
            AttachControl::Switch(_) => saw_switch = true,
            AttachControl::Overlay(frame) if saw_switch && frame.persistent => {
                replayed_clock = Some(frame);
                break;
            }
            _ => {}
        }
    }
    assert!(saw_switch, "refresh-client must queue the base Switch");
    let frame = replayed_clock.expect("clock overlay must be queued after the base Switch");
    let rendered = String::from_utf8(frame.frame).expect("clock frame is utf-8");
    assert!(rendered.contains("\u{1b}[?25l"));
}

#[tokio::test]
async fn clock_mode_updates_pane_formats_and_exits_on_any_keypress() {
    let handler = RequestHandler::new();
    let target = create_session(&handler, "alpha", TerminalSize { cols: 32, rows: 8 }).await;
    let requester_pid = std::process::id();
    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, target.session_name().clone(), control_tx)
        .await;

    assert!(matches!(
        handler
            .handle(Request::ClockMode(ClockModeRequest {
                target: Some(target.clone()),
            }))
            .await,
        Response::ClockMode(_)
    ));
    let _ = next_overlay(&mut control_rx).await;

    assert_eq!(
        list_panes_text(&handler, &target, "#{pane_in_mode} #{pane_mode}").await,
        "1 clock-mode\n"
    );

    handler
        .handle_attached_live_input_for_test(requester_pid, b"x")
        .await
        .expect("attached input succeeds");

    assert_eq!(
        list_panes_text(&handler, &target, "#{pane_in_mode} #{pane_mode}").await,
        "0 \n"
    );
}

#[tokio::test]
async fn clock_mode_exit_restores_underlying_hidden_cursor_state() {
    let handler = RequestHandler::new();
    let target = create_session(&handler, "alpha", TerminalSize { cols: 32, rows: 8 }).await;
    let requester_pid = std::process::id();
    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, target.session_name().clone(), control_tx)
        .await;

    {
        let state = handler.state.lock().await;
        let transcript = state
            .transcript_handle(&target)
            .expect("pane transcript exists");
        let mut transcript = transcript
            .lock()
            .expect("pane transcript mutex must not be poisoned");
        transcript.append_bytes(b"\x1b[?25l");
    }
    {
        let state = handler.state.lock().await;
        let pane_id = state
            .sessions
            .session(target.session_name())
            .and_then(|session| session.window_at(target.window_index()))
            .and_then(|window| window.pane(target.pane_index()))
            .expect("pane exists")
            .id();
        let screen = state
            .pane_screen_state(target.session_name(), pane_id)
            .expect("pane screen state exists");
        assert_eq!(screen.mode & mode::MODE_CURSOR, 0);
    }

    assert!(matches!(
        handler
            .handle(Request::ClockMode(ClockModeRequest {
                target: Some(target.clone()),
            }))
            .await,
        Response::ClockMode(_)
    ));
    let _ = next_overlay(&mut control_rx).await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"x")
        .await
        .expect("attached input succeeds");

    let restore = next_transient_overlay_matching(
        &mut control_rx,
        "clock mode restore frame with hidden cursor",
        |frame| frame.contains("\u{1b}[?25l") && !frame.contains("\u{1b}[?25h"),
    )
    .await;
    let frame = String::from_utf8(restore.frame).expect("restore frame is utf-8");
    assert!(frame.contains("\u{1b}[?25l"));
    assert!(!frame.contains("\u{1b}[?25h"));
}

#[tokio::test]
async fn clock_mode_exit_restores_visible_line_content() {
    let handler = RequestHandler::new();
    let target = create_session(&handler, "alpha", TerminalSize { cols: 16, rows: 3 }).await;
    let requester_pid = std::process::id();
    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, target.session_name().clone(), control_tx)
        .await;

    {
        let state = handler.state.lock().await;
        let transcript = state
            .transcript_handle(&target)
            .expect("pane transcript exists");
        let mut transcript = transcript
            .lock()
            .expect("pane transcript mutex must not be poisoned");
        transcript.append_bytes(b"\x1b[31mred\r\nmore");
    }

    assert!(matches!(
        handler
            .handle(Request::ClockMode(ClockModeRequest {
                target: Some(target.clone()),
            }))
            .await,
        Response::ClockMode(_)
    ));
    let _ = next_overlay(&mut control_rx).await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"q")
        .await
        .expect("attached input succeeds");

    let restore = next_transient_overlay_matching(
        &mut control_rx,
        "clock mode restore frame with visible line content",
        |frame| frame.contains("red") && frame.contains("more"),
    )
    .await;
    let frame = String::from_utf8(restore.frame).expect("restore frame is utf-8");
    assert!(frame.contains("red"));
    assert!(frame.contains("more"));
}

#[tokio::test]
async fn clock_mode_fires_hooks_and_control_notifications_on_entry_and_exit() {
    let handler = RequestHandler::new();
    let target = create_session(&handler, "alpha", TerminalSize { cols: 24, rows: 7 }).await;
    let requester_pid = std::process::id();
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, target.session_name().clone(), control_tx)
        .await;
    let mut notifications =
        register_control_client(&handler, 700, target.session_name().clone()).await;
    let _ = drain_control_notifications(&mut notifications);

    assert!(matches!(
        handler
            .handle(Request::SetHook(SetHookRequest {
                scope: ScopeSelector::Pane(target.clone()),
                hook: HookName::PaneModeChanged,
                command: "set-buffer -b pane-mode-hook ok".to_owned(),
                lifecycle: HookLifecycle::Persistent,
            }))
            .await,
        Response::SetHook(_)
    ));

    let response = dispatch_as(
        &handler,
        requester_pid,
        Request::ClockMode(ClockModeRequest {
            target: Some(target.clone()),
        }),
    )
    .await;
    assert!(matches!(response, Response::ClockMode(_)));

    let pane_id = pane_id(&handler, &target).await;
    assert_eq!(
        drain_control_notifications(&mut notifications),
        vec![format!("%pane-mode-changed %{pane_id}")]
    );

    let shown = handler
        .handle(Request::ShowBuffer(ShowBufferRequest {
            name: Some("pane-mode-hook".to_owned()),
        }))
        .await;
    let Response::ShowBuffer(buffer) = shown else {
        panic!("expected show-buffer response");
    };
    assert_eq!(buffer.command_output().stdout(), b"ok");

    let mut lifecycle_events = handler.subscribe_lifecycle_events();
    handler
        .handle_attached_live_input_for_test(requester_pid, b"q")
        .await
        .expect("attached input succeeds");
    drain_lifecycle_hooks(&handler, &mut lifecycle_events).await;

    assert_eq!(
        drain_control_notifications(&mut notifications),
        vec![format!("%pane-mode-changed %{pane_id}")]
    );
}
