use std::io;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::oneshot;
use tokio::time::Instant;

use super::PaneSet;
use crate::transport::TransportClient;
use crate::{Input, Pane, PaneId, PaneRef, RmuxEndpoint, RmuxError, SessionName};
use rmux_proto::{
    encode_frame, CommandOutput, ErrorResponse, FrameDecoder, KillPaneResponse, ListPanesResponse,
    ListSessionsResponse, PaneSnapshotCell, PaneSnapshotCursor, PaneSnapshotResponse, PaneTarget,
    PaneTargetRef, Request, Response, SendKeysResponse, CAPABILITY_HANDSHAKE,
    CAPABILITY_SDK_PANE_BY_ID,
};

#[tokio::test]
async fn snapshot_all_does_not_adopt_a_replacement_after_observing_a_vacant_slot() {
    let (panes, done, server) = vacant_slot_replacement_fixture(ReplacementRequest::Snapshot).await;

    let outcome = panes.snapshot_all().await;
    let _ = done.send(());
    let replacement_targeted = server.await.expect("server task");

    assert!(
        !replacement_targeted,
        "snapshot targeted the replacement pane"
    );
    assert!(outcome.is_success(), "snapshot batch: {outcome:?}");
    assert_eq!(outcome.successes()[0].pane_id(), None);
    let snapshot = outcome.successes()[0].value();
    assert_eq!((snapshot.cols, snapshot.rows, snapshot.revision), (0, 0, 0));
}

#[tokio::test]
async fn close_all_does_not_kill_a_replacement_after_observing_a_vacant_slot() {
    let (panes, done, server) = vacant_slot_replacement_fixture(ReplacementRequest::Close).await;

    let outcome = panes.close_all().await;
    let _ = done.send(());
    let replacement_targeted = server.await.expect("server task");

    assert!(!replacement_targeted, "close targeted the replacement pane");
    assert!(outcome.is_success(), "close batch: {outcome:?}");
    assert_eq!(outcome.successes()[0].pane_id(), None);
    assert!(matches!(
        outcome.successes()[0].value(),
        crate::PaneCloseOutcome::AlreadyClosed { .. }
    ));
}

#[tokio::test(start_paused = true)]
async fn visible_wait_does_not_adopt_a_replacement_after_observing_a_vacant_slot() {
    let (panes, done, server) = vacant_slot_replacement_fixture(ReplacementRequest::Snapshot).await;

    let outcome = panes
        .wait_all()
        .visible_text_contains("replacement")
        .timeout(Duration::from_millis(20))
        .poll_interval(Duration::from_millis(1))
        .await;
    let _ = done.send(());
    let replacement_targeted = server.await.expect("server task");

    assert!(!replacement_targeted, "wait targeted the replacement pane");
    let failure = &outcome.all().expect("all-panes outcome").failures()[0];
    assert!(matches!(failure.error(), RmuxError::WaitTimeout { .. }));
}

#[tokio::test]
async fn client_broadcast_does_not_write_to_a_replacement_after_observing_a_vacant_slot() {
    let (panes, done, server) = vacant_slot_replacement_fixture(ReplacementRequest::Input).await;

    let error = panes
        .broadcast(Input::Text("do not redirect"))
        .await
        .expect_err("vacant pane must fail the broadcast");
    let _ = done.send(());
    let replacement_targeted = server.await.expect("server task");

    assert!(
        !replacement_targeted,
        "broadcast targeted the replacement pane"
    );
    let RmuxError::PartialBroadcast { source } = error else {
        panic!("expected partial broadcast error, got {error:?}");
    };
    assert_eq!(source.failures().len(), 1);
    assert_eq!(source.failures()[0].pane_id(), None);
}

#[tokio::test]
async fn close_all_keeps_the_observed_pane_after_slot_replacement() {
    let (panes, server) = observed_pane_replacement_fixture(ReplacementRequest::Close).await;

    let outcome = panes.close_all().await;
    server.await.expect("server task");

    assert!(outcome.is_success(), "close batch: {outcome:?}");
    assert_eq!(outcome.successes()[0].pane_id(), Some(PaneId::new(1)));
    assert!(matches!(
        outcome.successes()[0].value(),
        crate::PaneCloseOutcome::Closed { .. }
    ));
}

#[tokio::test]
async fn client_broadcast_keeps_the_observed_pane_after_slot_replacement() {
    let (panes, server) = observed_pane_replacement_fixture(ReplacementRequest::Input).await;

    let outcome = panes
        .broadcast(Input::Text("original only"))
        .await
        .expect("broadcast to observed pane");
    server.await.expect("server task");

    assert_eq!(outcome.successes()[0].pane_id(), Some(PaneId::new(1)));
}

