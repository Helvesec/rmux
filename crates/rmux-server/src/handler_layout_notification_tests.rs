use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use rmux_core::LifecycleEvent;
use rmux_proto::{
    ControlMode, PaneKillRequest, PaneResizeRequest, PaneTargetRef, Request, ResizePaneAdjustment,
    Response, RmuxError, SessionName, TerminalSize,
};
use tokio::sync::{mpsc, oneshot};

use super::RequestHandler;
use crate::control::{ControlModeUpgrade, ControlServerEvent, CONTROL_SERVER_EVENT_CAPACITY};

#[path = "handler_layout_notification_tests/linked_transfer_aliases.rs"]
mod linked_transfer_aliases;

const CONTROL_NOTIFICATION_SETTLE: Duration = Duration::from_millis(100);
const CONTROL_NOTIFICATION_POLL: Duration = Duration::from_millis(10);

struct LayoutCommandCase {
    label: &'static str,
    setup: &'static [&'static str],
    warmup: Option<&'static str>,
    command: &'static str,
    expected: usize,
}

#[tokio::test]
async fn layout_command_sites_keep_one_canonical_notification_per_mutation_product_divergence() {
    // tmux 3.7b oracle, measured 2026-07-26 in control mode:
    // select-layout (same and different), next-layout and previous-layout
    // publish twice; move-pane publishes three times; resize-pane, swap-pane
    // and kill-pane publish once. RMUX intentionally has one canonical
    // WindowLayoutChanged producer for each committed mutation.
    let cases = [
        LayoutCommandCase {
            label: "select-layout-identical",
            setup: &["split-window -d -h -t {session}"],
            warmup: Some("select-layout -t {session} even-horizontal"),
            command: "select-layout -t {session} even-horizontal",
            expected: 1,
        },
        LayoutCommandCase {
            label: "select-layout-different",
            setup: &["split-window -d -h -t {session}"],
            warmup: None,
            command: "select-layout -t {session} even-vertical",
            expected: 1,
        },
        LayoutCommandCase {
            label: "next-layout",
            setup: &["split-window -d -h -t {session}"],
            warmup: None,
            command: "next-layout -t {session}",
            expected: 1,
        },
        LayoutCommandCase {
            label: "previous-layout",
            setup: &["split-window -d -h -t {session}"],
            warmup: None,
            command: "previous-layout -t {session}",
            expected: 1,
        },
        LayoutCommandCase {
            label: "resize-pane",
            setup: &["split-window -d -h -t {session}"],
            warmup: None,
            command: "resize-pane -t {session}:.0 -R 5",
            expected: 1,
        },
        LayoutCommandCase {
            label: "swap-pane",
            setup: &["split-window -d -h -t {session}"],
            warmup: None,
            command: "swap-pane -s {session}:.0 -t {session}:.1",
            expected: 1,
        },
        LayoutCommandCase {
            label: "move-pane",
            setup: &[
                "split-window -d -h -t {session}",
                "split-window -d -v -t {session}:.0",
            ],
            warmup: None,
            command: "move-pane -s {session}:.2 -t {session}:.1 -h",
            expected: 1,
        },
        LayoutCommandCase {
            label: "kill-pane",
            setup: &["split-window -d -h -t {session}"],
            warmup: None,
            command: "kill-pane -t {session}:.1",
            expected: 1,
        },
    ];

    let mut actual = Vec::with_capacity(cases.len());
    let mut expected = Vec::with_capacity(cases.len());
    for case in cases {
        let session =
            SessionName::new(format!("layout-notify-{}", case.label)).expect("test session name");
        let handler = RequestHandler::new();
        create_session(&handler, &session).await;
        for command in case.setup {
            run_detached_command(&handler, &render_command(command, &session)).await;
        }
        if let Some(command) = case.warmup {
            run_detached_command(&handler, &render_command(command, &session)).await;
        }
        let (control_pid, mut notifications) = register_control_client(&handler, &session).await;
        let _ = settle_control_notifications(&mut notifications).await;

        run_control_command(
            &handler,
            control_pid,
            &render_command(case.command, &session),
        )
        .await;
        let count = layout_change_count(&mut notifications).await;
        actual.push((case.label, count));
        expected.push((case.label, case.expected));
    }

    assert_eq!(actual, expected);
}

