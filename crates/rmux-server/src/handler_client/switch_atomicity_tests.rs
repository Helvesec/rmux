use super::*;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use rmux_core::command_parser::CommandParser;
use rmux_core::LifecycleEvent;
use rmux_proto::request::SwitchClientExt3Request;
use rmux_proto::{
    ControlMode, NewSessionRequest, NewWindowRequest, Request, Response, ScopeSelector,
    SplitDirection, SplitWindowRequest, SplitWindowTarget, TerminalPixels, TerminalSize,
};
use tokio::sync::mpsc;

use crate::control::{ControlModeUpgrade, ControlServerEvent, CONTROL_SERVER_EVENT_CAPACITY};

const ENVIRONMENT_HELPER: &str = "RMUX_TEST_SWITCH_ATOMICITY_ENVIRONMENT_HELPER";
const INITIAL_SIZE: TerminalSize = TerminalSize { cols: 80, rows: 24 };
const SWITCH_SIZE: TerminalSize = TerminalSize {
    cols: 117,
    rows: 39,
};

struct EnvironmentChild(std::process::Child);

struct ModeTreePreservationSnapshot {
    session_name: rmux_proto::SessionName,
    observer_pid: u32,
    observer_attach_id: u64,
    pane_id: rmux_proto::PaneId,
    mode_tree_state_id: u64,
    persistent_overlay_epoch: u64,
    overlay_generation: u64,
}

impl Drop for EnvironmentChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn switch_atomicity_environment_probe_helper() {
    if std::env::var_os(ENVIRONMENT_HELPER).is_some() {
        std::thread::sleep(std::time::Duration::from_secs(120));
    }
}

async fn spawn_environment_child(display: &str) -> EnvironmentChild {
    let executable = std::env::current_exe().expect("current test executable");
    let mut command = std::process::Command::new(executable);
    command.args([
        "--exact",
        "handler::client_support::switch_atomicity_tests::switch_atomicity_environment_probe_helper",
        "--test-threads=1",
    ]);
    command.env(ENVIRONMENT_HELPER, "1");
    command.env("DISPLAY", display);
    command
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let child = EnvironmentChild(command.spawn().expect("spawn environment helper"));
    let pid = child.0.id();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if rmux_os::process::environment(pid)
                .as_ref()
                .and_then(|environment| environment.get("DISPLAY"))
                .is_some_and(|value| value == display)
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("environment helper publishes DISPLAY");
    child
}

fn session_name(value: &str) -> rmux_proto::SessionName {
    rmux_proto::SessionName::new(value).expect("valid test session")
}

async fn create_session(handler: &RequestHandler, name: rmux_proto::SessionName) {
    let response = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: name,
            detached: true,
            size: Some(INITIAL_SIZE),
            environment: None,
        }))
        .await;
    assert!(matches!(response, Response::NewSession(_)), "{response:?}");
}

async fn create_runtime_window(
    handler: &RequestHandler,
    session_name: &rmux_proto::SessionName,
) -> u32 {
    let response = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: session_name.clone(),
            name: Some("atomic-target".to_owned()),
            detached: true,
            environment: None,
            command: None,
            start_directory: None,
            target_window_index: None,
            insert_at_target: false,
            process_command: None,
        })))
        .await;
    let Response::NewWindow(response) = response else {
        panic!("runtime window creation failed: {response:?}");
    };
    handler.wait_for_initial_panes_for_test().await;
    response.target.window_index()
}

async fn create_second_pane(handler: &RequestHandler, session_name: &rmux_proto::SessionName) {
    let response = handler
        .handle(Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Session(session_name.clone()),
            direction: SplitDirection::Vertical,
            before: false,
            environment: None,
        }))
        .await;
    assert!(matches!(response, Response::SplitWindow(_)), "{response:?}");
    handler.wait_for_initial_panes_for_test().await;
}

async fn pane_terminal_size(
    handler: &RequestHandler,
    session_name: &rmux_proto::SessionName,
    window_index: u32,
) -> TerminalSize {
    let master = handler
        .state
        .lock()
        .await
        .clone_pane_master_if_alive(session_name, window_index, 0)
        .expect("test pane terminal remains alive");
    let size = master.size().expect("test pane terminal exposes its size");
    TerminalSize {
        cols: size.cols,
        rows: size.rows,
    }
}

