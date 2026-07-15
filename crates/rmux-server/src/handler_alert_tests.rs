use super::{
    attach_support::{AttachRegistration, ClientFlags, ATTACH_CONTROL_BACKLOG_LIMIT},
    scripting_support::format_context_for_target,
    QueuedLifecycleEvent, RequestHandler,
};
use crate::format_runtime::render_runtime_template;
use crate::outer_terminal::{OuterTerminal, OuterTerminalContext};
use crate::pane_io::{pane_output_channel, AttachControl, AttachTarget};
use crate::server_access::current_owner_uid;
use rmux_core::{
    AlertFlags, OptionStore, PaneGeometry, PaneId, WINDOW_ACTIVITY, WINLINK_ACTIVITY, WINLINK_BELL,
    WINLINK_SILENCE,
};
#[cfg(unix)]
use rmux_proto::SendKeysRequest;
use rmux_proto::{
    DisplayMessageRequest, HookLifecycle, HookName, KillWindowRequest, LinkWindowRequest,
    NewSessionExtRequest, NewSessionRequest, NewWindowRequest, NextWindowRequest, OptionName,
    OptionScopeSelector, PaneStateCursorRequest, PaneStateEventDto, PaneTarget, PaneTargetRef,
    PreviousWindowRequest, Request, Response, ScopeSelector, SelectPaneRequest, SessionName,
    SetHookMutationRequest, SetOptionByNameRequest, SetOptionMode, SetOptionRequest,
    ShowMessagesRequest, SplitDirection, SplitWindowExtRequest, SplitWindowTarget,
    SubscribePaneStateRequest, Target, TerminalSize, UnlinkWindowRequest, WindowTarget,
};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};
use tokio::time::{timeout, Duration};

#[cfg(windows)]
const ALERT_TEST_EVENT_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(not(windows))]
const ALERT_TEST_EVENT_TIMEOUT: Duration = Duration::from_millis(500);
const ACTIVITY_BASELINE_SETTLE: Duration = Duration::from_millis(1200);
const ACTIVITY_BASELINE_TIMEOUT: Duration = Duration::from_secs(10);

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
    let mut last = None;
    loop {
        {
            let state = handler.state.lock().await;
            if let Ok((_, content)) = state.buffers.show(Some(name)) {
                let content = String::from_utf8_lossy(content).into_owned();
                if content == expected {
                    return;
                }
                last = Some(content);
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "buffer {name} did not reach {expected:?}; last={last:?}"
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
    handler
        .wait_for_pane_startup_to_finish_for_test(&PaneTarget::new(session.clone(), 0))
        .await;
    session
}

async fn create_grouped_session(
    handler: &RequestHandler,
    name: &str,
    group_target: &SessionName,
) -> SessionName {
    let session = session_name(name);
    let response = handler
        .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
            session_name: Some(session.clone()),
            working_directory: None,
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
            group_target: Some(group_target.clone()),
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
    assert!(matches!(response, Response::NewSession(_)), "{response:?}");
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

async fn create_quiet_window(handler: &RequestHandler, session: &SessionName) -> WindowTarget {
    let response = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: session.clone(),
            name: None,
            detached: true,
            start_directory: None,
            environment: None,
            command: Some(quiet_alert_command()),
            process_command: None,
            target_window_index: None,
            insert_at_target: false,
        })))
        .await;
    let Response::NewWindow(response) = response else {
        panic!("expected quiet new-window response");
    };
    handler
        .wait_for_pane_startup_to_finish_for_test(&PaneTarget::with_window(
            session.clone(),
            response.target.window_index(),
            0,
        ))
        .await;
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
    let Response::SplitWindow(response) = response else {
        panic!("expected split-window response");
    };
    handler
        .wait_for_pane_startup_to_finish_for_test(&response.pane)
        .await;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ActivitySnapshot {
    origin_pane_id: PaneId,
    window: i64,
    origin: i64,
    other: i64,
}

async fn two_pane_activity_snapshot(
    handler: &RequestHandler,
    session_name: &SessionName,
) -> ActivitySnapshot {
    let state = handler.state.lock().await;
    let session = state
        .sessions
        .session(session_name)
        .expect("session exists");
    let window = session.window_at(0).expect("window exists");
    let origin = window.pane(0).expect("origin pane exists");
    let other = window.pane(1).expect("other pane exists");
    ActivitySnapshot {
        origin_pane_id: origin.id(),
        window: window.activity_at(),
        origin: origin.activity_at(),
        other: other.activity_at(),
    }
}

async fn wait_for_two_pane_activity_to_settle(
    handler: &RequestHandler,
    session_name: &SessionName,
) -> ActivitySnapshot {
    let deadline = tokio::time::Instant::now() + ACTIVITY_BASELINE_TIMEOUT;
    let mut previous = two_pane_activity_snapshot(handler, session_name).await;
    let mut stable_since = tokio::time::Instant::now();

    loop {
        tokio::time::sleep(Duration::from_millis(25)).await;
        let current = two_pane_activity_snapshot(handler, session_name).await;
        let now = tokio::time::Instant::now();
        if current == previous {
            if now.duration_since(stable_since) >= ACTIVITY_BASELINE_SETTLE {
                return current;
            }
        } else {
            previous = current;
            stable_since = now;
        }
        assert!(
            now < deadline,
            "activity baseline did not settle; last snapshot: {previous:?}"
        );
    }
}

async fn wait_for_silence_timer_to_settle(
    handler: &RequestHandler,
    target: &WindowTarget,
) -> (u64, tokio::time::Instant) {
    let deadline = tokio::time::Instant::now() + ACTIVITY_BASELINE_TIMEOUT;
    let mut previous = handler
        .silence_timer_snapshot_for_test(target)
        .unwrap_or_else(|| panic!("silence timer for {target} is armed"));
    let mut stable_since = tokio::time::Instant::now();

    loop {
        tokio::time::sleep(Duration::from_millis(25)).await;
        let current = handler
            .silence_timer_snapshot_for_test(target)
            .unwrap_or_else(|| panic!("silence timer for {target} remains armed"));
        let now = tokio::time::Instant::now();
        if current == previous {
            if now.duration_since(stable_since) >= ACTIVITY_BASELINE_SETTLE {
                return current;
            }
        } else {
            previous = current;
            stable_since = now;
        }
        assert!(
            now < deadline,
            "silence timer baseline did not settle for {target}; last snapshot: {previous:?}"
        );
    }
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

async fn wait_for_winlink_flag(
    handler: &RequestHandler,
    target: &WindowTarget,
    flag: AlertFlags,
    expected: bool,
    wait_for: Duration,
) {
    let deadline = tokio::time::Instant::now() + wait_for;
    loop {
        let present = {
            let state = handler.state.lock().await;
            state
                .sessions
                .session(target.session_name())
                .unwrap_or_else(|| panic!("session for {target} exists"))
                .winlink_alert_flags(target.window_index())
                .contains(flag)
        };
        if present == expected {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "winlink flag {flag:?} on {target} did not become {expected}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
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
    matches!(
        hook_name,
        HookName::ClientActive
            | HookName::ClientFocusIn
            | HookName::ClientFocusOut
            | HookName::PaneSetClipboard
            | HookName::PaneTitleChanged
    )
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

async fn recv_client_detached_for(
    events: &mut broadcast::Receiver<QueuedLifecycleEvent>,
    attach_pid: u32,
) -> rmux_core::LifecycleEvent {
    loop {
        let event = events
            .recv()
            .await
            .expect("lifecycle event channel remains open");
        if matches!(
            &event.event,
            rmux_core::LifecycleEvent::ClientDetached {
                client_name: Some(client_name),
                ..
            } if client_name == &attach_pid.to_string()
        ) {
            return event.event;
        }
    }
}

async fn register_clipboard_attach(
    handler: &RequestHandler,
    attach_pid: u32,
    session: &SessionName,
) -> mpsc::UnboundedReceiver<AttachControl> {
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    let uid = current_owner_uid();
    handler
        .register_attach_with_access(
            attach_pid,
            session.clone(),
            None,
            AttachRegistration {
                control_tx,
                control_backlog: Arc::new(AtomicUsize::new(0)),
                closing: Arc::new(AtomicBool::new(false)),
                persistent_overlay_epoch: Arc::new(AtomicU64::new(0)),
                terminal_context: OuterTerminalContext::from_pairs(&[("TERM", "xterm-256color")]),
                flags: ClientFlags::default(),
                render_stream: false,
                uid,
                user: rmux_os::identity::UserIdentity::Uid(uid),
                can_write: true,
                client_size: Some(TerminalSize { cols: 80, rows: 24 }),
            },
        )
        .await
        .expect("clipboard attach registration succeeds");
    control_rx
}

fn clipboard_alert(session: SessionName, pane_id: PaneId) -> crate::pane_io::PaneAlertEvent {
    crate::pane_io::PaneAlertEvent {
        session_name: session,
        pane_id,
        bell_count: 0,
        title_changed: false,
        title_change: None,
        clipboard_set: true,
        clipboard_writes: vec![b"A".to_vec()],
        mouse_mode_changed: false,
        queue_activity_alert: false,
        generation: None,
    }
}

fn drain_clipboard_writes(receiver: &mut mpsc::UnboundedReceiver<AttachControl>) -> Vec<Vec<u8>> {
    let mut writes = Vec::new();
    while let Ok(control) = receiver.try_recv() {
        if let AttachControl::ClipboardWrite { bytes, .. } = control {
            writes.push(bytes);
        }
    }
    writes
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
        title_change: None,
        clipboard_set: false,
        clipboard_writes: Vec::new(),
        mouse_mode_changed: false,
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
async fn pane_alert_batch_coalesces_bell_across_two_panes_in_one_window() {
    let handler = RequestHandler::new();
    let session = create_quiet_session(&handler, "alerts-window-bell-coalescing").await;
    split_quiet_window(&handler, &session).await;
    set_option(
        &handler,
        ScopeSelector::Session(session.clone()),
        OptionName::VisualBell,
        "off",
    )
    .await;
    set_option(
        &handler,
        ScopeSelector::Session(session.clone()),
        OptionName::BellAction,
        "any",
    )
    .await;

    let pane_ids = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&session)
            .and_then(|session| session.window_at(0))
            .expect("two-pane window exists")
            .panes()
            .iter()
            .map(|pane| pane.id())
            .collect::<Vec<_>>()
    };
    assert_eq!(pane_ids.len(), 2);

    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(73, session.clone(), control_tx)
        .await;
    drain_attach_controls_until_idle(&mut control_rx).await;
    let mut lifecycle = handler.subscribe_lifecycle_events();
    let callback = handler.pane_alert_callback();
    for pane_id in pane_ids {
        callback(crate::pane_io::PaneAlertEvent {
            session_name: session.clone(),
            pane_id,
            bell_count: 1,
            title_changed: false,
            title_change: None,
            clipboard_set: false,
            clipboard_writes: Vec::new(),
            mouse_mode_changed: false,
            queue_activity_alert: false,
            generation: None,
        });
    }

    recv_lifecycle_hook(&mut lifecycle, HookName::AlertBell).await;
    assert_no_lifecycle_hooks(
        &mut lifecycle,
        &[HookName::AlertBell],
        Duration::from_millis(150),
        "one pane-alert flush must emit only one window-level bell hook",
    )
    .await;
    match recv_non_switch_control(&mut control_rx).await {
        AttachControl::Write(bytes) => assert_eq!(bytes, vec![0x07]),
        other => panic!("expected one bell write, got {other:?}"),
    }
    assert_no_non_switch_control(&mut control_rx).await;
}

#[tokio::test]
async fn pane_alert_callback_can_be_invoked_from_reader_thread() {
    let handler = RequestHandler::new();
    // Keep the real pane reader quiescent so this test observes only the
    // callback invoked below. In particular, an interactive Windows shell can
    // publish an initial title/activity event concurrently and consume the
    // one-shot activity alert before the synthetic reader-thread event runs.
    let session = create_quiet_session(&handler, "alerts-reader-thread").await;
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
            title_change: None,
            clipboard_set: false,
            clipboard_writes: Vec::new(),
            mouse_mode_changed: false,
            queue_activity_alert: true,
            generation: None,
        });
    })
    .join()
    .expect("reader-thread alert callback should not panic outside the Tokio runtime");

    recv_lifecycle_hook(&mut lifecycle, HookName::AlertActivity).await;
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
        title_change: None,
        clipboard_set: false,
        clipboard_writes: Vec::new(),
        mouse_mode_changed: false,
        queue_activity_alert: false,
        generation: None,
    });

    let event = recv_lifecycle(&mut lifecycle).await;
    assert_eq!(event.hook_name, HookName::PaneTitleChanged);
}

