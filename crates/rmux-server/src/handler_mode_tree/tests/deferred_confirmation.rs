use super::*;

use super::super::mode_tree_model::{ModeTreeActionIdentity, ModeTreeDeferredAction};
use super::super::mode_tree_order::session_item_id;
use crate::pane_io::AttachControl;

struct DeferredTreeFixture {
    handler: RequestHandler,
    attach_pid: u32,
    control_rx: mpsc::UnboundedReceiver<AttachControl>,
    first_name: SessionName,
    first_id: rmux_proto::SessionId,
    second_name: SessionName,
    second_id: rmux_proto::SessionId,
}

async fn create_session(
    handler: &RequestHandler,
    name: &str,
) -> (SessionName, rmux_proto::SessionId) {
    let session_name = SessionName::new(name).expect("valid session name");
    let response = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name.clone(),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))
        .await;
    assert!(matches!(response, Response::NewSession(_)), "{response:?}");
    let session_id = handler
        .state
        .lock()
        .await
        .sessions
        .session(&session_name)
        .expect("created session exists")
        .id();
    (session_name, session_id)
}

async fn deferred_tree_fixture(label: &str, pid_offset: u32) -> DeferredTreeFixture {
    let handler = RequestHandler::new();
    let (host_name, _) = create_session(&handler, &format!("{label}-host")).await;
    let (first_name, first_id) = create_session(&handler, &format!("{label}-first")).await;
    let (second_name, second_id) = create_session(&handler, &format!("{label}-second")).await;

    let attach_pid = std::process::id().saturating_add(pid_offset);
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(attach_pid, host_name, control_tx)
        .await;
    let parsed = CommandParser::new()
        .parse_arguments(["choose-tree", "-s"])
        .expect("choose-tree parses");
    let command = RequestHandler::parse_mode_tree_queue_command(parsed.commands()[0].clone())
        .expect("mode-tree command parses")
        .expect("mode-tree command recognized");
    handler
        .execute_queued_mode_tree(
            attach_pid,
            command,
            &QueueExecutionContext::without_caller_cwd(),
        )
        .await
        .expect("choose-tree opens");

    DeferredTreeFixture {
        handler,
        attach_pid,
        control_rx,
        first_name,
        first_id,
        second_name,
        second_id,
    }
}

async fn set_current_selection(
    handler: &RequestHandler,
    attach_pid: u32,
    session_id: rmux_proto::SessionId,
) {
    handler
        .active_attach
        .lock()
        .await
        .by_pid
        .get_mut(&attach_pid)
        .and_then(|active| active.mode_tree.as_mut())
        .expect("choose-tree remains active")
        .selected_id = Some(session_item_id(session_id));
}

async fn set_tagged_selection(
    handler: &RequestHandler,
    attach_pid: u32,
    session_id: rmux_proto::SessionId,
) {
    let mut active_attach = handler.active_attach.lock().await;
    let mode = active_attach
        .by_pid
        .get_mut(&attach_pid)
        .and_then(|active| active.mode_tree.as_mut())
        .expect("choose-tree remains active");
    mode.tagged.clear();
    mode.tagged.insert(session_item_id(session_id));
}

async fn confirm_prompt_and_wait_for_action(
    handler: &RequestHandler,
    attach_pid: u32,
    control_rx: &mut mpsc::UnboundedReceiver<AttachControl>,
) {
    while control_rx.try_recv().is_ok() {}
    handler
        .handle_attached_live_input_for_test(attach_pid, b"y")
        .await
        .expect("confirmation input succeeds");
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match control_rx.recv().await {
                Some(AttachControl::Overlay(_)) => break,
                Some(_) => {}
                None => panic!("attach control channel closed before action refresh"),
            }
        }
    })
    .await
    .expect("captured tree action refreshes the overlay");
}

async fn assert_only_first_was_killed(fixture: &DeferredTreeFixture) {
    let state = fixture.handler.state.lock().await;
    assert!(
        state.sessions.session(&fixture.first_name).is_none(),
        "the exact session captured by the prompt must be killed"
    );
    assert_eq!(
        state
            .sessions
            .session(&fixture.second_name)
            .map(rmux_core::Session::id),
        Some(fixture.second_id),
        "the later mode-tree selection must survive"
    );
}

