use rmux_proto::{
    BreakPaneRequest, JoinPaneRequest, LinkWindowRequest, MovePaneRequest, MoveWindowRequest,
    MoveWindowTarget, NewWindowRequest, PaneKillRequest, PaneTargetRef, SplitDirection,
    SwapPaneRequest, SwapWindowRequest, UnlinkWindowRequest, WindowTarget,
};

use crate::handler::pane_group_transfer_tests::{
    create_grouped_session as create_grouped_transfer_session,
    create_session as create_transfer_session, split_session as split_transfer_session,
};

use super::*;

const CONNECTION_ID: u64 = 81;

#[derive(Clone, Copy, Debug)]
enum TransferCase {
    Swap,
    Join,
    Move,
    Break,
}

impl TransferCase {
    const fn label(self) -> &'static str {
        match self {
            Self::Swap => "swap",
            Self::Join => "join",
            Self::Move => "move",
            Self::Break => "break",
        }
    }
}

#[tokio::test]
async fn swap_rekeys_existing_pane_output_subscription() {
    assert_subscription_follows_transfer(TransferCase::Swap).await;
}

#[tokio::test]
async fn join_rekeys_existing_pane_output_subscription() {
    assert_subscription_follows_transfer(TransferCase::Join).await;
}

#[tokio::test]
async fn move_rekeys_existing_pane_output_subscription() {
    assert_subscription_follows_transfer(TransferCase::Move).await;
}

#[tokio::test]
async fn break_rekeys_existing_pane_output_subscription() {
    assert_subscription_follows_transfer(TransferCase::Break).await;
}

#[tokio::test]
async fn swap_between_group_aliases_rekeys_subscription_to_linked_runtime_owner() {
    let handler = RequestHandler::new();
    let owner = create_transfer_session(&handler, "subscription-group-swap-owner").await;
    split_transfer_session(&handler, &owner).await;
    let linked_owner = create_transfer_session(&handler, "subscription-group-swap-linked").await;
    split_transfer_session(&handler, &linked_owner).await;
    link_window(
        &handler,
        WindowTarget::with_window(linked_owner.clone(), 0),
        WindowTarget::with_window(owner.clone(), 1),
        false,
    )
    .await;
    let peer =
        create_grouped_transfer_session(&handler, "subscription-group-swap-peer", &owner).await;
    handler.wait_for_initial_panes_for_test().await;

    let source = PaneTarget::with_window(owner.clone(), 0, 0);
    let (subscription_id, pane_id) = subscribe_to_target(&handler, &source).await;
    let swapped = handler
        .handle(Request::SwapPane(SwapPaneRequest {
            source,
            target: PaneTarget::with_window(peer, 1, 0),
            direction: None,
            detached: true,
            preserve_zoom: false,
        }))
        .await;
    assert!(matches!(swapped, Response::SwapPane(_)), "{swapped:?}");

    assert_window_owner_transfer(
        &handler,
        subscription_id,
        pane_id,
        linked_owner,
        "group-alias swap-pane",
    )
    .await;
}

#[tokio::test]
async fn unlink_window_rekeys_subscription_when_runtime_owner_slot_is_removed() {
    let handler = RequestHandler::new();
    let owner = SessionName::new("subscription-unlink-owner").expect("valid owner");
    let external = SessionName::new("subscription-unlink-external").expect("valid external");
    create_session(&handler, &owner).await;
    create_window(&handler, &owner, 1).await;
    create_session(&handler, &external).await;
    link_window(
        &handler,
        WindowTarget::with_window(owner.clone(), 0),
        WindowTarget::with_window(external.clone(), 1),
        false,
    )
    .await;

    let source = PaneTarget::with_window(owner.clone(), 0, 0);
    let (subscription_id, pane_id) = subscribe_to_target(&handler, &source).await;
    let unlinked = handler
        .handle(Request::UnlinkWindow(UnlinkWindowRequest {
            target: WindowTarget::with_window(owner, 0),
            kill_if_last: false,
        }))
        .await;
    assert!(
        matches!(unlinked, Response::UnlinkWindow(_)),
        "{unlinked:?}"
    );

    assert_window_owner_transfer(
        &handler,
        subscription_id,
        pane_id,
        external,
        "unlink-window",
    )
    .await;
}