async fn set_attached_geometry(handler: &RequestHandler, attach_pid: u32) {
    let mut active_attach = handler.active_attach.lock().await;
    let size_sequence = active_attach.next_size_sequence;
    active_attach.next_size_sequence = size_sequence.saturating_add(1);
    let active = active_attach
        .by_pid
        .get_mut(&attach_pid)
        .expect("attached test client exists");
    active.client_size = SWITCH_SIZE;
    active.client_pixels = Some(TerminalPixels::new(1170, 780));
    active.size_sequence = size_sequence;
    drop(active_attach);
    handler.bump_active_attach_epoch();
}

async fn open_zoomed_choose_tree(handler: &RequestHandler, attach_pid: u32) {
    let parsed = CommandParser::new()
        .parse_arguments(["choose-tree", "-Zw"])
        .expect("zoomed choose-tree parses");
    let command = RequestHandler::parse_mode_tree_queue_command(parsed.commands()[0].clone())
        .expect("zoomed choose-tree command is valid")
        .expect("choose-tree is recognized");
    handler
        .execute_queued_mode_tree(
            attach_pid,
            command,
            &super::super::scripting_support::QueueExecutionContext::without_caller_cwd(),
        )
        .await
        .expect("zoomed choose-tree opens");
}

async fn capture_mode_tree_preservation_snapshot(
    handler: &RequestHandler,
    session_name: &rmux_proto::SessionName,
    observer_pid: u32,
    observer_attach_id: u64,
) -> ModeTreePreservationSnapshot {
    let (pane_id, zoomed, pane_in_mode) = {
        let state = handler.state.lock().await;
        let session = state
            .sessions
            .session(session_name)
            .expect("session exists");
        let pane_id = session.active_pane_id().expect("active pane exists");
        (
            pane_id,
            session.window().is_zoomed(),
            state.pane_in_mode(session_name, pane_id),
        )
    };
    let active_attach = handler.active_attach.lock().await;
    let observer = active_attach
        .by_pid
        .get(&observer_pid)
        .expect("mode-tree observer remains attached");
    assert_eq!(observer.id, observer_attach_id);
    let mode_zoom_restore = observer
        .mode_tree
        .as_ref()
        .and_then(|mode| mode.zoom_restore.clone());
    assert!(
        observer.mode_tree.is_some(),
        "observer receives mode-tree state"
    );
    assert!(
        zoomed,
        "choose-tree -Z should zoom its host window; restore target: {mode_zoom_restore:?}"
    );
    assert!(pane_in_mode, "choose-tree host pane should remain in mode");
    ModeTreePreservationSnapshot {
        session_name: session_name.clone(),
        observer_pid,
        observer_attach_id,
        pane_id,
        mode_tree_state_id: observer.mode_tree_state_id,
        persistent_overlay_epoch: observer.persistent_overlay_epoch.load(Ordering::SeqCst),
        overlay_generation: observer.overlay_generation,
    }
}

async fn assert_mode_tree_preserved(
    handler: &RequestHandler,
    snapshot: &ModeTreePreservationSnapshot,
    observer_rx: &mut mpsc::UnboundedReceiver<crate::pane_io::AttachControl>,
) {
    {
        let state = handler.state.lock().await;
        let session = state
            .sessions
            .session(&snapshot.session_name)
            .expect("mode-tree session survives");
        assert!(
            session.window().is_zoomed(),
            "failed switch must preserve zoom"
        );
        assert!(
            state.pane_in_mode(&snapshot.session_name, snapshot.pane_id),
            "failed switch must preserve pane mode"
        );
    }
    let active_attach = handler.active_attach.lock().await;
    let observer = active_attach
        .by_pid
        .get(&snapshot.observer_pid)
        .expect("observer survives failed switch");
    assert_eq!(observer.id, snapshot.observer_attach_id);
    assert!(
        observer.mode_tree.is_some(),
        "failed switch must preserve tree"
    );
    assert_eq!(observer.mode_tree_state_id, snapshot.mode_tree_state_id);
    assert_eq!(
        observer.persistent_overlay_epoch.load(Ordering::SeqCst),
        snapshot.persistent_overlay_epoch
    );
    assert_eq!(observer.overlay_generation, snapshot.overlay_generation);
    drop(active_attach);
    assert!(
        matches!(
            observer_rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ),
        "failed switch must not send mode-tree dismissal controls"
    );
}