#[tokio::test]
async fn pane_state_title_alert_ignores_stale_generation() {
    let handler = RequestHandler::new();
    let session = create_quiet_session(&handler, "pane-title-state-generation").await;
    let target = PaneTarget::with_window(session.clone(), 0, 0);
    let (pane_id, generation) = {
        let state = handler.state.lock().await;
        let pane_id = state
            .sessions
            .session(&session)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0).map(|pane| pane.id()))
            .expect("window pane exists");
        (pane_id, state.pane_output_generation(&session, pane_id))
    };
    let subscription_id = match handler
        .handle_subscribe_pane_state(
            71,
            SubscribePaneStateRequest {
                target: PaneTargetRef::slot(target),
                include_title: true,
                include_options: false,
                include_foreground: false,
            },
        )
        .await
    {
        Response::SubscribePaneState(response) => response.subscription_id,
        response => panic!("subscribe-pane-state failed: {response:?}"),
    };

    let callback = handler.pane_alert_callback();
    callback(crate::pane_io::PaneAlertEvent {
        session_name: session.clone(),
        pane_id,
        bell_count: 0,
        title_changed: true,
        title_change: Some(("old".to_owned(), "current".to_owned())),
        clipboard_set: false,
        clipboard_writes: Vec::new(),
        mouse_mode_changed: false,
        queue_activity_alert: false,
        generation: Some(generation),
    });

    let revision = timeout(Duration::from_secs(2), async {
        let mut after_revision = 0;
        'wait_for_title: loop {
            match handler
                .handle_pane_state_cursor(
                    71,
                    PaneStateCursorRequest {
                        subscription_id,
                        after_revision,
                        wait: false,
                        max_events: Some(16),
                    },
                )
                .await
            {
                Response::PaneStateCursor(response) if !response.events.is_empty() => {
                    for event in response.events {
                        match event {
                            PaneStateEventDto::TitleChanged {
                                revision,
                                old_title,
                                new_title,
                                ..
                            } if old_title == "old" && new_title == "current" => {
                                break 'wait_for_title revision;
                            }
                            PaneStateEventDto::TitleChanged { revision, .. }
                            | PaneStateEventDto::OptionSet { revision, .. }
                            | PaneStateEventDto::OptionUnset { revision, .. }
                            | PaneStateEventDto::ForegroundChanged { revision, .. }
                            | PaneStateEventDto::Closed { revision, .. } => {
                                after_revision = after_revision.max(revision);
                            }
                            _ => {}
                        }
                    }
                }
                Response::PaneStateCursor(_) => {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                response => panic!("pane-state-cursor failed: {response:?}"),
            }
        }
    })
    .await
    .expect("current-generation title event must arrive");

    callback(crate::pane_io::PaneAlertEvent {
        session_name: session,
        pane_id,
        bell_count: 0,
        title_changed: true,
        title_change: Some(("current".to_owned(), "stale".to_owned())),
        clipboard_set: false,
        clipboard_writes: Vec::new(),
        mouse_mode_changed: false,
        queue_activity_alert: false,
        generation: Some(generation.saturating_add(1)),
    });
    tokio::time::sleep(Duration::from_millis(200)).await;

    match handler
        .handle_pane_state_cursor(
            71,
            PaneStateCursorRequest {
                subscription_id,
                after_revision: revision,
                wait: false,
                max_events: Some(16),
            },
        )
        .await
    {
        Response::PaneStateCursor(response) => assert!(
            !response.events.iter().any(|event| matches!(
                event,
                PaneStateEventDto::TitleChanged { new_title, .. } if new_title == "stale"
            )),
            "stale-generation title change leaked into pane-state stream: {:?}",
            response.events
        ),
        response => panic!("pane-state-cursor failed: {response:?}"),
    }
}

#[tokio::test]
async fn pane_state_reports_pane_option_unset_from_handler() {
    let handler = RequestHandler::new();
    let session = create_session(&handler, "pane-option-unset-handler").await;
    let target = PaneTarget::with_window(session.clone(), 0, 0);
    let pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&session)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0).map(|pane| pane.id()))
            .expect("pane exists")
    };
    let subscription_id = match handler
        .handle_subscribe_pane_state(
            72,
            SubscribePaneStateRequest {
                target: PaneTargetRef::slot(target.clone()),
                include_title: false,
                include_options: true,
                include_foreground: false,
            },
        )
        .await
    {
        Response::SubscribePaneState(response) => response.subscription_id,
        response => panic!("subscribe-pane-state failed: {response:?}"),
    };

    let set = handler
        .handle(Request::SetOptionByName(Box::new(SetOptionByNameRequest {
            scope: OptionScopeSelector::Pane(target.clone()),
            name: "@agent.state".to_owned(),
            value: Some("waiting".to_owned()),
            mode: SetOptionMode::Replace,
            only_if_unset: false,
            unset: false,
            unset_pane_overrides: false,
            format: false,
            format_target: None,
        })))
        .await;
    assert!(matches!(set, Response::SetOptionByName(_)), "{set:?}");

    let unset = handler
        .handle(Request::SetOptionByName(Box::new(SetOptionByNameRequest {
            scope: OptionScopeSelector::Pane(target),
            name: "@agent.state".to_owned(),
            value: None,
            mode: SetOptionMode::Replace,
            only_if_unset: false,
            unset: true,
            unset_pane_overrides: false,
            format: false,
            format_target: None,
        })))
        .await;
    assert!(matches!(unset, Response::SetOptionByName(_)), "{unset:?}");

    let cursor = handler
        .handle_pane_state_cursor(
            72,
            PaneStateCursorRequest {
                subscription_id,
                after_revision: 0,
                wait: false,
                max_events: Some(16),
            },
        )
        .await;
    let Response::PaneStateCursor(cursor) = cursor else {
        panic!("pane-state-cursor failed: {cursor:?}");
    };
    assert!(cursor.events.iter().any(|event| matches!(
        event,
        PaneStateEventDto::OptionUnset {
            pane_id: event_pane_id,
            name,
            old_value,
            ..
        } if *event_pane_id == pane_id
            && name == "@agent.state"
            && old_value.as_deref() == Some("waiting")
    )));
}

#[tokio::test]
async fn pane_state_reports_related_pane_option_unset_from_window_mass_unset() {
    let handler = RequestHandler::new();
    let session = create_session(&handler, "pane-option-related-unset").await;
    let target = PaneTarget::with_window(session.clone(), 0, 0);
    let window = WindowTarget::with_window(session.clone(), 0);
    let pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&session)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0).map(|pane| pane.id()))
            .expect("pane exists")
    };
    let subscription_id = match handler
        .handle_subscribe_pane_state(
            73,
            SubscribePaneStateRequest {
                target: PaneTargetRef::slot(target.clone()),
                include_title: false,
                include_options: true,
                include_foreground: false,
            },
        )
        .await
    {
        Response::SubscribePaneState(response) => response.subscription_id,
        response => panic!("subscribe-pane-state failed: {response:?}"),
    };

    let set_window = handler
        .handle(Request::SetOptionByName(Box::new(SetOptionByNameRequest {
            scope: OptionScopeSelector::Window(window.clone()),
            name: "@agent.state".to_owned(),
            value: Some("window".to_owned()),
            mode: SetOptionMode::Replace,
            only_if_unset: false,
            unset: false,
            unset_pane_overrides: false,
            format: false,
            format_target: None,
        })))
        .await;
    assert!(
        matches!(set_window, Response::SetOptionByName(_)),
        "{set_window:?}"
    );
    let set_pane = handler
        .handle(Request::SetOptionByName(Box::new(SetOptionByNameRequest {
            scope: OptionScopeSelector::Pane(target),
            name: "@agent.state".to_owned(),
            value: Some("pane".to_owned()),
            mode: SetOptionMode::Replace,
            only_if_unset: false,
            unset: false,
            unset_pane_overrides: false,
            format: false,
            format_target: None,
        })))
        .await;
    assert!(
        matches!(set_pane, Response::SetOptionByName(_)),
        "{set_pane:?}"
    );

    let unset_window = handler
        .handle(Request::SetOptionByName(Box::new(SetOptionByNameRequest {
            scope: OptionScopeSelector::Window(window),
            name: "@agent.state".to_owned(),
            value: None,
            mode: SetOptionMode::Replace,
            only_if_unset: false,
            unset: true,
            unset_pane_overrides: true,
            format: false,
            format_target: None,
        })))
        .await;
    assert!(
        matches!(unset_window, Response::SetOptionByName(_)),
        "{unset_window:?}"
    );

    let cursor = handler
        .handle_pane_state_cursor(
            73,
            PaneStateCursorRequest {
                subscription_id,
                after_revision: 0,
                wait: false,
                max_events: Some(16),
            },
        )
        .await;
    let Response::PaneStateCursor(cursor) = cursor else {
        panic!("pane-state-cursor failed: {cursor:?}");
    };
    assert!(cursor.events.iter().any(|event| matches!(
        event,
        PaneStateEventDto::OptionUnset {
            pane_id: event_pane_id,
            name,
            old_value,
            ..
        } if *event_pane_id == pane_id
            && name == "@agent.state"
            && old_value.as_deref() == Some("pane")
    )));
}