#[tokio::test]
async fn choose_tree_current_confirmation_uses_prompt_open_snapshot() {
    let mut fixture = deferred_tree_fixture("tree-confirm-current", 760).await;
    set_current_selection(&fixture.handler, fixture.attach_pid, fixture.first_id).await;

    fixture
        .handler
        .handle_attached_live_input_for_test(fixture.attach_pid, b"x")
        .await
        .expect("current kill confirmation opens");
    let prompt = fixture
        .handler
        .attached_prompt_render(fixture.attach_pid)
        .await
        .expect("current kill confirmation is active");
    assert!(prompt.prompt.contains(fixture.first_name.as_str()));

    set_current_selection(&fixture.handler, fixture.attach_pid, fixture.second_id).await;
    confirm_prompt_and_wait_for_action(
        &fixture.handler,
        fixture.attach_pid,
        &mut fixture.control_rx,
    )
    .await;

    assert_only_first_was_killed(&fixture).await;
}

#[tokio::test]
async fn choose_tree_tagged_confirmation_uses_prompt_open_snapshot() {
    let mut fixture = deferred_tree_fixture("tree-confirm-tagged", 780).await;
    set_tagged_selection(&fixture.handler, fixture.attach_pid, fixture.first_id).await;

    fixture
        .handler
        .handle_attached_live_input_for_test(fixture.attach_pid, b"X")
        .await
        .expect("tagged kill confirmation opens");
    let prompt = fixture
        .handler
        .attached_prompt_render(fixture.attach_pid)
        .await
        .expect("tagged kill confirmation is active");
    assert!(prompt.prompt.contains("Kill 1 tagged?"));

    set_tagged_selection(&fixture.handler, fixture.attach_pid, fixture.second_id).await;
    set_current_selection(&fixture.handler, fixture.attach_pid, fixture.second_id).await;
    confirm_prompt_and_wait_for_action(
        &fixture.handler,
        fixture.attach_pid,
        &mut fixture.control_rx,
    )
    .await;

    assert_only_first_was_killed(&fixture).await;
}

struct DeferredPaneFixture {
    handler: RequestHandler,
    attach_pid: u32,
    action_identity: ModeTreeActionIdentity,
    target: PaneTarget,
    target_pane_id: rmux_proto::PaneId,
    action: ModeTreeAction,
    _control_rx: mpsc::UnboundedReceiver<AttachControl>,
}

async fn deferred_pane_fixture(label: &str, pid_offset: u32) -> DeferredPaneFixture {
    let handler = RequestHandler::new();
    let (session_name, _) = create_session(&handler, label).await;
    assert!(matches!(
        handler
            .handle(Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Session(session_name.clone()),
                direction: SplitDirection::Horizontal,
                before: false,
                environment: None,
            }))
            .await,
        Response::SplitWindow(_)
    ));
    let (host_pane_id, target, target_pane_id) = {
        let state = handler.state.lock().await;
        let window = state
            .sessions
            .session(&session_name)
            .and_then(|session| session.window_at(0))
            .expect("pane confirmation window exists");
        let host_pane_id = window.active_pane().expect("active host pane exists").id();
        let target_pane = window
            .panes()
            .iter()
            .find(|pane| pane.id() != host_pane_id)
            .expect("a non-host pane exists");
        (
            host_pane_id,
            PaneTarget::with_window(session_name.clone(), 0, target_pane.index()),
            target_pane.id(),
        )
    };

    let attach_pid = std::process::id().saturating_add(pid_offset);
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(attach_pid, session_name, control_tx)
        .await;
    let parsed = CommandParser::new()
        .parse_arguments(["choose-tree"])
        .expect("choose-tree parses");
    let command = RequestHandler::parse_mode_tree_queue_command(parsed.commands()[0].clone())
        .expect("mode-tree command parses")
        .expect("mode-tree command recognized");
    handler
        .execute_queued_mode_tree(
            attach_pid,
            command,
            &QueueExecutionContext::without_caller_cwd(),
        )
        .await
        .expect("choose-tree opens");
    let action_identity = handler
        .current_mode_tree_action_identity(attach_pid)
        .await
        .expect("mode-tree action identity exists");
    let action = {
        let mut mode = handler
            .mode_tree_for_action_identity(action_identity)
            .await
            .expect("mode-tree state is current");
        let build = handler
            .build_mode_tree(&mut mode, attach_pid)
            .await
            .expect("choose-tree builds");
        build
            .items
            .values()
            .find_map(|item| match &item.action {
                ModeTreeAction::TreeTarget {
                    pane_id: Some(pane_id),
                    ..
                } if *pane_id == target_pane_id => Some(item.action.clone()),
                _ => None,
            })
            .expect("non-host pane action exists")
    };
    assert_ne!(host_pane_id, target_pane_id);
    handler
        .active_attach
        .lock()
        .await
        .by_pid
        .get_mut(&attach_pid)
        .and_then(|active| active.mode_tree.as_mut())
        .expect("mode-tree remains active")
        .auto_accept = true;

    DeferredPaneFixture {
        handler,
        attach_pid,
        action_identity,
        target,
        target_pane_id,
        action,
        _control_rx: control_rx,
    }
}

