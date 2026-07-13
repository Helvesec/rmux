use std::sync::atomic::Ordering;

use super::*;
use crate::handler::prompt_support::PromptInputEvent;

use super::super::mode_tree_order::client_item_id;

struct ClientIdentityFixture {
    session_name: SessionName,
    host_pid: u32,
    host_attach_id: u64,
    victim_pid: u32,
    victim_attach_id: u64,
    victim_item_id: String,
    host_rx: mpsc::UnboundedReceiver<crate::pane_io::AttachControl>,
    _victim_rx: mpsc::UnboundedReceiver<crate::pane_io::AttachControl>,
}

async fn client_identity_fixture(
    handler: &RequestHandler,
    label: &str,
    pid_offset: u32,
) -> ClientIdentityFixture {
    let session_name = SessionName::new(label).expect("valid session");
    assert!(matches!(
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session_name.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
        Response::NewSession(_)
    ));

    let host_pid = std::process::id().saturating_add(pid_offset);
    let victim_pid = host_pid.saturating_add(1);
    let (host_tx, host_rx) = mpsc::unbounded_channel();
    let (victim_tx, victim_rx) = mpsc::unbounded_channel();
    let host_attach_id = handler
        .register_attach(host_pid, session_name.clone(), host_tx)
        .await;
    let victim_attach_id = handler
        .register_attach(victim_pid, session_name.clone(), victim_tx)
        .await;

    let parsed = CommandParser::new()
        .parse_arguments(["choose-client"])
        .expect("choose-client parses");
    let command = RequestHandler::parse_mode_tree_queue_command(parsed.commands()[0].clone())
        .expect("mode-tree command parses")
        .expect("mode-tree command recognized");
    handler
        .execute_queued_mode_tree(
            host_pid,
            command,
            &QueueExecutionContext::without_caller_cwd(),
        )
        .await
        .expect("choose-client opens");

    ClientIdentityFixture {
        session_name,
        host_pid,
        host_attach_id,
        victim_pid,
        victim_attach_id,
        victim_item_id: client_item_id(victim_pid, victim_attach_id),
        host_rx,
        _victim_rx: victim_rx,
    }
}

async fn select_stale_client(handler: &RequestHandler, fixture: &ClientIdentityFixture) {
    handler
        .active_attach
        .lock()
        .await
        .by_pid
        .get_mut(&fixture.host_pid)
        .and_then(|active| active.mode_tree.as_mut())
        .expect("host choose-client remains active")
        .selected_id = Some(fixture.victim_item_id.clone());
}

async fn tag_stale_client(handler: &RequestHandler, fixture: &ClientIdentityFixture) {
    handler
        .active_attach
        .lock()
        .await
        .by_pid
        .get_mut(&fixture.host_pid)
        .and_then(|active| active.mode_tree.as_mut())
        .expect("host choose-client remains active")
        .tagged
        .insert(fixture.victim_item_id.clone());
}

async fn reconnect_victim(
    handler: &RequestHandler,
    fixture: &ClientIdentityFixture,
) -> (u64, mpsc::UnboundedReceiver<crate::pane_io::AttachControl>) {
    let (replacement_tx, replacement_rx) = mpsc::unbounded_channel();
    let replacement_attach_id = handler
        .register_attach(
            fixture.victim_pid,
            fixture.session_name.clone(),
            replacement_tx,
        )
        .await;
    assert_ne!(replacement_attach_id, fixture.victim_attach_id);
    (replacement_attach_id, replacement_rx)
}

async fn assert_reconnected_client_survives(
    handler: &RequestHandler,
    fixture: &ClientIdentityFixture,
    replacement_attach_id: u64,
    replacement_rx: &mut mpsc::UnboundedReceiver<crate::pane_io::AttachControl>,
) {
    let active_attach = handler.active_attach.lock().await;
    let host = active_attach
        .by_pid
        .get(&fixture.host_pid)
        .expect("choose-client host remains attached");
    assert_eq!(host.id, fixture.host_attach_id);
    assert!(
        !host.closing.load(Ordering::SeqCst),
        "stale selection must not fall back to detaching the choose-client host"
    );
    let active = active_attach
        .by_pid
        .get(&fixture.victim_pid)
        .expect("replacement client remains attached");
    assert_eq!(active.id, replacement_attach_id);
    assert!(
        !active.closing.load(Ordering::SeqCst),
        "stale choose-client action must not close the replacement"
    );
    drop(active_attach);
    while let Ok(control) = replacement_rx.try_recv() {
        assert!(
            !matches!(control, crate::pane_io::AttachControl::Detach),
            "stale choose-client action sent Detach to the replacement"
        );
    }
}

async fn wait_for_mode_tree_overlay(
    control_rx: &mut mpsc::UnboundedReceiver<crate::pane_io::AttachControl>,
) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match control_rx.recv().await {
                Some(crate::pane_io::AttachControl::Overlay(_)) => break,
                Some(_) => {}
                None => panic!("choose-client host control channel closed before overlay refresh"),
            }
        }
    })
    .await
    .expect("deferred choose-client action refreshes the mode-tree overlay");
}

#[tokio::test]
async fn choose_client_stale_selection_does_not_detach_reconnected_pid() {
    let handler = RequestHandler::new();
    let fixture = client_identity_fixture(&handler, "choose-client-stale-selection", 610).await;
    select_stale_client(&handler, &fixture).await;
    let (replacement_attach_id, mut replacement_rx) = reconnect_victim(&handler, &fixture).await;

    handler
        .perform_client_detach(fixture.host_pid)
        .await
        .expect("stale selection is a no-op");
    assert_reconnected_client_survives(
        &handler,
        &fixture,
        replacement_attach_id,
        &mut replacement_rx,
    )
    .await;
}

