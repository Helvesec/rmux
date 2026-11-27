use super::*;
use crate::daemon::ShutdownHandle;
use rmux_proto::OptionName;

#[cfg(unix)]
async fn assert_exit_empty_counts_replied_requester(drain_attach: bool) {
    let handler = RequestHandler::new();
    let (shutdown_handle, mut shutdown_rx) = ShutdownHandle::new();
    handler.install_shutdown_handle(shutdown_handle);
    let connection = handler.begin_detached_connection(7);
    let forwarder = drain_attach.then(|| handler.begin_attach_forwarder());
    handler.queue_shutdown_request(PendingShutdownReason::ExitEmpty);
    assert!(!handler.request_shutdown_if_pending_excluding_detached_connection(Some(7)));
    drop(forwarder);

    // A retry must not inherit permission to close this still-open SDK client.
    assert!(
        tokio::time::timeout(SHUTDOWN_RETRY_DELAY * 3, &mut shutdown_rx)
            .await
            .is_err(),
        "exit-empty closed the replied client after the attach drain"
    );
    assert!(!handler.request_shutdown_if_pending_excluding_detached_connection(Some(7)));
    drop(connection);
    tokio::time::timeout(SHUTDOWN_RETRY_DELAY * 3, shutdown_rx)
        .await
        .expect("exit-empty retries after the detached connection closes")
        .expect("shutdown receiver completes");
}

#[cfg(unix)]
#[tokio::test]
async fn unix_exit_empty_counts_replied_requester_without_attach() {
    assert_exit_empty_counts_replied_requester(false).await;
}

#[cfg(unix)]
#[tokio::test]
async fn unix_exit_empty_counts_replied_requester_after_attach_drain() {
    assert_exit_empty_counts_replied_requester(true).await;
}

#[test]
fn normal_request_close_linearizes_against_drain_admission() {
    let handler = RequestHandler::new();
    let admitted = handler
        .try_begin_normal_request(true)
        .expect("request is admitted before quiesce");

    handler.close_normal_request_admission();

    assert!(!handler.normal_requests_quiesced());
    assert!(!handler.normal_drain_requests_quiesced());
    assert!(
        handler.try_begin_normal_request(true).is_none(),
        "requests after the close linearization point are rejected"
    );

    drop(admitted);
    assert!(handler.normal_requests_quiesced());
    assert!(handler.normal_drain_requests_quiesced());
}

#[test]
fn cancel_safe_requests_do_not_hold_the_drain_barrier() {
    let handler = RequestHandler::new();
    let admitted = handler
        .try_begin_normal_request(false)
        .expect("cancel-safe request is admitted before quiesce");
    handler.close_normal_request_admission();

    assert!(!handler.normal_requests_quiesced());
    assert!(handler.normal_drain_requests_quiesced());

    drop(admitted);
    assert!(handler.normal_requests_quiesced());
}

#[tokio::test]
async fn later_full_reevaluation_tightens_a_scheduled_requester_exclusion() {
    let handler = RequestHandler::new();
    let (shutdown_handle, mut shutdown_rx) = ShutdownHandle::new();
    handler.install_shutdown_handle(shutdown_handle);

    let requester_connection_id = 7;
    let requester_connection = handler.begin_detached_connection(requester_connection_id);
    let forwarder = handler.begin_attach_forwarder();
    handler.queue_shutdown_request(PendingShutdownReason::ExitEmpty);

    assert!(
        !handler.request_shutdown_if_pending_excluding_detached_connection(Some(
            requester_connection_id
        )),
        "the attached wire drain should schedule a requester-excluding retry"
    );
    drop(forwarder);
    assert!(
        !handler.request_shutdown_if_pending(),
        "the later full reevaluation must count every detached connection"
    );

    assert!(
        tokio::time::timeout(SHUTDOWN_RETRY_DELAY * 3, &mut shutdown_rx)
            .await
            .is_err(),
        "the old requester exclusion survived the later full reevaluation"
    );

    drop(requester_connection);
    tokio::time::timeout(SHUTDOWN_RETRY_DELAY * 3, shutdown_rx)
        .await
        .expect("shutdown should follow the SDK connection close")
        .expect("shutdown receiver should complete cleanly");
}