#[tokio::test]
async fn snapshot_all_preserves_identity_lookup_errors() {
    let (panes, server) = lookup_error_fixture().await;

    let outcome = panes.snapshot_all().await;
    server.await.expect("server task");

    assert_eq!(outcome.failures().len(), 1);
    assert_lookup_error(outcome.failures()[0].error());
}

#[tokio::test]
async fn close_all_preserves_identity_lookup_errors() {
    let (panes, server) = lookup_error_fixture().await;

    let outcome = panes.close_all().await;
    server.await.expect("server task");

    assert_eq!(outcome.failures().len(), 1);
    assert_lookup_error(outcome.failures()[0].error());
}

#[tokio::test]
async fn visible_wait_preserves_identity_lookup_errors() {
    let (panes, server) = lookup_error_fixture().await;

    let outcome = panes.wait_all().visible_text_contains("ready").await;
    server.await.expect("server task");

    let failure = &outcome.all().expect("all-panes outcome").failures()[0];
    assert_lookup_error(failure.error());
}

#[tokio::test]
async fn client_broadcast_preserves_identity_lookup_errors() {
    let (panes, server) = lookup_error_fixture().await;

    let error = panes
        .broadcast(Input::Text("hello"))
        .await
        .expect_err("lookup error must fail the broadcast");
    server.await.expect("server task");

    let RmuxError::PartialBroadcast { source } = error else {
        panic!("expected partial broadcast error, got {error:?}");
    };
    assert_eq!(source.failures().len(), 1);
    assert_lookup_error(source.failures()[0].error());
}

#[tokio::test]
async fn snapshot_all_keeps_the_observed_pane_after_slot_replacement() {
    let (panes, mut server_stream) = pane_set_fixture().await;
    let server = tokio::spawn(async move {
        serve_pane_listing(&mut server_stream, &session_name(), Some(0), "0:0:%1\n").await;
        serve_pane_listing(
            &mut server_stream,
            &session_name(),
            None,
            "0:0:%2\n0:1:%1\n",
        )
        .await;
        serve_snapshot(
            &mut server_stream,
            PaneTargetRef::by_id(session_name(), PaneId::new(1)),
            "original",
        )
        .await;
    });

    let outcome = panes.snapshot_all().await;
    server.await.expect("server task");

    assert!(outcome.is_success(), "snapshot batch: {outcome:?}");
    assert_eq!(outcome.successes()[0].pane_id(), Some(PaneId::new(1)));
    assert_eq!(outcome.successes()[0].value().visible_text(), "original");
}

#[tokio::test]
async fn snapshot_all_follows_the_observed_pane_after_inter_session_move() {
    let (panes, mut server_stream) = pane_set_fixture().await;
    let server = tokio::spawn(async move {
        serve_pane_listing(&mut server_stream, &session_name(), Some(0), "0:0:%1\n").await;
        serve_pane_listing(&mut server_stream, &session_name(), None, "0:0:%2\n").await;
        serve_session_inventory(
            &mut server_stream,
            &format!("{}\t$1\n{}\t$2\n", session_name(), moved_session_name()),
        )
        .await;
        serve_pane_listing(&mut server_stream, &moved_session_name(), None, "2:3:%1\n").await;
        serve_snapshot(
            &mut server_stream,
            PaneTargetRef::by_id(moved_session_name(), PaneId::new(1)),
            "moved",
        )
        .await;
    });

    let outcome = panes.snapshot_all().await;
    server.await.expect("server task");

    assert!(outcome.is_success(), "snapshot batch: {outcome:?}");
    assert_eq!(outcome.successes()[0].pane_id(), Some(PaneId::new(1)));
    assert_eq!(outcome.successes()[0].value().visible_text(), "moved");
}

#[tokio::test]
async fn snapshot_all_does_not_read_a_replacement_after_the_observed_pane_closes() {
    let (panes, mut server_stream) = pane_set_fixture().await;
    let server = tokio::spawn(async move {
        serve_pane_listing(&mut server_stream, &session_name(), Some(0), "0:0:%1\n").await;
        for _ in 0..2 {
            serve_pane_listing(&mut server_stream, &session_name(), None, "0:0:%2\n").await;
            serve_session_inventory(&mut server_stream, &format!("{}\t$1\n", session_name())).await;
        }
    });

    let outcome = panes.snapshot_all().await;
    server.await.expect("server task");

    assert!(outcome.is_success(), "snapshot batch: {outcome:?}");
    assert_eq!(outcome.successes()[0].pane_id(), Some(PaneId::new(1)));
    let snapshot = outcome.successes()[0].value();
    assert_eq!((snapshot.cols, snapshot.rows, snapshot.revision), (0, 0, 0));
    assert_eq!(snapshot.visible_text(), "");
}

