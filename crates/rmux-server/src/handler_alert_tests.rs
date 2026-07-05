use super::{
    attach_support::{AttachRegistration, ClientFlags},
    scripting_support::format_context_for_target,
    QueuedLifecycleEvent, RequestHandler,
};
use crate::format_runtime::render_runtime_template;
use crate::outer_terminal::OuterTerminalContext;
use crate::pane_io::AttachControl;
use crate::server_access::current_owner_uid;
use rmux_core::{WINLINK_ACTIVITY, WINLINK_BELL, WINLINK_SILENCE};
#[cfg(unix)]
use rmux_proto::SendKeysRequest;
use rmux_proto::{
    DisplayMessageRequest, HookLifecycle, HookName, KillWindowRequest, NewSessionExtRequest,
    NewSessionRequest, NewWindowRequest, NextWindowRequest, OptionName, OptionScopeSelector,
    PaneTarget, PreviousWindowRequest, Request, Response, ScopeSelector, SelectPaneRequest,
    SessionName, SetHookMutationRequest, SetOptionByNameRequest, SetOptionMode, SetOptionRequest,
    ShowMessagesRequest, SplitDirection, SplitWindowExtRequest, SplitWindowTarget, Target,
    TerminalSize, WindowTarget,
};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};
use tokio::time::{timeout, Duration};

#[cfg(windows)]
const ALERT_TEST_EVENT_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(not(windows))]
const ALERT_TEST_EVENT_TIMEOUT: Duration = Duration::from_millis(500);

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

async fn create_session(handler: &RequestHandler, name: &str) -> SessionName {
    let session = session_name(name);
    let response = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session.clone(),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))
        .await;
    assert!(matches!(response, Response::NewSession(_)));
    session
}

async fn set_global_hook(handler: &RequestHandler, hook: HookName, command: &str) {
    let response = handler
        .handle(Request::SetHookMutation(SetHookMutationRequest {
            scope: ScopeSelector::Global,
            hook,
            command: Some(command.to_owned()),
            lifecycle: HookLifecycle::Persistent,
            append: false,
            unset: false,
            run_immediately: false,
            index: None,
        }))
        .await;
    assert!(matches!(response, Response::SetHook(_)), "{response:?}");
}

async fn wait_for_buffer(handler: &RequestHandler, name: &str, expected: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        {
            let state = handler.state.lock().await;
            if let Ok((_, content)) = state.buffers.show(Some(name)) {
                if String::from_utf8_lossy(content) == expected {
                    return;
                }
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "buffer {name} did not reach {expected:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn drain_lifecycle_hooks(
    handler: &RequestHandler,
    events: &mut broadcast::Receiver<QueuedLifecycleEvent>,
) {
    loop {
        match events.try_recv() {
            Ok(event) => handler.dispatch_lifecycle_hook(event).await,
            Err(broadcast::error::TryRecvError::Empty)
            | Err(broadcast::error::TryRecvError::Closed) => break,
            Err(broadcast::error::TryRecvError::Lagged(skipped)) => {
                panic!("lifecycle events lagged during test: {skipped}");
            }
        }
    }
}

async fn create_quiet_session(handler: &RequestHandler, name: &str) -> SessionName {
    let session = session_name(name);
    let response = handler
        .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
            session_name: Some(session.clone()),
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
            command: Some(quiet_alert_command()),
            process_command: None,
            client_environment: None,
            skip_environment_update: false,
        })))
        .await;
    assert!(matches!(response, Response::NewSession(_)));
    session
}

async fn create_window(handler: &RequestHandler, session: &SessionName) -> WindowTarget {
    let response = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: session.clone(),
            name: None,
            detached: true,
            start_directory: None,
            environment: None,
            command: None,
            process_command: None,
            target_window_index: None,
            insert_at_target: false,
        })))
        .await;
    let Response::NewWindow(response) = response else {
        panic!("expected new-window response");
    };
    response.target
}

async fn split_quiet_window(handler: &RequestHandler, session: &SessionName) {
    let response = handler
        .handle(Request::SplitWindowExt(Box::new(SplitWindowExtRequest {
            target: SplitWindowTarget::Session(session.clone()),
            direction: SplitDirection::Vertical,
            before: false,
            environment: None,
            command: Some(quiet_alert_command()),
            process_command: None,
            start_directory: None,
            keep_alive_on_exit: None,
            detached: false,
            size: None,
            preserve_zoom: false,
            full_size: false,
            stdin_payload: None,
        })))
        .await;
    assert!(matches!(response, Response::SplitWindow(_)));
}

#[cfg(unix)]
fn quiet_alert_command() -> Vec<String> {
    ["/bin/sh", "-c", "sleep 60"]
        .into_iter()
        .map(str::to_owned)
        .collect()
}

#[cfg(windows)]
fn quiet_alert_command() -> Vec<String> {
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

async fn display_message(handler: &RequestHandler, target: Target, message: &str) -> String {
    let response = handler
        .handle(Request::DisplayMessage(DisplayMessageRequest {
            target: Some(target),
            print: true,
            message: Some(message.to_owned()),
            empty_target_context: false,
        }))
        .await;
    let Response::DisplayMessage(response) = response else {
        panic!("expected display-message response");
    };
    let output = response
        .command_output()
        .expect("display-message -p returns output");
    String::from_utf8(output.stdout().to_vec())
        .expect("display-message stdout is utf-8")
        .trim_end()
        .to_owned()
}

async fn set_option(
    handler: &RequestHandler,
    scope: ScopeSelector,
    option: OptionName,
    value: &str,
) {
    let response = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope,
            option,
            value: value.to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(response, Response::SetOption(_)));
}

async fn set_server_option_by_name(handler: &RequestHandler, name: &str, value: &str) {
    let response = handler
        .handle(Request::SetOptionByName(Box::new(SetOptionByNameRequest {
            scope: OptionScopeSelector::ServerGlobal,
            name: name.to_owned(),
            value: Some(value.to_owned()),
            mode: SetOptionMode::Replace,
            only_if_unset: false,
            unset: false,
            unset_pane_overrides: false,
            format: false,
            format_target: None,
        })))
        .await;
    assert!(
        matches!(response, Response::SetOptionByName(_)),
        "{response:?}"
    );
}