#[tokio::test]
async fn pane_output_updates_activity_for_originating_pane_only() {
    let handler = RequestHandler::new();
    let session = create_quiet_session(&handler, "pane-output-activity").await;
    split_quiet_window(&handler, &session).await;
    let before = wait_for_two_pane_activity_to_settle(&handler, &session).await;

    tokio::time::sleep(Duration::from_secs(1)).await;
    handler
        .handle_pane_alert_event(crate::pane_io::PaneAlertEvent {
            session_name: session.clone(),
            pane_id: before.origin_pane_id,
            bell_count: 0,
            title_changed: false,
            title_change: None,
            clipboard_set: false,
            clipboard_writes: Vec::new(),
            mouse_mode_changed: false,
            queue_activity_alert: false,
            generation: None,
        })
        .await;

    let state = handler.state.lock().await;
    let session = state.sessions.session(&session).expect("session exists");
    let window = session.window_at(0).expect("window exists");
    let origin = window.pane(0).expect("origin pane exists");
    let other = window.pane(1).expect("other pane exists");
    assert!(window.activity_at() > before.window);
    assert!(origin.activity_at() > before.origin);
    assert_eq!(other.activity_at(), before.other);
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
            title_change: None,
            clipboard_set: true,
            clipboard_writes: Vec::new(),
            mouse_mode_changed: false,
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
            title_change: None,
            clipboard_set: true,
            clipboard_writes: Vec::new(),
            mouse_mode_changed: false,
            queue_activity_alert: false,
            generation: None,
        })
        .await;

    drain_lifecycle_hooks(&handler, &mut lifecycle).await;
    let state = handler.state.lock().await;
    assert!(state.buffers.show(Some("clipboard-hook")).is_err());
}

#[tokio::test]
async fn inbound_osc52_write_creates_paste_buffer_under_set_clipboard_on() {
    // tmux's input_osc_52 calls paste_add under `set-clipboard on`, so an
    // application's inbound OSC 52 write lands in a paste buffer a detached
    // client keeps and `list-buffers` shows (issue #91).
    let handler = RequestHandler::new();
    let session = create_quiet_session(&handler, "osc52-buffer-on").await;
    set_server_option_by_name(&handler, "set-clipboard", "on").await;
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
            title_change: None,
            clipboard_set: true,
            clipboard_writes: vec![b"hello".to_vec()],
            mouse_mode_changed: false,
            queue_activity_alert: false,
            generation: None,
        })
        .await;

    wait_for_buffer(&handler, "buffer0", "hello").await;
}

#[tokio::test]
async fn inbound_osc52_before_last_pane_eof_keeps_buffer_and_hook() {
    let handler = RequestHandler::new();
    let session = create_quiet_session(&handler, "osc52-before-eof").await;
    let _keeper = create_quiet_session(&handler, "osc52-before-eof-keeper").await;
    set_server_option_by_name(&handler, "set-clipboard", "on").await;
    set_global_hook(
        &handler,
        HookName::PaneSetClipboard,
        "set-buffer -b final-clipboard-hook fired",
    )
    .await;
    let lifecycle_events = handler
        .take_lifecycle_dispatch_receiver()
        .expect("test owns the lifecycle dispatch receiver");
    let (hook_shutdown, hook_shutdown_rx) = tokio::sync::oneshot::channel();
    let hook_handler = handler.clone();
    let hook_task = tokio::spawn(async move {
        hook_handler
            .consume_lifecycle_hooks(lifecycle_events, hook_shutdown_rx)
            .await;
    });
    let target = PaneTarget::with_window(session.clone(), 0, 0);
    let (pane_id, generation, pane_output) = {
        let state = handler.state.lock().await;
        let pane_id = state
            .sessions
            .session(&session)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0).map(|pane| pane.id()))
            .expect("window pane exists");
        let generation = state.pane_output_generation(&session, pane_id);
        let pane_output = state
            .runtime_pane_output_drain_handles(&session, pane_id)
            .1
            .expect("pane output sender exists");
        (pane_id, generation, pane_output)
    };

    {
        let mut state = handler.state.lock().await;
        state
            .mark_pane_dead_without_exit_details(&target)
            .expect("mark last pane naturally exited");
    }
    let pause = handler.install_pane_exit_commit_pause();
    let exit_handler = handler.clone();
    let exit_session = session.clone();
    let exit_task = tokio::spawn(async move {
        exit_handler
            .handle_pane_exit_event(crate::pane_io::PaneExitEvent::eof_pending(
                exit_session,
                pane_id,
                Some(generation),
            ))
            .await;
    });
    tokio::time::timeout(
        Duration::from_secs(1),
        pause.output_drain_started.notified(),
    )
    .await
    .expect("pane exit waits for pending output before alert flush");

    handler.pane_alert_callback()(crate::pane_io::PaneAlertEvent {
        session_name: session.clone(),
        pane_id,
        bell_count: 0,
        title_changed: false,
        title_change: None,
        clipboard_set: true,
        clipboard_writes: vec![b"hello-before-eof".to_vec()],
        mouse_mode_changed: false,
        queue_activity_alert: false,
        generation: Some(generation),
    });
    let _ = pane_output.send_for_generation(Some(generation), Vec::new());
    tokio::time::timeout(Duration::from_secs(1), pause.reached.notified())
        .await
        .expect("pane exit commits after pending output is published");
    pause.release.notify_one();
    exit_task.await.expect("pane exit task joins");

    wait_for_buffer(&handler, "buffer0", "hello-before-eof").await;
    wait_for_buffer(&handler, "final-clipboard-hook", "fired").await;
    assert!(handler
        .state
        .lock()
        .await
        .sessions
        .session(&session)
        .is_none());
    let _ = hook_shutdown.send(());
    tokio::time::timeout(Duration::from_secs(2), hook_task)
        .await
        .expect("lifecycle hook consumer stops")
        .expect("lifecycle hook consumer joins");
}

#[tokio::test]
async fn inbound_osc52_write_creates_no_buffer_without_set_clipboard_on() {
    // Under the `external` default tmux ignores an application's inbound OSC 52
    // entirely (input_osc_52 requires set-clipboard == 2), so no buffer appears.
    let handler = RequestHandler::new();
    let session = create_quiet_session(&handler, "osc52-buffer-external").await;
    set_server_option_by_name(&handler, "set-clipboard", "external").await;
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
            title_change: None,
            clipboard_set: true,
            clipboard_writes: vec![b"hello".to_vec()],
            mouse_mode_changed: false,
            queue_activity_alert: false,
            generation: None,
        })
        .await;

    let state = handler.state.lock().await;
    assert!(
        state.buffers.show(None).is_err(),
        "external must not create a paste buffer from inbound OSC 52"
    );
}

#[tokio::test]
async fn inactive_visible_pane_osc52_targets_each_attached_client_once() {
    // Oracle: tmux 3.7b relays an application OSC 52 from a visible inactive
    // pane to the outer terminal. The active pane's own output ring is not the
    // only eligible source.
    let handler = RequestHandler::new();
    let session = create_quiet_session(&handler, "osc52-inactive-visible").await;
    let unrelated = create_quiet_session(&handler, "osc52-inactive-unrelated").await;
    split_quiet_window(&handler, &session).await;
    set_server_option_by_name(&handler, "set-clipboard", "on").await;
    let inactive_pane_id = {
        let state = handler.state.lock().await;
        let session = state.sessions.session(&session).expect("session exists");
        let active = session.active_pane_id();
        session
            .window_at(0)
            .expect("window exists")
            .panes()
            .iter()
            .find_map(|pane| (Some(pane.id()) != active).then_some(pane.id()))
            .expect("inactive pane exists")
    };

    let mut first = register_clipboard_attach(&handler, 301, &session).await;
    let mut second = register_clipboard_attach(&handler, 302, &session).await;
    let mut other = register_clipboard_attach(&handler, 303, &unrelated).await;
    drain_attach_controls_until_idle(&mut first).await;
    drain_attach_controls_until_idle(&mut second).await;
    drain_attach_controls_until_idle(&mut other).await;

    handler
        .handle_pane_alert_event(clipboard_alert(session, inactive_pane_id))
        .await;

    assert_eq!(
        drain_clipboard_writes(&mut first),
        vec![b"\x1b]52;;QQ==\x07".to_vec()]
    );
    assert_eq!(
        drain_clipboard_writes(&mut second),
        vec![b"\x1b]52;;QQ==\x07".to_vec()]
    );
    assert!(drain_clipboard_writes(&mut other).is_empty());
}

#[tokio::test]
async fn active_pane_osc52_does_not_enqueue_a_second_clipboard_write() {
    let handler = RequestHandler::new();
    let session = create_quiet_session(&handler, "osc52-active-single-path").await;
    set_server_option_by_name(&handler, "set-clipboard", "on").await;
    let active_pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&session)
            .and_then(rmux_core::Session::active_pane_id)
            .expect("active pane exists")
    };
    let mut control_rx = register_clipboard_attach(&handler, 304, &session).await;
    drain_attach_controls_until_idle(&mut control_rx).await;

    handler
        .handle_pane_alert_event(clipboard_alert(session, active_pane_id))
        .await;

    assert!(
        drain_clipboard_writes(&mut control_rx).is_empty(),
        "the active pane is forwarded by its output ring, not a clipboard control"
    );
}

#[tokio::test]
async fn non_current_window_osc52_is_not_relayed_to_attached_clients() {
    let handler = RequestHandler::new();
    let session = create_quiet_session(&handler, "osc52-hidden-window").await;
    let hidden_window = create_quiet_window(&handler, &session).await;
    set_server_option_by_name(&handler, "set-clipboard", "on").await;
    let pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&session)
            .and_then(|session| session.window_at(hidden_window.window_index()))
            .and_then(|window| window.pane(0))
            .map(rmux_core::Pane::id)
            .expect("hidden window pane exists")
    };
    let mut control_rx = register_clipboard_attach(&handler, 305, &session).await;
    drain_attach_controls_until_idle(&mut control_rx).await;

    handler
        .handle_pane_alert_event(clipboard_alert(session, pane_id))
        .await;

    assert!(drain_clipboard_writes(&mut control_rx).is_empty());
}

