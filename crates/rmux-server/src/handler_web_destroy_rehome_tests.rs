//! A Web session share exposes exactly the session it names. When that session
//! is destroyed the viewer must be closed, never re-homed by
//! `detach-on-destroy` onto a session nobody shared, and no explicit client
//! move may carry it there either.

use super::*;
use rmux_proto::{KillWindowRequest, SwitchClientExt3Request};

const UNSHARED_SECRET: &str = "UNSHARED-SESSION-SECRET";
const VIEWER_CLOSE_DEADLINE: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy)]
enum SharedSessionDestruction {
    KillSession,
    KillLastWindow,
}

struct SharedSessionViewer {
    handler: RequestHandler,
    shared: SessionName,
    shared_id: SessionId,
    attach_pid: u32,
    reader: WebSessionAttachReader,
    _stream: Box<WebSessionStream>,
}

/// Creates an unshared session whose pane holds a secret, then a shared
/// session with one spectator whose initial render has been drained.
async fn spectator_beside_unshared_session(detach_on_destroy: &str) -> SharedSessionViewer {
    let handler = RequestHandler::new();
    let unshared = new_session(&handler, "unshared").await;
    let shared = new_session(&handler, "shared").await;
    {
        let state = handler.state.lock().await;
        let output = state
            .pane_output_for_target(&unshared, 0, 0)
            .expect("unshared pane output");
        let transcript = state
            .transcript_handle(&PaneTarget::new(unshared.clone(), 0))
            .expect("unshared pane transcript");
        drop(state);
        crate::pane_io::publish_pane_bytes_for_test(
            &transcript,
            &output,
            format!("{UNSHARED_SECRET}\r\n").into_bytes(),
        );
    }
    handler
        .state
        .lock()
        .await
        .options
        .set(
            ScopeSelector::Global,
            OptionName::DetachOnDestroy,
            detach_on_destroy.to_owned(),
            SetOptionMode::Replace,
        )
        .expect("detach-on-destroy is a valid session option");
    let shared_id = handler
        .state
        .lock()
        .await
        .sessions
        .session(&shared)
        .expect("shared session")
        .id();

    let created = create_share(
        &handler,
        share_request(WebShareScope::Session(shared.clone())),
    )
    .await;
    let token = token_from_url(created.spectator_url.as_deref().expect("spectator URL"));
    let WebShareStream::Session(mut stream) = handler
        .open_web_share(&token, None)
        .await
        .expect("spectator opens the shared session")
    else {
        panic!("a session share opens a session stream");
    };
    let attach_pid = stream.attach_pid;
    let mut reader = stream.take_attach_reader();
    let mut initial = Vec::new();
    while let Ok(Ok(Some(event))) = timeout(Duration::from_millis(500), reader.read_event()).await {
        if let WebSessionAttachEvent::Data(frame) = event {
            initial.extend_from_slice(&frame);
        }
    }
    assert!(
        !String::from_utf8_lossy(&initial).contains(UNSHARED_SECRET),
        "the initial render must show only the shared session"
    );
    SharedSessionViewer {
        handler,
        shared,
        shared_id,
        attach_pid,
        reader,
        _stream: stream,
    }
}

impl SharedSessionViewer {
    /// The session the viewer's attach identity is registered on, if any.
    async fn registered_session_id(&self) -> Option<SessionId> {
        self.handler
            .active_attach
            .lock()
            .await
            .by_pid
            .get(&self.attach_pid)
            .map(|active| active.session_id)
    }

    async fn client_name(&self) -> String {
        self.handler
            .active_attach
            .lock()
            .await
            .by_pid
            .get(&self.attach_pid)
            .expect("viewer is attached")
            .client_name
            .clone()
    }