#[tokio::test]
async fn custom_layout_resize_has_one_layout_notification_not_two() {
    let handler = RequestHandler::new();
    let session = SessionName::new("layout-notify-custom-resize").expect("test session name");
    create_session(&handler, &session).await;
    run_detached_command(&handler, &format!("split-window -d -h -t {session}")).await;
    run_detached_command(&handler, &format!("resize-window -t {session} -x 60 -y 20")).await;
    let small_layout = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&session)
            .expect("test session")
            .window()
            .layout_dump()
    };
    run_detached_command(&handler, &format!("resize-window -t {session} -x 80 -y 24")).await;

    let (control_pid, mut notifications) = register_control_client(&handler, &session).await;
    let _ = settle_control_notifications(&mut notifications).await;
    let mut lifecycle_events = handler.subscribe_lifecycle_events();
    run_control_command(
        &handler,
        control_pid,
        &format!("select-layout -t {session} \"{small_layout}\""),
    )
    .await;

    assert_eq!(layout_change_count(&mut notifications).await, 1);
    let events = std::iter::from_fn(|| lifecycle_events.try_recv().ok())
        .map(|event| event.event)
        .collect::<Vec<_>>();
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, LifecycleEvent::WindowLayoutChanged { .. }))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, LifecycleEvent::WindowResized { .. }))
            .count(),
        1,
        "deduplicating the layout producer must keep the resize hook"
    );
}

/// A detached 80x24 window split four times and resized to 6x4: five
/// stacked one-row panes hold it at 6x9. tmux 3.7b fires window-resized for
/// every applied refit of such a manual window; RMUX keeps its one canonical
/// layout notification beside it.
async fn manual_window_refit_events(handler: &RequestHandler, label: &str, commands: &[&str]) {
    let session = SessionName::new(format!("layout-notify-manual-{label}")).expect("test session");
    create_session(handler, &session).await;
    for _ in 0..4 {
        run_detached_command(handler, &format!("split-window -d -v -t {session}")).await;
    }
    run_detached_command(handler, &format!("resize-window -t {session} -x 6 -y 4")).await;
    let (control_pid, mut notifications) = register_control_client(handler, &session).await;
    let _ = settle_control_notifications(&mut notifications).await;

    for command in commands {
        let command = render_command(command, &session);
        let mut lifecycle_events = handler.subscribe_lifecycle_events();
        run_control_command(handler, control_pid, &command).await;
        assert_eq!(
            layout_change_count(&mut notifications).await,
            1,
            "{command}"
        );
        let events = std::iter::from_fn(|| lifecycle_events.try_recv().ok())
            .map(|event| event.event)
            .collect::<Vec<_>>();
        let count = |resized: bool| {
            events
                .iter()
                .filter(|event| match event {
                    LifecycleEvent::WindowResized { .. } => resized,
                    LifecycleEvent::WindowLayoutChanged { .. } => !resized,
                    _ => false,
                })
                .count()
        };
        assert_eq!((count(false), count(true)), (1, 1), "{command}");
    }
}

#[tokio::test]
async fn manual_window_kill_pane_refit_publishes_its_resize() {
    // kill-pane takes the 6x9 window to 6x7.
    let handler = RequestHandler::new();
    manual_window_refit_events(&handler, "kill", &["kill-pane -t {session}:.4"]).await;
}

#[tokio::test]
async fn manual_window_select_layout_refit_publishes_its_resize() {
    // After kill-pane, select-layout main-vertical takes 6x7 to 6x5.
    let handler = RequestHandler::new();
    manual_window_refit_events(
        &handler,
        "layout",
        &[
            "kill-pane -t {session}:.4",
            "select-layout -t {session} main-vertical",
        ],
    )
    .await;
}