async fn recv_lifecycle(
    receiver: &mut broadcast::Receiver<QueuedLifecycleEvent>,
) -> QueuedLifecycleEvent {
    timeout(ALERT_TEST_EVENT_TIMEOUT, receiver.recv())
        .await
        .expect("lifecycle event should arrive")
        .expect("lifecycle channel should stay open")
}

async fn recv_lifecycle_hook(
    receiver: &mut broadcast::Receiver<QueuedLifecycleEvent>,
    expected: HookName,
) -> QueuedLifecycleEvent {
    recv_lifecycle_hook_with_timeout(receiver, expected, ALERT_TEST_EVENT_TIMEOUT).await
}

async fn recv_lifecycle_hook_with_timeout(
    receiver: &mut broadcast::Receiver<QueuedLifecycleEvent>,
    expected: HookName,
    wait_for: Duration,
) -> QueuedLifecycleEvent {
    let deadline = tokio::time::Instant::now() + wait_for;
    let mut ignored = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "timed out waiting for lifecycle hook {expected:?}; ignored {ignored:?}"
        );
        match timeout(remaining, receiver.recv()).await {
            Err(_) => {
                panic!("timed out waiting for lifecycle hook {expected:?}; ignored {ignored:?}")
            }
            Ok(Err(error)) => {
                panic!("lifecycle channel closed while waiting for {expected:?}: {error:?}")
            }
            Ok(Ok(event)) if event.hook_name == expected => return event,
            Ok(Ok(event)) if is_lifecycle_noise(event.hook_name) => ignored.push(event.hook_name),
            Ok(Ok(event)) => panic!(
                "expected lifecycle hook {expected:?}, got {:?}; ignored {ignored:?}",
                event.hook_name
            ),
        }
    }
}

async fn recv_lifecycle_hooks(
    receiver: &mut broadcast::Receiver<QueuedLifecycleEvent>,
    expected: &[HookName],
) -> Vec<QueuedLifecycleEvent> {
    let deadline = tokio::time::Instant::now() + ALERT_TEST_EVENT_TIMEOUT;
    let mut pending = expected.to_vec();
    let mut received = Vec::new();
    let mut ignored = Vec::new();
    while !pending.is_empty() {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "timed out waiting for lifecycle hooks {pending:?}; received {:?}; ignored {ignored:?}",
            received
                .iter()
                .map(|event: &QueuedLifecycleEvent| event.hook_name)
                .collect::<Vec<_>>()
        );
        match timeout(remaining, receiver.recv()).await {
            Err(_) => panic!(
                "timed out waiting for lifecycle hooks {pending:?}; received {:?}; ignored {ignored:?}",
                received
                    .iter()
                    .map(|event: &QueuedLifecycleEvent| event.hook_name)
                    .collect::<Vec<_>>()
            ),
            Ok(Err(error)) => panic!("lifecycle channel closed while waiting for {pending:?}: {error:?}"),
            Ok(Ok(event)) => {
                if let Some(index) = pending
                    .iter()
                    .position(|hook_name| *hook_name == event.hook_name)
                {
                    pending.remove(index);
                    received.push(event);
                } else if is_lifecycle_noise(event.hook_name) {
                    ignored.push(event.hook_name);
                } else {
                    panic!(
                        "unexpected lifecycle hook {:?}; still waiting for {pending:?}; ignored {ignored:?}",
                        event.hook_name
                    );
                }
            }
        }
    }
    received
}

async fn assert_no_lifecycle_hooks(
    receiver: &mut broadcast::Receiver<QueuedLifecycleEvent>,
    forbidden: &[HookName],
    wait_for: Duration,
    message: &str,
) {
    let deadline = tokio::time::Instant::now() + wait_for;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return;
        }
        match timeout(remaining, receiver.recv()).await {
            Err(_) | Ok(Err(_)) => return,
            Ok(Ok(event)) => {
                assert!(
                    !forbidden.contains(&event.hook_name),
                    "{message}: unexpected lifecycle hook {:?}",
                    event.hook_name
                );
            }
        }
    }
}

fn is_lifecycle_noise(hook_name: HookName) -> bool {
    matches!(hook_name, HookName::PaneTitleChanged)
}

async fn recv_attach_control(
    receiver: &mut mpsc::UnboundedReceiver<AttachControl>,
) -> AttachControl {
    timeout(ALERT_TEST_EVENT_TIMEOUT, receiver.recv())
        .await
        .expect("attach control should arrive")
        .expect("attach control channel should stay open")
}

async fn recv_non_switch_control(
    receiver: &mut mpsc::UnboundedReceiver<AttachControl>,
) -> AttachControl {
    loop {
        match recv_attach_control(receiver).await {
            AttachControl::Switch(_) => {}
            other => return other,
        }
    }
}

async fn assert_no_non_switch_control(receiver: &mut mpsc::UnboundedReceiver<AttachControl>) {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(50);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return;
        }
        match timeout(remaining, receiver.recv()).await {
            Err(_) | Ok(None) => return,
            Ok(Some(AttachControl::Switch(_))) => {}
            Ok(Some(other)) => panic!("unexpected attach control: {other:?}"),
        }
    }
}

fn is_visual_bell_overlay(frame: &[u8]) -> bool {
    String::from_utf8_lossy(frame).contains("Bell in ")
}

async fn recv_visual_bell_overlay(receiver: &mut mpsc::UnboundedReceiver<AttachControl>) {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(500);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "visual bell overlay did not arrive before timeout"
        );
        match timeout(remaining, receiver.recv()).await {
            Err(_) | Ok(None) => panic!("visual bell overlay did not arrive before timeout"),
            Ok(Some(AttachControl::Switch(_))) => {}
            Ok(Some(AttachControl::Overlay(frame))) if is_visual_bell_overlay(&frame.frame) => {
                return;
            }
            Ok(Some(AttachControl::Overlay(_))) => {}
            Ok(Some(other)) => panic!("expected visual bell overlay, got {other:?}"),
        }
    }
}

async fn recv_visual_bell_write_and_overlay(receiver: &mut mpsc::UnboundedReceiver<AttachControl>) {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(500);
    let mut saw_write = false;
    let mut saw_overlay = false;
    loop {
        if saw_write && saw_overlay {
            return;
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "visual bell write+overlay did not arrive before timeout"
        );
        match timeout(remaining, receiver.recv()).await {
            Err(_) | Ok(None) => panic!("visual bell write+overlay did not arrive before timeout"),
            Ok(Some(AttachControl::Switch(_))) => {}
            Ok(Some(AttachControl::Write(_))) => saw_write = true,
            Ok(Some(AttachControl::Overlay(frame))) if is_visual_bell_overlay(&frame.frame) => {
                saw_overlay = true;
            }
            Ok(Some(AttachControl::Overlay(_))) => {}
            Ok(Some(other)) => panic!("expected visual bell write or overlay, got {other:?}"),
        }
    }
}

