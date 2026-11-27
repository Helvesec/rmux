use super::*;
use rmux_proto::{encode_frame, ErrorResponse, FrameDecoder, RmuxError as Proto};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn capability_fallback_requires_typed_unavailability_not_a_similar_message() {
    for (raw, unavailable) in [
        (Proto::Decode("old request decoder".into()), true),
        (Proto::UnknownCommand("handshake".into()), true),
        (
            Proto::UnsupportedCapability {
                feature: CAPABILITY_HANDSHAKE.into(),
                supported: vec![],
            },
            true,
        ),
        (Proto::Server("old request decoder".into()), false),
        (Proto::Message("unknown command: handshake".into()), false),
        (Proto::invalid_target("alpha:2", "missing slot"), false),
        (Proto::UnknownCommand("different-command".into()), false),
    ] {
        let (stream, mut peer) = tokio::io::duplex(4096);
        let client = TransportClient::spawn(stream);
        let server = async {
            let mut decoder = FrameDecoder::new();
            let mut bytes = [0; 256];
            loop {
                if let Some(request) = decoder.next_frame::<Request>().unwrap() {
                    assert!(matches!(request, Request::Handshake(_)));
                    break;
                }
                let read = peer.read(&mut bytes).await.unwrap();
                assert_ne!(read, 0);
                decoder.push_bytes(&bytes[..read]);
            }
            peer.write_all(
                &encode_frame(&Response::Error(ErrorResponse { error: raw.clone() })).unwrap(),
            )
            .await
            .unwrap();
        };
        let (result, ()) = tokio::join!(require(&client, &["capability.future"]), server);
        let error = result.unwrap_err();
        assert_eq!(
            is_unavailable(&error, "capability.future"),
            unavailable,
            "{raw:?}: {error:?}"
        );
        if unavailable {
            assert_eq!(error.feature(), Some(FEATURE_PROTOCOL_CAPABILITIES));
        } else if !matches!(raw, Proto::UnknownCommand(_)) {
            assert_eq!(error.protocol_source(), Some(&raw));
        }
    }
}