#[tokio::test]
async fn link_window_k_rekeys_subscription_for_detached_destination_runtime() {
    let handler = RequestHandler::new();
    let owner = SessionName::new("subscription-link-owner").expect("valid owner");
    let external = SessionName::new("subscription-link-external").expect("valid external");
    let replacement = SessionName::new("subscription-link-replacement").expect("valid replacement");
    create_session(&handler, &owner).await;
    create_window(&handler, &owner, 1).await;
    create_session(&handler, &external).await;
    create_session(&handler, &replacement).await;
    link_window(
        &handler,
        WindowTarget::with_window(owner.clone(), 0),
        WindowTarget::with_window(external.clone(), 1),
        false,
    )
    .await;

    let source = PaneTarget::with_window(owner.clone(), 0, 0);
    let (subscription_id, pane_id) = subscribe_to_target(&handler, &source).await;
    link_window(
        &handler,
        WindowTarget::with_window(replacement, 0),
        WindowTarget::with_window(owner, 0),
        true,
    )
    .await;

    assert_window_owner_transfer(
        &handler,
        subscription_id,
        pane_id,
        external,
        "link-window -k",
    )
    .await;
}

#[tokio::test]
async fn move_window_rekeys_subscription_across_sessions() {
    let handler = RequestHandler::new();
    let source_name = SessionName::new("subscription-move-window-source").expect("valid source");
    let target_name = SessionName::new("subscription-move-window-target").expect("valid target");
    create_session(&handler, &source_name).await;
    create_window(&handler, &source_name, 1).await;
    create_session(&handler, &target_name).await;

    let source = PaneTarget::with_window(source_name.clone(), 0, 0);
    let (subscription_id, pane_id) = subscribe_to_target(&handler, &source).await;
    let moved = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(source_name, 0)),
            target: MoveWindowTarget::Window(WindowTarget::with_window(target_name.clone(), 1)),
            renumber: false,
            kill_destination: false,
            detached: true,
            after: false,
            before: false,
        }))
        .await;
    assert!(matches!(moved, Response::MoveWindow(_)), "{moved:?}");

    assert_window_owner_transfer(
        &handler,
        subscription_id,
        pane_id,
        target_name,
        "move-window",
    )
    .await;
}

#[tokio::test]
async fn swap_window_rekeys_subscription_across_sessions() {
    let handler = RequestHandler::new();
    let source_name = SessionName::new("subscription-swap-window-source").expect("valid source");
    let target_name = SessionName::new("subscription-swap-window-target").expect("valid target");
    create_session(&handler, &source_name).await;
    create_session(&handler, &target_name).await;

    let source = PaneTarget::with_window(source_name.clone(), 0, 0);
    let (subscription_id, pane_id) = subscribe_to_target(&handler, &source).await;
    let swapped = handler
        .handle(Request::SwapWindow(SwapWindowRequest {
            source: WindowTarget::with_window(source_name, 0),
            target: WindowTarget::with_window(target_name.clone(), 0),
            detached: true,
        }))
        .await;
    assert!(matches!(swapped, Response::SwapWindow(_)), "{swapped:?}");

    assert_window_owner_transfer(
        &handler,
        subscription_id,
        pane_id,
        target_name,
        "swap-window",
    )
    .await;
}