fn switch_request(target: String) -> SwitchClientExt3Request {
    SwitchClientExt3Request {
        target_client: None,
        target: Some(target),
        key_table: None,
        last_session: false,
        next_session: false,
        previous_session: false,
        toggle_read_only: false,
        sort_order: None,
        skip_environment_update: false,
        zoom: false,
    }
}

#[tokio::test]
async fn closed_attach_switch_rolls_back_environment_geometry_selection_touch_and_runtime() {
    let handler = RequestHandler::new();
    let alpha = session_name("switch-atomic-attach-alpha");
    let beta = session_name("switch-atomic-attach-beta");
    create_session(&handler, alpha.clone()).await;
    create_session(&handler, beta.clone()).await;
    create_second_pane(&handler, &alpha).await;
    let target_window = create_runtime_window(&handler, &beta).await;
    let requester = spawn_environment_child("switch-atomic-attach-after").await;
    let attach_pid = requester.0.id();
    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(attach_pid, alpha.clone(), control_tx)
        .await;
    // `refresh_attached_session` deliberately prunes dead OS pids. Use the
    // live test-process pid for the observer so this regression fixture does
    // not accidentally exercise stale-client cleanup while opening the tree.
    let observer_pid = std::process::id();
    let (observer_tx, mut observer_rx) = mpsc::unbounded_channel();
    let observer_attach_id = handler
        .register_attach(observer_pid, alpha.clone(), observer_tx)
        .await;
    set_attached_geometry(&handler, attach_pid).await;
    open_zoomed_choose_tree(&handler, attach_pid).await;
    while control_rx.try_recv().is_ok() {}
    while observer_rx.try_recv().is_ok() {}
    let mode_tree_before =
        capture_mode_tree_preservation_snapshot(&handler, &alpha, observer_pid, observer_attach_id)
            .await;
    drop(control_rx);

    let initial_pixels = TerminalPixels::new(800, 480);
    let (before_session, before_display, before_runtime_resize_count) = {
        let mut state = handler.state.lock().await;
        state.environment.set(
            ScopeSelector::Session(beta.clone()),
            "DISPLAY".to_owned(),
            "switch-atomic-attach-before".to_owned(),
        );
        state.set_attached_terminal_pixels(&beta, Some(initial_pixels));
        (
            state.sessions.session(&beta).expect("beta exists").clone(),
            state
                .environment
                .session_value(&beta, "DISPLAY")
                .map(str::to_owned),
            state.window_runtime_resize_count_for_test(),
        )
    };
    let before_target_pty = pane_terminal_size(&handler, &beta, target_window).await;

    let response = handler
        .handle_switch_client_ext3(
            attach_pid,
            switch_request(format!("{beta}:{target_window}")),
        )
        .await;
    assert_eq!(
        response,
        Response::Error(rmux_proto::ErrorResponse {
            error: attached_client_required("switch-client"),
        })
    );

    let state = handler.state.lock().await;
    assert_eq!(state.sessions.session(&beta), Some(&before_session));
    assert_eq!(
        state.environment.session_value(&beta, "DISPLAY"),
        before_display.as_deref()
    );
    assert_eq!(
        state.attached_terminal_pixels_for_test(&beta),
        Some(initial_pixels)
    );
    assert_eq!(
        state.window_runtime_resize_count_for_test(),
        before_runtime_resize_count,
        "a deterministically closed receiver must not touch target PTYs"
    );
    drop(state);
    assert_eq!(
        pane_terminal_size(&handler, &beta, target_window).await,
        before_target_pty
    );
    assert_mode_tree_preserved(&handler, &mode_tree_before, &mut observer_rx).await;
    assert!(!handler
        .active_attach
        .lock()
        .await
        .by_pid
        .contains_key(&attach_pid));
}