#[tokio::test]
async fn inactive_pane_osc52_relay_requires_set_clipboard_on() {
    for (offset, option) in ["external", "off"].into_iter().enumerate() {
        let handler = RequestHandler::new();
        let session = create_quiet_session(&handler, &format!("osc52-relay-{option}")).await;
        split_quiet_window(&handler, &session).await;
        set_server_option_by_name(&handler, "set-clipboard", option).await;
        let inactive_pane_id = {
            let state = handler.state.lock().await;
            let session = state.sessions.session(&session).expect("session exists");
            let active = session.active_pane_id();
            session
                .window_at(0)
                .expect("window exists")
                .panes()
                .iter()
                .find_map(|pane| (Some(pane.id()) != active).then_some(pane.id()))
                .expect("inactive pane exists")
        };
        let mut control_rx =
            register_clipboard_attach(&handler, 306 + offset as u32, &session).await;
        drain_attach_controls_until_idle(&mut control_rx).await;

        handler
            .handle_pane_alert_event(clipboard_alert(session, inactive_pane_id))
            .await;

        assert!(
            drain_clipboard_writes(&mut control_rx).is_empty(),
            "set-clipboard {option} must not relay application OSC 52"
        );
    }
}

#[tokio::test]
async fn inactive_pane_osc52_is_enqueued_before_a_following_session_switch() {
    let handler = RequestHandler::new();
    let source = create_quiet_session(&handler, "osc52-switch-source").await;
    let destination = create_quiet_session(&handler, "osc52-switch-destination").await;
    split_quiet_window(&handler, &source).await;
    set_server_option_by_name(&handler, "set-clipboard", "on").await;
    let inactive_pane_id = {
        let state = handler.state.lock().await;
        let source = state.sessions.session(&source).expect("source exists");
        let active = source.active_pane_id();
        source
            .window_at(0)
            .expect("source window exists")
            .panes()
            .iter()
            .find_map(|pane| (Some(pane.id()) != active).then_some(pane.id()))
            .expect("inactive pane exists")
    };
    let destination_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&destination)
            .map(rmux_core::Session::id)
            .expect("destination exists")
    };
    let mut control_rx = register_clipboard_attach(&handler, 308, &source).await;
    drain_attach_controls_until_idle(&mut control_rx).await;

    handler.pane_alert_callback()(clipboard_alert(source, inactive_pane_id));
    let pane_output = pane_output_channel();
    let (pane_output_start_sequence, pane_output) = pane_output.subscribe_live_from_now();
    let target = AttachTarget {
        session_name: destination.clone(),
        pane_master: None,
        pane_output,
        pane_output_start_sequence,
        render_frame: b"destination".to_vec(),
        outer_terminal: OuterTerminal::resolve(
            &OptionStore::default(),
            OuterTerminalContext::default(),
        ),
        cursor_style: 0,
        active_pane_geometry: PaneGeometry::new(0, 0, 80, 24),
        raw_passthrough: false,
        kitty_graphics_passthrough: false,
        sixel_passthrough: false,
        persistent_overlay_state_id: None,
        live_pane: None,
    };
    handler
        .send_attach_control_for_session_identity(
            308,
            AttachControl::switch(target),
            "switch-client",
            destination,
            destination_id,
        )
        .await
        .expect("following switch succeeds");

    assert!(matches!(
        control_rx.try_recv(),
        Ok(AttachControl::ClipboardWrite { bytes, .. }) if bytes == b"\x1b]52;;QQ==\x07"
    ));
    assert!(matches!(
        control_rx.try_recv(),
        Ok(AttachControl::Switch(_))
    ));
}

#[tokio::test]
async fn busy_attach_lock_drops_osc52_relay_without_a_deferred_retry() {
    let handler = RequestHandler::new();
    let session = create_quiet_session(&handler, "osc52-busy-routing").await;
    split_quiet_window(&handler, &session).await;
    set_server_option_by_name(&handler, "set-clipboard", "on").await;
    let inactive_pane_id = {
        let state = handler.state.lock().await;
        let session = state.sessions.session(&session).expect("session exists");
        let active = session.active_pane_id();
        session
            .window_at(0)
            .expect("window exists")
            .panes()
            .iter()
            .find_map(|pane| (Some(pane.id()) != active).then_some(pane.id()))
            .expect("inactive pane exists")
    };
    let mut control_rx = register_clipboard_attach(&handler, 309, &session).await;
    drain_attach_controls_until_idle(&mut control_rx).await;

    let active_attach = handler.active_attach.lock().await;
    handler.pane_alert_callback()(clipboard_alert(session, inactive_pane_id));
    drop(active_attach);
    tokio::time::sleep(Duration::from_millis(200)).await;

    assert!(
        drain_clipboard_writes(&mut control_rx).is_empty(),
        "a failed try_lock must never be retried later against a new visibility state"
    );
}