#[tokio::test]
async fn join_and_move_reflow_minimum_target_and_publish_resize_product_divergence() {
    // tmux 3.7b rejects every width-one case below with "size or position no
    // space for a new pane". RMUX used to expand the target to the viable
    // three-column minimum instead, publishing a resize for a window the user
    // never resized; FMT1 R2 retires that divergence for the two transfer
    // commands, which now refuse atomically like tmux and publish nothing.
    // `split-window` keeps its own reflow. Both engines accept the width-three
    // neighborhood, where RMUX must not publish a spurious resize either.
    for operation in ["join-pane", "move-pane"] {
        for route in ["same", "cross"] {
            for (initial_width, expected_resize_events) in [(1, 0), (3, 0)] {
                let expect_refusal = initial_width == 1;
                let label = format!("{operation}-{route}-{initial_width}");
                let target = SessionName::new(format!("transfer-resize-target-{label}"))
                    .expect("target session");
                let source = SessionName::new(format!("transfer-resize-source-{label}"))
                    .expect("source session");
                let handler = RequestHandler::new();
                let lifecycle_dispatch = handler
                    .take_lifecycle_dispatch_receiver()
                    .expect("test owns lifecycle dispatch");
                let (hook_shutdown_tx, hook_shutdown_rx) = oneshot::channel();
                let hook_handler = handler.clone();
                let hook_task = tokio::spawn(async move {
                    hook_handler
                        .consume_lifecycle_hooks(lifecycle_dispatch, hook_shutdown_rx)
                        .await;
                });
                create_session(&handler, &target).await;
                run_detached_command(
                    &handler,
                    &format!("set-window-option -t {target}:0 window-size manual"),
                )
                .await;
                run_detached_command(
                    &handler,
                    &format!("resize-window -t {target}:0 -x {initial_width} -y 1"),
                )
                .await;

                let source_target = if route == "same" {
                    // `sleep` is not a Windows command, so cmd.exe would exit at
                    // once and destroy `:9` before the transfer. The shared
                    // stdin-discard command blocks on the pane's own terminal
                    // instead, which keeps the source window alive on both
                    // platforms.
                    run_detached_command(
                        &handler,
                        &format!(
                            "new-window -d -t {target}:9 {}",
                            crate::test_shell::command_quote(
                                &crate::test_shell::stdin_discard_command()
                            )
                        ),
                    )
                    .await;
                    format!("{target}:9.0")
                } else {
                    create_session(&handler, &source).await;
                    format!("{source}:0.0")
                };
                run_detached_command(&handler, "set-buffer -b transfer-events ''").await;
                run_detached_command(
                    &handler,
                    "set-hook -g window-layout-changed 'set-buffer -a -b transfer-events L'",
                )
                .await;
                run_detached_command(
                    &handler,
                    "set-hook -g window-resized 'set-buffer -a -b transfer-events R'",
                )
                .await;

                let (control_pid, mut notifications) =
                    register_control_client(&handler, &target).await;
                let _ = settle_control_notifications(&mut notifications).await;
                run_detached_command(&handler, "set-buffer -b transfer-events ''").await;
                let mut lifecycle_events = handler.subscribe_lifecycle_events();

                let command = format!("{} -h -d -s {source_target} -t {target}:0.0", operation);
                let error = run_control_command_result(&handler, control_pid, &command).await;
                if expect_refusal {
                    let error = error.unwrap_or_else(|| {
                        panic!("{label}: a destination with no room must refuse")
                    });
                    assert!(
                        error.to_string().contains("cannot split target pane"),
                        "{label}: unexpected refusal {error}"
                    );
                } else {
                    assert!(error.is_none(), "{label}: {error:?}");
                }
                let control_lines = settle_control_notifications(&mut notifications).await;
                let expected_hook_events = if expect_refusal { "" } else { "LL" };
                let hook_events =
                    wait_for_buffer_text(&handler, "transfer-events", expected_hook_events).await;
                let mut events = Vec::new();
                while let Ok(Ok(event)) =
                    tokio::time::timeout(CONTROL_NOTIFICATION_POLL, lifecycle_events.recv()).await
                {
                    events.push(event.event);
                }

                let (target_size, target_panes) = {
                    let state = handler.state.lock().await;
                    let window = state
                        .sessions
                        .session(&target)
                        .expect("target session")
                        .window_at(0)
                        .expect("target window");
                    (window.size(), window.pane_count())
                };
                assert_eq!(
                    (target_size, target_panes),
                    if expect_refusal {
                        (TerminalSize { cols: 1, rows: 1 }, 1)
                    } else {
                        (TerminalSize { cols: 3, rows: 1 }, 2)
                    },
                    "{label}: a refusal resizes nothing and moves no pane"
                );
                assert_eq!(
                    control_lines
                        .iter()
                        .filter(|line| line.starts_with("%layout-change "))
                        .count(),
                    usize::from(!expect_refusal),
                    "{label}: tmux 3.7b sends one control layout for the surviving target window"
                );
                assert_eq!(
                    events
                        .iter()
                        .filter(|event| matches!(event, LifecycleEvent::WindowLayoutChanged { .. }))
                        .count(),
                    if expect_refusal { 0 } else { 2 },
                    "{label}"
                );
                assert_eq!(
                    events
                        .iter()
                        .filter(|event| {
                            matches!(
                                event,
                                LifecycleEvent::WindowResized { target: resized }
                                    if resized.session_name() == &target
                                        && resized.window_index() == 0
                            )
                        })
                        .count(),
                    expected_resize_events,
                    "{label}: hooks={hook_events:?}, lifecycle={events:?}"
                );
                if expect_refusal {
                    // An empty buffer is either unset or empty depending on the
                    // buffer store; both mean no hook fired.
                    assert!(
                        hook_events.as_deref().unwrap_or_default().is_empty(),
                        "{label}: a refusal fires no layout or resize hook, got {hook_events:?}"
                    );
                } else {
                    assert_eq!(
                        hook_events,
                        Some(expected_hook_events.to_owned()),
                        "{label}: lifecycle hooks must observe the same committed resize"
                    );
                }
                let _ = expected_resize_events;

                let _ = hook_shutdown_tx.send(());
                hook_task.await.expect("lifecycle hook task");
            }
        }
    }
}