#[tokio::test]
async fn receiver_close_after_precheck_uses_runtime_rollback_for_the_residual_race() {
    let handler = RequestHandler::new();
    let alpha = session_name("switch-atomic-race-alpha");
    let beta = session_name("switch-atomic-race-beta");
    create_session(&handler, alpha.clone()).await;
    create_session(&handler, beta.clone()).await;
    let target_window = create_runtime_window(&handler, &beta).await;
    let requester = spawn_environment_child("switch-atomic-race-after").await;
    let attach_pid = requester.0.id();
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    handler.register_attach(attach_pid, alpha, control_tx).await;
    set_attached_geometry(&handler, attach_pid).await;
    let closing = handler
        .active_attach
        .lock()
        .await
        .by_pid
        .get(&attach_pid)
        .expect("attached test client exists")
        .closing
        .clone();

    let initial_pixels = TerminalPixels::new(800, 480);
    let (before_session, before_display, before_runtime_resize_count) = {
        let mut state = handler.state.lock().await;
        state.environment.set(
            ScopeSelector::Session(beta.clone()),
            "DISPLAY".to_owned(),
            "switch-atomic-race-before".to_owned(),
        );
        state.set_attached_terminal_pixels(&beta, Some(initial_pixels));
        (
            state.sessions.session(&beta).expect("beta exists").clone(),
            state
                .environment
                .session_value(&beta, "DISPLAY")
                .map(str::to_owned),
            state.window_runtime_resize_count_for_test(),
        )
    };
    let before_target_pty = pane_terminal_size(&handler, &beta, target_window).await;
    let pause = handler.install_attached_switch_post_closed_check_pause(attach_pid);
    let switch_handler = handler.clone();
    let switch_target = format!("{beta}:{target_window}");
    let switch = tokio::spawn(async move {
        switch_handler
            .handle_switch_client_ext3(attach_pid, switch_request(switch_target))
            .await
    });

    pause.reached.notified().await;
    drop(control_rx);
    pause.release.notify_one();
    assert_eq!(
        switch.await.expect("switch task joins"),
        Response::Error(rmux_proto::ErrorResponse {
            error: attached_client_required("switch-client"),
        })
    );

    let state = handler.state.lock().await;
    assert_eq!(state.sessions.session(&beta), Some(&before_session));
    assert_eq!(
        state.environment.session_value(&beta, "DISPLAY"),
        before_display.as_deref()
    );
    assert_eq!(
        state.attached_terminal_pixels_for_test(&beta),
        Some(initial_pixels)
    );
    assert!(
        state.window_runtime_resize_count_for_test() >= before_runtime_resize_count + 2,
        "the residual close-after-check race must resize once and explicitly roll the runtime back"
    );
    drop(state);
    assert_eq!(
        pane_terminal_size(&handler, &beta, target_window).await,
        before_target_pty
    );
    assert!(
        closing.load(Ordering::SeqCst),
        "send-failure removal marks the residual-race identity closing"
    );
    assert!(!handler
        .active_attach
        .lock()
        .await
        .by_pid
        .contains_key(&attach_pid));
}

#[tokio::test]
async fn full_switch_backlog_closes_and_removes_attach_before_runtime_mutation() {
    let handler = RequestHandler::new();
    let alpha = session_name("switch-atomic-backlog-alpha");
    let beta = session_name("switch-atomic-backlog-beta");
    create_session(&handler, alpha.clone()).await;
    create_session(&handler, beta.clone()).await;
    let target_window = create_runtime_window(&handler, &beta).await;
    let requester = spawn_environment_child("switch-atomic-backlog-host").await;
    let attach_pid = requester.0.id();
    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    handler.register_attach(attach_pid, alpha, control_tx).await;
    set_attached_geometry(&handler, attach_pid).await;
    let (backlog, closing) = {
        let active_attach = handler.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get(&attach_pid)
            .expect("attached test client exists");
        (active.control_backlog.clone(), active.closing.clone())
    };
    backlog.store(
        super::super::attach_support::ATTACH_CONTROL_BACKLOG_LIMIT,
        Ordering::Release,
    );
    let before_runtime_resize_count = handler
        .state
        .lock()
        .await
        .window_runtime_resize_count_for_test();

    let response = handler
        .handle_switch_client_ext3(
            attach_pid,
            switch_request(format!("{beta}:{target_window}")),
        )
        .await;
    assert!(
        matches!(response, Response::Error(ref error) if error.error.to_string().contains("not draining updates")),
        "{response:?}"
    );
    assert!(closing.load(Ordering::SeqCst));
    assert!(!handler
        .active_attach
        .lock()
        .await
        .by_pid
        .contains_key(&attach_pid));
    assert_eq!(
        handler
            .state
            .lock()
            .await
            .window_runtime_resize_count_for_test(),
        before_runtime_resize_count,
        "backlog rejection must happen before target PTY mutation"
    );
    let mut saw_detach = false;
    while let Ok(control) = control_rx.try_recv() {
        saw_detach |= matches!(control, crate::pane_io::AttachControl::Detach);
    }
    assert!(
        saw_detach,
        "overloaded client receives a best-effort detach"
    );
}

