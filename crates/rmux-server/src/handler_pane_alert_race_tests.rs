use std::collections::HashSet;

use super::{QueuedLifecycleEvent, RequestHandler};
use crate::pane_io::{AttachControl, PaneAlertEvent};
use rmux_core::{LifecycleEvent, PaneId, WINDOW_BELL, WINLINK_ACTIVITY, WINLINK_BELL};
use rmux_proto::{
    HookLifecycle, HookName, KillSessionRequest, LinkWindowRequest, NewSessionExtRequest,
    NewWindowRequest, OptionName, OptionScopeSelector, PaneTarget, Request, Response,
    ScopeSelector, SessionName, SetHookMutationRequest, SetOptionByNameRequest, SetOptionMode,
    SetOptionRequest, SplitDirection, SplitWindowExtRequest, SplitWindowTarget, TerminalSize,
    WaitForMode, WaitForRequest, WindowId, WindowTarget,
};
use tokio::sync::{broadcast, mpsc};
use tokio::time::{timeout, Duration, Instant};

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
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
            command: Some(quiet_command()),
            process_command: None,
            client_environment: None,
            skip_environment_update: false,
        })))
        .await;
    assert!(matches!(response, Response::NewSession(_)), "{response:?}");
    handler
        .wait_for_pane_startup_to_finish_for_test(&PaneTarget::new(session.clone(), 0))
        .await;
    session
}

async fn create_grouped_quiet_session(
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

async fn link_window_alias(handler: &RequestHandler, source: WindowTarget, target: WindowTarget) {
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

async fn create_quiet_window(handler: &RequestHandler, session: &SessionName) -> WindowTarget {
    let response = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: session.clone(),
            name: None,
            detached: true,
            start_directory: None,
            environment: None,
            command: Some(quiet_command()),
            process_command: None,
            target_window_index: None,
            insert_at_target: false,
        })))
        .await;
    let Response::NewWindow(response) = response else {
        panic!("expected quiet new-window response, got {response:?}");
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
    assert!(matches!(response, Response::SetOption(_)), "{response:?}");
}