#[tokio::test]
async fn inactive_pane_osc52_disconnects_a_non_draining_attach_at_the_backlog_limit() {
    let handler = RequestHandler::new();
    let session = create_quiet_session(&handler, "osc52-bounded-backlog").await;
    split_quiet_window(&handler, &session).await;
    set_server_option_by_name(&handler, "set-clipboard", "on").await;
    let inactive_pane_id = {
        let state = handler.state.lock().await;
        let session = state.sessions.session(&session).expect("session exists");
        let active = session.active_pane_id();
        session
            .window_at(0)
            .expect("window exists")
            .panes()
            .iter()
            .find_map(|pane| (Some(pane.id()) != active).then_some(pane.id()))
            .expect("inactive pane exists")
    };
    let mut control_rx = register_clipboard_attach(&handler, 310, &session).await;
    drain_attach_controls_until_idle(&mut control_rx).await;
    let callback = handler.pane_alert_callback();

    for _ in 0..=ATTACH_CONTROL_BACKLOG_LIMIT {
        callback(clipboard_alert(session.clone(), inactive_pane_id));
    }

    let mut writes = 0;
    let mut detaches = 0;
    while let Ok(control) = control_rx.try_recv() {
        match control {
            AttachControl::ClipboardWrite { .. } => writes += 1,
            AttachControl::Detach => detaches += 1,
            _ => {}
        }
    }
    assert_eq!(writes, ATTACH_CONTROL_BACKLOG_LIMIT);
    assert_eq!(detaches, 1);
    timeout(Duration::from_secs(2), async {
        loop {
            if !handler.active_attach.lock().await.by_pid.contains_key(&310) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the bounded relay must finish cleaning up a non-draining client");
}

#[tokio::test]
async fn inactive_pane_osc52_backlog_disconnect_runs_attach_cleanup_once() {
    let handler = RequestHandler::new();
    let session = create_quiet_session(&handler, "osc52-backlog-cleanup").await;
    split_quiet_window(&handler, &session).await;
    set_server_option_by_name(&handler, "set-clipboard", "on").await;
    let inactive_pane_id = {
        let state = handler.state.lock().await;
        let session = state.sessions.session(&session).expect("session exists");
        let active = session.active_pane_id();
        session
            .window_at(0)
            .expect("window exists")
            .panes()
            .iter()
            .find_map(|pane| (Some(pane.id()) != active).then_some(pane.id()))
            .expect("inactive pane exists")
    };
    let attach_pid = 312;
    let mut control_rx = register_clipboard_attach(&handler, attach_pid, &session).await;
    drain_attach_controls_until_idle(&mut control_rx).await;
    handler
        .set_attached_key_table_for_test(attach_pid, Some("osc52-cleanup".to_owned()))
        .await
        .expect("test key table installs");
    let (attach_id, control_backlog) = {
        let active_attach = handler.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get(&attach_pid)
            .expect("attach remains registered");
        (active.id, Arc::clone(&active.control_backlog))
    };
    {
        let mut state = handler.state.lock().await;
        assert_eq!(
            state
                .key_bindings
                .table("osc52-cleanup")
                .expect("test key table exists")
                .references(),
            1
        );
        state
            .options
            .set(
                ScopeSelector::Session(session.clone()),
                OptionName::DestroyUnattached,
                "on".to_owned(),
                SetOptionMode::Replace,
            )
            .expect("destroy-unattached option is valid");
    }
    control_backlog.store(ATTACH_CONTROL_BACKLOG_LIMIT, Ordering::Release);
    let mut events = handler.subscribe_lifecycle_events();

    handler
        .handle_pane_alert_event(clipboard_alert(session.clone(), inactive_pane_id))
        .await;

    assert!(!handler
        .active_attach
        .lock()
        .await
        .by_pid
        .contains_key(&attach_pid));
    {
        let state = handler.state.lock().await;
        assert!(state.key_bindings.table("osc52-cleanup").is_none());
        assert!(state.sessions.session(&session).is_none());
    }
    let detached = timeout(
        Duration::from_secs(2),
        recv_client_detached_for(&mut events, attach_pid),
    )
    .await
    .expect("clipboard backlog cleanup emits client-detached");
    assert!(matches!(
        detached,
        rmux_core::LifecycleEvent::ClientDetached { session_name, .. }
            if session_name == session
    ));

    handler.finish_attach(attach_pid, attach_id).await;
    assert!(
        timeout(
            Duration::from_millis(100),
            recv_client_detached_for(&mut events, attach_pid),
        )
        .await
        .is_err(),
        "a late forwarder finish must not repeat cleanup lifecycle"
    );
}

#[tokio::test]
async fn inactive_pane_osc52_closed_channel_still_finishes_attach_cleanup() {
    let handler = RequestHandler::new();
    let session = create_quiet_session(&handler, "osc52-send-cleanup").await;
    split_quiet_window(&handler, &session).await;
    set_server_option_by_name(&handler, "set-clipboard", "on").await;
    let inactive_pane_id = {
        let state = handler.state.lock().await;
        let session = state.sessions.session(&session).expect("session exists");
        let active = session.active_pane_id();
        session
            .window_at(0)
            .expect("window exists")
            .panes()
            .iter()
            .find_map(|pane| (Some(pane.id()) != active).then_some(pane.id()))
            .expect("inactive pane exists")
    };
    let attach_pid = 313;
    let control_rx = register_clipboard_attach(&handler, attach_pid, &session).await;
    drop(control_rx);
    let mut events = handler.subscribe_lifecycle_events();

    handler
        .handle_pane_alert_event(clipboard_alert(session.clone(), inactive_pane_id))
        .await;

    assert!(!handler
        .active_attach
        .lock()
        .await
        .by_pid
        .contains_key(&attach_pid));
    assert!(matches!(
        timeout(
            Duration::from_secs(2),
            recv_client_detached_for(&mut events, attach_pid),
        )
        .await
        .expect("closed control channel cleanup emits client-detached"),
        rmux_core::LifecycleEvent::ClientDetached { session_name, .. }
            if session_name == session
    ));
}

#[tokio::test]
async fn inactive_pane_osc52_backlog_is_bounded_by_encoded_bytes() {
    let handler = RequestHandler::new();
    let session = create_quiet_session(&handler, "osc52-byte-bounded-backlog").await;
    split_quiet_window(&handler, &session).await;
    set_server_option_by_name(&handler, "set-clipboard", "on").await;
    let inactive_pane_id = {
        let state = handler.state.lock().await;
        let session = state.sessions.session(&session).expect("session exists");
        let active = session.active_pane_id();
        session
            .window_at(0)
            .expect("window exists")
            .panes()
            .iter()
            .find_map(|pane| (Some(pane.id()) != active).then_some(pane.id()))
            .expect("inactive pane exists")
    };
    let mut control_rx = register_clipboard_attach(&handler, 311, &session).await;
    drain_attach_controls_until_idle(&mut control_rx).await;
    let control_backlog = {
        let active_attach = handler.active_attach.lock().await;
        active_attach
            .by_pid
            .get(&311)
            .expect("attach is registered")
            .control_backlog
            .clone()
    };
    let callback = handler.pane_alert_callback();
    let mut large_alert = clipboard_alert(session, inactive_pane_id);
    large_alert.clipboard_writes = vec![vec![b'A'; 512 * 1024]];

    for _ in 0..ATTACH_CONTROL_BACKLOG_LIMIT {
        callback(large_alert.clone());
    }

    let mut writes = 0;
    let mut detaches = 0;
    while let Ok(control) = control_rx.try_recv() {
        match control {
            AttachControl::ClipboardWrite { .. } => writes += 1,
            AttachControl::Detach => detaches += 1,
            _ => {}
        }
    }
    assert!(
        writes > 0 && writes < ATTACH_CONTROL_BACKLOG_LIMIT,
        "large clipboard controls must exhaust the byte budget before the message-count limit"
    );
    assert_eq!(detaches, 1);
    assert_eq!(
        control_backlog.load(Ordering::Acquire),
        0,
        "dropping queued clipboard controls releases every weighted reservation"
    );
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
            None,
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
        .await
        .expect("attach registration succeeds");
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
            title_change: None,
            clipboard_set: false,
            clipboard_writes: Vec::new(),
            mouse_mode_changed: false,
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
async fn pane_mouse_mode_alert_refreshes_the_active_attached_pane() {
    let handler = RequestHandler::new();
    let session = create_quiet_session(&handler, "active-mouse-mode-refresh").await;
    let pane_id = {
        let mut state = handler.state.lock().await;
        state
            .append_bytes_to_pane_transcript_for_test(&session, 0, 0, b"\x1b[?1003h\x1b[?1006h")
            .expect("active pane enables all-motion tracking");
        state
            .sessions
            .session(&session)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0).map(|pane| pane.id()))
            .expect("active pane exists")
    };

    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let uid = current_owner_uid();
    handler
        .register_attach_with_access(
            78,
            session.clone(),
            None,
            AttachRegistration {
                control_tx,
                control_backlog: Arc::new(AtomicUsize::new(0)),
                closing: Arc::new(AtomicBool::new(false)),
                persistent_overlay_epoch: Arc::new(AtomicU64::new(0)),
                terminal_context: OuterTerminalContext::from_pairs(&[("TERM", "xterm-256color")]),
                flags: ClientFlags::default(),
                render_stream: false,
                uid,
                user: rmux_os::identity::UserIdentity::Uid(uid),
                can_write: true,
                client_size: Some(TerminalSize { cols: 80, rows: 24 }),
            },
        )
        .await
        .expect("attach registration succeeds");

    handler.pane_alert_callback()(crate::pane_io::PaneAlertEvent {
        session_name: session,
        pane_id,
        bell_count: 0,
        title_changed: false,
        title_change: None,
        clipboard_set: false,
        clipboard_writes: Vec::new(),
        mouse_mode_changed: true,
        queue_activity_alert: false,
        generation: None,
    });

    let control = timeout(Duration::from_secs(2), control_rx.recv())
        .await
        .expect("active mouse-mode change should enqueue a refresh")
        .expect("attach control channel remains open");
    let AttachControl::Switch(target) = control else {
        panic!("expected active mouse-mode switch refresh, got {control:?}");
    };
    let start = target.outer_terminal.attach_start_sequence();
    assert!(
        start
            .windows(b"\x1b[?1003h".len())
            .any(|window| window == b"\x1b[?1003h"),
        "active pane all-motion tracking must reach the outer terminal"
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
    let session = create_quiet_session(&handler, "alerts-name").await;
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
        title_change: None,
        clipboard_set: false,
        clipboard_writes: Vec::new(),
        mouse_mode_changed: false,
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
    let session = create_quiet_session(&handler, "alerts-name-off").await;
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
            title_change: None,
            clipboard_set: false,
            clipboard_writes: Vec::new(),
            mouse_mode_changed: false,
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
    let alpha = create_quiet_session(&handler, "alerts-group-alpha").await;
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
            title_change: None,
            clipboard_set: false,
            clipboard_writes: Vec::new(),
            mouse_mode_changed: false,
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
async fn grouped_window_silence_option_synchronizes_runtime_timers_for_every_alias() {
    let handler = RequestHandler::new();
    let owner = create_quiet_session(&handler, "silence-group-owner").await;
    let peer = session_name("silence-group-peer");
    let response = handler
        .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
            session_name: Some(peer.clone()),
            working_directory: None,
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
            group_target: Some(owner.clone()),
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
        ScopeSelector::Window(WindowTarget::with_window(peer.clone(), 0)),
        OptionName::MonitorSilence,
        "1",
    )
    .await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
    loop {
        let both_silent = {
            let state = handler.state.lock().await;
            [owner.clone(), peer.clone()]
                .into_iter()
                .all(|session_name| {
                    state
                        .sessions
                        .session(&session_name)
                        .expect("group member exists")
                        .winlink_alert_flags(0)
                        .contains(WINLINK_SILENCE)
                })
        };
        if both_silent {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "monitor-silence timer must fire for every grouped window alias"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn new_group_peer_inherits_existing_silence_deadline() {
    let handler = RequestHandler::new();
    let owner = create_quiet_session(&handler, "silence-deadline-owner").await;
    set_option(
        &handler,
        ScopeSelector::Global,
        OptionName::MonitorSilence,
        "60",
    )
    .await;
    let owner_target = WindowTarget::with_window(owner.clone(), 0);
    let owner_deadline = handler
        .silence_timer_snapshot_for_test(&owner_target)
        .expect("owner silence timer is armed")
        .1;

    let peer = create_grouped_session(&handler, "silence-deadline-peer", &owner).await;
    let peer_deadline = handler
        .silence_timer_snapshot_for_test(&WindowTarget::with_window(peer, 0))
        .expect("new peer silence timer is inherited")
        .1;

    assert_eq!(peer_deadline, owner_deadline);
}

#[tokio::test]
async fn new_group_peer_does_not_rearm_expired_silence_state() {
    let handler = RequestHandler::new();
    let owner = create_quiet_session(&handler, "silence-expired-owner").await;
    set_option(
        &handler,
        ScopeSelector::Global,
        OptionName::MonitorSilence,
        "60",
    )
    .await;
    let owner_target = WindowTarget::with_window(owner.clone(), 0);
    let (session_id, window_id, generation) = handler
        .silence_timer_identity_for_test(&owner_target)
        .expect("owner silence timer is armed");
    handler
        .expire_silence_timer_for_test(owner_target.clone(), session_id, window_id, generation)
        .await;
    assert!(handler
        .silence_timer_snapshot_for_test(&owner_target)
        .is_none());

    let peer = create_grouped_session(&handler, "silence-expired-peer", &owner).await;
    assert!(handler
        .silence_timer_snapshot_for_test(&WindowTarget::with_window(peer, 0))
        .is_none());
}

#[tokio::test]
async fn new_group_peer_inherits_duplicate_alias_timer_state_by_slot() {
    let handler = RequestHandler::new();
    let owner = create_quiet_session(&handler, "silence-duplicate-owner").await;
    let linked = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(owner.clone(), 0),
            target: WindowTarget::with_window(owner.clone(), 1),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(linked, Response::LinkWindow(_)), "{linked:?}");
    set_option(
        &handler,
        ScopeSelector::Global,
        OptionName::MonitorSilence,
        "60",
    )
    .await;
    let first = WindowTarget::with_window(owner.clone(), 0);
    let second = WindowTarget::with_window(owner.clone(), 1);
    let (session_id, window_id, generation) = handler
        .silence_timer_identity_for_test(&first)
        .expect("first duplicate timer is armed");
    handler
        .expire_silence_timer_for_test(first.clone(), session_id, window_id, generation)
        .await;
    let second_deadline = handler
        .silence_timer_snapshot_for_test(&second)
        .expect("second duplicate timer remains armed")
        .1;

    let peer = create_grouped_session(&handler, "silence-duplicate-peer", &owner).await;
    assert!(handler
        .silence_timer_snapshot_for_test(&WindowTarget::with_window(peer.clone(), 0))
        .is_none());
    assert_eq!(
        handler
            .silence_timer_snapshot_for_test(&WindowTarget::with_window(peer, 1))
            .expect("second peer duplicate inherits its matching timer")
            .1,
        second_deadline
    );
}

#[tokio::test]
async fn new_window_insertion_preserves_group_timer_deadlines_and_arms_new_peers() {
    let handler = RequestHandler::new();
    let owner = create_quiet_session(&handler, "silence-new-window-owner").await;
    let _ = create_quiet_window(&handler, &owner).await;
    let peer = create_grouped_session(&handler, "silence-new-window-peer", &owner).await;

    set_option(
        &handler,
        ScopeSelector::Global,
        OptionName::MonitorSilence,
        "60",
    )
    .await;

    let before = {
        let state = handler.state.lock().await;
        let mut snapshots = Vec::new();
        for session_name in [&owner, &peer] {
            let session = state
                .sessions
                .session(session_name)
                .expect("group member exists before insertion");
            for window_index in 0..=1 {
                let target = WindowTarget::with_window(session_name.clone(), window_index);
                let window_id = session
                    .window_at(window_index)
                    .expect("pre-insertion window exists")
                    .id();
                let timer = handler
                    .silence_timer_snapshot_for_test(&target)
                    .expect("pre-insertion timer is armed");
                snapshots.push((
                    session_name.clone(),
                    session.id(),
                    window_index,
                    window_id,
                    timer,
                ));
            }
        }
        snapshots
    };

    let response = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: owner.clone(),
            name: None,
            detached: true,
            start_directory: None,
            environment: None,
            command: Some(quiet_alert_command()),
            process_command: None,
            target_window_index: Some(0),
            insert_at_target: true,
        })))
        .await;
    assert!(matches!(response, Response::NewWindow(_)), "{response:?}");

    for (session_name, session_id, previous_index, window_id, previous_timer) in before {
        let shifted = WindowTarget::with_window(session_name, previous_index + 1);
        let timer = handler
            .silence_timer_snapshot_for_test(&shifted)
            .expect("existing timer follows the shifted window");
        assert_eq!(
            timer.1, previous_timer.1,
            "new-window must preserve the existing absolute silence deadline"
        );
        let identity = handler
            .silence_timer_identity_for_test(&shifted)
            .expect("shifted timer identity exists");
        assert_eq!((identity.0, identity.1), (session_id, window_id));
    }

    for session_name in [&owner, &peer] {
        let target = WindowTarget::with_window(session_name.clone(), 0);
        let identity = handler
            .silence_timer_identity_for_test(&target)
            .expect("new grouped winlink is armed");
        let state = handler.state.lock().await;
        let session = state
            .sessions
            .session(session_name)
            .expect("group member survives insertion");
        assert_eq!(identity.0, session.id());
        assert_eq!(
            identity.1,
            session
                .window_at(0)
                .expect("new grouped winlink exists")
                .id()
        );
    }

    let shifted_owner = WindowTarget::with_window(owner, 1);
    let before_set_option = handler
        .silence_timer_snapshot_for_test(&shifted_owner)
        .expect("shifted owner timer remains armed");
    set_option(
        &handler,
        ScopeSelector::Window(shifted_owner.clone()),
        OptionName::MonitorSilence,
        "60",
    )
    .await;
    let after_set_option = handler
        .silence_timer_snapshot_for_test(&shifted_owner)
        .expect("SetOption rearms the shifted owner timer");
    assert!(after_set_option.0 > before_set_option.0);
    assert!(after_set_option.1 > before_set_option.1);
}

async fn assert_grouped_mixed_renumber_preserves_silence_timers(label: &str, unlink: bool) {
    let handler = RequestHandler::new();
    let owner = create_quiet_session(&handler, &format!("{label}-owner")).await;
    let _ = create_quiet_window(&handler, &owner).await;
    let _ = create_quiet_window(&handler, &owner).await;
    let peer = create_grouped_session(&handler, &format!("{label}-peer"), &owner).await;
    let external = create_quiet_session(&handler, &format!("{label}-external")).await;
    let external_alias = WindowTarget::with_window(external, 1);
    let response = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(owner.clone(), 2),
            target: external_alias.clone(),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(response, Response::LinkWindow(_)), "{response:?}");

    set_option(
        &handler,
        ScopeSelector::Session(owner.clone()),
        OptionName::RenumberWindows,
        "on",
    )
    .await;

    set_option(
        &handler,
        ScopeSelector::Global,
        OptionName::MonitorSilence,
        "60",
    )
    .await;
    let (external_identity_before, external_timer_before) = (
        handler
            .silence_timer_identity_for_test(&external_alias)
            .expect("external alias timer identity is armed"),
        handler
            .silence_timer_snapshot_for_test(&external_alias)
            .expect("external alias timer is armed"),
    );

    let before = {
        let state = handler.state.lock().await;
        [&owner, &peer]
            .into_iter()
            .map(|session_name| {
                let session = state
                    .sessions
                    .session(session_name)
                    .expect("group member exists before removal");
                let target = WindowTarget::with_window(session_name.clone(), 2);
                let window_id = session.window_at(2).expect("window two exists").id();
                let timer = handler
                    .silence_timer_snapshot_for_test(&target)
                    .expect("window two timer is armed");
                (session_name.clone(), session.id(), window_id, timer)
            })
            .collect::<Vec<_>>()
    };

    let target = WindowTarget::with_window(owner.clone(), 1);
    let response = if unlink {
        handler
            .handle(Request::UnlinkWindow(UnlinkWindowRequest {
                target,
                kill_if_last: true,
            }))
            .await
    } else {
        handler
            .handle(Request::KillWindow(KillWindowRequest {
                target,
                kill_all_others: false,
            }))
            .await
    };
    assert!(
        matches!(
            response,
            Response::KillWindow(_) | Response::UnlinkWindow(_)
        ),
        "grouped removal succeeds: {response:?}"
    );

    for (session_name, session_id, window_id, previous_timer) in before {
        let current_index = {
            let state = handler.state.lock().await;
            let session = state
                .sessions
                .session(&session_name)
                .expect("group member survives removal");
            session
                .windows()
                .iter()
                .find_map(|(window_index, window)| {
                    (window.id() == window_id).then_some(*window_index)
                })
                .expect("surviving window identity remains in every group member")
        };
        let shifted = WindowTarget::with_window(session_name.clone(), current_index);
        let timer = handler
            .silence_timer_snapshot_for_test(&shifted)
            .expect("every grouped alias keeps its silence timer at the final index");
        assert_eq!(timer.1, previous_timer.1);
        let identity = handler
            .silence_timer_identity_for_test(&shifted)
            .expect("surviving timer identity exists");
        assert_eq!((identity.0, identity.1), (session_id, window_id));
        if current_index != 2 {
            assert_eq!(
                handler
                    .silence_timer_snapshot_for_test(&WindowTarget::with_window(session_name, 2,)),
                None,
                "the stale pre-renumber timer key is removed"
            );
        }
    }
    assert_eq!(
        handler
            .silence_timer_identity_for_test(&external_alias)
            .expect("unrelated external alias timer survives"),
        external_identity_before,
    );
    assert_eq!(
        handler
            .silence_timer_snapshot_for_test(&external_alias)
            .expect("unrelated external alias deadline survives")
            .1,
        external_timer_before.1,
    );
}