#[tokio::test]
async fn layout_no_ops_are_silent_on_cli_and_stable_id_paths() {
    // tmux 3.7b emits no %layout-change for a directional resize in a
    // one-pane window, resize-pane -T, or a pane swapped with itself.
    let handler = RequestHandler::new();
    let session = SessionName::new("layout-notify-no-op").expect("test session name");
    create_session(&handler, &session).await;
    let (control_pid, mut notifications) = register_control_client(&handler, &session).await;
    let _ = settle_control_notifications(&mut notifications).await;

    for command in [
        format!("resize-pane -t {session}:.0 -R 5"),
        format!("resize-pane -t {session}:.0 -T"),
    ] {
        run_control_command(&handler, control_pid, &command).await;
        assert_eq!(
            layout_change_count(&mut notifications).await,
            0,
            "{command}"
        );
    }

    run_detached_command(&handler, &format!("split-window -d -h -t {session}")).await;
    let _ = settle_control_notifications(&mut notifications).await;
    let swap_self = format!("swap-pane -s {session}:.0 -t {session}:.0");
    run_control_command(&handler, control_pid, &swap_self).await;
    assert_eq!(
        layout_change_count(&mut notifications).await,
        0,
        "{swap_self}"
    );

    let pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&session)
            .expect("test session")
            .window()
            .pane(0)
            .expect("test pane")
            .id()
    };
    let response = handler
        .handle(Request::PaneResize(PaneResizeRequest {
            target: PaneTargetRef::by_id(session.clone(), pane_id),
            adjustment: ResizePaneAdjustment::Left { cells: 250 },
        }))
        .await;
    assert!(matches!(response, Response::ResizePane(_)), "{response:?}");
    let _ = settle_control_notifications(&mut notifications).await;

    let response = handler
        .handle(Request::PaneResize(PaneResizeRequest {
            target: PaneTargetRef::by_id(session.clone(), pane_id),
            adjustment: ResizePaneAdjustment::Left { cells: 1 },
        }))
        .await;
    assert!(matches!(response, Response::ResizePane(_)), "{response:?}");
    assert_eq!(layout_change_count(&mut notifications).await, 0);
}

