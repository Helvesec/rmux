use super::*;
use crate::web::{OutboundQueueResult, WebSocketOutbound};

#[tokio::test]
async fn web_blocked_reader_keeps_quota_without_blocking_global_state_or_independent_attach() {
    for snapshot in [false, true] {
        let handler = RequestHandler::new();
        let session = new_session(&handler, "web-blocked-reader").await;
        let independent = new_session(&handler, "web-healthy-attach").await;
        let target = {
            let state = handler.state.lock().await;
            WebSessionTarget::new(
                session.clone(),
                state.sessions.session(&session).unwrap().id(),
            )
        };
        let pid = std::process::id();
        let (control_tx, _control_rx) = tokio::sync::mpsc::unbounded_channel();
        handler
            .register_attach(pid, independent.clone(), control_tx)
            .await;
        let pane = PaneTarget::new(independent.clone(), 0);
        handler
            .state
            .lock()
            .await
            .start_pane_input_capture_for_test(&pane);
        let (slow, _unread_peer) = WebSocketOutbound::blocked_socket_for_test(snapshot).await;

        let state = handler.state.clone();
        let expected = independent.clone();
        WebSceneProjection::observe_next_render(move || {
            assert!(state
                .try_lock()
                .expect("Web renders outside the global lock")
                .sessions
                .session(&expected)
                .is_some());
        });
        timeout(Duration::from_millis(500), async {
            handler.web_session_snapshot(&target).await.unwrap();
            handler
                .handle_attached_live_input_for_test(pid, b"x")
                .await
                .unwrap();
            let state = handler.state.lock().await;
            assert_eq!(
                state.pane_input_capture_for_test(&pane),
                Some(b"x".to_vec()),
                "an independent attached client progresses while the Web reader is blocked"
            );
        })
        .await
        .expect("blocked Web reader must not retain shared locks");
        assert_eq!(
            slow.queue_frame(vec![0]),
            OutboundQueueResult::Backpressure,
            "in-flight data remains charged against the published per-connection quota"
        );
        assert_eq!(
            slow.queue_snapshot(vec![0]),
            OutboundQueueResult::Backpressure
        );
        assert_eq!(
            slow.queue_keyframe(vec![vec![0]]),
            OutboundQueueResult::Backpressure
        );
    }
}
