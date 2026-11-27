mod responses;

use std::time::Duration;

use rmux_proto::{
    capabilities_for_features, encode_frame, CommandOutput, FrameDecoder, HandshakeResponse,
    Request, Response, SessionName, WebShareCreatedResponse, WebShareResponse, WebShareScope,
    WebShareStoppedResponse, RMUX_WIRE_VERSION,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::{token_from_url, WebShareHandle};
use crate::transport::{OperationDeadline, TransportClient};

#[test]
fn token_from_url_reads_current_web_share_fragment_contract() {
    let url = "https://share.rmux.io/#e=ws://127.0.0.1:9777/share&t=abc123&theme=dark";
    assert_eq!(token_from_url(url), Some("abc123"));
}

#[test]
fn token_accessor_keeps_literal_empty_and_first_fragment_value() {
    for (url, expected) in [
        ("", None),
        ("literal?t=query", None),
        ("literal#t", None),
        ("literal#t=", Some("")),
        ("literal#t=&t=second", Some("")),
        (
            "literal#x=1&t=%20+literal==&t=second",
            Some("%20+literal=="),
        ),
        ("literal#T=wrong&t=right", Some("right")),
    ] {
        assert_eq!(token_from_url(url), expected);
    }
}

#[tokio::test]
async fn returned_web_share_clears_the_creation_operation_deadline() {
    let (client_stream, mut server_stream) = tokio::io::duplex(4096);
    let transport = TransportClient::spawn(client_stream)
        .with_default_timeout(Some(Duration::from_secs(1)))
        .with_operation_deadline(OperationDeadline::from_timeout(Some(
            Duration::from_millis(10),
        )));
    let handle = WebShareHandle::new(
        transport,
        WebShareCreatedResponse {
            share_id: "share-1".to_owned(),
            scope: WebShareScope::Session(SessionName::new("alpha").expect("valid session name")),
            spectator_url: None,
            operator_url: None,
            tunnel_provider: None,
            tunnel_public_url: None,
            expires_at_unix: None,
            operator_pairing_code: None,
            spectator_pairing_code: None,
            max_spectators: None,
            max_operators: None,
            operator: false,
            spectator: true,
            controls: false,
            kill_session_on_expire: false,
            output: CommandOutput::from_stdout(Vec::new()),
        },
    );
    tokio::time::sleep(Duration::from_millis(20)).await;

    let mut stop = tokio::spawn(async move { handle.stop().await });
    let request = tokio::select! {
        result = &mut stop => panic!("returned web-share retained an expired deadline: {result:?}"),
        request = read_transport_request(&mut server_stream) => request,
    };
    let Some(request) = request else {
        panic!(
            "web-share transport closed before a request: {:?}",
            stop.await
        );
    };
    assert!(matches!(request, Request::Handshake(_)));
    write_transport_response(
        &mut server_stream,
        Response::Handshake(HandshakeResponse {
            wire_version: RMUX_WIRE_VERSION,
            capabilities: capabilities_for_features(true)
                .into_iter()
                .map(str::to_owned)
                .collect(),
        }),
    )
    .await;
    assert!(matches!(
        read_transport_request(&mut server_stream).await,
        Some(Request::WebShare(_))
    ));
    write_transport_response(
        &mut server_stream,
        Response::WebShare(Box::new(WebShareResponse::Stopped(
            WebShareStoppedResponse {
                share_id: "share-1".to_owned(),
                stopped: true,
                output: CommandOutput::from_stdout(Vec::new()),
            },
        ))),
    )
    .await;
    stop.await
        .expect("web-share request task must not panic")
        .expect("returned web-share handle starts a fresh operation");
}

async fn read_transport_request(stream: &mut tokio::io::DuplexStream) -> Option<Request> {
    let mut decoder = FrameDecoder::new();
    let mut buffer = [0_u8; 256];
    loop {
        if let Some(request) = decoder
            .next_frame::<Request>()
            .expect("request frame decodes")
        {
            return Some(request);
        }
        let read = stream.read(&mut buffer).await.expect("read request");
        if read == 0 {
            return None;
        }
        decoder.push_bytes(&buffer[..read]);
    }
}

async fn write_transport_response(stream: &mut tokio::io::DuplexStream, response: Response) {
    let frame = encode_frame(&response).expect("response encodes");
    stream.write_all(&frame).await.expect("write response");
    stream.flush().await.expect("flush response");
}
