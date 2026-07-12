use super::*;

use rmux_proto::{LinkWindowRequest, WindowTarget};

use crate::pane_terminals::WindowLinkOccurrenceId;

use super::super::mode_tree_model::ChooseTreeTarget;
use super::super::mode_tree_order::{pane_item_id, window_item_id};

struct LinkedOccurrenceFixture {
    session_name: SessionName,
    attach_pid: u32,
    attach_id: u64,
    session_id: rmux_proto::SessionId,
    window_id: rmux_proto::WindowId,
    pane_id: rmux_proto::PaneId,
    old_occurrence_id: WindowLinkOccurrenceId,
    old_window_item_id: String,
    _control_rx: mpsc::UnboundedReceiver<crate::pane_io::AttachControl>,
}

async fn linked_occurrence_fixture(
    handler: &RequestHandler,
    label: &str,
    attach_pid_offset: u32,
) -> LinkedOccurrenceFixture {
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
    link_replacement_occurrence(handler, &session_name).await;

    let (session_id, window_id, pane_id, old_occurrence_id) = {
        let state = handler.state.lock().await;
        let session = state
            .sessions
            .session(&session_name)
            .expect("session exists");
        let window = session.window_at(2).expect("linked occurrence exists");
        (
            session.id(),
            window.id(),
            window.active_pane().expect("active pane exists").id(),
            state
                .window_link_occurrence_id(&session_name, 2)
                .expect("linked occurrence has an identity"),
        )
    };
    let old_window_item_id = window_item_id(session_id, 2, window_id, old_occurrence_id);

    let attach_pid = std::process::id().saturating_add(attach_pid_offset);
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    let attach_id = handler
        .register_attach(attach_pid, session_name.clone(), control_tx)
        .await;
    let parsed = CommandParser::new()
        .parse_arguments(["choose-tree", "-w"])
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
        .expect("mode-tree opens");

    LinkedOccurrenceFixture {
        session_name,
        attach_pid,
        attach_id,
        session_id,
        window_id,
        pane_id,
        old_occurrence_id,
        old_window_item_id,
        _control_rx: control_rx,
    }
}

async fn link_replacement_occurrence(handler: &RequestHandler, session_name: &SessionName) {
    let response = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(session_name.clone(), 0),
            target: WindowTarget::with_window(session_name.clone(), 2),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(response, Response::LinkWindow(_)), "{response:?}");
}

async fn replace_linked_occurrence(
    handler: &RequestHandler,
    fixture: &LinkedOccurrenceFixture,
) -> WindowLinkOccurrenceId {
    let mut state = handler.state.lock().await;
    state
        .unlink_window(
            WindowTarget::with_window(fixture.session_name.clone(), 2),
            false,
        )
        .expect("old occurrence unlinks");
    state
        .link_window(LinkWindowRequest {
            source: WindowTarget::with_window(fixture.session_name.clone(), 0),
            target: WindowTarget::with_window(fixture.session_name.clone(), 2),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        })
        .expect("replacement occurrence links");
    let replacement = state
        .sessions
        .session(&fixture.session_name)
        .and_then(|session| session.window_at(2))
        .expect("replacement occurrence exists");
    assert_eq!(replacement.id(), fixture.window_id);
    let replacement_occurrence_id = state
        .window_link_occurrence_id(&fixture.session_name, 2)
        .expect("replacement occurrence has an identity");
    assert_ne!(replacement_occurrence_id, fixture.old_occurrence_id);
    replacement_occurrence_id
}

async fn set_mode_tree_selection(handler: &RequestHandler, attach_pid: u32, selected_id: String) {
    handler
        .active_attach
        .lock()
        .await
        .by_pid
        .get_mut(&attach_pid)
        .and_then(|active| active.mode_tree.as_mut())
        .expect("mode-tree remains active")
        .selected_id = Some(selected_id);
}

