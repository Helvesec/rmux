use super::*;
use rmux_proto::{DisplayPanesRequest, NewSessionRequest, SessionName, TerminalSize};

#[tokio::test]
async fn parsed_display_panes_client_constructs_its_exact_session_before_dispatch() {
    let handler = RequestHandler::new();
    let alpha = SessionName::new("parsed-client-alpha").unwrap();
    let beta = SessionName::new("parsed-client-beta").unwrap();
    let mut receivers = Vec::new();
    for (pid, session) in [(101, alpha.clone()), (202, beta.clone())] {
        crate::session_fixture::assert_new_session(
            &handler
                .handle(Request::NewSession(NewSessionRequest {
                    session_name: session.clone(),
                    detached: true,
                    size: Some(TerminalSize { cols: 80, rows: 24 }),
                    environment: None,
                }))
                .await,
        );
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        handler.register_attach(pid, session, tx).await;
        receivers.push(rx);
    }
    let mut invocation =
        QueueInvocation::Request(Request::DisplayPanes(Box::new(DisplayPanesRequest {
            target: beta,
            duration_ms: Some(60_000),
            non_blocking: true,
            no_command: true,
            template: None,
            target_client: Some("101".to_owned()),
        })));
    let captured = handler
        .capture_queued_display_target_client(202, &mut invocation)
        .await;
    let QueueInvocation::Request(Request::DisplayPanes(request)) = invocation else {
        panic!("display-panes request retained")
    };
    assert_eq!(
        request.target, alpha,
        "parsed -t must construct the client's session"
    );
    assert!(
        matches!(captured, Some(QueuedDisplayTargetClient::Attached(identity))
        if identity.attach_pid() == 101)
    );
    assert!(
        handler
            .active_attach
            .lock()
            .await
            .by_pid
            .values()
            .all(|client| client.display_panes.is_none()),
        "adapter must not arm an overlay"
    );
}
