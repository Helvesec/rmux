use std::io;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::Instant;

use crate::transport::TransportClient;
use crate::{Pane, PaneRef, RmuxEndpoint, RmuxError, SessionName};
use rmux_proto::{
    encode_frame, CommandOutput, FrameDecoder, ListPanesResponse, PaneSnapshotCell,
    PaneSnapshotCursor, PaneSnapshotResponse, Request, Response, CAPABILITY_HANDSHAKE,
    CAPABILITY_SDK_PANE_BY_ID,
};

#[tokio::test(start_paused = true)]
async fn locator_click_shares_resolution_and_action_deadline() {
    let (client_stream, mut server_stream) = tokio::io::duplex(4096);
    let transport = TransportClient::spawn(client_stream);
    transport
        .cache_capabilities(vec![
            CAPABILITY_HANDSHAKE.to_owned(),
            CAPABILITY_SDK_PANE_BY_ID.to_owned(),
        ])
        .await;
    let pane = Pane::new(
        PaneRef::new(session_name(), 0, 0),
        RmuxEndpoint::Default,
        Some(Duration::from_millis(50)),
        transport,
    );
    let server = tokio::spawn(async move {
        serve_delayed_pane_lookup(&mut server_stream).await;
        assert!(matches!(
            read_request(&mut server_stream).await,
            Request::PaneSnapshotRef(_)
        ));
        write_response(
            &mut server_stream,
            Response::PaneSnapshot(snapshot_response("x")),
        )
        .await;
        serve_delayed_pane_lookup(&mut server_stream).await;
        std::future::pending::<()>().await;
    });

    let started = Instant::now();
    let error = pane
        .get_by_text("x")
        .click()
        .await
        .expect_err("mouse action must use locator resolution's remaining budget");

    assert_eq!(Instant::now() - started, Duration::from_millis(50));
    assert_timed_out(error);
    server.abort();
}

async fn serve_delayed_pane_lookup(stream: &mut tokio::io::DuplexStream) {
    assert!(matches!(read_request(stream).await, Request::ListPanes(_)));
    tokio::time::sleep(Duration::from_millis(35)).await;
    write_response(
        stream,
        Response::ListPanes(ListPanesResponse {
            output: CommandOutput::from_stdout("0:0:%1\n"),
        }),
    )
    .await;
}

fn snapshot_response(text: &str) -> PaneSnapshotResponse {
    PaneSnapshotResponse {
        cols: text.len() as u16,
        rows: 1,
        cells: text
            .chars()
            .map(|character| PaneSnapshotCell {
                text: character.to_string(),
                width: 1,
                padding: false,
                attributes: 0,
                fg: 0,
                bg: 0,
                us: 0,
                link: 0,
            })
            .collect(),
        cursor: PaneSnapshotCursor {
            row: 0,
            col: 0,
            visible: true,
            style: 0,
        },
        revision: 1,
    }
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
    SessionName::new("locator-deadline").expect("valid session name")
}

fn assert_timed_out(error: RmuxError) {
    match error {
        RmuxError::Transport { source, .. } => {
            assert_eq!(source.kind(), io::ErrorKind::TimedOut);
        }
        error => panic!("expected transport timeout, got {error:?}"),
    }
}
