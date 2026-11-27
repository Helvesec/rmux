use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use rmux_core::LifecycleEvent;
use rmux_proto::{
    ControlMode, HookLifecycle, HookName, LinkWindowRequest, NewSessionExtRequest, OptionName,
    PaneTarget, RenameWindowRequest, Request, Response, ScopeSelector, SessionName, SetHookRequest,
    SetOptionMode, SetOptionRequest, TerminalSize, WindowTarget,
};
use tokio::sync::{broadcast, mpsc, oneshot};

use super::{QueuedLifecycleEvent, RequestHandler};
use crate::control::{ControlModeUpgrade, ControlServerEvent, CONTROL_SERVER_EVENT_CAPACITY};
use crate::pane_io::PaneAlertEvent;

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

async fn create_session(handler: &RequestHandler, name: &str) -> SessionName {
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
            command: Some(quiet_command()),
            process_command: None,
            client_environment: None,
            skip_environment_update: false,
        })))
        .await;
    crate::session_fixture::assert_new_session(&response);
    handler
        .wait_for_pane_startup_to_finish_for_test(&PaneTarget::new(session.clone(), 0))
        .await;
    session
}

#[cfg(unix)]
fn quiet_command() -> Vec<String> {
    ["/bin/sh", "-c", "sleep 60"]
        .into_iter()
        .map(str::to_owned)
        .collect()
}

#[cfg(windows)]
fn quiet_command() -> Vec<String> {
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

async fn link_window(handler: &RequestHandler, source: WindowTarget, target: WindowTarget) {
    let response = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source,
            target,
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(response, Response::LinkWindow(_)), "{response:?}");
}

async fn register_control_client(
    handler: &RequestHandler,
    requester_pid: u32,
    session: &SessionName,
) -> mpsc::Receiver<ControlServerEvent> {
    let (event_tx, event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    handler
        .register_control_with_closing(
            requester_pid,
            ControlModeUpgrade {
                initial_command_count: 0,
                mode: ControlMode::Plain,
                terminal_context: crate::outer_terminal::OuterTerminalContext::default(),
            },
            event_tx,
            Arc::new(AtomicBool::new(false)),
        )
        .await;
    handler
        .set_control_session(requester_pid, Some(session.clone()))
        .await
        .expect("control session set succeeds");
    event_rx
}

fn drain_control_notifications(rx: &mut mpsc::Receiver<ControlServerEvent>) -> Vec<String> {
    let mut notifications = Vec::new();
    while let Ok(event) = rx.try_recv() {
        if let ControlServerEvent::Notification(line) = event {
            notifications.push(line);
        }
    }
    notifications
}

fn assert_control_rename_before_hook(
    rx: &mut mpsc::Receiver<ControlServerEvent>,
    expected_rename: &str,
    hook_buffer: &str,
) {
    let notifications = drain_control_notifications(rx);
    let rename_notifications = notifications
        .iter()
        .filter(|line| {
            line.starts_with("%window-renamed ") || line.starts_with("%unlinked-window-renamed ")
        })
        .map(String::as_str)
        .collect::<Vec<_>>();
    assert_eq!(rename_notifications, vec![expected_rename]);
    let rename_position = notifications
        .iter()
        .position(|line| line == expected_rename)
        .expect("rename notification exists");
    let hook_position = notifications
        .iter()
        .position(|line| line == &format!("%paste-buffer-changed {hook_buffer}"))
        .expect("hook side effect notification exists");
    assert!(
        rename_position < hook_position,
        "control rename notification must precede the hook side effect: {notifications:?}"
    );
}

async fn set_automatic_rename_format(handler: &RequestHandler, target: WindowTarget, value: &str) {
    let response = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Window(target),
            option: OptionName::AutomaticRenameFormat,
            value: value.to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(response, Response::SetOption(_)), "{response:?}");
}

