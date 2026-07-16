use std::io;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::Instant;

use super::*;
use crate::transport::TransportClient;
use crate::RmuxEndpoint;
use rmux_proto::{
    encode_frame, CommandOutput, FrameDecoder, HasSessionResponse, ListPanesResponse, Request,
    Response,
};

#[tokio::test(start_paused = true)]
async fn session_finder_shares_one_deadline_across_inventory_and_binding() {
    let (rmux, mut server_stream) = fixture_rmux();
    let server = tokio::spawn(async move {
        serve_delayed_has_session(&mut server_stream).await;
        serve_delayed_has_session(&mut server_stream).await;
    });

    let started = Instant::now();
    let error = rmux
        .find_sessions()
        .name("alpha")
        .all()
        .await
        .expect_err("session binding must use the inventory RPC's remaining budget");

    assert_eq!(Instant::now() - started, Duration::from_millis(50));
    assert_timed_out(error);
    server.abort();
}

#[tokio::test(start_paused = true)]
async fn pane_finder_shares_one_deadline_across_session_discovery() {
    let (rmux, mut server_stream) = fixture_rmux();
    let server = tokio::spawn(async move {
        serve_delayed_has_session(&mut server_stream).await;
        serve_delayed_has_session(&mut server_stream).await;
        assert!(matches!(
            read_request(&mut server_stream).await,
            Request::ListPanes(_)
        ));
        write_response(
            &mut server_stream,
            Response::ListPanes(ListPanesResponse {
                output: CommandOutput::from_stdout(Vec::<u8>::new()),
            }),
        )
        .await;
    });

    let started = Instant::now();
    let error = rmux
        .find_panes()
        .session("alpha")
        .all()
        .await
        .expect_err("pane discovery must not reset after its first session probe");

    assert_eq!(Instant::now() - started, Duration::from_millis(50));
    assert_timed_out(error);
    server.abort();
}

fn fixture_rmux() -> (Rmux, tokio::io::DuplexStream) {
    let (client_stream, server_stream) = tokio::io::duplex(4096);
    let rmux = Rmux::from_connected_transport(
        RmuxEndpoint::UnixSocket("/unused/rmux.sock".into()),
        Some(Duration::from_millis(50)),
        TransportClient::spawn(client_stream),
    );
    (rmux, server_stream)
}

async fn serve_delayed_has_session(stream: &mut tokio::io::DuplexStream) {
    assert!(matches!(read_request(stream).await, Request::HasSession(_)));
    tokio::time::sleep(Duration::from_millis(35)).await;
    write_response(
        stream,
        Response::HasSession(HasSessionResponse { exists: true }),
    )
    .await;
}

async fn read_request(stream: &mut tokio::io::DuplexStream) -> Request {
    let mut decoder = FrameDecoder::new();
    let mut buffer = [0_u8; 256];
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

async fn write_response(stream: &mut tokio::io::DuplexStream, response: Response) {
    let frame = encode_frame(&response).expect("response encodes");
    stream.write_all(&frame).await.expect("write response");
    stream.flush().await.expect("flush response");
}

fn assert_timed_out(error: RmuxError) {
    match error {
        RmuxError::Transport { source, .. } => {
            assert_eq!(source.kind(), io::ErrorKind::TimedOut);
        }
        error => panic!("expected transport timeout, got {error:?}"),
    }
}