async fn enable_clipboard_hooks(handler: &RequestHandler) {
    let response = handler
        .handle(Request::SetOptionByName(Box::new(SetOptionByNameRequest {
            scope: OptionScopeSelector::ServerGlobal,
            name: "set-clipboard".to_owned(),
            value: Some("on".to_owned()),
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

async fn set_global_hook(handler: &RequestHandler, hook: HookName, command: String) {
    let response = handler
        .handle(Request::SetHookMutation(SetHookMutationRequest {
            scope: ScopeSelector::Global,
            hook,
            command: Some(command),
            lifecycle: HookLifecycle::Persistent,
            append: false,
            unset: false,
            run_immediately: false,
            index: None,
        }))
        .await;
    assert!(matches!(response, Response::SetHook(_)), "{response:?}");
}

async fn pane_identity(
    handler: &RequestHandler,
    target: &WindowTarget,
) -> (PaneId, Option<u64>, WindowId) {
    let state = handler.state.lock().await;
    let window = state
        .sessions
        .session(target.session_name())
        .and_then(|session| session.window_at(target.window_index()))
        .expect("pane-alert target exists");
    let pane_id = window.active_pane().expect("active pane exists").id();
    (
        pane_id,
        Some(state.pane_output_generation(target.session_name(), pane_id)),
        window.id(),
    )
}

fn pane_event(
    session_name: SessionName,
    pane_id: PaneId,
    generation: Option<u64>,
) -> PaneAlertEvent {
    PaneAlertEvent {
        session_name,
        pane_id,
        bell_count: 1,
        title_changed: true,
        title_change: None,
        clipboard_set: true,
        clipboard_writes: Vec::new(),
        clipboard_queries: Vec::new(),
        mouse_mode_changed: false,
        alternate_mode_changed: false,
        queue_activity_alert: true,
        generation,
    }
}

async fn dispatch_expected_hooks(
    handler: &RequestHandler,
    receiver: &mut broadcast::Receiver<QueuedLifecycleEvent>,
    expected: &[HookName],
) {
    let mut remaining = expected.to_vec();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !remaining.is_empty() {
        let event = timeout(
            deadline.saturating_duration_since(Instant::now()),
            receiver.recv(),
        )
        .await
        .expect("expected lifecycle hook before timeout")
        .expect("lifecycle channel remains open");
        if let Some(position) = remaining.iter().position(|hook| *hook == event.hook_name) {
            remaining.remove(position);
            handler.dispatch_lifecycle_hook(event).await;
        }
    }
}

fn buffer_text(state: &crate::pane_terminals::HandlerState, name: &str) -> Option<String> {
    state
        .buffers
        .show(Some(name))
        .ok()
        .map(|(_, bytes)| String::from_utf8_lossy(bytes).into_owned())
}

fn collect_alert_hook_targets(
    receiver: &mut broadcast::Receiver<QueuedLifecycleEvent>,
) -> (HashSet<WindowTarget>, HashSet<WindowTarget>) {
    let mut activity_targets = HashSet::new();
    let mut bell_targets = HashSet::new();
    loop {
        match receiver.try_recv() {
            Ok(event) => match event.event {
                LifecycleEvent::AlertActivity { target } => {
                    activity_targets.insert(target);
                }
                LifecycleEvent::AlertBell { target } => {
                    bell_targets.insert(target);
                }
                _ => {}
            },
            Err(broadcast::error::TryRecvError::Empty | broadcast::error::TryRecvError::Closed) => {
                break;
            }
            Err(broadcast::error::TryRecvError::Lagged(skipped)) => {
                panic!("alert lifecycle receiver lagged by {skipped} events");
            }
        }
    }
    (activity_targets, bell_targets)
}

async fn drain_controls(receiver: &mut mpsc::UnboundedReceiver<AttachControl>) {
    loop {
        match timeout(Duration::from_millis(20), receiver.recv()).await {
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => return,
        }
    }
}

#[tokio::test]
async fn queued_pane_hook_does_not_block_an_unrelated_pane_alert() {
    let handler = RequestHandler::new();
    let hooked_session = create_quiet_session(&handler, "pane-alert-queued-hook").await;
    let live_session = create_quiet_session(&handler, "pane-alert-live-after-hook").await;
    let hooked_target = WindowTarget::with_window(hooked_session.clone(), 0);
    let live_target = WindowTarget::with_window(live_session.clone(), 0);
    set_option(
        &handler,
        ScopeSelector::Window(live_target.clone()),
        OptionName::MonitorBell,
        "on",
    )
    .await;
    set_global_hook(
        &handler,
        HookName::PaneTitleChanged,
        "set-buffer -b queued-title-hook fired".to_owned(),
    )
    .await;
    let (hooked_pane_id, hooked_generation, _) = pane_identity(&handler, &hooked_target).await;
    let (live_pane_id, live_generation, _) = pane_identity(&handler, &live_target).await;
    let mut lifecycle = handler.subscribe_lifecycle_events();
    let _undrained_lifecycle_events = handler
        .take_lifecycle_dispatch_receiver()
        .expect("test owns the lifecycle dispatch receiver");

    handler.pane_alert_callback()(PaneAlertEvent {
        session_name: hooked_session,
        pane_id: hooked_pane_id,
        bell_count: 0,
        title_changed: true,
        title_change: None,
        clipboard_set: false,
        clipboard_writes: Vec::new(),
        clipboard_queries: Vec::new(),
        mouse_mode_changed: false,
        alternate_mode_changed: false,
        queue_activity_alert: false,
        generation: hooked_generation,
    });
    let event = timeout(Duration::from_secs(2), lifecycle.recv())
        .await
        .expect("pane hook is queued before the deadline")
        .expect("lifecycle channel remains open");
    assert_eq!(event.hook_name, HookName::PaneTitleChanged);

    // Leave the hook dispatch receiver undrained. Normal pane-alert delivery
    // queues lifecycle work but does not wait for the hook command to run, so
    // a slow hook cannot retain the pane-alert serialization lock.
    handler.pane_alert_callback()(PaneAlertEvent {
        session_name: live_session.clone(),
        pane_id: live_pane_id,
        bell_count: 1,
        title_changed: false,
        title_change: None,
        clipboard_set: false,
        clipboard_writes: Vec::new(),
        clipboard_queries: Vec::new(),
        mouse_mode_changed: false,
        alternate_mode_changed: false,
        queue_activity_alert: false,
        generation: live_generation,
    });

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let bell_applied = {
            let state = handler.state.lock().await;
            state
                .sessions
                .session(&live_session)
                .is_some_and(|session| {
                    session
                        .winlink_alert_flags(live_target.window_index())
                        .contains(WINLINK_BELL)
                })
        };
        if bell_applied {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "unrelated pane bell remains blocked behind an unexecuted hook"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test]
async fn exiting_pane_hook_wait_does_not_block_unrelated_pane_alert_flush() {
    let handler = RequestHandler::new();
    let exiting_session = create_quiet_session(&handler, "pane-alert-exit-hook").await;
    let live_session = create_quiet_session(&handler, "pane-alert-live-peer").await;
    let exiting_target = WindowTarget::with_window(exiting_session.clone(), 0);
    let live_target = WindowTarget::with_window(live_session.clone(), 0);
    set_option(
        &handler,
        ScopeSelector::Window(live_target.clone()),
        OptionName::MonitorBell,
        "on",
    )
    .await;
    set_global_hook(
        &handler,
        HookName::PaneTitleChanged,
        "set-buffer -b exit-title-hook fired".to_owned(),
    )
    .await;
    let (exiting_pane_id, exiting_generation, _) = pane_identity(&handler, &exiting_target).await;
    let (live_pane_id, live_generation, _) = pane_identity(&handler, &live_target).await;
    let mut lifecycle = handler.subscribe_lifecycle_events();
    let lifecycle_events = handler
        .take_lifecycle_dispatch_receiver()
        .expect("test owns the lifecycle dispatch receiver");

    handler.pane_alert_callback()(PaneAlertEvent {
        session_name: exiting_session,
        pane_id: exiting_pane_id,
        bell_count: 0,
        title_changed: true,
        title_change: None,
        clipboard_set: false,
        clipboard_writes: Vec::new(),
        clipboard_queries: Vec::new(),
        mouse_mode_changed: false,
        alternate_mode_changed: false,
        queue_activity_alert: false,
        generation: exiting_generation,
    });
    let exit_handler = handler.clone();
    let mut exit_flush = tokio::spawn(async move {
        exit_handler
            .flush_pending_pane_alert_for_exit(exiting_pane_id, exiting_generation)
            .await;
    });

    let event = timeout(Duration::from_secs(2), lifecycle.recv())
        .await
        .expect("exiting pane hook is queued before the deadline")
        .expect("lifecycle channel remains open");
    assert_eq!(event.hook_name, HookName::PaneTitleChanged);
    assert!(
        !exit_flush.is_finished(),
        "exit flush waits for its ordered lifecycle hook"
    );

    handler.pane_alert_callback()(PaneAlertEvent {
        session_name: live_session.clone(),
        pane_id: live_pane_id,
        bell_count: 1,
        title_changed: false,
        title_change: None,
        clipboard_set: false,
        clipboard_writes: Vec::new(),
        clipboard_queries: Vec::new(),
        mouse_mode_changed: false,
        alternate_mode_changed: false,
        queue_activity_alert: false,
        generation: live_generation,
    });
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let bell_applied = {
            let state = handler.state.lock().await;
            state
                .sessions
                .session(&live_session)
                .is_some_and(|session| {
                    session
                        .winlink_alert_flags(live_target.window_index())
                        .contains(WINLINK_BELL)
                })
        };
        if bell_applied {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "unrelated pane bell remains globally blocked by the exiting hook"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(
        !exit_flush.is_finished(),
        "unrelated alert applies while the exiting pane still waits for its hook"
    );

    let (hook_shutdown, hook_shutdown_rx) = tokio::sync::oneshot::channel();
    let hook_handler = handler.clone();
    let hook_task = tokio::spawn(async move {
        hook_handler
            .consume_lifecycle_hooks(lifecycle_events, hook_shutdown_rx)
            .await;
    });
    timeout(Duration::from_secs(2), &mut exit_flush)
        .await
        .expect("exit flush completes after its hook is dispatched")
        .expect("exit flush task joins");
    {
        let state = handler.state.lock().await;
        assert_eq!(
            buffer_text(&state, "exit-title-hook").as_deref(),
            Some("fired"),
            "ordered hook completes before the exit flush returns"
        );
    }
    let _ = hook_shutdown.send(());
    timeout(Duration::from_secs(2), hook_task)
        .await
        .expect("lifecycle hook consumer stops")
        .expect("lifecycle hook consumer joins");
}

#[tokio::test]
async fn pane_alert_reserves_each_lifecycle_position_immediately_before_emission() {
    const WAIT_CHANNEL: &str = "pane-alert-per-event-sequencing";

    let handler = RequestHandler::new();
    let session = create_quiet_session(&handler, "pane-alert-per-event-sequencing").await;
    enable_clipboard_hooks(&handler).await;
    set_global_hook(
        &handler,
        HookName::PaneTitleChanged,
        format!("wait-for {WAIT_CHANNEL}"),
    )
    .await;
    set_global_hook(
        &handler,
        HookName::PaneSetClipboard,
        "set-buffer -b second-pane-hook finished".to_owned(),
    )
    .await;
    let target = WindowTarget::with_window(session.clone(), 0);
    let (pane_id, generation, _) = pane_identity(&handler, &target).await;
    let mut lifecycle = handler.subscribe_lifecycle_events();
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

    handler.pane_alert_callback()(PaneAlertEvent {
        session_name: session,
        pane_id,
        bell_count: 0,
        title_changed: true,
        title_change: None,
        clipboard_set: true,
        clipboard_writes: Vec::new(),
        clipboard_queries: Vec::new(),
        mouse_mode_changed: false,
        alternate_mode_changed: false,
        queue_activity_alert: false,
        generation,
    });
    let flush_handler = handler.clone();
    let mut flush = tokio::spawn(async move {
        flush_handler
            .flush_pending_pane_alert_for_exit(pane_id, generation)
            .await;
    });

    timeout(Duration::from_secs(2), async {
        loop {
            let waiting = handler
                .wait_for
                .lock()
                .expect("wait-for store remains available")
                .waiter_counts(WAIT_CHANNEL)
                .0;
            if waiting == 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the first pane hook reaches its deterministic wait seam");

    timeout(
        Duration::from_millis(250),
        handler.store_buffer(None, b"nested-between-pane-events".to_vec()),
    )
    .await
    .expect("the next pane event must not reserve ahead of an intervening publication")
    .expect("intervening buffer publication succeeds");

    let response = handler
        .handle(Request::WaitFor(WaitForRequest {
            channel: WAIT_CHANNEL.to_owned(),
            mode: WaitForMode::Signal,
        }))
        .await;
    assert!(matches!(response, Response::WaitFor(_)), "{response:?}");
    timeout(Duration::from_secs(2), &mut flush)
        .await
        .expect("pane alert flush completes without a lifecycle ticket cycle")
        .expect("pane alert flush task joins");

    let mut observed = Vec::new();
    while observed.len() < 3 {
        let event = timeout(Duration::from_secs(2), lifecycle.recv())
            .await
            .expect("ordered lifecycle event arrives")
            .expect("lifecycle channel remains open");
        if matches!(
            event.hook_name,
            HookName::PaneTitleChanged | HookName::PasteBufferChanged | HookName::PaneSetClipboard
        ) {
            observed.push(event.hook_name);
        }
    }
    assert_eq!(
        observed,
        vec![
            HookName::PaneTitleChanged,
            HookName::PasteBufferChanged,
            HookName::PaneSetClipboard,
        ]
    );
    {
        let state = handler.state.lock().await;
        assert_eq!(
            buffer_text(&state, "second-pane-hook").as_deref(),
            Some("finished")
        );
    }
    let _ = hook_shutdown.send(());
    timeout(Duration::from_secs(2), hook_task)
        .await
        .expect("lifecycle hook consumer stops")
        .expect("lifecycle hook consumer joins");
}

#[tokio::test]
async fn exiting_pane_activity_cannot_rearm_silence_after_newer_same_window_activity() {
    let handler = RequestHandler::new();
    let session = create_quiet_session(&handler, "pane-alert-exit-silence-order").await;
    let window_target = WindowTarget::with_window(session.clone(), 0);
    let split = handler
        .handle(Request::SplitWindowExt(Box::new(SplitWindowExtRequest {
            target: SplitWindowTarget::Session(session.clone()),
            direction: SplitDirection::Vertical,
            before: false,
            environment: None,
            command: Some(quiet_command()),
            process_command: None,
            start_directory: None,
            keep_alive_on_exit: None,
            detached: true,
            size: None,
            preserve_zoom: false,
            full_size: false,
            stdin_payload: None,
        })))
        .await;
    let Response::SplitWindow(split) = split else {
        panic!("expected split-window success, got {split:?}");
    };
    handler
        .wait_for_pane_startup_to_finish_for_test(&split.pane)
        .await;
    set_option(
        &handler,
        ScopeSelector::Window(window_target.clone()),
        OptionName::MonitorSilence,
        "60",
    )
    .await;
    set_global_hook(
        &handler,
        HookName::PaneTitleChanged,
        "set-buffer -b exit-silence-hook fired".to_owned(),
    )
    .await;

    let (exiting_pane_id, exiting_generation, live_pane_id, live_generation) = {
        let state = handler.state.lock().await;
        let window = state
            .sessions
            .session(&session)
            .and_then(|current| current.window_at(window_target.window_index()))
            .expect("two-pane alert window exists");
        let exiting_pane_id = window.pane(0).expect("exiting pane exists").id();
        let live_pane_id = window.pane(1).expect("live pane exists").id();
        (
            exiting_pane_id,
            Some(state.pane_output_generation(&session, exiting_pane_id)),
            live_pane_id,
            Some(state.pane_output_generation(&session, live_pane_id)),
        )
    };
    let initial_timer = handler
        .silence_timer_snapshot_for_test(&window_target)
        .expect("monitor-silence timer starts armed");
    let mut lifecycle = handler.subscribe_lifecycle_events();
    let lifecycle_events = handler
        .take_lifecycle_dispatch_receiver()
        .expect("test owns the lifecycle dispatch receiver");

    handler.pane_alert_callback()(PaneAlertEvent {
        session_name: session.clone(),
        pane_id: exiting_pane_id,
        bell_count: 0,
        title_changed: true,
        title_change: None,
        clipboard_set: false,
        clipboard_writes: Vec::new(),
        clipboard_queries: Vec::new(),
        mouse_mode_changed: false,
        alternate_mode_changed: false,
        queue_activity_alert: true,
        generation: exiting_generation,
    });
    let exit_handler = handler.clone();
    let mut exit_flush = tokio::spawn(async move {
        exit_handler
            .flush_pending_pane_alert_for_exit(exiting_pane_id, exiting_generation)
            .await;
    });

    let event = timeout(Duration::from_secs(2), lifecycle.recv())
        .await
        .expect("exiting pane title hook is queued")
        .expect("lifecycle channel remains open");
    assert_eq!(event.hook_name, HookName::PaneTitleChanged);
    assert!(!exit_flush.is_finished(), "exit flush waits for its hook");
    let exit_timer = handler
        .silence_timer_snapshot_for_test(&window_target)
        .expect("exit activity keeps the silence timer armed");
    assert_eq!(
        exit_timer.0,
        initial_timer.0.saturating_add(1),
        "the exiting activity resets silence exactly once before its hook wait"
    );

    handler.pane_alert_callback()(PaneAlertEvent {
        session_name: session,
        pane_id: live_pane_id,
        bell_count: 0,
        title_changed: false,
        title_change: None,
        clipboard_set: false,
        clipboard_writes: Vec::new(),
        clipboard_queries: Vec::new(),
        mouse_mode_changed: false,
        alternate_mode_changed: false,
        queue_activity_alert: true,
        generation: live_generation,
    });
    let newer_timer = timeout(Duration::from_secs(2), async {
        loop {
            let snapshot = handler
                .silence_timer_snapshot_for_test(&window_target)
                .expect("same-window activity keeps the timer armed");
            if snapshot.0 > exit_timer.0 {
                break snapshot;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("newer same-window activity resets silence while the hook waits");
    assert!(
        newer_timer.1 > exit_timer.1,
        "the newer activity owns a strictly later silence deadline"
    );
    assert!(
        !exit_flush.is_finished(),
        "newer activity applies before the exiting hook completes"
    );

    let (hook_shutdown, hook_shutdown_rx) = tokio::sync::oneshot::channel();
    let hook_handler = handler.clone();
    let hook_task = tokio::spawn(async move {
        hook_handler
            .consume_lifecycle_hooks(lifecycle_events, hook_shutdown_rx)
            .await;
    });
    timeout(Duration::from_secs(2), &mut exit_flush)
        .await
        .expect("exit flush completes after its hook")
        .expect("exit flush task joins");
    assert_eq!(
        handler.silence_timer_snapshot_for_test(&window_target),
        Some(newer_timer),
        "the older exiting batch must not rearm over newer same-window activity"
    );
    {
        let state = handler.state.lock().await;
        assert_eq!(
            buffer_text(&state, "exit-silence-hook").as_deref(),
            Some("fired"),
            "the ordered title hook still executes"
        );
    }
    let _ = hook_shutdown.send(());
    timeout(Duration::from_secs(2), hook_task)
        .await
        .expect("lifecycle hook consumer stops")
        .expect("lifecycle hook consumer joins");
}

#[tokio::test]
async fn pane_alert_reaches_every_linked_and_grouped_window_alias_once() {
    let handler = RequestHandler::new();
    let owner = create_quiet_session(&handler, "m-pane-alert-family-owner").await;
    let peer = create_grouped_quiet_session(&handler, "z-pane-alert-family-peer", &owner).await;
    let external = create_quiet_session(&handler, "a-pane-alert-family-external").await;
    let owner_target = WindowTarget::with_window(owner.clone(), 0);
    let peer_target = WindowTarget::with_window(peer.clone(), 0);
    let external_target = WindowTarget::with_window(external.clone(), 1);
    link_window_alias(&handler, owner_target.clone(), external_target.clone()).await;
    let family_targets = vec![owner_target.clone(), peer_target, external_target];

    for target in &family_targets {
        for (option, value) in [
            (OptionName::MonitorActivity, "on"),
            (OptionName::MonitorBell, "on"),
            (OptionName::MonitorSilence, "60"),
        ] {
            set_option(
                &handler,
                ScopeSelector::Window(target.clone()),
                option,
                value,
            )
            .await;
        }
    }
    for session in [owner.clone(), peer, external] {
        for option in [OptionName::ActivityAction, OptionName::BellAction] {
            set_option(
                &handler,
                ScopeSelector::Session(session.clone()),
                option,
                "any",
            )
            .await;
        }
    }
    let timer_generations = family_targets
        .iter()
        .map(|target| {
            handler
                .silence_timer_generation_for_test(target)
                .expect("family silence timer is armed")
        })
        .collect::<Vec<_>>();
    let (pane_id, generation, window_id) = pane_identity(&handler, &owner_target).await;
    let mut lifecycle = handler.subscribe_lifecycle_events();

    handler
        .handle_pane_alert_event(pane_event(owner, pane_id, generation))
        .await;

    {
        let state = handler.state.lock().await;
        for target in &family_targets {
            let session = state
                .sessions
                .session(target.session_name())
                .expect("family session survives");
            assert_eq!(
                session
                    .window_at(target.window_index())
                    .expect("family window survives")
                    .id(),
                window_id
            );
            let flags = session.winlink_alert_flags(target.window_index());
            assert!(
                flags.contains(WINLINK_ACTIVITY),
                "activity flag for {target}"
            );
            assert!(flags.contains(WINLINK_BELL), "bell flag for {target}");
        }
    }
    for (target, previous_generation) in family_targets.iter().zip(timer_generations) {
        assert_eq!(
            handler.silence_timer_generation_for_test(target),
            Some(previous_generation.saturating_add(1)),
            "one pane-alert batch resets the family silence timer once for {target}"
        );
    }
    let expected_targets = family_targets.into_iter().collect::<HashSet<_>>();
    let (activity_targets, bell_targets) = collect_alert_hook_targets(&mut lifecycle);
    assert_eq!(activity_targets, expected_targets);
    assert_eq!(bell_targets, expected_targets);
}

#[tokio::test]
async fn pane_alert_survives_an_earlier_alias_added_between_prepare_and_apply() {
    let handler = RequestHandler::new();
    let owner = create_quiet_session(&handler, "z-pane-alert-added-owner").await;
    let alias = create_quiet_session(&handler, "a-pane-alert-added-alias").await;
    let owner_target = WindowTarget::with_window(owner.clone(), 0);
    let alias_target = WindowTarget::with_window(alias.clone(), 1);
    for option in [OptionName::MonitorActivity, OptionName::MonitorBell] {
        set_option(
            &handler,
            ScopeSelector::Window(owner_target.clone()),
            option,
            "on",
        )
        .await;
    }
    for session in [owner.clone(), alias] {
        for option in [OptionName::ActivityAction, OptionName::BellAction] {
            set_option(
                &handler,
                ScopeSelector::Session(session.clone()),
                option,
                "any",
            )
            .await;
        }
    }
    let (pane_id, generation, window_id) = pane_identity(&handler, &owner_target).await;
    let prepared = handler
        .prepare_pane_alert_event(pane_event(owner, pane_id, generation))
        .await
        .expect("pane alert prepares before the alias exists");

    link_window_alias(&handler, owner_target.clone(), alias_target.clone()).await;
    for option in [OptionName::MonitorActivity, OptionName::MonitorBell] {
        set_option(
            &handler,
            ScopeSelector::Window(alias_target.clone()),
            option,
            "on",
        )
        .await;
    }
    let mut lifecycle = handler.subscribe_lifecycle_events();
    handler
        .apply_prepared_pane_alert_events(vec![prepared])
        .await;

    let expected_targets = [owner_target, alias_target]
        .into_iter()
        .collect::<HashSet<_>>();
    {
        let state = handler.state.lock().await;
        for target in &expected_targets {
            let session = state
                .sessions
                .session(target.session_name())
                .expect("alias session survives");
            assert_eq!(
                session
                    .window_at(target.window_index())
                    .expect("linked alias survives")
                    .id(),
                window_id
            );
            let flags = session.winlink_alert_flags(target.window_index());
            assert!(
                flags.contains(WINLINK_ACTIVITY),
                "activity flag for {target}"
            );
            assert!(flags.contains(WINLINK_BELL), "bell flag for {target}");
        }
    }
    let (activity_targets, bell_targets) = collect_alert_hook_targets(&mut lifecycle);
    assert_eq!(activity_targets, expected_targets);
    assert_eq!(bell_targets, expected_targets);
}

#[tokio::test]
async fn pane_alert_reindex_keeps_hooks_name_and_flags_on_the_original_window_id() {
    let handler = RequestHandler::new();
    let destination = create_quiet_session(&handler, "pane-alert-reindex-destination").await;
    let alerted = create_quiet_window(&handler, &destination).await;
    let source = create_quiet_session(&handler, "pane-alert-reindex-source").await;
    set_option(
        &handler,
        ScopeSelector::Window(alerted.clone()),
        OptionName::MonitorActivity,
        "on",
    )
    .await;
    set_option(
        &handler,
        ScopeSelector::Window(alerted.clone()),
        OptionName::AutomaticRenameFormat,
        "stable-pane-alert-name",
    )
    .await;
    enable_clipboard_hooks(&handler).await;
    let (pane_id, generation, alerted_window_id) = pane_identity(&handler, &alerted).await;
    for (hook, buffer) in [
        (HookName::PaneTitleChanged, "stable-pane-title"),
        (HookName::PaneSetClipboard, "stable-pane-clipboard"),
    ] {
        set_global_hook(
            &handler,
            hook,
            format!(
                "if-shell -F '#{{==:#{{window_id}}:#{{window_index}},{alerted_window_id}:2}}' 'set-buffer -b {buffer} ok' 'set-buffer -b {buffer} bad'"
            ),
        )
        .await;
    }
    let mut lifecycle = handler.subscribe_lifecycle_events();
    let pause = handler.install_pane_alert_apply_pause();
    let task_handler = handler.clone();
    let task_session = destination.clone();
    let mut task = tokio::spawn(async move {
        task_handler
            .handle_pane_alert_event(pane_event(task_session, pane_id, generation))
            .await;
    });
    timeout(Duration::from_secs(3), pause.reached.notified())
        .await
        .expect("pane alert reaches final-apply pause");

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
    pause.release.notify_one();
    timeout(Duration::from_secs(5), &mut task)
        .await
        .expect("pane alert finishes after reindex")
        .expect("pane alert task succeeds");

    dispatch_expected_hooks(
        &handler,
        &mut lifecycle,
        &[HookName::PaneTitleChanged, HookName::PaneSetClipboard],
    )
    .await;
    let state = handler.state.lock().await;
    let session = state
        .sessions
        .session(&destination)
        .expect("destination survives");
    let inserted = session.window_at(1).expect("inserted window exists");
    let moved = session.window_at(2).expect("alerted window moved");
    assert_eq!(moved.id(), alerted_window_id);
    assert_ne!(inserted.id(), alerted_window_id);
    assert_eq!(moved.name(), Some("stable-pane-alert-name"));
    assert_ne!(inserted.name(), Some("stable-pane-alert-name"));
    let moved_flags = session.winlink_alert_flags(2);
    assert!(moved_flags.contains(WINLINK_ACTIVITY));
    assert!(moved_flags.contains(WINLINK_BELL));
    assert!(!session
        .winlink_alert_flags(1)
        .intersects(WINLINK_ACTIVITY.union(WINLINK_BELL)));
    assert_eq!(
        buffer_text(&state, "stable-pane-title").as_deref(),
        Some("ok")
    );
    assert_eq!(
        buffer_text(&state, "stable-pane-clipboard").as_deref(),
        Some("ok")
    );
}

#[tokio::test]
async fn pane_alert_replacement_fails_closed_before_hooks_name_or_flags_reach_reused_slot() {
    let handler = RequestHandler::new();
    let destination = create_quiet_session(&handler, "pane-alert-replace-destination").await;
    let alerted = create_quiet_window(&handler, &destination).await;
    let source = create_quiet_session(&handler, "pane-alert-replace-source").await;
    set_option(
        &handler,
        ScopeSelector::Window(alerted.clone()),
        OptionName::MonitorActivity,
        "on",
    )
    .await;
    set_option(
        &handler,
        ScopeSelector::Window(alerted.clone()),
        OptionName::AutomaticRenameFormat,
        "stale-pane-alert-name",
    )
    .await;
    enable_clipboard_hooks(&handler).await;
    let (pane_id, generation, alerted_window_id) = pane_identity(&handler, &alerted).await;
    for (hook, buffer) in [
        (HookName::PaneTitleChanged, "stale-pane-title"),
        (HookName::PaneSetClipboard, "stale-pane-clipboard"),
    ] {
        set_global_hook(
            &handler,
            hook,
            format!("set-buffer -b {buffer} wrong-target"),
        )
        .await;
    }
    let mut lifecycle = handler.subscribe_lifecycle_events();
    let pause = handler.install_pane_alert_apply_pause();
    let task_handler = handler.clone();
    let task_session = destination.clone();
    let mut task = tokio::spawn(async move {
        task_handler
            .handle_pane_alert_event(pane_event(task_session, pane_id, generation))
            .await;
    });
    timeout(Duration::from_secs(3), pause.reached.notified())
        .await
        .expect("pane alert reaches replacement pause");

    let response = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(source, 0),
            target: alerted.clone(),
            after: false,
            before: false,
            kill_destination: true,
            detached: true,
        }))
        .await;
    assert!(matches!(response, Response::LinkWindow(_)), "{response:?}");
    pause.release.notify_one();
    timeout(Duration::from_secs(5), &mut task)
        .await
        .expect("pane alert finishes after replacement")
        .expect("pane alert task succeeds");

    dispatch_expected_hooks(
        &handler,
        &mut lifecycle,
        &[HookName::PaneTitleChanged, HookName::PaneSetClipboard],
    )
    .await;
    let state = handler.state.lock().await;
    let session = state
        .sessions
        .session(&destination)
        .expect("destination survives");
    let replacement = session
        .window_at(alerted.window_index())
        .expect("replacement occupies old slot");
    assert_ne!(replacement.id(), alerted_window_id);
    assert_ne!(replacement.name(), Some("stale-pane-alert-name"));
    assert!(!session
        .winlink_alert_flags(alerted.window_index())
        .intersects(WINLINK_ACTIVITY.union(WINLINK_BELL)));
    assert_eq!(buffer_text(&state, "stale-pane-title"), None);
    assert_eq!(buffer_text(&state, "stale-pane-clipboard"), None);
}

#[tokio::test]
async fn alert_plan_effects_follow_session_id_through_hook_rename_and_name_reuse() {
    let handler = RequestHandler::new();
    let alpha = create_quiet_session(&handler, "alert-plan-alpha").await;
    let alerted = create_quiet_window(&handler, &alpha).await;
    let beta = session_name("alert-plan-beta");
    let original_session_id = {
        let state = handler.state.lock().await;
        state.sessions.session(&alpha).expect("alpha exists").id()
    };
    set_option(
        &handler,
        ScopeSelector::Window(alerted.clone()),
        OptionName::MonitorBell,
        "on",
    )
    .await;
    for (option, value) in [
        (OptionName::BellAction, "any"),
        (OptionName::VisualBell, "both"),
    ] {
        set_option(
            &handler,
            ScopeSelector::Session(alpha.clone()),
            option,
            value,
        )
        .await;
    }
    set_global_hook(
        &handler,
        HookName::AlertBell,
        format!("rename-session -t {alpha} {beta}"),
    )
    .await;
    let (beta_tx, mut beta_rx) = mpsc::unbounded_channel();
    handler.register_attach(710, alpha.clone(), beta_tx).await;
    drain_controls(&mut beta_rx).await;
    let mut lifecycle = handler.subscribe_lifecycle_events();
    let pause = handler.install_alert_plan_effect_pause();
    let task_handler = handler.clone();
    let task_target = alerted.clone();
    let mut task = tokio::spawn(async move {
        task_handler
            .alerts_queue_window(task_target, WINDOW_BELL)
            .await;
    });
    timeout(Duration::from_secs(3), pause.reached.notified())
        .await
        .expect("alert plan pauses after hook enqueue");
    dispatch_expected_hooks(&handler, &mut lifecycle, &[HookName::AlertBell]).await;
    {
        let state = handler.state.lock().await;
        assert!(state.sessions.session(&alpha).is_none());
        assert_eq!(
            state
                .sessions
                .session(&beta)
                .expect("hook renamed beta")
                .id(),
            original_session_id
        );
    }

    let reused_alpha = create_quiet_session(&handler, alpha.as_str()).await;
    let (alpha_tx, mut alpha_rx) = mpsc::unbounded_channel();
    handler.register_attach(711, reused_alpha, alpha_tx).await;
    drain_controls(&mut beta_rx).await;
    drain_controls(&mut alpha_rx).await;
    pause.release.notify_one();
    timeout(Duration::from_secs(5), &mut task)
        .await
        .expect("renamed alert plan finishes")
        .expect("alert plan task succeeds");

    let deadline = Instant::now() + Duration::from_secs(3);
    let (mut bell, mut overlay, mut refresh) = (false, false, false);
    while !(bell && overlay && refresh) {
        let control = timeout(
            deadline.saturating_duration_since(Instant::now()),
            beta_rx.recv(),
        )
        .await
        .expect("original session receives all alert effects")
        .expect("original client stays attached");
        match control {
            AttachControl::Write(bytes) if bytes == vec![0x07] => bell = true,
            AttachControl::Overlay(_) => overlay = true,
            AttachControl::Refresh | AttachControl::Switch(_) => refresh = true,
            _ => {}
        }
    }
    assert!(
        timeout(Duration::from_millis(150), alpha_rx.recv())
            .await
            .is_err(),
        "reused alpha incarnation must receive no old alert-plan effect"
    );
}

#[tokio::test]
async fn alert_plan_effects_fail_closed_after_session_destroy_and_name_reuse() {
    let handler = RequestHandler::new();
    let alpha = create_quiet_session(&handler, "alert-plan-destroy-alpha").await;
    let alerted = create_quiet_window(&handler, &alpha).await;
    set_option(
        &handler,
        ScopeSelector::Window(alerted.clone()),
        OptionName::MonitorBell,
        "on",
    )
    .await;
    for (option, value) in [
        (OptionName::BellAction, "any"),
        (OptionName::VisualBell, "both"),
    ] {
        set_option(
            &handler,
            ScopeSelector::Session(alpha.clone()),
            option,
            value,
        )
        .await;
    }
    let pause = handler.install_alert_plan_effect_pause();
    let task_handler = handler.clone();
    let mut task = tokio::spawn(async move {
        task_handler.alerts_queue_window(alerted, WINDOW_BELL).await;
    });
    timeout(Duration::from_secs(3), pause.reached.notified())
        .await
        .expect("alert plan pauses before effects");
    let response = handler
        .handle(Request::KillSession(KillSessionRequest {
            target: alpha.clone(),
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        }))
        .await;
    assert!(matches!(response, Response::KillSession(_)), "{response:?}");
    let reused_alpha = create_quiet_session(&handler, alpha.as_str()).await;
    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    handler.register_attach(712, reused_alpha, control_tx).await;
    drain_controls(&mut control_rx).await;
    pause.release.notify_one();
    timeout(Duration::from_secs(5), &mut task)
        .await
        .expect("destroyed alert plan finishes")
        .expect("alert plan task succeeds");
    assert!(
        timeout(Duration::from_millis(150), control_rx.recv())
            .await
            .is_err(),
        "new alpha incarnation must receive no destroyed-session alert effect"
    );
}
