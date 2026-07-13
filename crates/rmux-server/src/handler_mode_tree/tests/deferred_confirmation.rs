use super::*;

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