#[tokio::test]
async fn wait_all_keeps_the_observed_pane_after_slot_replacement() {
    assert_replacement_wait_keeps_observed_pane(false).await;
}

#[tokio::test]
async fn wait_any_keeps_the_observed_pane_after_slot_replacement() {
    assert_replacement_wait_keeps_observed_pane(true).await;
}

#[tokio::test(start_paused = true)]
async fn snapshot_all_shares_identity_and_snapshot_deadline_per_pane() {
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
    let panes = PaneSet::new([pane]);
    let server = tokio::spawn(async move {
        serve_delayed_pane_lookup(&mut server_stream).await;
        serve_delayed_pane_lookup(&mut server_stream).await;
        std::future::pending::<()>().await;
    });

    let started = Instant::now();
    let outcome = panes.snapshot_all().await;

    assert_eq!(Instant::now() - started, Duration::from_millis(50));
    assert_eq!(outcome.failures().len(), 1);
    assert_timed_out(outcome.failures()[0].error());
    server.abort();
}

async fn assert_replacement_wait_keeps_observed_pane(any: bool) {
    let (panes, mut server_stream) = pane_set_fixture().await;
    let server = tokio::spawn(async move {
        serve_pane_listing(&mut server_stream, &session_name(), Some(0), "0:0:%1\n").await;
        serve_pane_listing(
            &mut server_stream,
            &session_name(),
            None,
            "0:0:%2\n0:1:%1\n",
        )
        .await;
        serve_snapshot(
            &mut server_stream,
            PaneTargetRef::by_id(session_name(), PaneId::new(1)),
            "ready",
        )
        .await;
    });

    let outcome = if any {
        panes.wait_any().visible_text_contains("ready").await
    } else {
        panes.wait_all().visible_text_contains("ready").await
    };
    server.await.expect("server task");

    let success = if any {
        outcome
            .any()
            .expect("wait_any outcome")
            .success()
            .expect("matching pane")
    } else {
        &outcome.all().expect("wait_all outcome").successes()[0]
    };
    assert_eq!(success.pane_id(), Some(PaneId::new(1)));
    assert_eq!(success.value().visible_text(), "ready");
}

async fn pane_set_fixture() -> (PaneSet, tokio::io::DuplexStream) {
    let (client_stream, server_stream) = tokio::io::duplex(4096);
    let transport = TransportClient::spawn(client_stream);
    let pane = Pane::new(
        PaneRef::new(session_name(), 0, 0),
        RmuxEndpoint::Default,
        Some(Duration::from_secs(1)),
        transport.clone(),
    );
    transport
        .cache_capabilities(vec![
            CAPABILITY_HANDSHAKE.to_owned(),
            CAPABILITY_SDK_PANE_BY_ID.to_owned(),
        ])
        .await;
    (PaneSet::new([pane]), server_stream)
}

#[derive(Clone, Copy)]
enum ReplacementRequest {
    Snapshot,
    Close,
    Input,
}

async fn vacant_slot_replacement_fixture(
    replacement_request: ReplacementRequest,
) -> (PaneSet, oneshot::Sender<()>, tokio::task::JoinHandle<bool>) {
    let (panes, mut server_stream) = pane_set_fixture().await;
    let (done_tx, mut done_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        serve_pane_listing(&mut server_stream, &session_name(), Some(0), "").await;

        let request = tokio::select! {
            _ = &mut done_rx => return false,
            request = read_request(&mut server_stream) => request,
        };
        let Request::ListPanes(request) = request else {
            panic!("expected replacement pane lookup");
        };
        assert_eq!(request.target, session_name());
        assert_eq!(request.target_window_index, Some(0));
        write_response(
            &mut server_stream,
            Response::ListPanes(ListPanesResponse {
                output: CommandOutput::from_stdout("0:0:%2\n"),
            }),
        )
        .await;

        let replacement = PaneTargetRef::by_id(session_name(), PaneId::new(2));
        match replacement_request {
            ReplacementRequest::Snapshot => {
                let Request::PaneSnapshotRef(request) = read_request(&mut server_stream).await
                else {
                    panic!("expected replacement pane snapshot");
                };
                assert_eq!(request.target, replacement);
                write_response(
                    &mut server_stream,
                    Response::PaneSnapshot(snapshot_response("replacement")),
                )
                .await;
            }
            ReplacementRequest::Close => {
                let Request::PaneKill(request) = read_request(&mut server_stream).await else {
                    panic!("expected replacement pane close");
                };
                assert_eq!(request.target, replacement);
                write_response(
                    &mut server_stream,
                    Response::KillPane(KillPaneResponse {
                        target: PaneTarget::with_window(session_name(), 0, 0),
                        window_destroyed: false,
                    }),
                )
                .await;
            }
            ReplacementRequest::Input => {
                let Request::PaneInput(request) = read_request(&mut server_stream).await else {
                    panic!("expected replacement pane input");
                };
                assert_eq!(request.target, replacement);
                write_response(
                    &mut server_stream,
                    Response::SendKeys(SendKeysResponse { key_count: 1 }),
                )
                .await;
            }
        }
        true
    });
    (panes, done_tx, server)
}

