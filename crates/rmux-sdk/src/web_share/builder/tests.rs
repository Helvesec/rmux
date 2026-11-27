mod identity;
mod theme;

use super::{system_time_to_unix, whole_seconds_ceil, WebShareBuilder};
use crate::transport::TransportClient;
use crate::{Pane, PaneRef, RmuxEndpoint};
use rmux_proto::{
    encode_frame, CommandOutput, ErrorResponse, FrameDecoder, HandshakeResponse, ListPanesResponse,
    PaneId, PaneTargetRef, Request, Response, SessionName, WebShareRequest, WebShareScope,
};
use std::time::{Duration, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

async fn read_request(stream: &mut DuplexStream) -> Request {
    let mut decoder = FrameDecoder::new();
    let mut buffer = [0_u8; 1024];
    loop {
        if let Some(request) = decoder
            .next_frame::<Request>()
            .expect("request frame decodes")
        {
            return request;
        }
        let read = stream.read(&mut buffer).await.expect("read request");
        assert_ne!(read, 0, "client closed before request");
        decoder.push_bytes(&buffer[..read]);
    }
}

async fn write_response(stream: &mut DuplexStream, response: Response) {
    let frame = encode_frame(&response).expect("response encodes");
    stream.write_all(&frame).await.expect("write response");
    stream.flush().await.expect("flush response");
}

#[test]
fn ttl_ceil_rejects_only_explicit_zero_later() {
    assert_eq!(whole_seconds_ceil(Duration::ZERO), 0);
    assert_eq!(whole_seconds_ceil(Duration::from_millis(1)), 1);
    assert_eq!(whole_seconds_ceil(Duration::from_secs(3)), 3);
    assert_eq!(whole_seconds_ceil(Duration::new(3, 1)), 4);
}

#[test]
fn system_time_to_unix_returns_seconds() {
    assert_eq!(
        system_time_to_unix(UNIX_EPOCH + Duration::from_secs(42)).expect("valid deadline"),
        42
    );
}

#[test]
fn system_time_to_unix_rejects_pre_epoch_deadlines() {
    let error = system_time_to_unix(UNIX_EPOCH - Duration::from_secs(1))
        .expect_err("pre-epoch deadline must be rejected locally");
    assert!(error
        .to_string()
        .contains("web-share expiration must not be before the Unix epoch"));
}

#[tokio::test]
async fn positive_compat_aliases_restore_default_web_share_choices() {
    let (client, _server) = tokio::io::duplex(64);
    let transport = TransportClient::spawn(client);
    let scope = WebShareScope::Session(SessionName::new("alpha").expect("valid session"));
    let builder = WebShareBuilder::new(&transport, scope)
        .hide_viewers()
        .show_viewer_count()
        .no_pin()
        .pairing_code();

    assert!(builder.url_options.show_viewers);
    assert!(builder.require_pin);
}

#[tokio::test]
async fn pane_share_resolves_visible_slot_to_stable_id_before_create() {
    let alpha = SessionName::new("alpha").expect("valid session");
    let (client, mut server) = tokio::io::duplex(8192);
    let transport = TransportClient::spawn(client);
    let pane = Pane::new(
        PaneRef::new(alpha.clone(), 1, 3),
        RmuxEndpoint::Default,
        None,
        transport,
    );

    let share = tokio::spawn(async move { pane.share().no_pin().await });

    assert!(matches!(
        read_request(&mut server).await,
        Request::Handshake(_)
    ));
    let mut handshake = HandshakeResponse::current();
    if !handshake
        .capabilities
        .iter()
        .any(|capability| capability == rmux_proto::CAPABILITY_WEB_SHARE)
    {
        handshake
            .capabilities
            .push(rmux_proto::CAPABILITY_WEB_SHARE.to_owned());
    }
    write_response(&mut server, Response::Handshake(handshake)).await;

    match read_request(&mut server).await {
        Request::ListPanes(request) => {
            assert_eq!(request.target, alpha);
            assert_eq!(request.target_window_index, Some(1));
        }
        request => panic!("expected visible slot resolution, got {request:?}"),
    }
    write_response(
        &mut server,
        Response::ListPanes(ListPanesResponse {
            output: CommandOutput::from_stdout("1:3:%7\n"),
        }),
    )
    .await;

    match read_request(&mut server).await {
        Request::WebShare(request) => match *request {
            WebShareRequest::Create(request) => assert_eq!(
                request.scope,
                WebShareScope::Pane(PaneTargetRef::by_id(
                    SessionName::new("alpha").expect("valid session"),
                    PaneId::new(7),
                ))
            ),
            request => panic!("expected web-share create, got {request:?}"),
        },
        request => panic!("expected web-share request, got {request:?}"),
    }
    write_response(
        &mut server,
        Response::Error(ErrorResponse {
            error: rmux_proto::RmuxError::Server("stop after target assertion".to_owned()),
        }),
    )
    .await;

    let result = share.await.expect("share task completes");
    let Err(error) = result else {
        panic!("stub server must stop the share after observing the request");
    };
    assert!(error.to_string().contains("stop after target assertion"));
}
