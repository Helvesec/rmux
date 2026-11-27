use std::error::Error;

use rmux_proto::{
    encode_frame, ErrorResponse, FrameDecoder, Request, Response, RmuxError as Proto,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::transport::TransportClient;
use crate::{
    protocol_diagnostic, DiagnosticSeverity, PaneId, ProtocolError, RmuxError, SessionName,
};

// Text and classification characterized on 3a848032, before the public cutoff.
fn protocol_cases() -> Vec<(Proto, &'static str)> {
    vec![
        (
            Proto::EmptySessionName,
            "invalid session name: session names must be non-empty",
        ),
        (
            Proto::InvalidSessionNameCharacter,
            "invalid session name: session names must not contain ':' or '.'",
        ),
        (
            Proto::invalid_target("alpha:2", "missing slot"),
            "invalid target 'alpha:2': missing slot",
        ),
        (
            Proto::DuplicateSession("alpha".into()),
            "duplicate session: alpha",
        ),
        (
            Proto::SessionNotFound("alpha".into()),
            "session not found: alpha",
        ),
        (Proto::Server("failure".into()), "server error: failure"),
        (Proto::Message("failure".into()), "failure"),
        (
            Proto::InvalidSetOption("failure".into()),
            "invalid set-option request: failure",
        ),
        (
            Proto::FrameTooLarge {
                length: 42,
                maximum: 41,
            },
            "codec frame length 42 exceeds maximum 41",
        ),
        (Proto::EmptyFrame, "codec frame payloads must not be empty"),
        (
            Proto::BadFrameMagic(17),
            "bad RMUX frame magic: expected 0x52, got 0x11",
        ),
        (
            Proto::IncompleteFrame {
                expected: 8,
                received: 3,
            },
            "incomplete frame: expected 8 payload bytes, received 3",
        ),
        (
            Proto::Encode("failure".into()),
            "failed to encode frame payload: failure",
        ),
        (
            Proto::Decode("failure".into()),
            "failed to decode frame payload: failure",
        ),
    ]
}

async fn request_error(
    client: &TransportClient,
    peer: &mut tokio::io::DuplexStream,
    error: Proto,
) -> RmuxError {
    let request = Request::HasSession(rmux_proto::HasSessionRequest {
        target: SessionName::new("alpha").unwrap(),
    });
    let expected = request.clone();
    let server = async {
        let mut decoder = FrameDecoder::new();
        let mut bytes = [0; 256];
        loop {
            if let Some(request) = decoder.next_frame::<Request>().unwrap() {
                assert_eq!(request, expected);
                break;
            }
            let read = peer.read(&mut bytes).await.unwrap();
            assert_ne!(read, 0);
            decoder.push_bytes(&bytes[..read]);
        }
        peer.write_all(&encode_frame(&Response::Error(ErrorResponse { error })).unwrap())
            .await
            .unwrap();
    };
    let (result, ()) = tokio::join!(client.request(request), server);
    result.unwrap_err()
}

#[tokio::test]
async fn daemon_protocol_diagnostics_preserve_fields_display_debug_and_sdk_source_chain() {
    let (stream, mut peer) = tokio::io::duplex(4096);
    let client = TransportClient::spawn(stream);
    for (raw, message) in protocol_cases() {
        let error = request_error(&client, &mut peer, raw.clone()).await;
        assert_eq!(error.protocol_source(), Some(&raw));
        assert_eq!(
            error.to_string(),
            format!(
                "rmux protocol error: {message}\nhint: {}",
                super::PROTOCOL_HINT
            )
        );
        assert_eq!(
            format!("{error:?}"),
            format!("Protocol {{ source: {raw:?} }}")
        );
        let source = error.source().unwrap();
        assert!(source.is::<ProtocolError>());
        assert!(!source.is::<Proto>());
        assert_eq!(source.to_string(), message);
        assert!(source.source().is_none());
        let diagnostic = protocol_diagnostic(&error);
        assert_eq!(diagnostic.severity(), DiagnosticSeverity::Error);
        assert_eq!(diagnostic.feature(), None);
        assert_eq!(diagnostic.hint(), Some(super::PROTOCOL_HINT));
        assert_eq!(
            diagnostic.message(),
            format!("rmux protocol error: {message}")
        );
        let RmuxError::Protocol { source } = error else {
            panic!("protocol")
        };
        assert_eq!(
            source.duplicate_session(),
            match &raw {
                Proto::DuplicateSession(name) => Some(name.as_str()),
                _ => None,
            }
        );
        assert_eq!(
            source.session_not_found(),
            match &raw {
                Proto::SessionNotFound(name) => Some(name.as_str()),
                _ => None,
            }
        );
        assert_eq!(
            source.invalid_target(),
            match &raw {
                Proto::InvalidTarget { value, reason } => Some((value.as_str(), reason.as_str())),
                _ => None,
            }
        );
        assert_eq!(
            source.server_message(),
            match &raw {
                Proto::Server(message) => Some(message.as_str()),
                _ => None,
            }
        );
    }
}

#[tokio::test]
async fn daemon_typed_outcomes_and_capability_guidance_do_not_depend_on_text() {
    let (stream, mut peer) = tokio::io::duplex(4096);
    let client = TransportClient::spawn(stream);
    let session = SessionName::new("alpha").unwrap();
    let missing = request_error(
        &client,
        &mut peer,
        Proto::pane_not_found(session.clone(), PaneId::new(7)),
    )
    .await;
    assert!(
        matches!(&missing, RmuxError::PaneNotFound { session_name, pane_id } if session_name == &session && *pane_id == PaneId::new(7))
    );
    assert!(missing.source().is_none());
    let running = request_error(&client, &mut peer, Proto::ProcessStillRunning).await;
    assert!(
        matches!(&running, RmuxError::ProcessStillRunning { message } if message == "pane still active; use -k to force respawn")
    );
    let spawn = request_error(
        &client,
        &mut peer,
        Proto::spawn_failed("precise spawn failure"),
    )
    .await;
    assert!(
        matches!(&spawn, RmuxError::SpawnFailed { message } if message == "precise spawn failure")
    );
    let lease = request_error(&client, &mut peer, Proto::owned_session_lease_lost(session)).await;
    assert!(
        matches!(&lease, RmuxError::OwnedSessionLeaseLost { message } if message == "owned session lease lost for alpha")
    );
    for error in [running, spawn, lease] {
        assert!(error.source().is_none());
        assert_eq!(error.hint(), None);
        assert_eq!(protocol_diagnostic(&error).message(), error.to_string());
    }
    for (raw, feature, hint) in [
        (Proto::UnsupportedCapability { feature: "capability.future".into(), supported: vec!["capability.current".into()] }, "capability.future", "connect to an rmux daemon that advertises capability `capability.future`; supported capabilities: capability.current"),
        (Proto::UnsupportedWireVersion { got: 99, minimum: 1, maximum: 9 }, "protocol.wire_version", "upgrade the rmux daemon or use an SDK that supports wire version 99 (supported range 1..=9)"),
        (Proto::UnknownCommand("display-menu!".into()), "command.display-menu_", "upgrade the rmux daemon or use a command advertised by the negotiated command inventory before sending `display-menu!`"),
    ] {
        let error = request_error(&client, &mut peer, raw).await;
        assert_eq!(error.feature(), Some(feature));
        assert_eq!(error.hint(), Some(hint));
        assert_eq!(error.to_string(), format!("unsupported feature `{feature}`\nhint: {hint}"));
        assert!(error.source().is_none());
        let diagnostic = protocol_diagnostic(&error);
        assert_eq!(diagnostic.severity(), DiagnosticSeverity::Unsupported);
        assert_eq!(diagnostic.feature(), Some(feature));
        assert_eq!(diagnostic.hint(), Some(hint));
    }
    for text in [
        "session not found: alpha",
        "duplicate session: alpha",
        "owned session lease lost for alpha",
    ] {
        let error = request_error(&client, &mut peer, Proto::Server(text.into())).await;
        assert!(matches!(error.protocol_source(), Some(Proto::Server(message)) if message == text));
    }
}