#[tokio::test]
async fn successful_attach_switch_applies_mode_tree_dismissal_after_delivery() {
    let handler = RequestHandler::new();
    let alpha = session_name("switch-atomic-mode-tree-alpha");
    let beta = session_name("switch-atomic-mode-tree-beta");
    create_session(&handler, alpha.clone()).await;
    create_session(&handler, beta.clone()).await;
    create_second_pane(&handler, &alpha).await;

    let requester = spawn_environment_child("switch-atomic-mode-tree-host").await;
    let attach_pid = requester.0.id();
    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(attach_pid, alpha.clone(), control_tx)
        .await;
    let observer_pid = std::process::id();
    let (observer_tx, mut observer_rx) = mpsc::unbounded_channel();
    let observer_attach_id = handler
        .register_attach(observer_pid, alpha.clone(), observer_tx)
        .await;
    open_zoomed_choose_tree(&handler, attach_pid).await;
    while control_rx.try_recv().is_ok() {}
    while observer_rx.try_recv().is_ok() {}
    let mode_tree_before =
        capture_mode_tree_preservation_snapshot(&handler, &alpha, observer_pid, observer_attach_id)
            .await;

    let response = handler
        .handle_switch_client_ext3(attach_pid, switch_request(beta.to_string()))
        .await;
    assert!(
        matches!(response, Response::SwitchClient(_)),
        "{response:?}"
    );

    {
        let state = handler.state.lock().await;
        let alpha_session = state.sessions.session(&alpha).expect("alpha survives");
        assert!(
            !alpha_session.window().is_zoomed(),
            "committed switch restores mode-tree zoom"
        );
        assert!(
            !state.pane_in_mode(&alpha, mode_tree_before.pane_id),
            "committed switch clears the host pane mode"
        );
    }
    let active_attach = handler.active_attach.lock().await;
    let observer = active_attach
        .by_pid
        .get(&observer_pid)
        .expect("observer remains attached");
    assert!(observer.mode_tree.is_none());
    assert_eq!(
        observer.mode_tree_state_id,
        mode_tree_before.mode_tree_state_id.saturating_add(1)
    );
    assert_eq!(
        observer.persistent_overlay_epoch.load(Ordering::SeqCst),
        observer.mode_tree_state_id
    );
    assert!(
        observer.overlay_generation > mode_tree_before.overlay_generation,
        "committed dismissal must advance the shared overlay generation"
    );
    let switched = active_attach
        .by_pid
        .get(&attach_pid)
        .expect("switching attach survives");
    assert_eq!(switched.session_name, beta);
    assert!(switched.mode_tree.is_none());
    drop(active_attach);

    let mut saw_advance = false;
    while let Ok(control) = observer_rx.try_recv() {
        saw_advance |= matches!(
            control,
            crate::pane_io::AttachControl::AdvancePersistentOverlayState(_)
        );
    }
    assert!(
        saw_advance,
        "committed dismissal advances observer overlay state"
    );
}