#[tokio::test]
async fn grouped_mixed_renumber_kill_and_unlink_preserve_every_silence_timer() {
    assert_grouped_mixed_renumber_preserves_silence_timers("silence-mixed-kill", false).await;
    assert_grouped_mixed_renumber_preserves_silence_timers("silence-mixed-unlink", true).await;
}

#[tokio::test]
async fn link_window_arms_silence_timer_for_non_syntactic_group_peer() {
    let handler = RequestHandler::new();
    let owner = create_quiet_session(&handler, "silence-link-owner").await;
    let peer = create_grouped_session(&handler, "silence-link-peer", &owner).await;
    let source = create_quiet_session(&handler, "silence-link-source").await;
    let source_target = WindowTarget::with_window(source.clone(), 0);
    let owner_target = WindowTarget::with_window(owner.clone(), 1);
    let peer_target = WindowTarget::with_window(peer.clone(), 1);

    set_option(
        &handler,
        ScopeSelector::Window(source_target.clone()),
        OptionName::MonitorSilence,
        "60",
    )
    .await;
    assert_eq!(
        handler.silence_timer_generation_for_test(&peer_target),
        None,
        "the grouped peer must not have a timer before its linked window exists"
    );

    let response = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: source_target.clone(),
            target: owner_target.clone(),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(response, Response::LinkWindow(_)), "{response:?}");

    for target in [source_target, owner_target, peer_target] {
        assert!(
            handler.silence_timer_generation_for_test(&target).is_some(),
            "link-window must arm monitor-silence for alias {target}"
        );
    }
}

#[tokio::test]
async fn link_window_rearms_existing_silence_timer_for_non_syntactic_group_peer() {
    let handler = RequestHandler::new();
    let owner = create_quiet_session(&handler, "silence-relink-owner").await;
    let peer = create_grouped_session(&handler, "silence-relink-peer", &owner).await;
    let source = create_quiet_session(&handler, "silence-relink-source").await;
    let unrelated = create_quiet_session(&handler, "silence-relink-unrelated").await;
    let source_target = WindowTarget::with_window(source.clone(), 0);
    let owner_target = WindowTarget::with_window(owner.clone(), 0);
    let peer_target = WindowTarget::with_window(peer.clone(), 0);
    let unrelated_target = WindowTarget::with_window(unrelated, 0);

    set_option(
        &handler,
        ScopeSelector::Window(peer_target.clone()),
        OptionName::MonitorSilence,
        "60",
    )
    .await;
    let previous_peer_generation = handler
        .silence_timer_generation_for_test(&peer_target)
        .expect("group peer timer is initially armed");

    set_option(
        &handler,
        ScopeSelector::Window(source_target.clone()),
        OptionName::MonitorSilence,
        "60",
    )
    .await;
    set_option(
        &handler,
        ScopeSelector::Window(unrelated_target.clone()),
        OptionName::MonitorSilence,
        "60",
    )
    .await;
    let unrelated_generation = handler
        .silence_timer_generation_for_test(&unrelated_target)
        .expect("unrelated timer is initially armed");
    assert_eq!(
        handler.silence_timer_generation_for_test(&peer_target),
        Some(previous_peer_generation),
        "source option setup must not rearm the unrelated group peer"
    );

    let response = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: source_target,
            target: owner_target,
            after: false,
            before: false,
            kill_destination: true,
            detached: true,
        }))
        .await;
    assert!(matches!(response, Response::LinkWindow(_)), "{response:?}");

    let next_peer_generation = handler
        .silence_timer_generation_for_test(&peer_target)
        .expect("group peer timer remains armed after replacement");
    assert!(
        next_peer_generation > previous_peer_generation,
        "link-window must replace the non-syntactic peer's stale timer"
    );
    assert_eq!(
        handler.silence_timer_generation_for_test(&unrelated_target),
        Some(unrelated_generation),
        "link-window must not rearm silence timers outside the affected family"
    );
}

#[tokio::test]
async fn link_window_does_not_rearm_unrelated_same_session_silence_timer() {
    let handler = RequestHandler::new();
    let destination = create_quiet_session(&handler, "silence-same-session-destination").await;
    let source = create_quiet_session(&handler, "silence-same-session-source").await;
    let unrelated_target = WindowTarget::with_window(destination.clone(), 0);
    let link_target = create_window(&handler, &destination).await;
    let source_target = WindowTarget::with_window(source, 0);

    set_option(
        &handler,
        ScopeSelector::Window(unrelated_target.clone()),
        OptionName::MonitorSilence,
        "60",
    )
    .await;
    set_option(
        &handler,
        ScopeSelector::Window(source_target.clone()),
        OptionName::MonitorSilence,
        "60",
    )
    .await;
    let unrelated_generation = handler
        .silence_timer_generation_for_test(&unrelated_target)
        .expect("unrelated same-session timer is initially armed");

    let response = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: source_target,
            target: link_target.clone(),
            after: false,
            before: false,
            kill_destination: true,
            detached: true,
        }))
        .await;
    assert!(matches!(response, Response::LinkWindow(_)), "{response:?}");

    assert_eq!(
        handler.silence_timer_generation_for_test(&unrelated_target),
        Some(unrelated_generation),
        "link-window must not postpone an unrelated same-session silence timer"
    );
    assert!(
        handler
            .silence_timer_generation_for_test(&link_target)
            .is_some(),
        "the newly linked destination timer remains armed"
    );
}