#[tokio::test]
async fn stable_id_resize_and_kill_keep_required_layout_notifications() {
    let handler = RequestHandler::new();
    let session = SessionName::new("layout-notify-stable-id").expect("test session name");
    create_session(&handler, &session).await;
    run_detached_command(&handler, &format!("split-window -d -h -t {session}")).await;
    let (first_pane_id, second_pane_id) = {
        let state = handler.state.lock().await;
        let window = state
            .sessions
            .session(&session)
            .expect("test session")
            .window();
        (
            window.pane(0).expect("first test pane").id(),
            window.pane(1).expect("second test pane").id(),
        )
    };
    let (_control_pid, mut notifications) = register_control_client(&handler, &session).await;
    let _ = settle_control_notifications(&mut notifications).await;

    let response = handler
        .handle(Request::PaneResize(PaneResizeRequest {
            target: PaneTargetRef::by_id(session.clone(), first_pane_id),
            adjustment: ResizePaneAdjustment::Right { cells: 2 },
        }))
        .await;
    assert!(matches!(response, Response::ResizePane(_)), "{response:?}");
    assert_eq!(layout_change_count(&mut notifications).await, 1);

    let response = handler
        .handle(Request::PaneKill(PaneKillRequest {
            target: PaneTargetRef::by_id(session, second_pane_id),
            kill_all_except: false,
        }))
        .await;
    assert!(matches!(response, Response::KillPane(_)), "{response:?}");
    assert_eq!(layout_change_count(&mut notifications).await, 1);
}

async fn create_session(handler: &RequestHandler, session: &SessionName) {
    run_detached_command(handler, &format!("new-session -d -s {session} -x 80 -y 24")).await;
}

async fn run_detached_command(handler: &RequestHandler, command: &str) {
    let parsed = handler
        .parse_control_commands(command)
        .await
        .unwrap_or_else(|error| panic!("failed to parse {command:?}: {error}"));
    handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .unwrap_or_else(|error| panic!("failed to execute {command:?}: {error}"));
}

async fn run_control_command(handler: &RequestHandler, control_pid: u32, command: &str) {
    let error = run_control_command_result(handler, control_pid, command).await;
    assert!(error.is_none(), "failed to execute {command:?}: {error:?}");
}

/// Runs a control command and hands back its error instead of asserting.
async fn run_control_command_result(
    handler: &RequestHandler,
    control_pid: u32,
    command: &str,
) -> Option<RmuxError> {
    let parsed = handler
        .parse_control_commands(command)
        .await
        .unwrap_or_else(|error| panic!("failed to parse {command:?}: {error}"));
    handler
        .execute_control_commands(control_pid, parsed)
        .await
        .error
}

async fn register_control_client(
    handler: &RequestHandler,
    session: &SessionName,
) -> (u32, mpsc::Receiver<ControlServerEvent>) {
    let control_pid = std::process::id();
    let (event_tx, event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    handler
        .register_control_with_closing(
            control_pid,
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
        .set_control_session(control_pid, Some(session.clone()))
        .await
        .expect("set test control session");
    (control_pid, event_rx)
}

async fn layout_change_count(notifications: &mut mpsc::Receiver<ControlServerEvent>) -> usize {
    settle_control_notifications(notifications)
        .await
        .iter()
        .filter(|line| line.starts_with("%layout-change "))
        .count()
}

async fn settle_control_notifications(
    notifications: &mut mpsc::Receiver<ControlServerEvent>,
) -> Vec<String> {
    let mut deadline = tokio::time::Instant::now() + CONTROL_NOTIFICATION_SETTLE;
    let mut lines = Vec::new();
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(CONTROL_NOTIFICATION_POLL, notifications.recv()).await {
            Ok(Some(ControlServerEvent::Notification(line))) => {
                deadline = tokio::time::Instant::now() + CONTROL_NOTIFICATION_SETTLE;
                lines.push(line);
            }
            Ok(Some(_)) | Err(_) => {}
            Ok(None) => break,
        }
    }
    lines
}

async fn wait_for_buffer_text(
    handler: &RequestHandler,
    name: &str,
    expected: &str,
) -> Option<String> {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(500);
    loop {
        let content = {
            let state = handler.state.lock().await;
            state
                .buffers
                .show(Some(name))
                .ok()
                .map(|(_, content)| String::from_utf8_lossy(content).into_owned())
        };
        if content.as_deref() == Some(expected) || tokio::time::Instant::now() >= deadline {
            return content;
        }
        tokio::time::sleep(CONTROL_NOTIFICATION_POLL).await;
    }
}

fn render_command(command: &str, session: &SessionName) -> String {
    command.replace("{session}", session.as_str())
}