#[tokio::test]
async fn concurrent_switch_recomputes_mode_tree_source_under_commit_locks() {
    let handler = RequestHandler::new();
    let alpha = session_name("switch-atomic-concurrent-alpha");
    let beta = session_name("switch-atomic-concurrent-beta");
    create_session(&handler, alpha.clone()).await;
    create_session(&handler, beta.clone()).await;
    create_second_pane(&handler, &beta).await;
    let requester = spawn_environment_child("switch-atomic-concurrent-host").await;
    let attach_pid = requester.0.id();
    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(attach_pid, alpha.clone(), control_tx)
        .await;

    // The first switch resolves while alpha is current, so an early cached
    // `switch_changes_session` value would be false. Hold it after size
    // selection while a second switch moves the same attach id to beta.
    let pause = handler.install_attached_size_selection_pause();
    let first_handler = handler.clone();
    let first_target = alpha.to_string();
    let first_switch = tokio::spawn(async move {
        first_handler
            .handle_switch_client_ext3(attach_pid, switch_request(first_target))
            .await
    });
    pause.reached.notified().await;

    let second_response = handler
        .handle_switch_client_ext3(attach_pid, switch_request(beta.to_string()))
        .await;
    assert!(
        matches!(second_response, Response::SwitchClient(_)),
        "{second_response:?}"
    );
    let observer_pid = std::process::id();
    let (observer_tx, mut observer_rx) = mpsc::unbounded_channel();
    let observer_attach_id = handler
        .register_attach(observer_pid, beta.clone(), observer_tx)
        .await;
    open_zoomed_choose_tree(&handler, attach_pid).await;
    while control_rx.try_recv().is_ok() {}
    while observer_rx.try_recv().is_ok() {}
    let mode_tree_before =
        capture_mode_tree_preservation_snapshot(&handler, &beta, observer_pid, observer_attach_id)
            .await;

    pause.release.notify_one();
    let first_response = first_switch.await.expect("first switch task joins");
    assert!(
        matches!(first_response, Response::SwitchClient(_)),
        "{first_response:?}"
    );

    {
        let state = handler.state.lock().await;
        let beta_session = state.sessions.session(&beta).expect("beta survives");
        assert!(
            !beta_session.window().is_zoomed(),
            "the actual beta source zoom must be restored"
        );
        assert!(
            !state.pane_in_mode(&beta, mode_tree_before.pane_id),
            "the actual beta source pane mode must be cleared"
        );
    }
    let active_attach = handler.active_attach.lock().await;
    let observer = active_attach
        .by_pid
        .get(&observer_pid)
        .expect("beta observer remains attached");
    assert!(observer.mode_tree.is_none());
    assert_eq!(
        observer.mode_tree_state_id,
        mode_tree_before.mode_tree_state_id.saturating_add(1)
    );
    assert_eq!(
        observer.persistent_overlay_epoch.load(Ordering::SeqCst),
        observer.mode_tree_state_id
    );
    assert!(
        observer.overlay_generation > mode_tree_before.overlay_generation,
        "concurrent dismissal must advance the shared overlay generation"
    );
    let switching = active_attach
        .by_pid
        .get(&attach_pid)
        .expect("switching attach survives");
    assert_eq!(switching.session_name, alpha);
    drop(active_attach);
    let mut saw_advance = false;
    while let Ok(control) = observer_rx.try_recv() {
        saw_advance |= matches!(
            control,
            crate::pane_io::AttachControl::AdvancePersistentOverlayState(_)
        );
    }
    assert!(saw_advance, "the actual source observer is dismissed");
}