#[tokio::test]
async fn link_window_before_preserves_unchanged_and_reindexed_silence_deadlines() {
    let handler = RequestHandler::new();
    let destination = create_quiet_session(&handler, "silence-before-destination").await;
    let source = create_quiet_session(&handler, "silence-before-source").await;
    let unchanged_target = WindowTarget::with_window(destination.clone(), 0);
    let shifted_target = create_quiet_window(&handler, &destination).await;
    let source_target = WindowTarget::with_window(source, 0);

    set_option(
        &handler,
        ScopeSelector::Window(unchanged_target.clone()),
        OptionName::MonitorSilence,
        "60",
    )
    .await;
    set_option(
        &handler,
        ScopeSelector::Window(shifted_target.clone()),
        OptionName::MonitorSilence,
        "60",
    )
    .await;
    set_option(
        &handler,
        ScopeSelector::Window(source_target.clone()),
        OptionName::MonitorSilence,
        "60",
    )
    .await;
    let unchanged_snapshot = handler
        .silence_timer_snapshot_for_test(&unchanged_target)
        .expect("unchanged timer is armed");
    let shifted_snapshot = handler
        .silence_timer_snapshot_for_test(&shifted_target)
        .expect("shifted timer is armed");

    let response = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: source_target,
            target: shifted_target,
            after: false,
            before: true,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(response, Response::LinkWindow(_)), "{response:?}");

    assert_eq!(
        handler.silence_timer_snapshot_for_test(&unchanged_target),
        Some(unchanged_snapshot),
        "a window below the insertion keeps its timer generation and deadline"
    );
    let reindexed_target = WindowTarget::with_window(destination, 2);
    let reindexed_snapshot = handler
        .silence_timer_snapshot_for_test(&reindexed_target)
        .expect("reindexed window keeps an armed timer");
    assert_eq!(
        reindexed_snapshot.1, shifted_snapshot.1,
        "a reindexed timer keeps its original deadline"
    );
}

#[tokio::test]
async fn link_window_after_moves_silence_expiry_to_new_target_without_delay() {
    let handler = RequestHandler::new();
    let destination = create_quiet_session(&handler, "silence-after-destination").await;
    let source = create_quiet_session(&handler, "silence-after-source").await;
    let after_anchor = WindowTarget::with_window(destination.clone(), 0);
    let shifted_target = create_quiet_window(&handler, &destination).await;

    set_option(
        &handler,
        ScopeSelector::Window(shifted_target.clone()),
        OptionName::MonitorSilence,
        "4",
    )
    .await;
    // ConPTY can deliver the final quiet-command startup activity after pane
    // startup itself has completed. Establish the timer baseline only after
    // that coalesced activity has drained; this test exercises structural
    // rekeying, not startup scheduling latency.
    let _ = wait_for_silence_timer_to_settle(&handler, &shifted_target).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    // Capture the reference deadline immediately before the link. The quiet
    // pane's ConPTY startup output can be processed late under suite load and
    // legally re-arm the silence timer through the activity reset (activity
    // restarts the idle clock, tmux parity), so a deadline captured before the
    // sleep conflates that re-arm with the rekey restart this test guards
    // against. A rekey restart still trips the assertion: it would move the
    // post-link deadline ~1s past this pre-link capture.
    let pre_link_deadline = handler
        .silence_timer_snapshot_for_test(&shifted_target)
        .expect("timer to shift is still armed before the link")
        .1;

    let response = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(source, 0),
            target: after_anchor,
            after: true,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(response, Response::LinkWindow(_)), "{response:?}");

    assert_eq!(
        handler.silence_timer_generation_for_test(&shifted_target),
        None,
        "the old target must not retain the shifted window timer"
    );
    let reindexed_target = WindowTarget::with_window(destination.clone(), 2);
    assert_eq!(
        handler
            .silence_timer_snapshot_for_test(&reindexed_target)
            .expect("shifted timer exists at the new target")
            .1,
        pre_link_deadline,
        "rekeying must not restart the elapsed silence interval"
    );

    let deadline = pre_link_deadline + Duration::from_secs(2);
    loop {
        let fired_at_new_target = {
            let state = handler.state.lock().await;
            state
                .sessions
                .session(&destination)
                .expect("destination session exists")
                .winlink_alert_flags(reindexed_target.window_index())
                .contains(WINLINK_SILENCE)
        };
        if fired_at_new_target {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "shifted silence timer did not fire at its preserved deadline"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let state = handler.state.lock().await;
    assert!(
        !state
            .sessions
            .session(&destination)
            .expect("destination session exists")
            .winlink_alert_flags(shifted_target.window_index())
            .contains(WINLINK_SILENCE),
        "the aborted old-target task must not raise a silence flag"
    );
}

#[tokio::test]
async fn prepared_alert_hook_follows_reindexed_window_identity() {
    let handler = RequestHandler::new();
    let destination = create_quiet_session(&handler, "alert-hook-reindex-destination").await;
    let alerted = create_quiet_window(&handler, &destination).await;
    let source = create_quiet_session(&handler, "alert-hook-reindex-source").await;
    set_option(
        &handler,
        ScopeSelector::Window(alerted.clone()),
        OptionName::MonitorActivity,
        "on",
    )
    .await;
    let alerted_window_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&destination)
            .and_then(|session| session.window_at(alerted.window_index()))
            .expect("alerted window exists before reindex")
            .id()
    };
    let response = handler
        .handle(Request::SetHookMutation(SetHookMutationRequest {
            scope: ScopeSelector::Window(alerted.clone()),
            hook: HookName::AlertActivity,
            command: Some(format!(
                "if-shell -F '#{{==:#{{window_id}}:#{{window_index}}:#{{hook_window}},{alerted_window_id}:2:{alerted_window_id}}}' 'set-buffer -b stable-alert ok' 'set-buffer -b stable-alert bad'"
            )),
            lifecycle: HookLifecycle::Persistent,
            append: false,
            unset: false,
            run_immediately: false,
            index: None,
        }))
        .await;
    assert!(matches!(response, Response::SetHook(_)), "{response:?}");
    let plans = {
        let mut state = handler.state.lock().await;
        handler.alerts_queue_window_locked(&mut state, alerted.clone(), WINDOW_ACTIVITY, 0)
    };
    assert_eq!(plans.len(), 1, "activity materializes one alert plan");

    let response = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(source, 0),
            target: WindowTarget::with_window(destination.clone(), 0),
            after: true,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(response, Response::LinkWindow(_)), "{response:?}");
    {
        let state = handler.state.lock().await;
        let session = state
            .sessions
            .session(&destination)
            .expect("destination exists after reindex");
        assert_eq!(
            session.window_at(2).expect("alerted window moved").id(),
            alerted_window_id
        );
        assert_ne!(
            session.window_at(1).expect("inserted alias exists").id(),
            alerted_window_id
        );
    }

    let mut lifecycle = handler.subscribe_lifecycle_events();
    handler.execute_alert_plans(plans).await;
    let event = recv_lifecycle_hook(&mut lifecycle, HookName::AlertActivity).await;
    let hook_window = event
        .formats
        .iter()
        .find_map(|(name, value)| (name == "hook_window").then_some(value.as_str()));
    let expected_hook_window = alerted_window_id.to_string();
    assert_eq!(hook_window, Some(expected_hook_window.as_str()));
    assert_eq!(event.hooks.len(), 1, "old window binding is frozen in plan");

    handler.dispatch_lifecycle_hook(event).await;
    wait_for_buffer(&handler, "stable-alert", "ok").await;
}

#[tokio::test]
async fn prepared_alert_hooks_fail_closed_after_stable_window_is_replaced() {
    let handler = RequestHandler::new();
    let destination = create_quiet_session(&handler, "alert-hook-replace-destination").await;
    let alerted = create_quiet_window(&handler, &destination).await;
    let source = create_quiet_session(&handler, "alert-hook-replace-source").await;
    set_option(
        &handler,
        ScopeSelector::Window(alerted.clone()),
        OptionName::MonitorActivity,
        "on",
    )
    .await;

    let (alerted_window_id, replacement_window_id) = {
        let state = handler.state.lock().await;
        let alerted_window_id = state
            .sessions
            .session(&destination)
            .and_then(|session| session.window_at(alerted.window_index()))
            .expect("alerted window exists")
            .id();
        let replacement_window_id = state
            .sessions
            .session(&source)
            .and_then(|session| session.window_at(0))
            .expect("replacement window exists")
            .id();
        (alerted_window_id, replacement_window_id)
    };

    for (command, append) in [
        (
            format!(
                "link-window -k -s {source}:0 -t {destination}:{}",
                alerted.window_index()
            ),
            false,
        ),
        ("set-buffer -b stale-alert-dispatch ran".to_owned(), true),
    ] {
        let response = handler
            .handle(Request::SetHookMutation(SetHookMutationRequest {
                scope: ScopeSelector::Window(alerted.clone()),
                hook: HookName::AlertActivity,
                command: Some(command),
                lifecycle: HookLifecycle::Persistent,
                append,
                unset: false,
                run_immediately: false,
                index: None,
            }))
            .await;
        assert!(matches!(response, Response::SetHook(_)), "{response:?}");
    }

    let plans = {
        let mut state = handler.state.lock().await;
        handler.alerts_queue_window_locked(&mut state, alerted.clone(), WINDOW_ACTIVITY, 0)
    };
    assert_eq!(plans.len(), 1, "activity materializes one alert plan");

    let mut lifecycle = handler.subscribe_lifecycle_events();
    handler.execute_alert_plans(plans).await;
    let event = recv_lifecycle_hook(&mut lifecycle, HookName::AlertActivity).await;
    assert_eq!(event.hooks.len(), 2, "both alert hooks are frozen in plan");

    handler.dispatch_lifecycle_hook(event).await;

    let state = handler.state.lock().await;
    let destination_session = state
        .sessions
        .session(&destination)
        .expect("destination survives replacement");
    let replacement = destination_session
        .window_at(alerted.window_index())
        .expect("replacement occupies the old numeric slot");
    assert_eq!(replacement.id(), replacement_window_id);
    assert_ne!(replacement.id(), alerted_window_id);
    assert!(
        state.buffers.show(Some("stale-alert-dispatch")).is_err(),
        "later alert hooks must not run against a replacement numeric target"
    );
}