async fn assert_replacement_survives(
    handler: &RequestHandler,
    fixture: &LinkedOccurrenceFixture,
    occurrence_id: WindowLinkOccurrenceId,
) {
    let state = handler.state.lock().await;
    let window = state
        .sessions
        .session(&fixture.session_name)
        .and_then(|session| session.window_at(2))
        .expect("replacement occurrence survives");
    assert_eq!(window.id(), fixture.window_id);
    assert_eq!(
        state.window_link_occurrence_id(&fixture.session_name, 2),
        Some(occurrence_id)
    );
}

#[tokio::test]
async fn choose_tree_stale_window_action_rejects_relinked_same_window_at_mutation_lock() {
    let handler = RequestHandler::new();
    let fixture =
        linked_occurrence_fixture(&handler, "choose-tree-window-occurrence-action-aba", 501).await;
    let stale_action = ModeTreeAction::window_tree_target(
        fixture.session_name.clone(),
        fixture.session_id,
        2,
        fixture.window_id,
        fixture.old_occurrence_id,
    );
    let replacement_occurrence_id = replace_linked_occurrence(&handler, &fixture).await;

    let error = handler
        .perform_tree_kill_actions(fixture.attach_pid, vec![stale_action])
        .await
        .expect_err("stale occurrence action must fail closed");
    assert!(
        matches!(error, RmuxError::InvalidTarget { .. }),
        "{error:?}"
    );
    assert_replacement_survives(&handler, &fixture, replacement_occurrence_id).await;
}

#[tokio::test]
async fn choose_tree_stale_pane_action_rejects_relinked_window_occurrence() {
    let handler = RequestHandler::new();
    let fixture =
        linked_occurrence_fixture(&handler, "choose-tree-pane-occurrence-action-aba", 502).await;
    let stale_action = ModeTreeAction::pane_tree_target(
        fixture.session_name.clone(),
        fixture.session_id,
        2,
        fixture.window_id,
        fixture.old_occurrence_id,
        0,
        fixture.pane_id,
    );
    let replacement_occurrence_id = replace_linked_occurrence(&handler, &fixture).await;

    let error = handler
        .perform_tree_kill_actions(fixture.attach_pid, vec![stale_action])
        .await
        .expect_err("stale pane occurrence action must fail closed");
    assert!(
        matches!(error, RmuxError::InvalidTarget { .. }),
        "{error:?}"
    );
    assert_replacement_survives(&handler, &fixture, replacement_occurrence_id).await;
}

#[tokio::test]
async fn choose_tree_pane_action_resolves_exact_alias_when_pane_id_is_duplicated() {
    let handler = RequestHandler::new();
    let fixture =
        linked_occurrence_fixture(&handler, "choose-tree-pane-duplicate-alias", 506).await;
    let action = ModeTreeAction::pane_tree_target(
        fixture.session_name.clone(),
        fixture.session_id,
        2,
        fixture.window_id,
        fixture.old_occurrence_id,
        0,
        fixture.pane_id,
    );

    handler
        .perform_tree_kill_actions(fixture.attach_pid, vec![action])
        .await
        .expect("current pane occurrence action succeeds");

    let state = handler.state.lock().await;
    let session = state
        .sessions
        .session(&fixture.session_name)
        .expect("session survives");
    assert_eq!(
        session.window_at(0).map(rmux_core::Window::id),
        Some(fixture.window_id),
        "the other alias sharing the PaneId must survive"
    );
    assert!(
        session.window_at(2).is_none(),
        "the selected occurrence must be the one removed"
    );
}

#[tokio::test]
async fn choose_tree_stale_current_selection_does_not_kill_relinked_occurrence() {
    let handler = RequestHandler::new();
    let fixture =
        linked_occurrence_fixture(&handler, "choose-tree-window-occurrence-selection-aba", 503)
            .await;
    set_mode_tree_selection(
        &handler,
        fixture.attach_pid,
        fixture.old_window_item_id.clone(),
    )
    .await;
    let replacement_occurrence_id = replace_linked_occurrence(&handler, &fixture).await;

    handler
        .perform_tree_kill_current(fixture.attach_pid)
        .await
        .expect("stale current selection is a no-op");
    assert_replacement_survives(&handler, &fixture, replacement_occurrence_id).await;
}