#[tokio::test]
async fn closed_control_switch_preserves_environment_selection_and_touch() {
    let handler = RequestHandler::new();
    let alpha = session_name("switch-atomic-control-alpha");
    let beta = session_name("switch-atomic-control-beta");
    create_session(&handler, alpha.clone()).await;
    create_session(&handler, beta.clone()).await;
    let target_window = create_runtime_window(&handler, &beta).await;
    let alpha_id = handler
        .state
        .lock()
        .await
        .sessions
        .session(&alpha)
        .expect("alpha exists")
        .id();
    let requester = spawn_environment_child("switch-atomic-control-after").await;
    let control_pid = requester.0.id();
    let (event_tx, mut event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let control_id = handler
        .register_control_with_closing(
            control_pid,
            ControlModeUpgrade {
                mode: ControlMode::Plain,
                terminal_context: crate::outer_terminal::OuterTerminalContext::default(),
                initial_command_count: 0,
            },
            event_tx,
            Arc::new(AtomicBool::new(false)),
        )
        .await;
    handler
        .set_control_session(control_pid, Some(alpha.clone()))
        .await
        .expect("initial control session set succeeds");
    assert!(matches!(
        event_rx.try_recv(),
        Ok(ControlServerEvent::SessionChanged(Some(_))
            | ControlServerEvent::SessionChangedAt { .. })
    ));
    drop(event_rx);

    let (before_session, before_display) = {
        let mut state = handler.state.lock().await;
        state.environment.set(
            ScopeSelector::Session(beta.clone()),
            "DISPLAY".to_owned(),
            "switch-atomic-control-before".to_owned(),
        );
        (
            state.sessions.session(&beta).expect("beta exists").clone(),
            state
                .environment
                .session_value(&beta, "DISPLAY")
                .map(str::to_owned),
        )
    };
    let mut lifecycle = handler.subscribe_lifecycle_events();

    let response = handler
        .handle_switch_client_ext3(
            control_pid,
            switch_request(format!("{beta}:{target_window}")),
        )
        .await;
    assert_eq!(
        response,
        Response::Error(rmux_proto::ErrorResponse {
            error: attached_client_required("switch-client"),
        })
    );
    let state = handler.state.lock().await;
    assert_eq!(state.sessions.session(&beta), Some(&before_session));
    assert_eq!(
        state.environment.session_value(&beta, "DISPLAY"),
        before_display.as_deref()
    );
    drop(state);
    {
        let active_control = handler.active_control.lock().await;
        let active = active_control
            .by_pid
            .get(&control_pid)
            .expect("failed switch keeps the exact closing control registration");
        assert_eq!(active.id, control_id);
        assert!(active.closing.load(Ordering::SeqCst));
        assert_eq!(active.session_name.as_ref(), Some(&alpha));
        assert_eq!(active.session_id, Some(alpha_id));
        assert_eq!(active.last_session, None);
        assert_eq!(active.last_session_id, None);
    }

    handler.finish_control(control_pid, control_id).await;

    assert!(!handler
        .active_control
        .lock()
        .await
        .by_pid
        .contains_key(&control_pid));
    let detached = tokio::time::timeout(std::time::Duration::from_secs(1), lifecycle.recv())
        .await
        .expect("transport finish publishes client-detached")
        .expect("lifecycle channel remains open");
    assert_eq!(detached.control_session_identity, Some(alpha_id));
    assert!(matches!(
        detached.event,
        LifecycleEvent::ClientDetached {
            session_name,
            client_name: Some(client_name),
        } if session_name == alpha && client_name == control_pid.to_string()
    ));
}

#[tokio::test]
async fn attach_identity_replacement_after_size_selection_commits_no_target_mutation() {
    let handler = RequestHandler::new();
    let alpha = session_name("switch-atomic-replace-alpha");
    let beta = session_name("switch-atomic-replace-beta");
    create_session(&handler, alpha.clone()).await;
    create_session(&handler, beta.clone()).await;
    create_second_pane(&handler, &alpha).await;
    let target_window = create_runtime_window(&handler, &beta).await;
    let requester = spawn_environment_child("switch-atomic-replace-after").await;
    let attach_pid = requester.0.id();
    let (old_tx, mut old_rx) = mpsc::unbounded_channel();
    let old_id = handler
        .register_attach(attach_pid, alpha.clone(), old_tx)
        .await;
    let observer_pid = std::process::id();
    let (observer_tx, mut observer_rx) = mpsc::unbounded_channel();
    let observer_attach_id = handler
        .register_attach(observer_pid, alpha.clone(), observer_tx)
        .await;
    set_attached_geometry(&handler, attach_pid).await;
    open_zoomed_choose_tree(&handler, attach_pid).await;
    while old_rx.try_recv().is_ok() {}
    while observer_rx.try_recv().is_ok() {}
    let mode_tree_before =
        capture_mode_tree_preservation_snapshot(&handler, &alpha, observer_pid, observer_attach_id)
            .await;
    let before_session = {
        let mut state = handler.state.lock().await;
        state.environment.set(
            ScopeSelector::Session(beta.clone()),
            "DISPLAY".to_owned(),
            "switch-atomic-replace-before".to_owned(),
        );
        state.sessions.session(&beta).expect("beta exists").clone()
    };
    let pause = handler.install_attached_size_selection_pause();

    let switch_handler = handler.clone();
    let switch_target = format!("{beta}:{target_window}");
    let switch = tokio::spawn(async move {
        switch_handler
            .handle_switch_client_ext3(attach_pid, switch_request(switch_target))
            .await
    });
    pause.reached.notified().await;
    let (replacement_tx, mut replacement_rx) = mpsc::unbounded_channel();
    let replacement_id = handler
        .register_attach(attach_pid, alpha.clone(), replacement_tx)
        .await;
    assert_ne!(replacement_id, old_id);
    pause.release.notify_one();

    assert_eq!(
        switch.await.expect("switch task joins"),
        Response::Error(rmux_proto::ErrorResponse {
            error: attached_client_required("switch-client"),
        })
    );
    assert!(matches!(
        replacement_rx.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
    let state = handler.state.lock().await;
    assert_eq!(state.sessions.session(&beta), Some(&before_session));
    assert_eq!(
        state.environment.session_value(&beta, "DISPLAY"),
        Some("switch-atomic-replace-before")
    );
    drop(state);
    assert_mode_tree_preserved(&handler, &mode_tree_before, &mut observer_rx).await;
    let active_attach = handler.active_attach.lock().await;
    let replacement = active_attach
        .by_pid
        .get(&attach_pid)
        .expect("replacement attach survives");
    assert_eq!(replacement.id, replacement_id);
    assert_eq!(replacement.session_name, alpha);
}