async fn recv_visual_bell_delivery(receiver: &mut mpsc::UnboundedReceiver<AttachControl>) {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(500);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "visual bell delivery did not arrive before timeout"
        );
        match timeout(remaining, receiver.recv()).await {
            Err(_) | Ok(None) => panic!("visual bell delivery did not arrive before timeout"),
            Ok(Some(AttachControl::Switch(_))) => {}
            Ok(Some(AttachControl::Write(_))) => return,
            Ok(Some(AttachControl::Overlay(frame))) if is_visual_bell_overlay(&frame.frame) => {
                return;
            }
            Ok(Some(AttachControl::Overlay(_))) => {}
            Ok(Some(other)) => panic!("expected visual bell delivery, got {other:?}"),
        }
    }
}

async fn assert_no_visual_bell_delivery(receiver: &mut mpsc::UnboundedReceiver<AttachControl>) {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(50);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return;
        }
        match timeout(remaining, receiver.recv()).await {
            Err(_) | Ok(None) => return,
            Ok(Some(AttachControl::Switch(_))) => {}
            Ok(Some(AttachControl::Overlay(frame))) if is_visual_bell_overlay(&frame.frame) => {
                panic!("unexpected visual bell overlay: {frame:?}");
            }
            Ok(Some(AttachControl::Overlay(_))) => {}
            Ok(Some(AttachControl::Write(bytes))) => {
                panic!("unexpected visual bell write: {bytes:?}");
            }
            Ok(Some(other)) => panic!("unexpected attach control: {other:?}"),
        }
    }
}

async fn drain_attach_controls_until_idle(receiver: &mut mpsc::UnboundedReceiver<AttachControl>) {
    loop {
        match timeout(Duration::from_millis(20), receiver.recv()).await {
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => return,
        }
    }
}

async fn drain_attach_controls_until_quiet(
    receiver: &mut mpsc::UnboundedReceiver<AttachControl>,
    quiet_for: Duration,
    timeout_after: Duration,
) {
    let deadline = tokio::time::Instant::now() + timeout_after;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return;
        }
        match timeout(quiet_for.min(remaining), receiver.recv()).await {
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => return,
        }
    }
}

#[tokio::test]
async fn pane_alert_event_sets_bell_and_activity_flags_and_emits_alert_hooks() {
    let handler = RequestHandler::new();
    let session = create_session(&handler, "alerts").await;
    let window = create_window(&handler, &session).await;
    set_option(
        &handler,
        ScopeSelector::Window(window.clone()),
        OptionName::MonitorActivity,
        "on",
    )
    .await;

    let pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&session)
            .and_then(|session| session.window_at(window.window_index()))
            .and_then(|window| window.pane(0))
            .expect("window pane exists")
            .id()
    };
    let mut lifecycle = handler.subscribe_lifecycle_events();

    handler.pane_alert_callback()(crate::pane_io::PaneAlertEvent {
        session_name: session.clone(),
        pane_id,
        bell_count: 1,
        title_changed: false,
        clipboard_set: false,
        queue_activity_alert: true,
        generation: None,
    });

    let events = recv_lifecycle_hooks(
        &mut lifecycle,
        &[HookName::AlertBell, HookName::AlertActivity],
    )
    .await;
    let hook_names = events
        .iter()
        .map(|event| event.hook_name)
        .collect::<Vec<_>>();
    assert!(hook_names.contains(&HookName::AlertBell));
    assert!(hook_names.contains(&HookName::AlertActivity));

    let state = handler.state.lock().await;
    let session = state.sessions.session(&session).expect("session exists");
    let flags = session.winlink_alert_flags(window.window_index());
    assert!(flags.contains(WINLINK_BELL));
    assert!(flags.contains(WINLINK_ACTIVITY));
}

#[tokio::test]
async fn pane_alert_callback_can_be_invoked_from_reader_thread() {
    let handler = RequestHandler::new();
    let session = create_session(&handler, "alerts-reader-thread").await;
    set_option(
        &handler,
        ScopeSelector::Window(WindowTarget::with_window(session.clone(), 0)),
        OptionName::MonitorActivity,
        "on",
    )
    .await;
    set_option(
        &handler,
        ScopeSelector::Session(session.clone()),
        OptionName::ActivityAction,
        "any",
    )
    .await;
    let pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&session)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0).map(|pane| pane.id()))
            .expect("window pane exists")
    };
    let mut lifecycle = handler.subscribe_lifecycle_events();
    let callback = handler.pane_alert_callback();

    std::thread::spawn(move || {
        callback(crate::pane_io::PaneAlertEvent {
            session_name: session,
            pane_id,
            bell_count: 0,
            title_changed: false,
            clipboard_set: false,
            queue_activity_alert: true,
            generation: None,
        });
    })
    .join()
    .expect("reader-thread alert callback should not panic outside the Tokio runtime");

    let event = recv_lifecycle(&mut lifecycle).await;
    assert_eq!(event.hook_name, HookName::AlertActivity);
}

#[tokio::test]
async fn pane_title_change_output_emits_lifecycle_hook_event() {
    let handler = RequestHandler::new();
    let session = create_quiet_session(&handler, "pane-title-hook").await;
    let pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&session)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0).map(|pane| pane.id()))
            .expect("window pane exists")
    };
    let mut lifecycle = handler.subscribe_lifecycle_events();

    handler.pane_alert_callback()(crate::pane_io::PaneAlertEvent {
        session_name: session,
        pane_id,
        bell_count: 0,
        title_changed: true,
        clipboard_set: false,
        queue_activity_alert: false,
        generation: None,
    });

    let event = recv_lifecycle(&mut lifecycle).await;
    assert_eq!(event.hook_name, HookName::PaneTitleChanged);
}