#[tokio::test]
async fn choose_tree_stale_tag_does_not_kill_relinked_occurrence() {
    let handler = RequestHandler::new();
    let fixture =
        linked_occurrence_fixture(&handler, "choose-tree-window-occurrence-tag-aba", 504).await;
    handler
        .active_attach
        .lock()
        .await
        .by_pid
        .get_mut(&fixture.attach_pid)
        .and_then(|active| active.mode_tree.as_mut())
        .expect("mode-tree remains active")
        .tagged
        .insert(fixture.old_window_item_id.clone());
    let replacement_occurrence_id = replace_linked_occurrence(&handler, &fixture).await;

    handler
        .perform_tree_kill_tagged(fixture.attach_pid)
        .await
        .expect("stale tagged selection is a no-op");
    assert_replacement_survives(&handler, &fixture, replacement_occurrence_id).await;
}

#[tokio::test]
async fn choose_tree_accept_does_not_select_relinked_stale_occurrence() {
    let handler = RequestHandler::new();
    let fixture =
        linked_occurrence_fixture(&handler, "choose-tree-window-occurrence-accept-aba", 505).await;
    set_mode_tree_selection(
        &handler,
        fixture.attach_pid,
        fixture.old_window_item_id.clone(),
    )
    .await;
    let replacement_occurrence_id = replace_linked_occurrence(&handler, &fixture).await;

    handler
        .accept_mode_tree_selection(fixture.attach_pid)
        .await
        .expect("stale accept selection is a no-op");
    assert_replacement_survives(&handler, &fixture, replacement_occurrence_id).await;
    assert_eq!(
        handler
            .state
            .lock()
            .await
            .sessions
            .session(&fixture.session_name)
            .expect("session survives")
            .active_window_index(),
        0,
        "stale accept must not select the replacement at index 2",
    );
}

#[tokio::test]
async fn choose_tree_default_accept_revalidates_occurrence_at_selection_lock() {
    let handler = RequestHandler::new();
    let fixture = linked_occurrence_fixture(
        &handler,
        "choose-tree-window-occurrence-accept-lock-aba",
        507,
    )
    .await;
    let stale_target = ChooseTreeTarget {
        session_name: fixture.session_name.clone(),
        session_id: fixture.session_id,
        window_index: Some(2),
        window_id: Some(fixture.window_id),
        window_occurrence_id: Some(fixture.old_occurrence_id),
        pane_index: None,
        pane_id: None,
    };
    let replacement_occurrence_id = replace_linked_occurrence(&handler, &fixture).await;

    let error = handler
        .apply_choose_tree_default_target(fixture.attach_pid, fixture.attach_id, stale_target)
        .await
        .expect_err("default accept must reject a replaced occurrence at the state lock");
    assert!(
        matches!(error, RmuxError::InvalidTarget { .. }),
        "{error:?}"
    );
    assert_replacement_survives(&handler, &fixture, replacement_occurrence_id).await;
}

#[test]
fn pane_item_identity_changes_with_its_window_link_occurrence() {
    assert_ne!(
        pane_item_id(
            rmux_proto::SessionId::new(1),
            2,
            rmux_proto::WindowId::new(3),
            WindowLinkOccurrenceId::new_for_test(4),
            rmux_proto::PaneId::new(5),
        ),
        pane_item_id(
            rmux_proto::SessionId::new(1),
            2,
            rmux_proto::WindowId::new(3),
            WindowLinkOccurrenceId::new_for_test(6),
            rmux_proto::PaneId::new(5),
        ),
    );
}