#[tokio::test]
async fn concurrent_relative_links_serialize_model_and_timer_rekeys() {
    let handler = RequestHandler::new();
    let destination = create_quiet_session(&handler, "silence-concurrent-links-destination").await;
    let first_shifted = create_quiet_window(&handler, &destination).await;
    let second_shifted = create_quiet_window(&handler, &destination).await;
    let source_one = create_quiet_session(&handler, "silence-concurrent-links-source-one").await;
    let source_two = create_quiet_session(&handler, "silence-concurrent-links-source-two").await;

    for target in [&first_shifted, &second_shifted] {
        set_option(
            &handler,
            ScopeSelector::Window(target.clone()),
            OptionName::MonitorSilence,
            "60",
        )
        .await;
    }
    let first_deadline = handler
        .silence_timer_snapshot_for_test(&first_shifted)
        .expect("first shifted timer exists")
        .1;
    let second_deadline = handler
        .silence_timer_snapshot_for_test(&second_shifted)
        .expect("second shifted timer exists")
        .1;

    let pause = handler.install_silence_timer_apply_pause();
    let first_handler = handler.clone();
    let first_destination = destination.clone();
    let first_link = std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("first link test runtime")
            .block_on(first_handler.handle(Request::LinkWindow(LinkWindowRequest {
                source: WindowTarget::with_window(source_one, 0),
                target: WindowTarget::with_window(first_destination, 0),
                after: true,
                before: false,
                kill_destination: false,
                detached: true,
            })))
    });
    pause.reached.wait();

    let second_handler = handler.clone();
    let second_destination = destination.clone();
    let (completed_tx, completed_rx) = std::sync::mpsc::channel();
    let second_link = std::thread::spawn(move || {
        let response = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("second link test runtime")
            .block_on(
                second_handler.handle(Request::LinkWindow(LinkWindowRequest {
                    source: WindowTarget::with_window(source_two, 0),
                    target: WindowTarget::with_window(second_destination, 0),
                    after: true,
                    before: false,
                    kill_destination: false,
                    detached: true,
                })),
            );
        let _ = completed_tx.send(());
        response
    });
    assert!(
        completed_rx
            .recv_timeout(std::time::Duration::from_millis(100))
            .is_err(),
        "the second link must wait while the first owns state through timer apply"
    );
    pause.release.wait();

    let first_response = first_link.join().expect("first link thread joins");
    let second_response = second_link.join().expect("second link thread joins");
    assert!(matches!(first_response, Response::LinkWindow(_)));
    assert!(matches!(second_response, Response::LinkWindow(_)));

    assert_eq!(
        handler
            .silence_timer_snapshot_for_test(&WindowTarget::with_window(destination.clone(), 3,))
            .expect("first original timer reaches final index")
            .1,
        first_deadline
    );
    assert_eq!(
        handler
            .silence_timer_snapshot_for_test(&WindowTarget::with_window(destination, 4))
            .expect("second original timer reaches final index")
            .1,
        second_deadline
    );
}

#[tokio::test]
async fn activity_waiting_on_relative_link_resets_the_rekeyed_timer() {
    let handler = RequestHandler::new();
    let destination = create_quiet_session(&handler, "silence-link-activity-destination").await;
    let shifted = create_quiet_window(&handler, &destination).await;
    let source = create_quiet_session(&handler, "silence-link-activity-source").await;
    set_option(
        &handler,
        ScopeSelector::Window(shifted.clone()),
        OptionName::MonitorSilence,
        "60",
    )
    .await;
    let original_deadline = handler
        .silence_timer_snapshot_for_test(&shifted)
        .expect("timer exists before link")
        .1;

    let pause = handler.install_silence_timer_apply_pause();
    let link_handler = handler.clone();
    let link_destination = destination.clone();
    let link = std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("link activity test runtime")
            .block_on(link_handler.handle(Request::LinkWindow(LinkWindowRequest {
                source: WindowTarget::with_window(source, 0),
                target: WindowTarget::with_window(link_destination, 0),
                after: true,
                before: false,
                kill_destination: false,
                detached: true,
            })))
    });
    pause.reached.wait();

    let activity_target = WindowTarget::with_window(destination.clone(), 2);
    let activity_handler = handler.clone();
    let activity_target_task = activity_target.clone();
    let (completed_tx, completed_rx) = std::sync::mpsc::channel();
    let activity = std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("activity test runtime")
            .block_on(activity_handler.alerts_queue_window(activity_target_task, WINDOW_ACTIVITY));
        let _ = completed_tx.send(());
    });
    assert!(
        completed_rx
            .recv_timeout(std::time::Duration::from_millis(100))
            .is_err(),
        "activity must wait for the link's state+timer transaction"
    );
    pause.release.wait();
    assert!(matches!(
        link.join().expect("link thread joins"),
        Response::LinkWindow(_)
    ));
    activity.join().expect("activity thread joins");

    let reset_deadline = handler
        .silence_timer_snapshot_for_test(&activity_target)
        .expect("activity keeps rekeyed timer armed")
        .1;
    assert!(
        reset_deadline > original_deadline,
        "activity ordered after the link must win over the preserved link deadline"
    );
}

#[tokio::test]
async fn expired_silence_flag_follows_window_across_reindex() {
    let handler = RequestHandler::new();
    let destination = create_quiet_session(&handler, "silence-expiry-reindex-destination").await;
    let shifted = create_quiet_window(&handler, &destination).await;
    let source = create_quiet_session(&handler, "silence-expiry-reindex-source").await;

    // Keep the timer on an independent current-thread runtime so its expiry
    // can hold the model lock while this test coordinates a concurrent link.
    let pause = handler.install_silence_timer_apply_pause();
    let timer_handler = handler.clone();
    let timer_target = shifted.clone();
    let (timer_armed_tx, timer_armed_rx) = std::sync::mpsc::channel();
    let (timer_runtime_release_tx, timer_runtime_release_rx) = tokio::sync::oneshot::channel();
    let timer_runtime = std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("silence expiry test runtime")
            .block_on(async move {
                set_option(
                    &timer_handler,
                    ScopeSelector::Window(timer_target),
                    OptionName::MonitorSilence,
                    "1",
                )
                .await;
                timer_armed_tx
                    .send(())
                    .expect("timer armed receiver remains alive");
                let _ = timer_runtime_release_rx.await;
                tokio::time::sleep(Duration::from_millis(50)).await;
            });
    });
    timer_armed_rx
        .recv_timeout(std::time::Duration::from_secs(3))
        .expect("silence timer is armed on its runtime");
    pause.reached.wait();

    let link_handler = handler.clone();
    let link_destination = destination.clone();
    let (link_completed_tx, link_completed_rx) = std::sync::mpsc::channel();
    let link = std::thread::spawn(move || {
        let response = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("silence expiry link runtime")
            .block_on(link_handler.handle(Request::LinkWindow(LinkWindowRequest {
                source: WindowTarget::with_window(source, 0),
                target: WindowTarget::with_window(link_destination, 0),
                after: true,
                before: false,
                kill_destination: false,
                detached: true,
            })));
        let _ = link_completed_tx.send(());
        response
    });
    assert!(
        link_completed_rx
            .recv_timeout(std::time::Duration::from_millis(100))
            .is_err(),
        "link must wait until expiry materializes its alert under the model lock"
    );
    pause.release.wait();
    let response = link.join().expect("expiry link thread joins");
    assert!(matches!(response, Response::LinkWindow(_)), "{response:?}");

    let reindexed = WindowTarget::with_window(destination.clone(), 2);
    let replacement = WindowTarget::with_window(destination, 1);
    let (reindexed_has_silence, replacement_has_silence) = {
        let state = handler.state.lock().await;
        let session = state
            .sessions
            .session(reindexed.session_name())
            .expect("destination session exists after link");
        (
            session
                .winlink_alert_flags(reindexed.window_index())
                .contains(WINLINK_SILENCE),
            session
                .winlink_alert_flags(replacement.window_index())
                .contains(WINLINK_SILENCE),
        )
    };
    let _ = timer_runtime_release_tx.send(());
    timer_runtime.join().expect("silence timer thread joins");

    assert!(
        reindexed_has_silence,
        "the expired flag must follow the pre-link window to its new index"
    );
    assert!(
        !replacement_has_silence,
        "the newly inserted alias must not inherit the pre-link expiry"
    );
}

#[tokio::test]
async fn activity_clears_persistent_silence_and_allows_second_expiry() {
    let handler = RequestHandler::new();
    let session = create_quiet_session(&handler, "silence-persistent-reset").await;
    let target = create_quiet_window(&handler, &session).await;
    set_option(
        &handler,
        ScopeSelector::Window(target.clone()),
        OptionName::MonitorSilence,
        "1",
    )
    .await;
    wait_for_winlink_flag(
        &handler,
        &target,
        WINLINK_SILENCE,
        true,
        Duration::from_secs(3),
    )
    .await;

    handler
        .alerts_queue_window(target.clone(), WINDOW_ACTIVITY)
        .await;
    wait_for_winlink_flag(
        &handler,
        &target,
        WINLINK_SILENCE,
        false,
        Duration::from_millis(200),
    )
    .await;
    wait_for_winlink_flag(
        &handler,
        &target,
        WINLINK_SILENCE,
        true,
        Duration::from_secs(3),
    )
    .await;
}

#[tokio::test]
async fn window_alias_activity_resets_silence_timers_for_entire_family() {
    let handler = RequestHandler::new();
    let owner = create_quiet_session(&handler, "silence-reset-owner").await;
    let peer = session_name("silence-reset-peer");
    let response = handler
        .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
            session_name: Some(peer.clone()),
            working_directory: None,
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
            group_target: Some(owner.clone()),
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
    let linked = create_quiet_session(&handler, "silence-reset-linked").await;
    let response = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(owner.clone(), 0),
            target: WindowTarget::with_window(linked.clone(), 1),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(response, Response::LinkWindow(_)), "{response:?}");
    let family_targets = [
        WindowTarget::with_window(owner.clone(), 0),
        WindowTarget::with_window(peer.clone(), 0),
        WindowTarget::with_window(linked.clone(), 1),
    ];

    set_option(
        &handler,
        ScopeSelector::Window(WindowTarget::with_window(peer.clone(), 0)),
        OptionName::MonitorSilence,
        "2",
    )
    .await;

    tokio::time::sleep(Duration::from_millis(1200)).await;
    let pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&owner)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0).map(|pane| pane.id()))
            .expect("grouped window pane exists")
    };
    handler
        .handle_pane_alert_event(crate::pane_io::PaneAlertEvent {
            session_name: owner.clone(),
            pane_id,
            bell_count: 0,
            title_changed: false,
            title_change: None,
            clipboard_set: false,
            clipboard_writes: Vec::new(),
            mouse_mode_changed: false,
            queue_activity_alert: true,
            generation: None,
        })
        .await;

    tokio::time::sleep(Duration::from_millis(1100)).await;
    {
        let state = handler.state.lock().await;
        for target in &family_targets {
            let flags = state
                .sessions
                .session(target.session_name())
                .expect("window alias session exists")
                .winlink_alert_flags(target.window_index());
            assert!(
                !flags.contains(WINLINK_SILENCE),
                "activity must postpone silence for window alias {target}"
            );
        }
    }

    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let family_silent = {
            let state = handler.state.lock().await;
            family_targets.iter().all(|target| {
                state
                    .sessions
                    .session(target.session_name())
                    .expect("window alias session exists")
                    .winlink_alert_flags(target.window_index())
                    .contains(WINLINK_SILENCE)
            })
        };
        if family_silent {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "grouped aliases did not expire from the refreshed silence deadline"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
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