async fn assert_subscription_follows_transfer(case: TransferCase) {
    let handler = RequestHandler::new();
    let source_name =
        SessionName::new(format!("subscription-{}-source", case.label())).expect("valid source");
    let target_name =
        SessionName::new(format!("subscription-{}-target", case.label())).expect("valid target");
    create_session(&handler, &source_name).await;
    create_session(&handler, &target_name).await;

    let source_target = PaneTarget::with_window(source_name.clone(), 0, 0);
    let target_target = PaneTarget::with_window(target_name.clone(), 0, 0);
    let source_pane_id = pane_id_for_target(&handler, &source_target).await;
    let subscribed = handler
        .handle_subscribe_pane_output_ref(
            CONNECTION_ID,
            SubscribePaneOutputRefRequest {
                target: PaneTargetRef::by_id(source_name.clone(), source_pane_id),
                start: PaneOutputSubscriptionStart::Now,
            },
        )
        .await;
    let Response::SubscribePaneOutput(subscribed) = subscribed else {
        panic!(
            "{} subscription should succeed: {subscribed:?}",
            case.label()
        );
    };

    let response = match case {
        TransferCase::Swap => {
            handler
                .handle(Request::SwapPane(SwapPaneRequest {
                    source: source_target,
                    target: target_target,
                    direction: None,
                    detached: false,
                    preserve_zoom: false,
                }))
                .await
        }
        TransferCase::Join => {
            handler
                .handle(Request::JoinPane(JoinPaneRequest {
                    source: source_target,
                    target: target_target,
                    direction: SplitDirection::Vertical,
                    detached: false,
                    before: false,
                    full_size: false,
                    size: None,
                }))
                .await
        }
        TransferCase::Move => {
            handler
                .handle(Request::MovePane(MovePaneRequest {
                    source: source_target,
                    target: target_target,
                    direction: SplitDirection::Vertical,
                    detached: false,
                    before: false,
                    full_size: false,
                    size: None,
                }))
                .await
        }
        TransferCase::Break => {
            handler
                .handle(Request::BreakPane(Box::new(BreakPaneRequest {
                    source: source_target,
                    target: Some(WindowTarget::with_window(target_name.clone(), 1)),
                    name: None,
                    detached: false,
                    after: false,
                    before: false,
                    print_target: false,
                    format: None,
                })))
                .await
        }
    };
    assert_transfer_succeeded(case, &response);

    let moved_target = pane_target_for_id(&handler, &target_name, source_pane_id).await;
    let canonical_key = {
        let state = handler.state.lock().await;
        state
            .pane_output_subscription_key_for_pane_id(source_pane_id)
            .expect("moved pane has a canonical output key")
    };
    let registered_key = handler
        .pane_output_subscription_key_for_test(subscribed.subscription_id)
        .expect("subscription survives the transfer");
    assert_eq!(
        registered_key,
        canonical_key,
        "{} canonical key",
        case.label()
    );
    assert_eq!(
        registered_key.runtime_session_name(),
        &target_name,
        "{} moves the output owner to the destination session",
        case.label()
    );

    let expected = format!("{}-after-transfer", case.label()).into_bytes();
    handler
        .send_pane_output_for_test(&moved_target, expected.clone())
        .await;
    let cursor = handler
        .handle_pane_output_cursor(
            CONNECTION_ID,
            PaneOutputCursorRequest {
                subscription_id: subscribed.subscription_id,
                max_events: Some(16),
            },
        )
        .await;
    let Response::PaneOutputCursor(cursor) = cursor else {
        panic!(
            "{} moved subscription should remain readable: {cursor:?}",
            case.label()
        );
    };
    assert!(
        cursor.events.iter().any(|event| event.bytes == expected),
        "{} receiver must remain attached to the moved sender",
        case.label()
    );

    let killed = handler
        .handle(Request::PaneKill(PaneKillRequest {
            target: PaneTargetRef::by_id(target_name, source_pane_id),
            kill_all_except: false,
        }))
        .await;
    assert!(
        matches!(killed, Response::KillPane(_)),
        "{} moved pane cleanup should succeed: {killed:?}",
        case.label()
    );
    let cursor_after_kill = handler
        .handle_pane_output_cursor(
            CONNECTION_ID,
            PaneOutputCursorRequest {
                subscription_id: subscribed.subscription_id,
                max_events: Some(1),
            },
        )
        .await;
    assert!(
        matches!(
            cursor_after_kill,
            Response::Error(ref error) if error.error.to_string().contains("subscription not found")
        ),
        "{} kill must clean the rekeyed record: {cursor_after_kill:?}",
        case.label()
    );
}

async fn create_session(handler: &RequestHandler, session_name: &SessionName) {
    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name.clone(),
            detached: true,
            size: None,
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)), "{created:?}");
    handler.wait_for_initial_panes_for_test().await;
}

async fn create_window(handler: &RequestHandler, session_name: &SessionName, window_index: u32) {
    let created = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: session_name.clone(),
            name: None,
            detached: true,
            environment: None,
            command: None,
            start_directory: None,
            target_window_index: Some(window_index),
            insert_at_target: false,
            process_command: None,
        })))
        .await;
    assert!(matches!(created, Response::NewWindow(_)), "{created:?}");
    handler.wait_for_initial_panes_for_test().await;
}

async fn link_window(
    handler: &RequestHandler,
    source: WindowTarget,
    target: WindowTarget,
    kill_destination: bool,
) {
    let linked = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source,
            target,
            after: false,
            before: false,
            kill_destination,
            detached: true,
        }))
        .await;
    assert!(matches!(linked, Response::LinkWindow(_)), "{linked:?}");
}

