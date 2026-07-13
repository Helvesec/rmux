use super::*;

use std::time::Instant;

use rmux_core::events::PaneOutputSubscriptionKey;
use rmux_proto::{
    PaneId, PaneOutputCursorRequest, PaneOutputSubscriptionStart, SessionId,
    SubscribePaneOutputRequest,
};

use crate::handler::exited_output_support::RetainedExitedPaneIdentities;
use crate::pane_io::pane_output_channel_with_limits;

async fn create_session(handler: &RequestHandler, name: &SessionName) -> SessionId {
    let response = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: name.clone(),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))
        .await;
    assert!(matches!(response, Response::NewSession(_)), "{response:?}");
    handler
        .state
        .lock()
        .await
        .sessions
        .session(name)
        .expect("created session exists")
        .id()
}

async fn create_grouped_session(
    handler: &RequestHandler,
    name: &SessionName,
    group_target: &SessionName,
) -> SessionId {
    let response = handler
        .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
            session_name: Some(name.clone()),
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
    handler
        .state
        .lock()
        .await
        .sessions
        .session(name)
        .expect("created grouped session exists")
        .id()
}

async fn rename(handler: &RequestHandler, old_name: SessionName, new_name: SessionName) {
    let response = handler
        .handle(Request::RenameSession(RenameSessionRequest {
            target: old_name,
            new_name: new_name.clone(),
        }))
        .await;
    assert_eq!(
        response,
        Response::RenameSession(rmux_proto::RenameSessionResponse {
            session_name: new_name,
        })
    );
}

async fn subscribe_retained(
    handler: &RequestHandler,
    connection_id: u64,
    target: PaneTarget,
) -> (PaneOutputSubscriptionKey, Vec<Vec<u8>>) {
    let response = handler
        .handle_subscribe_pane_output(
            connection_id,
            SubscribePaneOutputRequest {
                target: target.clone(),
                start: PaneOutputSubscriptionStart::Oldest,
            },
        )
        .await;
    let Response::SubscribePaneOutput(subscribe) = response else {
        panic!("retained output should replay for {target}: {response:?}");
    };
    let pane_key = handler
        .pane_output_subscription_key_for_test(subscribe.subscription_id)
        .expect("retained subscription records its runtime pane identity");
    let response = handler
        .handle_pane_output_cursor(
            connection_id,
            PaneOutputCursorRequest {
                subscription_id: subscribe.subscription_id,
                max_events: Some(8),
            },
        )
        .await;
    let Response::PaneOutputCursor(cursor) = response else {
        panic!("retained subscription should expose its cursor: {response:?}");
    };
    (
        pane_key,
        cursor.events.into_iter().map(|event| event.bytes).collect(),
    )
}

#[tokio::test]
async fn rename_session_rekeys_retained_output_for_late_replay() {
    let handler = RequestHandler::new();
    let alpha = session_name("retained-rename-alpha");
    let beta = session_name("retained-rename-beta");
    let session_id = create_session(&handler, &alpha).await;
    let pane_id = PaneId::new(90_001);
    let old_target = PaneTarget::with_window(alpha.clone(), 0, 91);
    let old_pane = PaneOutputSubscriptionKey::new(alpha.clone(), pane_id);
    let output = pane_output_channel_with_limits(8, 1024);
    output.send(b"before rename".to_vec());
    output.send(Vec::new());
    handler
        .retain_exited_pane_output(
            old_target.clone(),
            old_pane.clone(),
            RetainedExitedPaneIdentities::new(session_id, session_id),
            output,
        )
        .await;

    rename(&handler, alpha, beta.clone()).await;
    let new_target = PaneTarget::with_window(beta.clone(), 0, 91);
    let new_pane = PaneOutputSubscriptionKey::new(beta, pane_id);
    assert!(handler
        .retained_exited_pane_output(&old_target, Instant::now())
        .is_none());
    assert!(handler
        .retained_exited_pane_output_by_pane(&old_pane, Instant::now())
        .is_none());

    let (registered_pane, events) = subscribe_retained(&handler, 71, new_target).await;
    assert_eq!(registered_pane, new_pane);
    assert_eq!(events, vec![b"before rename".to_vec(), Vec::new()]);
}

#[tokio::test]
async fn retained_output_captured_before_rename_but_inserted_afterward_is_normalized() {
    let handler = RequestHandler::new();
    let alpha = session_name("retained-late-alpha");
    let beta = session_name("retained-late-beta");
    let session_id = create_session(&handler, &alpha).await;
    rename(&handler, alpha.clone(), beta.clone()).await;

    let pane_id = PaneId::new(90_002);
    let stale_target = PaneTarget::with_window(alpha.clone(), 0, 92);
    let stale_pane = PaneOutputSubscriptionKey::new(alpha, pane_id);
    let output = pane_output_channel_with_limits(8, 1024);
    output.send(b"late insert".to_vec());
    output.send(Vec::new());
    handler
        .retain_exited_pane_output(
            stale_target.clone(),
            stale_pane.clone(),
            RetainedExitedPaneIdentities::new(session_id, session_id),
            output,
        )
        .await;

    assert!(handler
        .retained_exited_pane_output(&stale_target, Instant::now())
        .is_none());
    assert!(handler
        .retained_exited_pane_output_by_pane(&stale_pane, Instant::now())
        .is_none());
    let normalized_target = PaneTarget::with_window(beta.clone(), 0, 92);
    let (registered_pane, events) = subscribe_retained(&handler, 72, normalized_target).await;
    assert_eq!(
        registered_pane,
        PaneOutputSubscriptionKey::new(beta, pane_id)
    );
    assert_eq!(events, vec![b"late insert".to_vec(), Vec::new()]);
}

#[tokio::test]
async fn grouped_alias_and_runtime_owner_renames_rekey_distinct_retained_identities() {
    let handler = RequestHandler::new();
    let owner = session_name("retained-group-owner");
    let peer = session_name("retained-group-peer");
    let renamed_owner = session_name("retained-group-owner-renamed");
    let renamed_peer = session_name("retained-group-peer-renamed");
    let owner_id = create_session(&handler, &owner).await;
    let peer_id = create_grouped_session(&handler, &peer, &owner).await;
    let pane_id = PaneId::new(90_003);
    let target = PaneTarget::with_window(peer.clone(), 0, 93);
    let pane = PaneOutputSubscriptionKey::new(owner.clone(), pane_id);
    let output = pane_output_channel_with_limits(8, 1024);
    output.send(b"grouped retained".to_vec());
    output.send(Vec::new());
    handler
        .retain_exited_pane_output(
            target,
            pane,
            RetainedExitedPaneIdentities::new(peer_id, owner_id),
            output,
        )
        .await;

    rename(&handler, peer, renamed_peer.clone()).await;
    rename(&handler, owner, renamed_owner.clone()).await;

    let renamed_target = PaneTarget::with_window(renamed_peer, 0, 93);
    let (registered_pane, events) = subscribe_retained(&handler, 73, renamed_target).await;
    assert_eq!(
        registered_pane,
        PaneOutputSubscriptionKey::new(renamed_owner, pane_id)
    );
    assert_eq!(events, vec![b"grouped retained".to_vec(), Vec::new()]);
}