#[tokio::test]
async fn idle_shutdown_retry_preserves_excluded_detached_connection() {
    let handler = RequestHandler::new();
    let (shutdown_handle, shutdown_rx) = ShutdownHandle::new();
    handler.install_shutdown_handle(shutdown_handle);

    let requester_connection_id = 7;
    let _requester_connection = handler.begin_detached_connection(requester_connection_id);
    handler.queue_shutdown_request(PendingShutdownReason::SeamlessUpgradeIdle);

    let active_connections = handler
        .active_detached_connections
        .lock()
        .expect("active detached connection mutex must not be poisoned");
    assert!(!handler
        .request_shutdown_if_pending_excluding_detached_connection(Some(requester_connection_id)));
    drop(active_connections);

    tokio::time::timeout(std::time::Duration::from_millis(500), shutdown_rx)
        .await
        .expect("retry should preserve requester exclusion and request shutdown")
        .expect("shutdown receiver should complete cleanly");
}

#[tokio::test]
async fn idle_shutdown_retries_after_in_flight_detached_request() {
    let handler = RequestHandler::new();
    let (shutdown_handle, shutdown_rx) = ShutdownHandle::new();
    handler.install_shutdown_handle(shutdown_handle);
    let _request = handler.begin_detached_request();

    handler.queue_shutdown_request(PendingShutdownReason::ExitEmpty);
    assert!(
        !handler.request_shutdown_if_pending(),
        "in-flight detached requests should defer, not cancel, exit-empty shutdown"
    );
    drop(_request);

    tokio::time::timeout(std::time::Duration::from_millis(500), shutdown_rx)
        .await
        .expect("retry should request shutdown after detached request finishes")
        .expect("shutdown receiver should complete cleanly");
}

#[tokio::test]
async fn idle_shutdown_retries_after_attach_forwarder_drain() {
    let handler = RequestHandler::new();
    let (shutdown_handle, shutdown_rx) = ShutdownHandle::new();
    handler.install_shutdown_handle(shutdown_handle);
    let forwarder = handler.begin_attach_forwarder();

    handler.queue_shutdown_request(PendingShutdownReason::ExitEmpty);
    assert!(
        !handler.request_shutdown_if_pending(),
        "an attached wire drain should defer, not cancel, exit-empty shutdown"
    );
    drop(forwarder);

    tokio::time::timeout(std::time::Duration::from_millis(500), shutdown_rx)
        .await
        .expect("retry should request shutdown after the attach forwarder drains")
        .expect("shutdown receiver should complete cleanly");
}

#[tokio::test]
async fn lifecycle_close_cancels_pending_shutdown_retry() {
    let handler = RequestHandler::new();
    let (shutdown_handle, shutdown_rx) = ShutdownHandle::new();
    handler.install_shutdown_handle(shutdown_handle);
    let state = handler.state.lock().await;

    handler.queue_shutdown_request(PendingShutdownReason::ExitEmpty);
    assert!(
        !handler.request_shutdown_if_pending(),
        "the held state lock forces the retry path"
    );
    handler.close_normal_and_drain_lifecycle_producers().await;
    drop(state);

    assert!(
        tokio::time::timeout(SHUTDOWN_RETRY_DELAY * 2, shutdown_rx)
            .await
            .is_err(),
        "a cancelled retry cannot request shutdown after the lane is sealed"
    );
}

#[tokio::test]
async fn exit_unattached_requires_exit_empty_and_revalidates_policy_before_shutdown() {
    use rmux_proto::{ScopeSelector, SessionName, SetOptionMode, TerminalSize};
    let handler = RequestHandler::new();
    {
        let mut state = handler.state.lock().await;
        state
            .sessions
            .create_session_with_base_index(
                SessionName::new("idle").unwrap(),
                TerminalSize { cols: 80, rows: 24 },
                0,
            )
            .unwrap();
        state
            .options
            .set(
                ScopeSelector::Global,
                OptionName::ExitUnattached,
                "on".into(),
                SetOptionMode::Replace,
            )
            .unwrap();
        state
            .options
            .set(
                ScopeSelector::Global,
                OptionName::ExitEmpty,
                "off".into(),
                SetOptionMode::Replace,
            )
            .unwrap();
    }
    assert!(!handler.queue_shutdown_if_server_empty().await);
    {
        let mut state = handler.state.lock().await;
        state
            .options
            .set(
                ScopeSelector::Global,
                OptionName::ExitEmpty,
                "on".into(),
                SetOptionMode::Replace,
            )
            .unwrap();
    }
    assert!(handler.queue_shutdown_if_server_empty().await);
    assert_eq!(
        handler.pending_idle_shutdown_state(PendingShutdownReason::ExitUnattached, None),
        IdleShutdownState::StillApplies
    );
    {
        let mut state = handler.state.lock().await;
        state
            .options
            .set(
                ScopeSelector::Global,
                OptionName::ExitUnattached,
                "off".into(),
                SetOptionMode::Replace,
            )
            .unwrap();
    }
    assert_eq!(
        handler.pending_idle_shutdown_state(PendingShutdownReason::ExitUnattached, None),
        IdleShutdownState::Stale
    );
    assert!(!handler.request_shutdown_if_pending());
    assert!(!handler.shutdown_requested.load(Ordering::SeqCst));
}