async fn subscribe_to_target(
    handler: &RequestHandler,
    target: &PaneTarget,
) -> (rmux_proto::PaneOutputSubscriptionId, PaneId) {
    let pane_id = pane_id_for_target(handler, target).await;
    let response = handler
        .handle_subscribe_pane_output_ref(
            CONNECTION_ID,
            SubscribePaneOutputRefRequest {
                target: PaneTargetRef::by_id(target.session_name().clone(), pane_id),
                start: PaneOutputSubscriptionStart::Now,
            },
        )
        .await;
    let Response::SubscribePaneOutput(subscribed) = response else {
        panic!("subscription should succeed: {response:?}");
    };
    (subscribed.subscription_id, pane_id)
}

async fn assert_window_owner_transfer(
    handler: &RequestHandler,
    subscription_id: rmux_proto::PaneOutputSubscriptionId,
    pane_id: PaneId,
    destination_session: SessionName,
    label: &str,
) {
    let moved_target = pane_target_for_id(handler, &destination_session, pane_id).await;
    let canonical_key = {
        let state = handler.state.lock().await;
        state
            .pane_output_subscription_key_for_pane_id(pane_id)
            .expect("moved window pane has a canonical output key")
    };
    let registered_key = handler
        .pane_output_subscription_key_for_test(subscription_id)
        .expect("subscription survives the window owner transfer");
    assert_eq!(registered_key, canonical_key, "{label} canonical key");
    assert_eq!(
        registered_key.runtime_session_name(),
        &destination_session,
        "{label} moves the output owner to the surviving destination"
    );

    let expected = format!("{label}-after-transfer").into_bytes();
    handler
        .send_pane_output_for_test(&moved_target, expected.clone())
        .await;
    let cursor = handler
        .handle_pane_output_cursor(
            CONNECTION_ID,
            PaneOutputCursorRequest {
                subscription_id,
                max_events: Some(16),
            },
        )
        .await;
    let Response::PaneOutputCursor(cursor) = cursor else {
        panic!("{label} moved subscription should remain readable: {cursor:?}");
    };
    assert!(
        cursor.events.iter().any(|event| event.bytes == expected),
        "{label} receiver follows the moved sender"
    );

    let killed = handler
        .handle(Request::PaneKill(PaneKillRequest {
            target: PaneTargetRef::by_id(destination_session, pane_id),
            kill_all_except: false,
        }))
        .await;
    assert!(
        matches!(killed, Response::KillPane(_)),
        "{label}: {killed:?}"
    );
    let cursor_after_kill = handler
        .handle_pane_output_cursor(
            CONNECTION_ID,
            PaneOutputCursorRequest {
                subscription_id,
                max_events: Some(1),
            },
        )
        .await;
    assert!(
        matches!(
            cursor_after_kill,
            Response::Error(ref error) if error.error.to_string().contains("subscription not found")
        ),
        "{label} kill cleans the rekeyed record: {cursor_after_kill:?}"
    );
}

async fn pane_id_for_target(handler: &RequestHandler, target: &PaneTarget) -> PaneId {
    let state = handler.state.lock().await;
    state
        .sessions
        .session(target.session_name())
        .and_then(|session| session.window_at(target.window_index()))
        .and_then(|window| window.pane(target.pane_index()))
        .map(rmux_core::Pane::id)
        .expect("pane target exists")
}

async fn pane_target_for_id(
    handler: &RequestHandler,
    session_name: &SessionName,
    pane_id: PaneId,
) -> PaneTarget {
    let state = handler.state.lock().await;
    let session = state
        .sessions
        .session(session_name)
        .expect("destination session survives");
    session
        .windows()
        .iter()
        .find_map(|(window_index, window)| {
            window
                .panes()
                .iter()
                .find(|pane| pane.id() == pane_id)
                .map(|pane| {
                    PaneTarget::with_window(session_name.clone(), *window_index, pane.index())
                })
        })
        .expect("moved pane is reachable by stable id")
}

fn assert_transfer_succeeded(case: TransferCase, response: &Response) {
    let succeeded = matches!(
        (case, response),
        (TransferCase::Swap, Response::SwapPane(_))
            | (TransferCase::Join, Response::JoinPane(_))
            | (TransferCase::Move, Response::MovePane(_))
            | (TransferCase::Break, Response::BreakPane(_))
    );
    assert!(succeeded, "{} transfer failed: {response:?}", case.label());
}