async fn set_window_renamed_hook(handler: &RequestHandler, buffer_name: &str) {
    let response = handler
        .handle(Request::SetHook(SetHookRequest {
            scope: ScopeSelector::Global,
            hook: HookName::WindowRenamed,
            command: format!("set-buffer -b {buffer_name} fired"),
            lifecycle: HookLifecycle::OneShot,
        }))
        .await;
    assert!(matches!(response, Response::SetHook(_)), "{response:?}");
}

async fn recv_window_renamed(
    events: &mut broadcast::Receiver<QueuedLifecycleEvent>,
) -> QueuedLifecycleEvent {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let event = events
                .recv()
                .await
                .expect("lifecycle event channel remains open");
            if matches!(event.event, LifecycleEvent::WindowRenamed { .. }) {
                return event;
            }
        }
    })
    .await
    .expect("window-renamed lifecycle event should be published")
}

fn assert_no_additional_window_renamed(events: &mut broadcast::Receiver<QueuedLifecycleEvent>) {
    while let Ok(event) = events.try_recv() {
        assert!(
            !matches!(event.event, LifecycleEvent::WindowRenamed { .. }),
            "rename mutation published a duplicate window-renamed event"
        );
    }
}

fn assert_hook_window_name(event: &QueuedLifecycleEvent, expected: &str) {
    assert_eq!(
        event
            .formats
            .iter()
            .find_map(|(name, value)| (name == "hook_window_name").then_some(value.as_str())),
        Some(expected)
    );
}