/// R3135: a ShutdownIfIdle already answered `shutdown: true` must keep its
/// non-early release; the upgrade launcher relaunches without the startup lock.
#[cfg(unix)]
#[tokio::test]
async fn r3135_answered_upgrade_shutdown_keeps_its_endpoint_until_the_drain() {
    let handler = RequestHandler::new();
    let (shutdown_handle, shutdown_rx) = ShutdownHandle::new();
    handler.install_shutdown_handle(shutdown_handle.clone());

    let requester_connection_id = 7;
    let requester_connection = handler.begin_detached_connection(requester_connection_id);
    let response = handler
        .handle_shutdown_if_idle(requester_connection_id)
        .await;
    assert!(
        matches!(response, rmux_proto::Response::ShutdownIfIdle(ref r) if r.shutdown),
        "the upgrade client is told the daemon shuts down: {response:?}"
    );
    // The first evaluation is deferred (busy registry), as on a loaded daemon.
    let busy = handler
        .active_detached_connections
        .lock()
        .expect("active detached connection mutex must not be poisoned");
    assert!(!handler
        .request_shutdown_if_pending_excluding_detached_connection(Some(requester_connection_id)));
    drop(busy);
    // The upgrade client drops its connection; a session-less lifecycle path
    // (pane reap, control client finish) re-queues the idle exit meanwhile.
    drop(requester_connection);
    let _ = handler.request_shutdown_if_server_empty().await;

    tokio::time::timeout(std::time::Duration::from_millis(500), shutdown_rx)
        .await
        .expect("shutdown requested")
        .expect("shutdown receiver should complete cleanly");
    assert!(
        !shutdown_handle.releases_endpoint_early(),
        "an answered seamless upgrade was turned into an early endpoint release"
    );
}

/// An exit-empty queued after an answered upgrade keeps its own retry policy:
/// another client's open connection defers it instead of cancelling it, and
/// the shutdown that follows still keeps the endpoint until the drain.
#[cfg(unix)]
#[tokio::test]
async fn exit_empty_after_an_answered_upgrade_survives_another_open_connection() {
    let handler = RequestHandler::new();
    let (shutdown_handle, shutdown_rx) = ShutdownHandle::new();
    handler.install_shutdown_handle(shutdown_handle.clone());

    let requester_connection_id = 7;
    let requester_connection = handler.begin_detached_connection(requester_connection_id);
    let response = handler
        .handle_shutdown_if_idle(requester_connection_id)
        .await;
    assert!(
        matches!(response, rmux_proto::Response::ShutdownIfIdle(ref r) if r.shutdown),
        "the upgrade client is told the daemon shuts down: {response:?}"
    );
    let busy = handler
        .active_detached_connections
        .lock()
        .expect("active detached connection mutex must not be poisoned");
    assert!(!handler
        .request_shutdown_if_pending_excluding_detached_connection(Some(requester_connection_id)));
    drop(busy);
    // Another client connects before the upgrade client leaves; a session-less
    // lifecycle path then queues exit-empty while that connection is open.
    let other_connection = handler.begin_detached_connection(8);
    drop(requester_connection);
    assert!(!handler.request_shutdown_if_server_empty().await);
    drop(other_connection);

    tokio::time::timeout(SHUTDOWN_RETRY_DELAY * 6, shutdown_rx)
        .await
        .expect("exit-empty still shuts the empty daemon down once the other client leaves")
        .expect("shutdown receiver should complete cleanly");
    assert!(
        !shutdown_handle.releases_endpoint_early(),
        "an answered seamless upgrade was turned into an early endpoint release"
    );
}
