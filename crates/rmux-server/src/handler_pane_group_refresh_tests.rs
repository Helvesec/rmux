use std::time::Duration;

use rmux_proto::{PaneTarget, Request, Response, SwapPaneRequest};
use tokio::sync::mpsc;
use tokio::time::timeout;

use super::pane_group_transfer_tests::{create_grouped_session, create_session, split_session};
use super::RequestHandler;
use crate::pane_io::AttachControl;

async fn drain_controls(control_rx: &mut mpsc::UnboundedReceiver<AttachControl>) {
    while timeout(Duration::from_millis(50), control_rx.recv())
        .await
        .is_ok()
    {}
}

#[tokio::test]
async fn swap_pane_refreshes_attached_non_syntactic_group_peer() {
    let handler = RequestHandler::new();
    let owner = create_session(&handler, "swap-refresh-owner").await;
    split_session(&handler, &owner).await;
    let peer = create_grouped_session(&handler, "swap-refresh-peer", &owner).await;
    let target = create_session(&handler, "swap-refresh-target").await;
    handler.wait_for_initial_panes_for_test().await;

    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    handler.register_attach(42, peer.clone(), control_tx).await;
    drain_controls(&mut control_rx).await;

    let response = handler
        .handle(Request::SwapPane(SwapPaneRequest {
            source: PaneTarget::with_window(owner, 0, 0),
            target: PaneTarget::with_window(target, 0, 0),
            direction: None,
            detached: true,
            preserve_zoom: false,
        }))
        .await;
    assert!(matches!(response, Response::SwapPane(_)), "{response:?}");

    let control = timeout(Duration::from_secs(2), control_rx.recv())
        .await
        .expect("non-syntactic grouped peer must be refreshed")
        .expect("peer attach remains connected");
    let AttachControl::Switch(target) = control else {
        panic!("expected peer switch refresh, got {control:?}");
    };
    let target = target.into_target();
    assert_eq!(target.session_name, peer);
}