#[tokio::test]
async fn pane_set_clipboard_output_emits_hook_without_activity() {
    let handler = RequestHandler::new();
    let session = create_quiet_session(&handler, "pane-clipboard-hook").await;
    set_server_option_by_name(&handler, "set-clipboard", "on").await;
    set_global_hook(
        &handler,
        HookName::PaneSetClipboard,
        "set-buffer -a -b clipboard-hook clip,",
    )
    .await;
    let mut lifecycle = handler.subscribe_lifecycle_events();
    let pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&session)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0).map(|pane| pane.id()))
            .expect("window pane exists")
    };

    handler
        .handle_pane_alert_event(crate::pane_io::PaneAlertEvent {
            session_name: session,
            pane_id,
            bell_count: 0,
            title_changed: false,
            clipboard_set: true,
            queue_activity_alert: false,
            generation: None,
        })
        .await;

    drain_lifecycle_hooks(&handler, &mut lifecycle).await;
    wait_for_buffer(&handler, "clipboard-hook", "clip,").await;
}

#[tokio::test]
async fn pane_set_clipboard_hook_requires_set_clipboard_on() {
    let handler = RequestHandler::new();
    let session = create_quiet_session(&handler, "pane-clipboard-external").await;
    set_global_hook(
        &handler,
        HookName::PaneSetClipboard,
        "set-buffer -a -b clipboard-hook clip,",
    )
    .await;
    let mut lifecycle = handler.subscribe_lifecycle_events();
    let pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&session)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0).map(|pane| pane.id()))
            .expect("window pane exists")
    };

    handler
        .handle_pane_alert_event(crate::pane_io::PaneAlertEvent {
            session_name: session,
            pane_id,
            bell_count: 0,
            title_changed: false,
            clipboard_set: true,
            queue_activity_alert: false,
            generation: None,
        })
        .await;

    drain_lifecycle_hooks(&handler, &mut lifecycle).await;
    let state = handler.state.lock().await;
    assert!(state.buffers.show(Some("clipboard-hook")).is_err());
}

#[tokio::test]
async fn select_pane_does_not_synthesize_focus_lifecycle_hooks() {
    let handler = RequestHandler::new();
    let session = create_quiet_session(&handler, "pane-focus-hooks").await;
    split_quiet_window(&handler, &session).await;
    assert!(matches!(
        handler
            .handle(Request::SelectPane(Box::new(SelectPaneRequest {
                target: PaneTarget::with_window(session.clone(), 0, 0),
                preserve_zoom: false,
                title: None,
                style: None,
                input_disabled: None,
            })))
            .await,
        Response::SelectPane(_)
    ));

    let mut lifecycle = handler.subscribe_lifecycle_events();
    assert!(matches!(
        handler
            .handle(Request::SelectPane(Box::new(SelectPaneRequest {
                target: PaneTarget::with_window(session, 0, 1),
                preserve_zoom: false,
                title: None,
                style: None,
                input_disabled: None,
            })))
            .await,
        Response::SelectPane(_)
    ));

    recv_lifecycle_hook(&mut lifecycle, HookName::WindowPaneChanged).await;
    assert_no_lifecycle_hooks(
        &mut lifecycle,
        &[HookName::PaneFocusIn, HookName::PaneFocusOut],
        Duration::from_millis(100),
        "select-pane must not synthesize pane-focus-in/out hooks",
    )
    .await;
}

#[tokio::test]
async fn pane_alert_callback_coalesces_inactive_pane_refreshes_by_session() {
    let handler = RequestHandler::new();
    let session = create_quiet_session(&handler, "inactive-output-refresh").await;
    set_option(
        &handler,
        ScopeSelector::Window(WindowTarget::with_window(session.clone(), 0)),
        OptionName::AutomaticRename,
        "off",
    )
    .await;
    split_quiet_window(&handler, &session).await;
    split_quiet_window(&handler, &session).await;

    let inactive_panes = {
        let state = handler.state.lock().await;
        let session = state.sessions.session(&session).expect("session exists");
        let active_pane_id = session.active_pane_id();
        session
            .window_at(0)
            .expect("window exists")
            .panes()
            .iter()
            .filter_map(|pane| (Some(pane.id()) != active_pane_id).then_some(pane.id()))
            .take(2)
            .collect::<Vec<_>>()
    };
    assert_eq!(inactive_panes.len(), 2);

    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let control_backlog = Arc::new(AtomicUsize::new(0));
    let uid = current_owner_uid();
    handler
        .register_attach_with_access(
            77,
            session.clone(),
            AttachRegistration {
                control_tx,
                control_backlog: Arc::clone(&control_backlog),
                closing: Arc::new(AtomicBool::new(false)),
                persistent_overlay_epoch: Arc::new(AtomicU64::new(0)),
                terminal_context: OuterTerminalContext::default(),
                flags: ClientFlags::default(),
                render_stream: false,
                uid,
                user: rmux_os::identity::UserIdentity::Uid(uid),
                can_write: true,
                client_size: Some(TerminalSize { cols: 80, rows: 24 }),
            },
        )
        .await;
    drain_attach_controls_until_quiet(
        &mut control_rx,
        Duration::from_millis(150),
        Duration::from_secs(2),
    )
    .await;
    let baseline_backlog = control_backlog.load(Ordering::Acquire);

    let first_callback = handler.pane_alert_callback();
    let second_callback = handler.pane_alert_callback();
    for (callback, pane_id) in [
        (first_callback.as_ref(), inactive_panes[0]),
        (second_callback.as_ref(), inactive_panes[1]),
    ] {
        callback(crate::pane_io::PaneAlertEvent {
            session_name: session.clone(),
            pane_id,
            bell_count: 0,
            title_changed: false,
            clipboard_set: false,
            queue_activity_alert: false,
            generation: None,
        });
    }

    let first = timeout(Duration::from_secs(2), control_rx.recv())
        .await
        .expect("inactive pane output should enqueue one refresh")
        .expect("attach control channel is open");
    assert!(matches!(first, AttachControl::Switch(_)));

    tokio::time::sleep(Duration::from_millis(150)).await;
    let mut extra_switches = 0;
    while let Ok(control) = control_rx.try_recv() {
        if matches!(control, AttachControl::Switch(_)) {
            extra_switches += 1;
        }
    }

    assert_eq!(
        extra_switches, 0,
        "inactive pane output from one coalesced reader batch should repaint each attached session once"
    );
    assert_eq!(
        control_backlog.load(Ordering::Acquire),
        baseline_backlog + 1
    );
}

