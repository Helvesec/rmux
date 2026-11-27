use rmux_proto::{NewSessionRequest, Request, SessionName, TerminalSize};

use super::{preview_lines_for_target, PaneTarget, Utf8Config};
use crate::handler::RequestHandler;

#[tokio::test]
async fn poisoned_preview_keeps_its_strict_capture_fallback() {
    let handler = RequestHandler::new();
    let session = SessionName::new("preview-poison").unwrap();
    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 20, rows: 3 }),
                environment: None,
            }))
            .await,
    );
    let state = handler.state.lock().await;
    let target = PaneTarget::new(session, 0);
    let transcript = state.transcript_handle(&target).unwrap();
    transcript.lock().append_bytes(b"PREVIEW");
    let utf8 = Utf8Config::default();
    let lines = preview_lines_for_target(&state, &target, 2, 20, &utf8);
    assert_eq!(lines.len(), 2);
    assert!(lines[0].contains("PREVIEW"));
    transcript.poison_for_test();
    assert!(preview_lines_for_target(&state, &target, 0, 20, &utf8).is_empty());
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        preview_lines_for_target(&state, &target, 2, 20, &utf8)
    }))
    .is_err());
}
