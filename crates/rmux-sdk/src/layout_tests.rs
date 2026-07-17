use std::io;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::Instant;

use crate::transport::TransportClient;
use crate::{RmuxEndpoint, RmuxError, Session, SessionName};
use rmux_proto::{
    encode_frame, CommandOutput, FrameDecoder, ListPanesResponse, Request, Response,
    CAPABILITY_HANDSHAKE, CAPABILITY_SDK_PANE_BY_ID,
};

#[tokio::test(start_paused = true)]
async fn layout_apply_shares_inventory_and_configuration_deadline() {
    let (client_stream, mut server_stream) = tokio::io::duplex(4096);
    let transport = TransportClient::spawn(client_stream);
    transport
        .cache_capabilities(vec![
            CAPABILITY_HANDSHAKE.to_owned(),
            CAPABILITY_SDK_PANE_BY_ID.to_owned(),
        ])
        .await;
    let session = Session::new(
        session_name(),
        RmuxEndpoint::Default,
        Some(Duration::from_millis(50)),
        transport,
        false,
        None,
    );
    let server = tokio::spawn(async move {
        assert!(matches!(
            read_request(&mut server_stream).await,
            Request::ListPanes(_)
        ));
        tokio::time::sleep(Duration::from_millis(35)).await;
        write_response(
            &mut server_stream,
            Response::ListPanes(ListPanesResponse {
                output: CommandOutput::from_stdout("0:0:%1:1\n"),
            }),
        )
        .await;

        assert!(matches!(
            read_request(&mut server_stream).await,
            Request::ListPanes(_)
        ));
        tokio::time::sleep(Duration::from_millis(35)).await;
        write_response(
            &mut server_stream,
            Response::ListPanes(ListPanesResponse {
                output: CommandOutput::from_stdout("0:0:%1\n"),
            }),
        )
        .await;
        std::future::pending::<()>().await;
    });

    let started = Instant::now();
    let error = session
        .layout()
        .grid(1, 1)
        .pane("root")
        .apply()
        .await
        .expect_err("root configuration must use inventory's remaining budget");

    assert_eq!(Instant::now() - started, Duration::from_millis(50));
    assert_timed_out(error);
    server.abort();
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

fn session_name() -> SessionName {
    SessionName::new("layout-deadline").expect("valid session name")
}

fn assert_timed_out(error: RmuxError) {
    match error {
        RmuxError::Transport { source, .. } => {
            assert_eq!(source.kind(), io::ErrorKind::TimedOut);
        }
        error => panic!("expected transport timeout, got {error:?}"),
    }
}