    async fn destroy_shared_session(&self, destruction: SharedSessionDestruction) {
        let request = match destruction {
            SharedSessionDestruction::KillSession => Request::KillSession(KillSessionRequest {
                target: self.shared.clone(),
                kill_all_except_target: false,
                clear_alerts: false,
                kill_group: false,
            }),
            SharedSessionDestruction::KillLastWindow => Request::KillWindow(KillWindowRequest {
                target: WindowTarget::with_window(self.shared.clone(), 0),
                kill_all_others: false,
            }),
        };
        let response = self.handler.handle(request).await;
        assert!(
            !matches!(response, Response::Error(_)),
            "{destruction:?} of the shared session failed: {response:?}"
        );
        assert!(
            self.handler
                .state
                .lock()
                .await
                .sessions
                .session(&self.shared)
                .is_none(),
            "{destruction:?} must destroy the shared session"
        );
    }

    /// Reads everything the viewer receives until its attach stream ends.
    async fn drain_until_closed(&mut self) -> Result<Vec<u8>, Vec<u8>> {
        let mut received = Vec::new();
        let deadline = Instant::now() + VIEWER_CLOSE_DEADLINE;
        loop {
            let Ok(event) = tokio::time::timeout_at(deadline, self.reader.read_event()).await
            else {
                return Err(received);
            };
            match event {
                Ok(Some(WebSessionAttachEvent::Data(frame))) => received.extend_from_slice(&frame),
                Ok(Some(WebSessionAttachEvent::Resize)) => {}
                Ok(None) | Err(_) => return Ok(received),
            }
        }
    }
}

#[tokio::test]
async fn destroying_a_shared_session_closes_its_web_viewer_under_every_destroy_policy() {
    let mut violations = Vec::new();
    for policy in ["off", "no-detached", "previous", "next", "on"] {
        for destruction in [
            SharedSessionDestruction::KillSession,
            SharedSessionDestruction::KillLastWindow,
        ] {
            let mut viewer = spectator_beside_unshared_session(policy).await;
            viewer.destroy_shared_session(destruction).await;

            let case = format!("detach-on-destroy {policy}, {destruction:?}");
            let registered = viewer.registered_session_id().await;
            if registered.is_some_and(|session_id| session_id != viewer.shared_id) {
                violations.push(format!("{case}: viewer re-homed onto {registered:?}"));
            }
            let (received, closed) = match viewer.drain_until_closed().await {
                Ok(received) => (received, true),
                Err(received) => (received, false),
            };
            if String::from_utf8_lossy(&received).contains(UNSHARED_SECRET) {
                violations.push(format!("{case}: viewer received the unshared session"));
            }
            if !closed {
                violations.push(format!("{case}: viewer stream stayed open"));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "a web viewer must be closed with its shared session, never re-homed:\n{}",
        violations.join("\n")
    );
}

#[tokio::test]
async fn switch_client_cannot_move_a_web_viewer_onto_an_unshared_session() {
    let mut viewer = spectator_beside_unshared_session("on").await;
    let client_name = viewer.client_name().await;

    let response = viewer
        .handler
        .handle(Request::SwitchClientExt3(Box::new(
            SwitchClientExt3Request {
                target_client: Some(client_name),
                target: Some("unshared".to_owned()),
                key_table: None,
                last_session: false,
                next_session: false,
                previous_session: false,
                toggle_read_only: false,
                sort_order: None,
                skip_environment_update: false,
                zoom: false,
            },
        )))
        .await;

    assert!(
        matches!(response, Response::Error(_)),
        "switch-client must refuse to move a web share viewer: {response:?}"
    );
    assert_eq!(
        viewer.registered_session_id().await,
        Some(viewer.shared_id),
        "the web viewer must stay registered on its shared session"
    );
    let mut received = Vec::new();
    while let Ok(Ok(Some(event))) =
        timeout(Duration::from_millis(500), viewer.reader.read_event()).await
    {
        if let WebSessionAttachEvent::Data(frame) = event {
            received.extend_from_slice(&frame);
        }
    }
    assert!(
        !String::from_utf8_lossy(&received).contains(UNSHARED_SECRET),
        "the web viewer received the screen of a session that was never shared"
    );
}
