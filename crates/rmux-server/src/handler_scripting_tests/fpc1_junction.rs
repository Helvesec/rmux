use super::frs1::{cleanup, floating_rect, setup, window};
use super::frs1_sibling_producers::{spawn_requester, spawn_requester_with, ChildGuard};
use super::*;
use crate::handler::display_message_tests::REQUESTER_HOLD_DIR;
use crate::handler::{ControlRegistration, QueuedLifecycleEvent};
use rmux_core::LifecycleEvent;
use rmux_os::identity::UserIdentity;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::mpsc;

static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn requester_uid() -> u32 {
    // The access store cannot modify root or the server owner's identity.
    crate::server_access::current_owner_uid()
        .wrapping_add(1)
        .max(1)
}

async fn grant_unattached_write_access(
    handler: &RequestHandler,
    requester_pid: u32,
) -> mpsc::Receiver<ControlServerEvent> {
    let uid = requester_uid();
    let (event_tx, event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    handler
        .register_control_with_access(
            requester_pid,
            ControlModeUpgrade {
                initial_command_count: 0,
                mode: rmux_proto::ControlMode::Plain,
                terminal_context: OuterTerminalContext::default(),
            },
            ControlRegistration {
                event_tx,
                closing: Arc::new(AtomicBool::new(false)),
                uid,
                user: UserIdentity::Uid(uid),
                can_write: true,
            },
        )
        .await
        .expect("unattached requester receives write access");
    event_rx
}

pub(super) fn canonical_request(command: &str) -> Request {
    Request::SourceFile(Box::new(SourceFileRequest {
        paths: vec![rmux_proto::INTERNAL_CANONICAL_COMMAND_EXECUTION_PATH.to_owned()],
        quiet: false,
        parse_only: false,
        verbose: false,
        expand_paths: false,
        target: None,
        caller_cwd: None,
        stdin: Some(command.to_owned()),
    }))
}

fn offered_request(args: &[&str]) -> Request {
    let mut request = RuntimeCommandExecutionRequest::new(
        args.iter().map(|argument| (*argument).to_owned()).collect(),
        None,
    );
    request.handshake.required_capabilities.push(
        if args[0] == "set-pane-placement" {
            rmux_proto::CAPABILITY_SET_PANE_PLACEMENT_V1
        } else {
            rmux_proto::CAPABILITY_RESIZE_PANE_POSITION_V1
        }
        .to_owned(),
    );
    Request::RuntimeCommandExecution(Box::new(request))
}

type JunctionFixture = (
    RequestHandler,
    SessionName,
    SessionName,
    std::path::PathBuf,
    ChildGuard,
    mpsc::Receiver<ControlServerEvent>,
);

pub(super) async fn fixture() -> JunctionFixture {
    fixture_with_quiet_windows(false).await
}

pub(super) async fn quiet_fixture() -> JunctionFixture {
    fixture_with_quiet_windows(true).await
}

async fn fixture_with_quiet_windows(quiet: bool) -> JunctionFixture {
    let (handler, primary) = if quiet {
        super::frs1::setup_quiet_window("frs-sibling-primary").await
    } else {
        setup("frs-sibling-primary").await
    };
    let linked = session_name("junction-linked");
    create_background_identity_session(&handler, linked.clone()).await;
    assert!(matches!(
        handler
            .handle(Request::LinkWindow(LinkWindowRequest {
                source: WindowTarget::with_window(primary.clone(), 0),
                target: WindowTarget::with_window(linked.clone(), 1),
                after: false,
                before: false,
                kill_destination: false,
                detached: true,
            }))
            .await,
        Response::LinkWindow(_)
    ));
    create_background_identity_session(&handler, session_name("junction-decoy")).await;
    if quiet {
        // These windows inherit the silent command, but also need an explicit
        // name and completed pane startup before becoming snapshot witnesses.
        for name in [&linked, &session_name("junction-decoy")] {
            super::frs1::queue(
                &handler,
                &format!("rename-window -t {name}:0 snapshot-witness"),
            )
            .await
            .unwrap();
            super::frs1::wait_for_quiet_window_terminals(&handler, name).await;
        }
    }
    let fixture_id = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let root = temp_root(&format!("fpc1-junction-{fixture_id}"));
    fs::create_dir_all(&root).expect("junction root");
    let socket = root.join("server.sock");
    handler.set_socket_path(&socket);
    let (session_id, pane_id) = {
        let state = handler.state.lock().await;
        (
            state
                .sessions
                .session(&linked)
                .expect("linked session")
                .id(),
            state
                .sessions
                .session(&primary)
                .expect("primary session")
                .window_at(0)
                .expect("primary window")
                .pane(0)
                .expect("tiled pane")
                .id(),
        )
    };
    let rmux = format!("{},1,{}", socket.display(), session_id.as_u32());
    let pane = pane_id.to_string();
    let requester = spawn_requester(&rmux, &pane).await;
    handler
        .set_test_access_mode_for_uid(requester_uid(), crate::server_access::AccessMode::ReadWrite)
        .expect("test access store accepts writer");
    let events = grant_unattached_write_access(&handler, requester.0.id()).await;
    (handler, primary, linked, root, requester, events)
}

async fn anchor_matrix(canonical: bool) {
    let mut results = Vec::new();
    for (label, target, anchor) in [
        (
            "qualified",
            "frs-sibling-primary:0.1",
            "junction-linked:1.0",
        ),
        ("relative", ".1", ".0"),
        ("mixed", "frs-sibling-primary:0.1", ".0"),
    ] {
        let (handler, primary, _linked, root, requester, _events) = fixture().await;
        let before = window(&handler, &primary).await;
        let pane_id = before.pane(1).expect("float pane").id();
        let lifecycle = handler
            .state
            .lock()
            .await
            .pane_lifecycle(pane_id)
            .expect("float lifecycle")
            .clone();
        let args = [
            "set-pane-placement",
            "-t",
            target,
            "-a",
            anchor,
            "-h",
            "tiled",
        ];
        let request = if canonical {
            canonical_request(&args.join(" "))
        } else {
            offered_request(&args)
        };
        let response = handler.dispatch(requester.0.id(), request).await.response;
        let after = window(&handler, &primary).await;
        let converted = !after.floating_stack().contains(pane_id);
        let lifecycle_after = handler
            .state
            .lock()
            .await
            .pane_lifecycle(pane_id)
            .expect("float lifecycle after request")
            .clone();
        assert_eq!(after.pane(1).expect("source pane").id(), pane_id);
        assert_eq!(lifecycle.process, lifecycle_after.process);
        assert_eq!(lifecycle.generation, lifecycle_after.generation);
        assert_eq!(before.active_pane_index(), after.active_pane_index());
        cleanup(&handler).await;
        drop(requester);
        fs::remove_dir_all(root).expect("remove junction root");
        results.push((label, converted, response));
    }
    assert!(
        results.iter().all(|(_, converted, _)| *converted),
        "qualified source must not erase inherited context consumed by relative -a: {results:?}"
    );
}

#[tokio::test]
async fn junction_review_runtime_qualified_source_relative_anchor() {
    anchor_matrix(false).await;
}

#[tokio::test]
async fn junction_review_canonical_qualified_source_relative_anchor() {
    anchor_matrix(true).await;
}

#[tokio::test]
async fn junction_control_queue_keeps_context_for_relative_anchor() {
    let (handler, primary, _linked, root, requester, _events) = fixture().await;
    let before = window(&handler, &primary).await;
    let pane_id = before.pane(1).expect("float pane").id();
    let commands = handler
        .parse_control_commands("set-pane-placement -t frs-sibling-primary:0.1 -a .0 -h tiled")
        .await
        .expect("control parser admits placement command");
    handler
        .execute_parsed_commands_for_test(requester.0.id(), commands)
        .await
        .expect("control queue resolves relative anchor in inherited occurrence");
    assert!(!window(&handler, &primary)
        .await
        .floating_stack()
        .contains(pane_id));
    cleanup(&handler).await;
    drop(requester);
    fs::remove_dir_all(root).expect("remove junction root");
}

#[tokio::test]
async fn junction_source_binding_validation_keeps_context_for_relative_anchor() {
    let (handler, primary, _linked, root, requester, _events) = fixture().await;
    let pane_id = window(&handler, &primary)
        .await
        .pane(1)
        .expect("float pane")
        .id();
    let source = root.join("binding.conf");
    fs::write(
        &source,
        "bind-key X { set-pane-placement -t frs-sibling-primary:0.1 -a .0 -h tiled }\n",
    )
    .expect("write junction binding");
    let response = handler
        .dispatch(
            requester.0.id(),
            source_file_request(
                vec![source.to_string_lossy().into_owned()],
                Some(root.clone()),
            ),
        )
        .await
        .response;
    assert!(
        matches!(response, Response::SourceFile(ref result)
            if result.exit_status().is_none() && result.stderr().is_empty()),
        "source validation must retain context for nested relative anchor: {response:?}"
    );
    assert!(window(&handler, &primary)
        .await
        .floating_stack()
        .contains(pane_id));
    cleanup(&handler).await;
    drop(requester);
    fs::remove_dir_all(root).expect("remove junction root");
}

/// A dispatch reads the requester's environment for its inherited context.
/// The fixture must hand over a requester only once it runs its own code: on
/// the Windows CI of r8 the move grammar below lost that context while the
/// helper was still starting. A held helper stays short of that point.
#[tokio::test]
async fn junction_requester_is_handed_over_only_once_it_runs_its_own_code() {
    let fixture_id = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let root = temp_root(&format!("fpc1-junction-hold-{fixture_id}"));
    fs::create_dir_all(&root).expect("hold root");
    let rmux = format!("{},1,1", root.join("server.sock").display());
    let hold = root.display().to_string();
    let spawned = tokio::spawn(async move {
        spawn_requester_with(&rmux, "%0", &[(REQUESTER_HOLD_DIR, hold)]).await
    });
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        while !root.join("holding").exists() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("held requester reaches its own code");
    // The environment is readable since exec; give a fixture that waits for
    // it alone ample time to return.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let handed_over_while_starting = spawned.is_finished();
    fs::write(root.join("release"), b"").expect("release held requester");
    drop(spawned.await.expect("requester spawn task"));
    fs::remove_dir_all(&root).expect("remove hold root");
    assert!(
        !handed_over_while_starting,
        "the fixture handed over a requester that had not reached its readiness point"
    );
}

#[tokio::test]
async fn junction_review_move_grammar_and_inherited_occurrence() {
    let (handler, primary, linked, root, requester, _events) = fixture().await;
    let before = window(&handler, &primary).await;
    let pane_id = before.pane(1).expect("float pane").id();
    let size = floating_rect(&before, 1);
    let resize_before = handler
        .state
        .lock()
        .await
        .window_runtime_resize_count_for_test();
    for args in [
        vec!["resizep", "-X9", "-Y-2", "-t", ".1"],
        vec![
            "resize-pane",
            "-Y",
            "8",
            "-tfrs-sibling-primary:0.1",
            "-X",
            "7",
        ],
        vec![
            "resize-pane",
            "-t",
            "junction-decoy:0.0",
            "-X",
            "5",
            "-t",
            ".1",
        ],
    ] {
        let response = handler
            .dispatch(requester.0.id(), offered_request(&args))
            .await
            .response;
        let result = execution_result(response);
        assert!(
            matches!(
                result,
                RuntimeCommandExecutionResponse::Complete {
                    exit_status: 0,
                    error: None,
                    ..
                }
            ),
            "{args:?} must move the float in the inherited occurrence: {result:?}"
        );
    }
    let after = window(&handler, &primary).await;
    assert_eq!(after.pane(1).expect("float pane").id(), pane_id);
    assert_eq!(
        floating_rect(&after, 1),
        rmux_core::FloatingPaneRect::new(5, 8, size.cols(), size.rows()).unwrap()
    );
    assert_eq!(before.active_pane_index(), after.active_pane_index());
    assert_eq!(
        before
            .floating_stack()
            .placements()
            .iter()
            .map(|placement| placement.pane_id())
            .collect::<Vec<_>>(),
        after
            .floating_stack()
            .placements()
            .iter()
            .map(|placement| placement.pane_id())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        handler
            .state
            .lock()
            .await
            .window_runtime_resize_count_for_test(),
        resize_before
    );
    let state = handler.state.lock().await;
    let linked_window = state
        .sessions
        .session(&linked)
        .expect("linked session")
        .window_at(1)
        .expect("linked window");
    assert_eq!(floating_rect(linked_window, 1), floating_rect(&after, 1));
    drop(state);
    let duplicate = handler
        .dispatch(
            requester.0.id(),
            offered_request(&[
                "set-pane-placement",
                "-t",
                ".1",
                "-t",
                "frs-sibling-primary:0.1",
                "floating",
            ]),
        )
        .await
        .response;
    assert!(matches!(
        execution_result(duplicate),
        RuntimeCommandExecutionResponse::Unsupported(_)
    ));
    cleanup(&handler).await;
    drop(requester);
    fs::remove_dir_all(root).expect("remove junction root");
}

fn notification_count(
    events: &mut tokio::sync::broadcast::Receiver<QueuedLifecycleEvent>,
) -> usize {
    std::iter::from_fn(|| events.try_recv().ok())
        .filter(|event| matches!(event.event, LifecycleEvent::WindowLayoutChanged { .. }))
        .count()
}

pub(super) async fn stale_requester(
    handler: &RequestHandler,
    primary: &SessionName,
    root: &std::path::Path,
) -> (ChildGuard, mpsc::Receiver<ControlServerEvent>) {
    let (decoy_id, pane_id) = {
        let state = handler.state.lock().await;
        (
            state
                .sessions
                .session(&session_name("junction-decoy"))
                .expect("decoy session")
                .id(),
            state
                .sessions
                .session(primary)
                .expect("primary session")
                .window_at(0)
                .expect("primary window")
                .pane(0)
                .expect("primary tiled pane")
                .id(),
        )
    };
    let rmux = format!(
        "{},1,{}",
        root.join("server.sock").display(),
        decoy_id.as_u32()
    );
    let pane = pane_id.to_string();
    let requester = spawn_requester(&rmux, &pane).await;
    let events = grant_unattached_write_access(handler, requester.0.id()).await;
    (requester, events)
}

#[tokio::test]
async fn junction_stale_relative_anchor_refuses_before_effect_or_notification() {
    // Whole-Window equality (name and activity_at included) needs the quiet
    // witness: the default shell's output drives automatic rename.
    let (handler, primary, _linked, root, valid, _control_events) = quiet_fixture().await;
    let (stale, _stale_events) = stale_requester(&handler, &primary, &root).await;

    let primary_before = window(&handler, &primary).await;
    let decoy_before = window(&handler, &session_name("junction-decoy")).await;
    let resize_before = handler
        .state
        .lock()
        .await
        .window_runtime_resize_count_for_test();
    let mut notifications = handler.subscribe_lifecycle_events();
    let response = handler
        .dispatch(
            stale.0.id(),
            offered_request(&[
                "set-pane-placement",
                "-t",
                "frs-sibling-primary:0.1",
                "-a",
                ".0",
                "-h",
                "tiled",
            ]),
        )
        .await
        .response;
    assert!(
        format!("{response:?}").contains("is not present in inherited session"),
        "stale relative anchor must fail at inherited context resolution: {response:?}"
    );
    let primary_after = window(&handler, &primary).await;
    let decoy_after = window(&handler, &session_name("junction-decoy")).await;
    assert_eq!(primary_after, primary_before);
    assert_eq!(decoy_after, decoy_before);
    assert_eq!(
        handler
            .state
            .lock()
            .await
            .window_runtime_resize_count_for_test(),
        resize_before
    );
    assert_eq!(notification_count(&mut notifications), 0);

    cleanup(&handler).await;
    drop(stale);
    drop(valid);
    fs::remove_dir_all(root).expect("remove junction root");
}

#[tokio::test]
async fn junction_review_link_window_implicit_source_consumes_context() {
    let mut results = Vec::new();
    for (label, command) in [
        (
            "qualified",
            "link-window -d -s junction-linked:1 -t junction-decoy:2",
        ),
        ("relative", "link-window -d -s : -t junction-decoy:2"),
        ("omitted", "link-window -d -t junction-decoy:2"),
    ] {
        let (handler, primary, _linked, root, requester, _events) = fixture().await;
        let expected = window(&handler, &primary).await.id();
        let response = handler
            .dispatch(requester.0.id(), canonical_request(command))
            .await
            .response;
        let actual = handler
            .state
            .lock()
            .await
            .sessions
            .session(&session_name("junction-decoy"))
            .expect("decoy session")
            .window_at(2)
            .map(|window| window.id());
        cleanup(&handler).await;
        drop(requester);
        fs::remove_dir_all(root).expect("remove junction root");
        results.push((label, actual == Some(expected), response));
    }
    assert!(
        results.iter().all(|(_, correct, _)| *correct),
        "omitted -s must consume inherited current window: {results:?}"
    );
}

#[tokio::test]
async fn junction_stale_implicit_link_source_refuses_before_effect() {
    // Same quiet witness: on Windows, cmd.exe output renamed the window from
    // "rmux" to "cmd.exe" between the snapshot and the comparison.
    let (handler, primary, _linked, root, valid, _events) = quiet_fixture().await;
    let (stale, _stale_events) = stale_requester(&handler, &primary, &root).await;
    let primary_before = window(&handler, &primary).await;
    let decoy = session_name("junction-decoy");
    let decoy_before = window(&handler, &decoy).await;
    let resize_before = handler
        .state
        .lock()
        .await
        .window_runtime_resize_count_for_test();
    let mut notifications = handler.subscribe_lifecycle_events();

    let response = handler
        .dispatch(
            stale.0.id(),
            canonical_request("link-window -d -t junction-decoy:2"),
        )
        .await
        .response;
    assert!(
        matches!(&response, Response::SourceFile(result)
            if result.exit_status() == Some(1)
                && String::from_utf8_lossy(result.stderr())
                    .contains("is not present in inherited session")),
        "stale implicit source must fail at inherited context resolution: {response:?}"
    );
    assert_eq!(window(&handler, &primary).await, primary_before);
    assert_eq!(window(&handler, &decoy).await, decoy_before);
    assert!(handler
        .state
        .lock()
        .await
        .sessions
        .session(&decoy)
        .expect("decoy session")
        .window_at(2)
        .is_none());
    assert_eq!(
        handler
            .state
            .lock()
            .await
            .window_runtime_resize_count_for_test(),
        resize_before
    );
    assert_eq!(notification_count(&mut notifications), 0);

    cleanup(&handler).await;
    drop(stale);
    drop(valid);
    fs::remove_dir_all(root).expect("remove junction root");
}