#[tokio::test]
async fn confirmed_pane_kill_rejects_a_respawned_output_generation() {
    use super::super::mode_tree_test_support::{
        install_mode_tree_identity_pause, ModeTreeIdentityPausePoint,
    };

    let fixture = deferred_pane_fixture("tree-confirm-pane-respawn", 790).await;
    let before_generation = fixture
        .handler
        .state
        .lock()
        .await
        .pane_output_generation_for_target(&fixture.target, fixture.target_pane_id);
    let pause = install_mode_tree_identity_pause(ModeTreeIdentityPausePoint::DeferredAction(
        fixture.attach_pid,
    ));
    let confirm_handler = fixture.handler.clone();
    let identity = fixture.action_identity;
    let action = fixture.action.clone();
    let confirm = tokio::spawn(async move {
        confirm_handler
            .confirm_mode_tree_action_for_identity(
                identity,
                "kill selected pane?".to_owned(),
                ModeTreeDeferredAction::KillCurrentTreeSelection {
                    targets: vec![action],
                },
            )
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), pause.reached.notified())
        .await
        .expect("confirmed kill reaches deferred commit");

    let response = fixture
        .handler
        .handle(Request::RespawnPane(Box::new(
            rmux_proto::RespawnPaneRequest {
                target: fixture.target.clone(),
                kill: true,
                start_directory: None,
                environment: None,
                command: None,
                process_command: None,
            },
        )))
        .await;
    assert!(matches!(response, Response::RespawnPane(_)), "{response:?}");
    pause.release.notify_one();

    assert!(
        confirm.await.expect("confirmed kill task joins").is_err(),
        "the stale confirmation must reject the respawned pane"
    );
    let state = fixture.handler.state.lock().await;
    let window = state
        .sessions
        .session(fixture.target.session_name())
        .and_then(|session| session.window_at(fixture.target.window_index()))
        .expect("window survives");
    assert!(window
        .panes()
        .iter()
        .any(|pane| pane.id() == fixture.target_pane_id));
    assert!(
        state.pane_output_generation_for_target(&fixture.target, fixture.target_pane_id)
            > before_generation
    );
}

#[tokio::test]
async fn confirmed_pane_kill_accepts_the_unchanged_output_generation() {
    let fixture = deferred_pane_fixture("tree-confirm-pane-current", 791).await;
    fixture
        .handler
        .confirm_mode_tree_action_for_identity(
            fixture.action_identity,
            "kill selected pane?".to_owned(),
            ModeTreeDeferredAction::KillCurrentTreeSelection {
                targets: vec![fixture.action],
            },
        )
        .await
        .expect("the exact pane generation is killed");

    let state = fixture.handler.state.lock().await;
    let window = state
        .sessions
        .session(fixture.target.session_name())
        .and_then(|session| session.window_at(fixture.target.window_index()))
        .expect("window survives");
    assert!(!window
        .panes()
        .iter()
        .any(|pane| pane.id() == fixture.target_pane_id));
}