#[tokio::test]
async fn pane_exit_callback_can_be_invoked_from_reader_thread() {
    let handler = RequestHandler::new();
    let session = create_session(&handler, "exit-reader-thread").await;
    let pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&session)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0).map(|pane| pane.id()))
            .expect("window pane exists")
    };
    let callback = handler.pane_exit_callback();

    std::thread::spawn(move || {
        callback(crate::pane_io::PaneExitEvent::eof_published(
            session, pane_id, None,
        ));
    })
    .join()
    .expect("reader-thread exit callback should not panic outside the Tokio runtime");
}

#[tokio::test]
async fn pane_alert_event_updates_automatic_window_name_without_disabling_auto_rename() {
    let handler = RequestHandler::new();
    let session = create_session(&handler, "alerts-name").await;
    set_option(
        &handler,
        ScopeSelector::Window(WindowTarget::with_window(session.clone(), 0)),
        OptionName::AutomaticRenameFormat,
        "updated-name",
    )
    .await;
    let pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&session)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0).map(|pane| pane.id()))
            .expect("window pane exists")
    };

    handler.pane_alert_callback()(crate::pane_io::PaneAlertEvent {
        session_name: session.clone(),
        pane_id,
        bell_count: 0,
        title_changed: false,
        clipboard_set: false,
        queue_activity_alert: true,
        generation: None,
    });

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        {
            let state = handler.state.lock().await;
            let window = state
                .sessions
                .session(&session)
                .and_then(|session| session.window_at(0))
                .expect("window exists");
            if window.name() == Some("updated-name") && state.tracks_auto_named_window(&session, 0)
            {
                break;
            }
        }

        assert!(
            tokio::time::Instant::now() < deadline,
            "automatic window name was not updated before timeout"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn pane_alert_event_respects_automatic_rename_off() {
    let handler = RequestHandler::new();
    let session = create_session(&handler, "alerts-name-off").await;
    let target = WindowTarget::with_window(session.clone(), 0);
    set_option(
        &handler,
        ScopeSelector::Window(target.clone()),
        OptionName::AutomaticRenameFormat,
        "updated-name",
    )
    .await;
    set_option(
        &handler,
        ScopeSelector::Window(target),
        OptionName::AutomaticRename,
        "off",
    )
    .await;
    let pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&session)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0).map(|pane| pane.id()))
            .expect("window pane exists")
    };

    handler
        .handle_pane_alert_event(crate::pane_io::PaneAlertEvent {
            session_name: session.clone(),
            pane_id,
            bell_count: 0,
            title_changed: false,
            clipboard_set: false,
            queue_activity_alert: true,
            generation: None,
        })
        .await;

    let state = handler.state.lock().await;
    let window = state
        .sessions
        .session(&session)
        .and_then(|session| session.window_at(0))
        .expect("window exists");
    assert_ne!(window.name(), Some("updated-name"));
}