#[tokio::test]
async fn choose_client_stale_tag_does_not_detach_reconnected_pid() {
    let handler = RequestHandler::new();
    let fixture = client_identity_fixture(&handler, "choose-client-stale-tag", 620).await;
    tag_stale_client(&handler, &fixture).await;
    let (replacement_attach_id, mut replacement_rx) = reconnect_victim(&handler, &fixture).await;

    handler
        .perform_client_detach(fixture.host_pid)
        .await
        .expect("stale tag is a no-op");
    assert_reconnected_client_survives(
        &handler,
        &fixture,
        replacement_attach_id,
        &mut replacement_rx,
    )
    .await;
    assert!(
        handler
            .active_attach
            .lock()
            .await
            .by_pid
            .get(&fixture.host_pid)
            .and_then(|active| active.mode_tree.as_ref())
            .expect("host choose-client remains active")
            .tagged
            .is_empty(),
        "the stale tag is pruned so it cannot block a later fallback"
    );
}

#[tokio::test]
async fn choose_client_stale_tag_key_action_is_a_no_op_before_fallback() {
    let handler = RequestHandler::new();
    let fixture = client_identity_fixture(&handler, "choose-client-stale-tag-key", 625).await;
    tag_stale_client(&handler, &fixture).await;
    let (replacement_attach_id, mut replacement_rx) = reconnect_victim(&handler, &fixture).await;

    assert!(handler
        .handle_mode_tree_key_event(fixture.host_pid, PromptInputEvent::Char('d'))
        .await
        .expect("stale tagged key action succeeds"));
    assert_reconnected_client_survives(
        &handler,
        &fixture,
        replacement_attach_id,
        &mut replacement_rx,
    )
    .await;
    assert!(
        handler
            .active_attach
            .lock()
            .await
            .by_pid
            .get(&fixture.host_pid)
            .and_then(|active| active.mode_tree.as_ref())
            .expect("host choose-client remains active")
            .tagged
            .is_empty(),
        "the stale tag is pruned for the next explicit action"
    );

    assert!(handler
        .handle_mode_tree_key_event(fixture.host_pid, PromptInputEvent::Char('d'))
        .await
        .expect("the next untagged action uses the current selection"));
    assert!(
        handler
            .active_attach
            .lock()
            .await
            .by_pid
            .get(&fixture.host_pid)
            .expect("host remains registered until detach is observed")
            .closing
            .load(Ordering::SeqCst),
        "the stale tag must not disable current-selection fallback indefinitely"
    );
}

#[tokio::test]
async fn choose_client_stale_accept_does_not_detach_reconnected_pid() {
    let handler = RequestHandler::new();
    let fixture = client_identity_fixture(&handler, "choose-client-stale-accept", 630).await;
    select_stale_client(&handler, &fixture).await;
    let (replacement_attach_id, mut replacement_rx) = reconnect_victim(&handler, &fixture).await;

    handler
        .accept_mode_tree_selection(fixture.host_pid)
        .await
        .expect("stale accept is a no-op");
    assert_reconnected_client_survives(
        &handler,
        &fixture,
        replacement_attach_id,
        &mut replacement_rx,
    )
    .await;
}

#[tokio::test]
async fn choose_client_confirmation_uses_captured_attach_identity() {
    let handler = RequestHandler::new();
    let mut fixture =
        client_identity_fixture(&handler, "choose-client-confirm-identity", 635).await;
    select_stale_client(&handler, &fixture).await;

    handler
        .handle_attached_live_input_for_test(fixture.host_pid, b"x")
        .await
        .expect("live choose-client delete key opens confirmation");
    assert_eq!(
        handler
            .attached_prompt_render(fixture.host_pid)
            .await
            .expect("choose-client confirmation is active")
            .prompt,
        "detach selected clients?"
    );

    let (replacement_attach_id, mut replacement_rx) = reconnect_victim(&handler, &fixture).await;
    while fixture.host_rx.try_recv().is_ok() {}

    handler
        .handle_attached_live_input_for_test(fixture.host_pid, b"y")
        .await
        .expect("live confirmation input succeeds");
    wait_for_mode_tree_overlay(&mut fixture.host_rx).await;

    assert_reconnected_client_survives(
        &handler,
        &fixture,
        replacement_attach_id,
        &mut replacement_rx,
    )
    .await;
}

#[tokio::test]
async fn choose_client_detach_validates_attach_id_at_send_lock() {
    let handler = RequestHandler::new();
    let fixture = client_identity_fixture(&handler, "choose-client-send-lock-aba", 640).await;
    select_stale_client(&handler, &fixture).await;
    let pause = super::super::super::attach_support::install_attach_control_identity_pause(
        fixture.victim_pid,
    );
    let detach_handler = handler.clone();
    let host_pid = fixture.host_pid;
    let detach = tokio::spawn(async move { detach_handler.perform_client_detach(host_pid).await });

    pause.reached.notified().await;
    let (replacement_attach_id, mut replacement_rx) = reconnect_victim(&handler, &fixture).await;
    pause.release.notify_one();
    detach
        .await
        .expect("detach task joins")
        .expect("stale exact send is ignored");

    assert_reconnected_client_survives(
        &handler,
        &fixture,
        replacement_attach_id,
        &mut replacement_rx,
    )
    .await;
}

#[test]
fn choose_client_item_key_includes_attach_identity() {
    assert_ne!(client_item_id(42, 7), client_item_id(42, 8));
}