async fn observed_pane_replacement_fixture(
    replacement_request: ReplacementRequest,
) -> (PaneSet, tokio::task::JoinHandle<()>) {
    let (panes, mut server_stream) = pane_set_fixture().await;
    let server = tokio::spawn(async move {
        serve_pane_listing(&mut server_stream, &session_name(), Some(0), "0:0:%1\n").await;
        serve_pane_listing(
            &mut server_stream,
            &session_name(),
            None,
            "0:0:%2\n0:1:%1\n",
        )
        .await;

        let observed = PaneTargetRef::by_id(session_name(), PaneId::new(1));
        match replacement_request {
            ReplacementRequest::Close => {
                let Request::PaneKill(request) = read_request(&mut server_stream).await else {
                    panic!("expected observed pane close");
                };
                assert_eq!(request.target, observed);
                write_response(
                    &mut server_stream,
                    Response::KillPane(KillPaneResponse {
                        target: PaneTarget::with_window(session_name(), 0, 1),
                        window_destroyed: false,
                    }),
                )
                .await;
            }
            ReplacementRequest::Input => {
                let Request::PaneInput(request) = read_request(&mut server_stream).await else {
                    panic!("expected observed pane input");
                };
                assert_eq!(request.target, observed);
                write_response(
                    &mut server_stream,
                    Response::SendKeys(SendKeysResponse { key_count: 1 }),
                )
                .await;
            }
            ReplacementRequest::Snapshot => panic!("snapshot coverage uses dedicated tests"),
        }
    });
    (panes, server)
}

async fn lookup_error_fixture() -> (PaneSet, tokio::task::JoinHandle<()>) {
    let (panes, mut server_stream) = pane_set_fixture().await;
    let server = tokio::spawn(async move {
        assert!(matches!(
            read_request(&mut server_stream).await,
            Request::ListPanes(_)
        ));
        write_response(
            &mut server_stream,
            Response::Error(ErrorResponse {
                error: rmux_proto::RmuxError::Server("injected identity lookup failure".to_owned()),
            }),
        )
        .await;
    });
    (panes, server)
}

fn assert_lookup_error(error: &RmuxError) {
    assert!(
        matches!(
            error,
            RmuxError::Protocol {
                source: rmux_proto::RmuxError::Server(message),
            } if message == "injected identity lookup failure"
        ),
        "expected injected identity lookup failure, got {error:?}"
    );
}

async fn serve_pane_listing(
    stream: &mut tokio::io::DuplexStream,
    expected_session: &SessionName,
    expected_window_index: Option<u32>,
    output: &str,
) {
    let Request::ListPanes(request) = read_request(stream).await else {
        panic!("expected list-panes request");
    };
    assert_eq!(&request.target, expected_session);
    assert_eq!(request.target_window_index, expected_window_index);
    write_response(
        stream,
        Response::ListPanes(ListPanesResponse {
            output: CommandOutput::from_stdout(output),
        }),
    )
    .await;
}

async fn serve_session_inventory(stream: &mut tokio::io::DuplexStream, output: &str) {
    assert!(matches!(
        read_request(stream).await,
        Request::ListSessions(_)
    ));
    write_response(
        stream,
        Response::ListSessions(ListSessionsResponse {
            output: CommandOutput::from_stdout(output),
        }),
    )
    .await;
}

async fn serve_snapshot(
    stream: &mut tokio::io::DuplexStream,
    expected_target: PaneTargetRef,
    text: &str,
) {
    let Request::PaneSnapshotRef(request) = read_request(stream).await else {
        panic!("expected pane-snapshot-ref request");
    };
    assert_eq!(request.target, expected_target);
    write_response(stream, Response::PaneSnapshot(snapshot_response(text))).await;
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
    SessionName::new("pane-set-deadline").expect("valid session name")
}

fn moved_session_name() -> SessionName {
    SessionName::new("pane-set-moved").expect("valid session name")
}

fn assert_timed_out(error: &RmuxError) {
    match error {
        RmuxError::Transport { source, .. } => {
            assert_eq!(source.kind(), io::ErrorKind::TimedOut);
        }
        error => panic!("expected transport timeout, got {error:?}"),
    }
}