async fn wait_for_hook_buffer(handler: &RequestHandler, buffer_name: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let actual = {
            let state = handler.state.lock().await;
            state
                .buffers
                .show(Some(buffer_name))
                .ok()
                .map(|(_, bytes)| String::from_utf8_lossy(bytes).into_owned())
        };
        if actual.as_deref() == Some("fired") {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "rename hook buffer {buffer_name} did not fire; last={actual:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn window_id(handler: &RequestHandler, target: &WindowTarget) -> u32 {
    let state = handler.state.lock().await;
    state
        .sessions
        .session(target.session_name())
        .and_then(|session| session.window_at(target.window_index()))
        .map(|window| window.id().as_u32())
        .expect("window exists")
}

async fn active_pane_id(handler: &RequestHandler, target: &WindowTarget) -> rmux_proto::PaneId {
    let state = handler.state.lock().await;
    state
        .sessions
        .session(target.session_name())
        .and_then(|session| session.window_at(target.window_index()))
        .and_then(rmux_core::Window::active_pane)
        .map(rmux_core::Pane::id)
        .expect("active pane exists")
}

async fn assert_linked_window_names(
    handler: &RequestHandler,
    targets: &[WindowTarget],
    expected: &str,
) {
    let state = handler.state.lock().await;
    for target in targets {
        let name = state
            .sessions
            .session(target.session_name())
            .and_then(|session| session.window_at(target.window_index()))
            .and_then(rmux_core::Window::name);
        assert_eq!(name, Some(expected), "unexpected name for {target}");
    }
}

#[tokio::test]
async fn automatic_and_manual_renames_publish_one_link_aware_event_before_the_hook() {
    // Oracle probe 2026-07-26, pinned tmux 3.7b: both automatic and manual
    // renames publish one linked/unlinked control notification per client,
    // followed by exactly one window-renamed hook.
    let handler = RequestHandler::new();
    let alpha = create_session(&handler, "auto-rename-alpha").await;
    let beta = create_session(&handler, "auto-rename-beta").await;
    let gamma = create_session(&handler, "auto-rename-gamma").await;
    let alpha_target = WindowTarget::with_window(alpha.clone(), 0);
    let gamma_target = WindowTarget::with_window(gamma.clone(), 1);
    link_window(&handler, alpha_target.clone(), gamma_target.clone()).await;
    set_automatic_rename_format(&handler, alpha_target.clone(), "automatic-oracle").await;
    set_window_renamed_hook(&handler, "automatic-rename-hook").await;
    let lifecycle_dispatch = handler
        .take_lifecycle_dispatch_receiver()
        .expect("test owns lifecycle hook dispatch");
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let hook_handler = handler.clone();
    let hook_task = tokio::spawn(async move {
        hook_handler
            .consume_lifecycle_hooks(lifecycle_dispatch, shutdown_rx)
            .await;
    });

    let mut alpha_control = register_control_client(&handler, 51_001, &alpha).await;
    let mut beta_control = register_control_client(&handler, 51_002, &beta).await;
    let mut gamma_control = register_control_client(&handler, 51_003, &gamma).await;
    let _ = drain_control_notifications(&mut alpha_control);
    let _ = drain_control_notifications(&mut beta_control);
    let _ = drain_control_notifications(&mut gamma_control);
    let mut lifecycle = handler.subscribe_lifecycle_events();
    let pane_id = active_pane_id(&handler, &alpha_target).await;
    let shared_window_id = window_id(&handler, &alpha_target).await;

    handler
        .handle_pane_alert_event(PaneAlertEvent {
            session_name: alpha.clone(),
            pane_id,
            bell_count: 0,
            title_changed: false,
            title_change: None,
            path_changed: false,
            clipboard_set: false,
            clipboard_writes: Vec::new(),
            clipboard_queries: Vec::new(),
            mouse_mode_changed: false,
            alternate_mode_changed: false,
            queue_activity_alert: true,
            generation: None,
        })
        .await;

    let automatic_event = recv_window_renamed(&mut lifecycle).await;
    wait_for_hook_buffer(&handler, "automatic-rename-hook").await;
    assert_eq!(automatic_event.hooks.len(), 1);
    assert_control_rename_before_hook(
        &mut alpha_control,
        &format!("%window-renamed @{shared_window_id} automatic-oracle"),
        "automatic-rename-hook",
    );
    assert_control_rename_before_hook(
        &mut beta_control,
        &format!("%unlinked-window-renamed @{shared_window_id} automatic-oracle"),
        "automatic-rename-hook",
    );
    assert_control_rename_before_hook(
        &mut gamma_control,
        &format!("%window-renamed @{shared_window_id} automatic-oracle"),
        "automatic-rename-hook",
    );
    assert_linked_window_names(
        &handler,
        &[alpha_target.clone(), gamma_target.clone()],
        "automatic-oracle",
    )
    .await;
    assert_hook_window_name(&automatic_event, "automatic-oracle");
    assert_no_additional_window_renamed(&mut lifecycle);

    set_window_renamed_hook(&handler, "manual-rename-hook").await;
    let response = handler
        .handle(Request::RenameWindow(RenameWindowRequest {
            target: alpha_target,
            name: "manual-oracle".to_owned(),
        }))
        .await;
    assert!(
        matches!(response, Response::RenameWindow(_)),
        "{response:?}"
    );

    let manual_event = recv_window_renamed(&mut lifecycle).await;
    wait_for_hook_buffer(&handler, "manual-rename-hook").await;
    assert_eq!(manual_event.hooks.len(), 1);
    assert_control_rename_before_hook(
        &mut alpha_control,
        &format!("%window-renamed @{shared_window_id} manual-oracle"),
        "manual-rename-hook",
    );
    assert_control_rename_before_hook(
        &mut beta_control,
        &format!("%unlinked-window-renamed @{shared_window_id} manual-oracle"),
        "manual-rename-hook",
    );
    assert_control_rename_before_hook(
        &mut gamma_control,
        &format!("%window-renamed @{shared_window_id} manual-oracle"),
        "manual-rename-hook",
    );
    assert_linked_window_names(&handler, &[gamma_target], "manual-oracle").await;
    assert_hook_window_name(&manual_event, "manual-oracle");
    assert_no_additional_window_renamed(&mut lifecycle);

    shutdown_tx.send(()).expect("hook dispatcher stays alive");
    hook_task.await.expect("hook dispatcher shuts down cleanly");
}