#[tokio::test]
async fn pane_alert_event_updates_grouped_session_window_names() {
    let handler = RequestHandler::new();
    let alpha = create_session(&handler, "alerts-group-alpha").await;
    let beta = session_name("alerts-group-beta");
    let response = handler
        .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
            session_name: Some(beta.clone()),
            working_directory: None,
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
            group_target: Some(alpha.clone()),
            attach_if_exists: false,
            detach_other_clients: false,
            kill_other_clients: false,
            flags: None,
            window_name: None,
            print_session_info: false,
            print_format: None,
            command: None,
            process_command: None,
            client_environment: None,
            skip_environment_update: false,
        })))
        .await;
    assert!(matches!(response, Response::NewSession(_)));
    set_option(
        &handler,
        ScopeSelector::Window(WindowTarget::with_window(alpha.clone(), 0)),
        OptionName::AutomaticRenameFormat,
        "updated-name",
    )
    .await;

    let pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&alpha)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0).map(|pane| pane.id()))
            .expect("window pane exists")
    };

    handler
        .handle_pane_alert_event(crate::pane_io::PaneAlertEvent {
            session_name: alpha.clone(),
            pane_id,
            bell_count: 0,
            title_changed: false,
            clipboard_set: false,
            queue_activity_alert: true,
            generation: None,
        })
        .await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        {
            let state = handler.state.lock().await;
            let alpha_name = state
                .sessions
                .session(&alpha)
                .and_then(|session| session.window_at(0))
                .and_then(|window| window.name())
                .map(str::to_owned);
            let beta_name = state
                .sessions
                .session(&beta)
                .and_then(|session| session.window_at(0))
                .and_then(|window| window.name())
                .map(str::to_owned);
            if alpha_name.as_deref() == Some("updated-name")
                && beta_name.as_deref() == Some("updated-name")
            {
                break;
            }
        }

        assert!(
            tokio::time::Instant::now() < deadline,
            "grouped sessions did not share the automatic window name before timeout"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[cfg(unix)]
#[tokio::test]
async fn shell_input_updates_window_name_and_foreground_process_formats() {
    let handler = RequestHandler::new();
    let session = create_session(&handler, "alerts-foreground").await;
    let target = PaneTarget::with_window(session.clone(), 0, 0);
    let expected_path = std::fs::canonicalize("/tmp")
        .unwrap_or_else(|_| std::path::PathBuf::from("/tmp"))
        .to_string_lossy()
        .into_owned();
    let expected = format!("sleep|{expected_path}|sleep");

    let response = handler
        .handle(Request::SendKeys(SendKeysRequest {
            target: target.clone(),
            keys: vec!["cd /tmp && exec sleep 120".to_owned(), "Enter".to_owned()],
        }))
        .await;
    assert!(matches!(response, Response::SendKeys(_)));

    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let rendered = display_message(
            &handler,
            Target::Pane(target.clone()),
            "#{window_name}|#{pane_current_path}|#{pane_current_command}",
        )
        .await;
        if rendered == expected {
            break;
        }

        assert!(
            tokio::time::Instant::now() < deadline,
            "foreground formats did not update before timeout; last={rendered:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn visual_bell_modes_dispatch_overlay_write_and_action_gating() {
    let handler = RequestHandler::new();
    let session = create_session(&handler, "visual").await;
    let other_window = create_window(&handler, &session).await;
    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(42, session.clone(), control_tx)
        .await;
    drain_attach_controls_until_idle(&mut control_rx).await;
    let current_window = WindowTarget::new(session.clone());

    set_option(
        &handler,
        ScopeSelector::Session(session.clone()),
        OptionName::VisualBell,
        "off",
    )
    .await;
    handler
        .alerts_queue_window(current_window.clone(), rmux_core::WINDOW_BELL)
        .await;
    match recv_non_switch_control(&mut control_rx).await {
        AttachControl::Write(bytes) => assert_eq!(bytes, vec![0x07]),
        other => panic!("expected bell write, got {other:?}"),
    }
    assert_no_visual_bell_delivery(&mut control_rx).await;

    set_option(
        &handler,
        ScopeSelector::Session(session.clone()),
        OptionName::VisualBell,
        "on",
    )
    .await;
    handler
        .alerts_queue_window(current_window.clone(), rmux_core::WINDOW_BELL)
        .await;
    recv_visual_bell_overlay(&mut control_rx).await;
    assert_no_visual_bell_delivery(&mut control_rx).await;

    set_option(
        &handler,
        ScopeSelector::Session(session.clone()),
        OptionName::VisualBell,
        "both",
    )
    .await;
    handler
        .alerts_queue_window(current_window, rmux_core::WINDOW_BELL)
        .await;
    recv_visual_bell_write_and_overlay(&mut control_rx).await;

    set_option(
        &handler,
        ScopeSelector::Session(session.clone()),
        OptionName::BellAction,
        "other",
    )
    .await;
    handler
        .alerts_queue_window(WindowTarget::new(session.clone()), rmux_core::WINDOW_BELL)
        .await;
    assert_no_visual_bell_delivery(&mut control_rx).await;

    handler
        .alerts_queue_window(other_window.clone(), rmux_core::WINDOW_BELL)
        .await;
    recv_visual_bell_delivery(&mut control_rx).await;
    let state = handler.state.lock().await;
    let flags = state
        .sessions
        .session(&session)
        .expect("session exists")
        .winlink_alert_flags(other_window.window_index());
    assert!(flags.contains(WINLINK_BELL));
}

#[tokio::test]
async fn silence_monitor_sets_flags_after_idle() {
    let handler = RequestHandler::new();
    let session = create_session(&handler, "silence").await;
    let window = create_window(&handler, &session).await;
    set_option(
        &handler,
        ScopeSelector::Window(window.clone()),
        OptionName::MonitorSilence,
        "1",
    )
    .await;

    let mut lifecycle = handler.subscribe_lifecycle_events();
    recv_lifecycle_hook_with_timeout(
        &mut lifecycle,
        HookName::AlertSilence,
        Duration::from_secs(4),
    )
    .await;

    let state = handler.state.lock().await;
    let flags = state
        .sessions
        .session(&session)
        .expect("session exists")
        .winlink_alert_flags(window.window_index());
    assert!(flags.contains(WINLINK_SILENCE));
}

#[tokio::test]
async fn show_messages_formats_log_and_terminal_info_and_prunes_to_limit() {
    let handler = RequestHandler::new();
    let session = create_session(&handler, "messages").await;
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(77, session.clone(), control_tx)
        .await;

    {
        let mut state = handler.state.lock().await;
        state.add_message("one");
        state.add_message("two");
    }
    set_option(
        &handler,
        ScopeSelector::Global,
        OptionName::MessageLimit,
        "1",
    )
    .await;

    let response = handler
        .handle(Request::ShowMessages(ShowMessagesRequest {
            jobs: false,
            terminals: false,
            target_client: None,
        }))
        .await;
    let Response::ShowMessages(response) = response else {
        panic!("expected show-messages response");
    };
    let rendered = String::from_utf8_lossy(response.output.stdout()).into_owned();
    assert!(rendered.contains(": two"));
    assert!(!rendered.contains(": one"));

    let response = handler
        .handle(Request::ShowMessages(ShowMessagesRequest {
            jobs: false,
            terminals: true,
            target_client: Some("77".to_owned()),
        }))
        .await;
    let Response::ShowMessages(response) = response else {
        panic!("expected show-messages response");
    };
    let rendered = String::from_utf8_lossy(response.output.stdout()).into_owned();
    assert!(rendered.contains("Terminal 0:"));
    assert!(rendered.contains("client 77"));
    assert!(!rendered.contains(": two"));

    let response = handler
        .handle(Request::ShowMessages(ShowMessagesRequest {
            jobs: true,
            terminals: false,
            target_client: Some("77".to_owned()),
        }))
        .await;
    let Response::ShowMessages(response) = response else {
        panic!("expected show-messages response");
    };
    assert!(response.output.stdout().is_empty());

    set_option(
        &handler,
        ScopeSelector::Global,
        OptionName::MessageLimit,
        "0",
    )
    .await;
    let state = handler.state.lock().await;
    assert!(state.message_log.is_empty());
}

#[tokio::test]
async fn show_messages_log_is_available_without_current_client() {
    let handler = RequestHandler::new();
    let _session = create_session(&handler, "messages-detached").await;
    {
        let mut state = handler.state.lock().await;
        state.add_message("detached log entry");
    }

    let response = handler
        .handle(Request::ShowMessages(ShowMessagesRequest {
            jobs: false,
            terminals: false,
            target_client: None,
        }))
        .await;
    let Response::ShowMessages(response) = response else {
        panic!("expected detached show-messages log output");
    };
    let rendered = String::from_utf8_lossy(response.output.stdout());
    assert!(rendered.contains("detached log entry"));

    for (jobs, terminals) in [(true, false), (false, true), (true, true)] {
        let response = handler
            .handle(Request::ShowMessages(ShowMessagesRequest {
                jobs,
                terminals,
                target_client: None,
            }))
            .await;
        let Response::ShowMessages(response) = response else {
            panic!("expected detached show-messages -J/-T to succeed with empty output");
        };
        assert!(response.output.stdout().is_empty());
    }
}

#[tokio::test]
async fn format_variables_focus_clearing_and_alert_navigation_follow_winlink_flags() {
    let handler = RequestHandler::new();
    let session = create_session(&handler, "formats").await;
    let window_one = create_window(&handler, &session).await;
    let window_two = create_window(&handler, &session).await;

    {
        let mut state = handler.state.lock().await;
        let session = state
            .sessions
            .session_mut(&session)
            .expect("session exists");
        let combined = WINLINK_ACTIVITY.union(WINLINK_BELL).union(WINLINK_SILENCE);
        assert!(session.add_winlink_alert_flags(window_one.window_index(), combined));
        assert!(session.add_winlink_alert_flags(window_two.window_index(), WINLINK_BELL));
        assert!(session.add_winlink_alert_flags(0, WINLINK_ACTIVITY));
    }

    let rendered = {
        let state = handler.state.lock().await;
        let session_context =
            format_context_for_target(&state, &Target::Session(session.clone()), 0).unwrap();
        let window_context =
            format_context_for_target(&state, &Target::Window(window_one.clone()), 0).unwrap();
        (
            render_runtime_template(
                "#{session_alerts}|#{session_activity_flag}|#{session_bell_flag}|#{session_silence_flag}",
                &session_context,
                false,
            ),
            render_runtime_template(
                "#{window_activity_flag}|#{window_bell_flag}|#{window_silence_flag}",
                &window_context,
                false,
            ),
        )
    };
    assert_eq!(rendered.0, "0#,1#!~,2!|1|1|1");
    assert_eq!(rendered.1, "1|1|1");

    let next = handler
        .handle(Request::NextWindow(NextWindowRequest {
            target: session.clone(),
            alerts_only: true,
        }))
        .await;
    assert_eq!(
        next,
        Response::NextWindow(rmux_proto::NextWindowResponse {
            target: window_one.clone(),
        })
    );
    {
        let state = handler.state.lock().await;
        let session = state.sessions.session(&session).expect("session exists");
        assert!(session
            .winlink_alert_flags(window_one.window_index())
            .is_empty());
    }

    let previous = handler
        .handle(Request::PreviousWindow(PreviousWindowRequest {
            target: session.clone(),
            alerts_only: true,
        }))
        .await;
    assert_eq!(
        previous,
        Response::PreviousWindow(rmux_proto::PreviousWindowResponse {
            target: WindowTarget::new(session.clone()),
        })
    );

    let wrapped_previous = handler
        .handle(Request::PreviousWindow(PreviousWindowRequest {
            target: session.clone(),
            alerts_only: true,
        }))
        .await;
    assert_eq!(
        wrapped_previous,
        Response::PreviousWindow(rmux_proto::PreviousWindowResponse { target: window_two })
    );
}

#[tokio::test]
async fn activity_deduplication_skips_second_alert_on_same_winlink() {
    let handler = RequestHandler::new();
    let session = create_session(&handler, "dedup").await;
    let window = create_window(&handler, &session).await;
    set_option(
        &handler,
        ScopeSelector::Window(window.clone()),
        OptionName::MonitorActivity,
        "on",
    )
    .await;

    let mut lifecycle = handler.subscribe_lifecycle_events();

    // First activity fires the hook.
    handler
        .alerts_queue_window(window.clone(), rmux_core::WINDOW_ACTIVITY)
        .await;
    recv_lifecycle_hook(&mut lifecycle, HookName::AlertActivity).await;

    // Second activity on the same winlink is suppressed (flag already set).
    handler
        .alerts_queue_window(window.clone(), rmux_core::WINDOW_ACTIVITY)
        .await;
    assert_no_lifecycle_hooks(
        &mut lifecycle,
        &[HookName::AlertActivity],
        Duration::from_millis(100),
        "duplicate activity alert should not fire",
    )
    .await;

    // Bell on the same winlink still fires (bells are never deduplicated).
    handler
        .alerts_queue_window(window.clone(), rmux_core::WINDOW_BELL)
        .await;
    recv_lifecycle_hook(&mut lifecycle, HookName::AlertBell).await;
}

#[tokio::test]
async fn action_none_blocks_all_delivery() {
    let handler = RequestHandler::new();
    let session = create_session(&handler, "none-action").await;
    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(55, session.clone(), control_tx)
        .await;
    drain_attach_controls_until_idle(&mut control_rx).await;

    set_option(
        &handler,
        ScopeSelector::Session(session.clone()),
        OptionName::BellAction,
        "none",
    )
    .await;

    handler
        .alerts_queue_window(WindowTarget::new(session.clone()), rmux_core::WINDOW_BELL)
        .await;
    assert_no_non_switch_control(&mut control_rx).await;

    // Winlink flags are not set on the current window when clients are attached
    // (tmux clears flags on the current window on every client activity check).
    let state = handler.state.lock().await;
    let session_obj = state.sessions.session(&session).expect("session exists");
    let flags = session_obj.winlink_alert_flags(0);
    assert!(
        !flags.contains(WINLINK_BELL),
        "bell flag should not be set on the current window with attached clients"
    );
}

#[tokio::test]
async fn action_none_on_non_current_window_still_sets_winlink_flags() {
    let handler = RequestHandler::new();
    let session = create_session(&handler, "none-noncurr").await;
    let other_window = create_window(&handler, &session).await;
    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(56, session.clone(), control_tx)
        .await;
    drain_attach_controls_until_idle(&mut control_rx).await;

    set_option(
        &handler,
        ScopeSelector::Session(session.clone()),
        OptionName::BellAction,
        "none",
    )
    .await;

    let mut lifecycle = handler.subscribe_lifecycle_events();

    handler
        .alerts_queue_window(other_window.clone(), rmux_core::WINDOW_BELL)
        .await;
    // action=none blocks delivery (no bell, no overlay, no hook).
    assert_no_non_switch_control(&mut control_rx).await;

    // No lifecycle/hook event should fire with action=none.
    let deadline = tokio::time::Instant::now() + Duration::from_millis(100);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match timeout(remaining, lifecycle.recv()).await {
            Err(_) | Ok(Err(_)) => break,
            Ok(Ok(event)) => {
                assert_ne!(
                    event.hook_name,
                    HookName::AlertBell,
                    "alert-bell hook should not fire with action=none"
                );
            }
        }
    }

    // But winlink flags are still set — action only gates delivery, not flag persistence.
    // This matches tmux: the status line shows the alert indicator even with action=none.
    let state = handler.state.lock().await;
    let session_obj = state.sessions.session(&session).expect("session exists");
    let flags = session_obj.winlink_alert_flags(other_window.window_index());
    assert!(
        flags.contains(WINLINK_BELL),
        "bell flag should be set on a non-current window even with action=none"
    );
}

#[tokio::test]
async fn empty_session_alerts_when_no_windows_are_alerted() {
    let handler = RequestHandler::new();
    let session = create_session(&handler, "empty-alerts").await;
    let _window = create_window(&handler, &session).await;

    let rendered = {
        let state = handler.state.lock().await;
        let context =
            format_context_for_target(&state, &Target::Session(session.clone()), 0).unwrap();
        render_runtime_template("#{session_alerts}", &context, false)
    };
    assert_eq!(rendered, "");
}

#[tokio::test]
async fn next_window_alert_errors_when_no_alerted_windows_exist() {
    let handler = RequestHandler::new();
    let session = create_session(&handler, "no-alert-nav").await;
    let _window = create_window(&handler, &session).await;

    let response = handler
        .handle(Request::NextWindow(NextWindowRequest {
            target: session.clone(),
            alerts_only: true,
        }))
        .await;
    assert!(matches!(response, Response::Error(_)));

    let response = handler
        .handle(Request::PreviousWindow(PreviousWindowRequest {
            target: session.clone(),
            alerts_only: true,
        }))
        .await;
    assert!(matches!(response, Response::Error(_)));
}

#[tokio::test]
async fn alert_message_logged_even_without_attached_clients() {
    let handler = RequestHandler::new();
    let session = create_session(&handler, "detached-log").await;
    let window = create_window(&handler, &session).await;

    set_option(
        &handler,
        ScopeSelector::Session(session.clone()),
        OptionName::VisualBell,
        "on",
    )
    .await;

    handler
        .alerts_queue_window(window.clone(), rmux_core::WINDOW_BELL)
        .await;

    // Give async tasks time to complete.
    tokio::time::sleep(Duration::from_millis(50)).await;

    let state = handler.state.lock().await;
    assert!(
        !state.message_log.is_empty(),
        "alert message should be logged even with no attached clients"
    );
    let last_message = &state.message_log.back().unwrap().msg;
    assert!(
        last_message.contains("Bell"),
        "logged message should mention the alert kind"
    );
}

#[tokio::test]
async fn kill_window_clears_alert_flags_for_removed_window() {
    let handler = RequestHandler::new();
    let session = create_session(&handler, "kill-alert").await;
    let window = create_window(&handler, &session).await;

    {
        let mut state = handler.state.lock().await;
        let session_obj = state
            .sessions
            .session_mut(&session)
            .expect("session exists");
        session_obj.add_winlink_alert_flags(window.window_index(), WINLINK_BELL);
    }

    let response = handler
        .handle(Request::KillWindow(KillWindowRequest {
            target: window.clone(),
            kill_all_others: false,
        }))
        .await;
    assert!(matches!(response, Response::KillWindow(_)));

    let state = handler.state.lock().await;
    let session_obj = state.sessions.session(&session).expect("session exists");
    // The killed window's alert flags should not exist.
    let flags = session_obj.winlink_alert_flags(window.window_index());
    assert!(
        flags.is_empty(),
        "alert flags should be cleared after killing window"
    );
    // Session-level alert flags should not include the killed window's bell.
    let session_flags = session_obj.session_alert_flags();
    assert!(
        !session_flags.contains(WINLINK_BELL),
        "session-level bell flag should be cleared after killing the only alerted window"
    );
}

#[tokio::test]
async fn silence_deduplication_skips_second_silence_on_same_winlink() {
    let handler = RequestHandler::new();
    let session = create_session(&handler, "silence-dedup").await;
    let window = create_window(&handler, &session).await;
    set_option(
        &handler,
        ScopeSelector::Window(window.clone()),
        OptionName::MonitorSilence,
        "1",
    )
    .await;

    let mut lifecycle = handler.subscribe_lifecycle_events();

    // First silence fires the hook.
    handler
        .alerts_queue_window(window.clone(), rmux_core::WINDOW_SILENCE)
        .await;
    recv_lifecycle_hook(&mut lifecycle, HookName::AlertSilence).await;

    // Second silence on the same winlink is suppressed (flag already set).
    handler
        .alerts_queue_window(window.clone(), rmux_core::WINDOW_SILENCE)
        .await;
    assert_no_lifecycle_hooks(
        &mut lifecycle,
        &[HookName::AlertSilence],
        Duration::from_millis(100),
        "duplicate silence alert should not fire",
    )
    .await;
}

#[tokio::test]
async fn show_messages_invalid_target_client_returns_error() {
    let handler = RequestHandler::new();
    let _session = create_session(&handler, "bad-target").await;

    let response = handler
        .handle(Request::ShowMessages(ShowMessagesRequest {
            jobs: false,
            terminals: true,
            target_client: Some("not-a-number".to_owned()),
        }))
        .await;
    assert!(
        matches!(response, Response::Error(_)),
        "non-numeric target client should produce an error"
    );
}

#[tokio::test]
async fn show_messages_message_log_resolves_target_client() {
    let handler = RequestHandler::new();
    let _session = create_session(&handler, "bad-message-target").await;

    let response = handler
        .handle(Request::ShowMessages(ShowMessagesRequest {
            jobs: false,
            terminals: false,
            target_client: Some("999999".to_owned()),
        }))
        .await;
    assert_eq!(
        response,
        Response::Error(rmux_proto::ErrorResponse {
            error: rmux_proto::RmuxError::Server("can't find client: 999999".to_owned())
        })
    );
}

#[tokio::test]
async fn select_window_clears_alert_flags_on_newly_selected_window() {
    let handler = RequestHandler::new();
    let session = create_session(&handler, "select-clear").await;
    let window_one = create_window(&handler, &session).await;

    {
        let mut state = handler.state.lock().await;
        let session_obj = state
            .sessions
            .session_mut(&session)
            .expect("session exists");
        session_obj.add_winlink_alert_flags(
            window_one.window_index(),
            WINLINK_BELL.union(WINLINK_ACTIVITY),
        );
    }

    // Selecting the alerted window should clear its flags.
    let response = handler
        .handle(Request::NextWindow(NextWindowRequest {
            target: session.clone(),
            alerts_only: false,
        }))
        .await;
    let Response::NextWindow(next) = &response else {
        panic!("expected next-window response, got {response:?}");
    };
    assert_eq!(next.target.window_index(), window_one.window_index());

    let state = handler.state.lock().await;
    let session_obj = state.sessions.session(&session).expect("session exists");
    assert_eq!(session_obj.active_window_index(), window_one.window_index());
    let flags = session_obj.winlink_alert_flags(window_one.window_index());
    assert!(
        flags.is_empty(),
        "alert flags should be cleared when selecting a window via next-window"
    );
}
