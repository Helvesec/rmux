use super::*;
use rmux_proto::{BindKeyRequest, KillPaneRequest};

use crate::hook_runtime::HookExecutionContext;

#[tokio::test]
async fn c3_f2_background_attach_rejects_stale_client_before_admission() {
    use crate::handler::scripting_support::send_keys_admission_probe;
    use crate::handler::with_expected_attach_and_session_identity;

    // Keep both panes and the session alive; only the client lifetime changes.
    for replace in [false, true] {
        let handler = RequestHandler::new();
        let (session, targets) = two_panes(&handler, "c3-pre-admission").await;
        let pid = 82993;
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        handler.register_attach(pid, session.clone(), tx).await;
        let identity = handler.active_attach_identity_for_test(pid).await;
        let (pause, result) = send_keys_admission_probe::install(&handler);
        let commands = CommandParser::new()
            .parse("run-shell -b -C 'send-keys -c = -l TAIL'")
            .unwrap();
        with_expected_attach_and_session_identity(
            identity,
            session.clone(),
            identity.session_id(),
            handler.execute_parsed_commands_for_test(pid, commands),
        )
        .await
        .expect("background queue starts");
        wait_for_pause(&pause).await;
        handler.finish_attach(pid, identity.attach_id()).await;
        let (replacement_tx, _replacement_rx) = tokio::sync::mpsc::unbounded_channel();
        if replace {
            handler.register_attach(pid, session, replacement_tx).await;
            assert_ne!(
                handler
                    .active_attach_identity_for_test(pid)
                    .await
                    .attach_id(),
                identity.attach_id()
            );
        }
        pause.release.notify_one();
        let rejection = tokio::time::timeout(Duration::from_secs(2), result)
            .await
            .expect("admission completes")
            .expect("admission returns without panic")
            .expect_err("stale attached client must be rejected");
        assert!(
            rejection.to_string().contains("client identity changed"),
            "{rejection}"
        );
        tokio::time::timeout(Duration::from_secs(2), async {
            while handler.background_task_running_for_test("rmux-run-shell") {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("background queue finishes");
        let state = handler.state.lock().await;
        for target in &targets {
            assert_eq!(
                state.pane_input_capture_for_test(target),
                Some(Vec::new()),
                "replace={replace}: surviving panes must not receive stale input"
            );
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Entry {
    Control,
    Binding,
}

#[tokio::test]
async fn c3_focus_disabled_fixture_keeps_survivor_empty_without_queued_command() {
    let handler = RequestHandler::new();
    #[cfg(windows)]
    let parked = {
        let state = handler.state.lock().await;
        let guard = state.pane_output_read_demand.acquire_attached_client();
        // Park both real readers before their panes exist, making a delayed
        // native preamble deterministic. A fresh handler allocates ids 0/1.
        guard.set_pane_off(0, true);
        guard.set_pane_off(1, true);
        guard
    };
    let (session, targets) = two_panes(&handler, "c3-focus-cause").await;
    #[cfg(windows)]
    {
        let output = handler
            .state
            .lock()
            .await
            .pane_output_for_target(&session, 0, 1)
            .expect("survivor output exists");
        let _demand = output.acquire_read_demand();
        tokio::time::timeout(Duration::from_secs(15), async {
            while output.capture_with_next_sequence(|| ()).0 == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("native startup output is published before focus reconciliation");
        drop(parked);
    }
    let _entries = register_entries(&handler, &session).await;
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    handler.register_attach(77, session.clone(), tx).await;
    let response = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Window(WindowTarget::with_window(session.clone(), 0)),
            option: OptionName::SynchronizePanes,
            value: "on".into(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(response, Response::SetOption(_)), "{response:?}");
    assert_eq!(
        handler
            .state
            .lock()
            .await
            .pane_input_capture_for_test(&targets[1]),
        Some(Vec::new()),
        "survivor input is empty before pane removal"
    );
    // No send-keys command exists in this control: any captured byte comes
    // from pane removal/focus reconciliation, not the rejected queue.
    let killed = handler
        .handle(Request::KillPane(KillPaneRequest {
            target: targets[0].clone(),
            kill_all_except: false,
        }))
        .await;
    assert!(matches!(killed, Response::KillPane(_)), "{killed:?}");
    let state = handler.state.lock().await;
    assert_eq!(
        state.pane_input_capture_for_test(&PaneTarget::with_window(session, 0, 0)),
        Some(Vec::new()),
        "focus-disabled fixture emits input even without a queued command"
    );
}

async fn two_panes(handler: &RequestHandler, name: &str) -> (SessionName, [PaneTarget; 2]) {
    let session = create_session(handler, name).await;
    let first = PaneTarget::with_window(session.clone(), 0, 0);
    handler
        .wait_for_pane_startup_to_finish_for_test(&first)
        .await;
    let response = handler
        .handle(Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Pane(first.clone()),
            direction: SplitDirection::Horizontal,
            before: false,
            environment: None,
        }))
        .await;
    let Response::SplitWindow(split) = response else {
        panic!("{response:?}")
    };
    handler
        .wait_for_pane_startup_to_finish_for_test(&split.pane)
        .await;
    let second = PaneTarget::with_window(session.clone(), 0, 1);
    let response = handler
        .handle(Request::SelectPane(Box::new(SelectPaneRequest {
            target: first.clone(),
            title: None,
            style: None,
            input_disabled: None,
            preserve_zoom: false,
        })))
        .await;
    assert!(matches!(response, Response::SelectPane(_)), "{response:?}");
    for target in [&first, &second] {
        handler.disable_pane_focus_reports_for_test(target).await;
        handler
            .state
            .lock()
            .await
            .start_pane_input_capture_for_test(target);
    }
    (session, [first, second])
}

async fn run_entry(handler: &RequestHandler, entry: Entry, command: &str) -> Result<(), String> {
    match entry {
        Entry::Control => {
            let parsed = CommandParser::new().parse(command).unwrap();
            let result = handler.execute_control_commands(82991, parsed).await;
            result.error.map_or(Ok(()), |error| Err(error.to_string()))
        }
        Entry::Binding => {
            let bound = handler
                .handle(Request::BindKey(Box::new(BindKeyRequest {
                    table_name: "root".into(),
                    key: "x".into(),
                    note: None,
                    repeat: false,
                    command: Some(command.split_whitespace().map(str::to_owned).collect()),
                })))
                .await;
            assert!(matches!(bound, Response::BindKey(_)), "{bound:?}");
            handler
                .handle_attached_live_input(82992, &mut Vec::new(), b"x")
                .await
                .map_err(|error| error.to_string())
        }
    }
}

async fn register_entries(
    handler: &RequestHandler,
    session: &SessionName,
) -> (
    tokio::sync::mpsc::Receiver<ControlServerEvent>,
    tokio::sync::mpsc::UnboundedReceiver<crate::pane_io::AttachControl>,
) {
    let (_id, events) = register_control_for_session(handler, 82991, session.clone()).await;
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    handler.register_attach(82992, session.clone(), tx).await;
    (events, rx)
}

async fn synchronized_queue(entry: Entry) {
    for sync in ["off", "on"] {
        let handler = RequestHandler::new();
        let (session, targets) = two_panes(&handler, "c3-sync").await;
        let _entries = register_entries(&handler, &session).await;
        let response = handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Window(WindowTarget::with_window(session.clone(), 0)),
                option: OptionName::SynchronizePanes,
                value: sync.into(),
                mode: SetOptionMode::Replace,
            }))
            .await;
        assert!(matches!(response, Response::SetOption(_)), "{response:?}");
        let result = run_entry(&handler, entry, "send-keys -t c3-sync:0.0 -l X").await;
        assert!(result.is_ok(), "{entry:?}/{sync}: {result:?}");
        let state = handler.state.lock().await;
        assert_eq!(
            state.pane_input_capture_for_test(&targets[0]),
            Some(b"X".to_vec())
        );
        assert_eq!(
            state.pane_input_capture_for_test(&targets[1]),
            Some(if sync == "on" {
                b"X".to_vec()
            } else {
                Vec::new()
            })
        );
    }
}

#[tokio::test]
async fn c3_f1_control_synchronized_queue_reaches_all_live_panes() {
    synchronized_queue(Entry::Control).await;
}

#[tokio::test]
async fn c3_f1_binding_synchronized_queue_reaches_all_live_panes() {
    synchronized_queue(Entry::Binding).await;
}

async fn stale_queue(entry: Entry, implicit_client: bool) {
    let handler = RequestHandler::new();
    let (session, targets) = two_panes(&handler, "c3-stale").await;
    let _entries = register_entries(&handler, &session).await;
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    handler.register_attach(77, session.clone(), tx).await;
    let response = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Window(WindowTarget::with_window(session, 0)),
            option: OptionName::SynchronizePanes,
            value: "on".into(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(response, Response::SetOption(_)), "{response:?}");
    let command = if implicit_client {
        "send-keys -c 77 -l TAIL"
    } else {
        "send-keys -t c3-stale:0.0 -l TAIL"
    };
    let pause = install_queue_exact_target_capture_pause(&handler, "send-keys");
    let queued_handler = handler.clone();
    let queued = tokio::spawn(async move { run_entry(&queued_handler, entry, command).await });
    wait_for_pause(&pause).await;
    let killed = handler
        .handle(Request::KillPane(KillPaneRequest {
            target: targets[0].clone(),
            kill_all_except: false,
        }))
        .await;
    assert!(matches!(killed, Response::KillPane(_)), "{killed:?}");
    pause.release.notify_one();
    let result = tokio::time::timeout(Duration::from_secs(2), queued)
        .await
        .expect("queue completes")
        .expect("task joins");
    let state = handler.state.lock().await;
    let surviving = state
        .sessions
        .session(targets[1].session_name())
        .unwrap()
        .window()
        .panes()
        .first()
        .unwrap();
    let survivor_target =
        PaneTarget::with_window(targets[1].session_name().clone(), 0, surviving.index());
    assert_eq!(
        state.pane_input_capture_for_test(&survivor_target),
        Some(Vec::new()),
        "{entry:?}: a stale command must never reach the survivor: {result:?}"
    );
    if matches!(entry, Entry::Control) {
        assert!(result
            .expect_err("stale target must fail")
            .contains("target identity changed"));
    }
}

#[tokio::test]
async fn c3_f1_synchronized_queue_rejects_replaced_primary() {
    for entry in [Entry::Control, Entry::Binding] {
        stale_queue(entry, false).await;
    }
}

#[tokio::test]
async fn c3_f2_implicit_client_queue_rejects_replaced_pane() {
    for entry in [Entry::Control, Entry::Binding] {
        stale_queue(entry, true).await;
    }
}

#[tokio::test]
async fn c3_f2_implicit_client_queue_rejects_replaced_registration() {
    for entry in [Entry::Control, Entry::Binding] {
        let handler = RequestHandler::new();
        let (original, original_targets) = two_panes(&handler, "c3-client-old").await;
        let (replacement, replacement_targets) = two_panes(&handler, "c3-client-new").await;
        let _entries = register_entries(&handler, &original).await;
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        handler.register_attach(77, original, tx).await;
        let pause = install_queue_exact_target_capture_pause(&handler, "send-keys");
        let queued_handler = handler.clone();
        let queued = tokio::spawn(async move {
            run_entry(&queued_handler, entry, "send-keys -c 77 -l TAIL").await
        });
        wait_for_pause(&pause).await;
        let (tx, _replacement_rx) = tokio::sync::mpsc::unbounded_channel();
        handler.register_attach(77, replacement, tx).await;
        pause.release.notify_one();
        let result = tokio::time::timeout(Duration::from_secs(2), queued)
            .await
            .expect("queue completes")
            .expect("task joins");
        let state = handler.state.lock().await;
        for target in original_targets.iter().chain(&replacement_targets) {
            assert_eq!(
                state.pane_input_capture_for_test(target),
                Some(Vec::new()),
                "{entry:?}: replacement client must not inherit input: {result:?}"
            );
        }
        if matches!(entry, Entry::Control) {
            assert!(result
                .expect_err("stale client must fail")
                .contains("client identity changed"));
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum HookTrigger {
    Request,
    Queue,
    Control,
    Binding,
}

/// One live, focus-quiet pane per session, its input captured.
async fn captured_session(handler: &RequestHandler, name: &str) -> PaneTarget {
    let session = create_session(handler, name).await;
    let pane = PaneTarget::with_window(session, 0, 0);
    handler
        .wait_for_pane_startup_to_finish_for_test(&pane)
        .await;
    handler.disable_pane_focus_reports_for_test(&pane).await;
    handler
        .state
        .lock()
        .await
        .start_pane_input_capture_for_test(&pane);
    pane
}

/// tmux 3.7b `cmd_find_target()` takes an omitted target from the queue
/// item's own state (`cmdq_get_current()`) before asking its client
/// (`cmd_find_from_client()`). An after-hook inherits the state of the
/// command that fired it, so `send-keys` without `-t` in `after-rename-window`
/// types into the renamed window: neither the most recent session (`b`), nor
/// the first by name (`a`), nor the session of the client that ran it (`a`).
#[tokio::test]
async fn after_hook_send_keys_without_target_types_into_the_inherited_target() {
    for trigger in [
        HookTrigger::Request,
        HookTrigger::Queue,
        HookTrigger::Control,
        HookTrigger::Binding,
    ] {
        let handler = RequestHandler::new();
        let renamed = captured_session(&handler, "c").await;
        let first_by_name = captured_session(&handler, "a").await;
        let most_recent = captured_session(&handler, "b").await;
        let _entries = register_entries(&handler, first_by_name.session_name()).await;
        let hook = CommandParser::new()
            .parse("set-hook -g after-rename-window 'send-keys -l OFFMARK'")
            .unwrap();
        handler
            .execute_parsed_commands(
                std::process::id(),
                hook,
                QueueExecutionContext::without_caller_cwd(),
            )
            .await
            .expect("set-hook succeeds");
        let renamed_window = WindowTarget::with_window(renamed.session_name().clone(), 0);
        let result = match trigger {
            HookTrigger::Request => {
                let response = handler
                    .handle(Request::RenameWindow(rmux_proto::RenameWindowRequest {
                        target: renamed_window,
                        name: "zz".to_owned(),
                    }))
                    .await;
                assert!(
                    matches!(response, Response::RenameWindow(_)),
                    "{response:?}"
                );
                Ok(())
            }
            HookTrigger::Queue => handler
                .execute_parsed_commands(
                    std::process::id(),
                    CommandParser::new()
                        .parse("rename-window -t =c:0 zz")
                        .unwrap(),
                    QueueExecutionContext::without_caller_cwd(),
                )
                .await
                .map(|_| ())
                .map_err(|error| error.to_string()),
            HookTrigger::Control => {
                run_entry(&handler, Entry::Control, "rename-window -t =c:0 zz").await
            }
            HookTrigger::Binding => {
                run_entry(&handler, Entry::Binding, "rename-window -t =c:0 zz").await
            }
        };
        assert!(result.is_ok(), "{trigger:?}: {result:?}");
        let state = handler.state.lock().await;
        assert_eq!(
            state
                .sessions
                .session(renamed.session_name())
                .and_then(|session| session.window_at(0))
                .and_then(rmux_core::Window::name),
            Some("zz"),
            "{trigger:?}: rename-window ran"
        );
        for (pane, expected) in [
            (&renamed, b"OFFMARK".to_vec()),
            (&first_by_name, Vec::new()),
            (&most_recent, Vec::new()),
        ] {
            assert_eq!(
                state.pane_input_capture_for_test(pane),
                Some(expected),
                "{trigger:?}: {pane}"
            );
        }
    }
}

/// A command hook inherits the state of its command. A lifecycle hook without
/// a valid notification state may carry a substitute for a vanished pane
/// (rmux's `pane-exited` fallback), where tmux 3.7b has no state
/// (`cmd_find_from_nothing()`) and asks the client, so `send-keys` there keeps
/// the client fallback and never types into the substitute.
#[tokio::test]
async fn only_command_hooks_bind_send_keys_to_their_inherited_target() {
    for (execution, inherited_receives) in [
        (
            HookExecutionContext::command(HookName::AfterRenameWindow),
            true,
        ),
        (HookExecutionContext::lifecycle(HookName::PaneExited), false),
    ] {
        let handler = RequestHandler::new();
        let inherited = captured_session(&handler, "c").await;
        let first_by_name = captured_session(&handler, "a").await;
        let most_recent = captured_session(&handler, "b").await;
        let current_target = Some(Target::Pane(inherited.clone()));
        with_hook_execution(
            execution,
            Vec::new(),
            handler.execute_hook_command_with_context(
                std::process::id(),
                "send-keys -l OFFMARK",
                current_target,
            ),
        )
        .await
        .expect("hook send-keys succeeds");
        let state = handler.state.lock().await;
        let received = [&inherited, &first_by_name, &most_recent]
            .map(|pane| state.pane_input_capture_for_test(pane) == Some(b"OFFMARK".to_vec()));
        assert_eq!(
            received[0], inherited_receives,
            "{execution:?}: {received:?}"
        );
        assert_eq!(
            received.iter().filter(|received| **received).count(),
            1,
            "{execution:?}: exactly one pane receives the input: {received:?}"
        );
    }
}

async fn wait_for_offmark(handler: &RequestHandler, panes: &[&PaneTarget]) -> Vec<bool> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let received = {
            let state = handler.state.lock().await;
            panes
                .iter()
                .map(|pane| state.pane_input_capture_for_test(pane) == Some(b"OFFMARK".to_vec()))
                .collect::<Vec<_>>()
        };
        if received.iter().any(|received| *received) || tokio::time::Instant::now() >= deadline {
            return received;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

/// tmux 3.7b `notify_insert_hook()` gives a lifecycle hook its notification's
/// state while `cmd_find_valid_state()` accepts it, and starts from
/// `cmd_find_from_nothing()` only when it is empty or invalid. `window-renamed`
/// and `session-renamed` keep a valid state, so `send-keys` without `-t` types
/// into the renamed window or session (tmux: `c`, `cc`), not the most recent
/// session (`b`) nor the first by name (`a`). `pane-exited` fires for a pane
/// tmux removes before the hook runs: it has no state, so the input reaches
/// the most recent session (tmux: `b`), never rmux's substitute in `c`.
#[tokio::test]
async fn lifecycle_hook_send_keys_without_target_types_into_a_valid_notification_state() {
    for (hook, trigger, session_after, receiver) in [
        ("window-renamed", "rename-window -t =c:0 zz", "c", 0),
        ("session-renamed", "rename-session -t =c cc", "cc", 0),
        ("pane-exited", "", "c", 3),
    ] {
        let handler = RequestHandler::new();
        let (_, [active, other]) = two_panes(&handler, "c").await;
        let first_by_name = captured_session(&handler, "a").await;
        let most_recent = captured_session(&handler, "b").await;
        handler
            .execute_parsed_commands(
                std::process::id(),
                CommandParser::new()
                    .parse(&format!("set-hook -g {hook} 'send-keys -l OFFMARK'"))
                    .unwrap(),
                QueueExecutionContext::without_caller_cwd(),
            )
            .await
            .expect("set-hook succeeds");
        // Hooks wait in the dispatch queue until the captures follow a rename.
        let lifecycle_dispatch = handler
            .take_lifecycle_dispatch_receiver()
            .expect("test owns lifecycle hook dispatch");
        if !trigger.is_empty() {
            handler
                .execute_parsed_commands(
                    std::process::id(),
                    CommandParser::new().parse(trigger).unwrap(),
                    QueueExecutionContext::without_caller_cwd(),
                )
                .await
                .expect("trigger succeeds");
        }
        let session_after = SessionName::new(session_after).unwrap();
        let [active, other] = [active, other]
            .map(|pane| PaneTarget::with_window(session_after.clone(), 0, pane.pane_index()));
        {
            let state = handler.state.lock().await;
            state.start_pane_input_capture_for_test(&active);
            state.start_pane_input_capture_for_test(&other);
        }
        let (shutdown, shutdown_rx) = tokio::sync::oneshot::channel();
        let hook_handler = handler.clone();
        let hook_task = tokio::spawn(async move {
            hook_handler
                .consume_lifecycle_hooks(lifecycle_dispatch, shutdown_rx)
                .await;
        });
        if trigger.is_empty() {
            // The pane stays so that only the hook's state decides the target.
            let event = handler
                .prepare_lifecycle_event_for_test(rmux_core::LifecycleEvent::PaneExited {
                    target: active.clone(),
                    pane_id: None,
                    window_id: None,
                    window_name: None,
                })
                .await;
            assert_eq!(event.hooks.len(), 1, "{hook}: the hook is dispatched");
            handler.dispatch_lifecycle_hook(event).await;
        }
        let received =
            wait_for_offmark(&handler, &[&active, &other, &first_by_name, &most_recent]).await;
        let expected: Vec<bool> = (0..4).map(|index| index == receiver).collect();
        let _ = shutdown.send(());
        hook_task.await.expect("lifecycle hook task joins");
        assert_eq!(received, expected, "{hook}: c:0.0, c:0.1, a, b");
    }
}

#[path = "queue_exact_target_send_keys_reset.rs"]
mod reset_regressions;
