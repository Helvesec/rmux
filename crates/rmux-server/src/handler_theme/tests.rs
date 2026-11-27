use super::*;
#[cfg(feature = "web")]
use crate::outer_terminal::OuterTerminalContext;
use rmux_proto::{NewSessionRequest, PaneTarget, Request, Response, SessionName};
use tokio::sync::mpsc;

async fn fixture(name: &str) -> (RequestHandler, PaneTarget) {
    let handler = RequestHandler::new();
    let session = SessionName::new(name).unwrap();
    let response = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session.clone(),
            detached: true,
            size: None,
            environment: None,
        }))
        .await;
    crate::session_fixture::assert_new_session(&response);
    let target = PaneTarget::with_window(session, 0, 0);
    assert!(query(&handler, &target, b"\x1b[?1004h").await.is_empty());
    let focus_mode = handler
        .state
        .lock()
        .await
        .transcript_handle(&target)
        .expect("theme fixture transcript exists")
        .lock()
        .mode();
    assert_ne!(focus_mode & rmux_core::input::mode::MODE_FOCUSON, 0);
    handler.start_attached_input_capture_for_test(&target).await;
    (handler, target)
}

async fn query(handler: &RequestHandler, target: &PaneTarget, bytes: &[u8]) -> Vec<u8> {
    let pane = handler
        .state
        .lock()
        .await
        .transcript_handle(target)
        .unwrap();
    let replies = pane.lock().append_bytes_with_effects(bytes).replies;
    replies
}

#[tokio::test]
async fn theme_suspension_resume_and_same_pid_replacement_preserve_registration_authority() {
    let (handler, target) = fixture("theme-registration").await;
    let pid = 938001;
    let (tx, _rx) = mpsc::unbounded_channel();
    handler
        .register_attach_with_client_name(
            pid,
            "/dev/theme-test".into(),
            target.session_name().clone(),
            tx,
        )
        .await;
    let original = handler.active_attach_identity(pid).await.unwrap();
    assert_eq!(
        handler
            .attached_input_target(pid)
            .await
            .expect("native registration has an attached pane target"),
        target
    );
    assert_eq!(
        handler
            .attached_input_capture_for_test(&target)
            .await
            .unwrap(),
        b"\x1b[I",
        "native registration publishes the initial pane focus-in edge"
    );
    handler.start_attached_input_capture_for_test(&target).await;
    query(&handler, &target, b"\x1b[?2031h").await;
    handler
        .handle_attached_live_input_for_test(pid, b"\x1b[?997;1n")
        .await
        .unwrap();
    assert_eq!(
        handler
            .attached_input_capture_for_test(&target)
            .await
            .unwrap(),
        b"\x1b[?997;1n"
    );
    assert_eq!(
        query(&handler, &target, b"\x1b[?996n").await,
        b"\x1b[?997;1n"
    );
    let response = handler
        .handle(Request::SuspendClient(rmux_proto::SuspendClientRequest {
            target_client: Some(pid.to_string()),
        }))
        .await;
    assert!(matches!(response, Response::SuspendClient(_)));
    assert!(query(&handler, &target, b"\x1b[?996n").await.is_empty());
    assert!(handler.handle_attached_unlock_for_identity(original).await);
    assert_eq!(
        handler
            .attached_input_capture_for_test(&target)
            .await
            .unwrap(),
        b"\x1b[?997;1n\x1b[?997;1n"
    );
    let mut pending = Vec::new();
    handler
        .handle_attached_live_input_inner_for_identity(original, &mut pending, b"\x1b[?997;")
        .await
        .unwrap();
    let (tx, _replacement_rx) = mpsc::unbounded_channel();
    handler
        .register_attach_with_client_name(
            pid,
            "/dev/theme-test".into(),
            target.session_name().clone(),
            tx,
        )
        .await;
    assert_ne!(handler.active_attach_identity(pid).await, Some(original));
    assert!(query(&handler, &target, b"\x1b[?996n").await.is_empty());
    assert!(!handler.handle_attached_unlock_for_identity(original).await);
    let _ = handler
        .handle_attached_live_input_inner_for_identity(
            original,
            &mut pending,
            b"2n\x1b]11;#ffffff\x07",
        )
        .await;
    assert!(query(&handler, &target, b"\x1b[?996n").await.is_empty());
    handler
        .handle_attached_live_input_for_test(pid, b"\x1b[?997;2n")
        .await
        .unwrap();
    assert_eq!(
        query(&handler, &target, b"\x1b[?996n").await,
        b"\x1b[?997;2n"
    );
}

#[cfg(feature = "web")]
#[tokio::test]
async fn theme_web_context_has_no_native_background_or_report_authority() {
    let (handler, target) = fixture("theme-web-authority").await;
    let pid = 938002;
    let (tx, _rx) = mpsc::unbounded_channel();
    handler
        .register_attach_with_terminal_context(
            pid,
            target.session_name().clone(),
            tx,
            OuterTerminalContext::default().without_theme_reports(),
        )
        .await;
    handler
        .handle_attached_live_input_for_test(pid, b"\x1b[?997;2n\x1b]11;#ffffff\x07")
        .await
        .unwrap();
    assert!(query(&handler, &target, b"\x1b[?996n").await.is_empty());
    assert_eq!(
        query(&handler, &target, b"\x1b]11;#000000\x07\x1b[?996n").await,
        b"\x1b[?997;1n"
    );
}

#[path = "sixel_tests.rs"]
mod sixel_tests;
