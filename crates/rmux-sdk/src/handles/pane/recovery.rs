use rmux_proto::{
    PaneOutputRecoveryRequest, Request, Response, CAPABILITY_SDK_PANE_OUTPUT_RECOVERY,
};

use crate::events::streams::PaneOutputStream;
use crate::handles::session::unexpected_response;
use crate::{PaneOutputRecovery, Result, RmuxError};

use super::snapshot::snapshot_from_response;
use super::target::is_stale_pane_id_target_error;
use super::Pane;

impl Pane {
    /// Atomically captures a complete ANSI renderer keyframe and subscribes
    /// to raw pane output at the exact first sequence after that keyframe.
    pub async fn recover_output(&self) -> Result<PaneOutputRecovery> {
        let pane = self.begin_operation_handle();
        crate::capabilities::require(pane.transport(), &[CAPABILITY_SDK_PANE_OUTPUT_RECOVERY])
            .await?;
        let target = pane.required_resolved_proto_target_ref().await?;
        match recover(&pane, target.clone()).await {
            Ok(recovery) => Ok(recovery),
            Err(error) if pane.is_stable_id() && is_stale_pane_id_target_error(&error, &target) => {
                let retry_target = pane.resolved_proto_target_ref().await?.ok_or_else(|| {
                    RmuxError::protocol(rmux_proto::RmuxError::Server(
                        "pane no longer exists".to_owned(),
                    ))
                })?;
                recover(&pane, retry_target).await
            }
            Err(error) => Err(error),
        }
    }
}

async fn recover(pane: &Pane, target: rmux_proto::PaneTargetRef) -> Result<PaneOutputRecovery> {
    let requested_pane_id = target.pane_id();
    let requested_session = target.session_name().clone();
    let response = pane
        .transport()
        .request(Request::PaneOutputRecovery(PaneOutputRecoveryRequest {
            target,
        }))
        .await?;
    let response = match response {
        Response::PaneOutputRecovery(response) => *response,
        response => return Err(unexpected_response("pane-output-recovery", response)),
    };
    // The daemon registers this subscription before sending the atomic
    // response. Take ownership immediately so every subsequent validation or
    // conversion failure drops the guard and unsubscribes it.
    let output =
        PaneOutputStream::from_registered(pane.transport().clone(), response.subscription_id)?;
    if requested_pane_id.is_some_and(|pane_id| response.pane_id != pane_id)
        || response.target.session_name() != &requested_session
    {
        return Err(RmuxError::protocol(rmux_proto::RmuxError::Server(format!(
            "rmux daemon sent recovery identity {}:{} for requested {}:{}",
            response.target.session_name(),
            response.pane_id,
            requested_session,
            requested_pane_id
                .map(|pane_id| pane_id.to_string())
                .unwrap_or_else(|| "slot".to_owned()),
        ))));
    }
    if response.cursor.next_sequence != response.keyframe.next_sequence {
        return Err(RmuxError::protocol(rmux_proto::RmuxError::Server(format!(
            "rmux daemon sent recovery cursor {} for keyframe boundary {}",
            response.cursor.next_sequence, response.keyframe.next_sequence,
        ))));
    }
    if (response.snapshot.cols, response.snapshot.rows)
        != (response.keyframe.cols, response.keyframe.rows)
    {
        return Err(RmuxError::protocol(rmux_proto::RmuxError::Server(format!(
            "rmux daemon sent recovery snapshot geometry {}x{} for keyframe geometry {}x{}",
            response.snapshot.cols,
            response.snapshot.rows,
            response.keyframe.cols,
            response.keyframe.rows,
        ))));
    }
    let snapshot = snapshot_from_response(response.snapshot)?;
    Ok(PaneOutputRecovery {
        cols: response.keyframe.cols,
        rows: response.keyframe.rows,
        keyframe: response.keyframe.bytes,
        snapshot,
        alternate: response.keyframe.alternate,
        next_sequence: response.keyframe.next_sequence,
        output,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::TransportClient;
    use crate::{PaneOutputChunk, PaneRef, RmuxEndpoint};
    use rmux_proto::{
        encode_frame, FrameDecoder, HandshakeResponse, PaneId, PaneOutputCursor,
        PaneOutputCursorRequest, PaneOutputCursorResponse, PaneOutputEvent, PaneOutputKeyframe,
        PaneOutputLagNotice, PaneOutputLagResponse, PaneOutputRecoveryResponse,
        PaneOutputSubscriptionId, PaneRecentOutput, PaneSnapshotCell, PaneSnapshotCursor,
        PaneSnapshotResponse, PaneTarget, PaneTargetRef, SessionName, UnsubscribePaneOutputRequest,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

    fn target() -> PaneTarget {
        PaneTarget::new(SessionName::new("recovery").expect("session"), 0)
    }

    fn pane(transport: TransportClient) -> Pane {
        Pane::new(
            PaneRef::from(target()),
            RmuxEndpoint::Default,
            None,
            transport,
        )
    }

    fn subscription_id() -> PaneOutputSubscriptionId {
        PaneOutputSubscriptionId::new(41)
    }

    async fn read_request(stream: &mut DuplexStream) -> Request {
        let mut decoder = FrameDecoder::new();
        let mut buffer = [0_u8; 1024];
        loop {
            if let Some(request) = decoder.next_frame().expect("request decodes") {
                return request;
            }
            let read = stream.read(&mut buffer).await.expect("read request");
            assert_ne!(read, 0, "transport closed before request");
            decoder.push_bytes(&buffer[..read]);
        }
    }

    async fn write_response(stream: &mut DuplexStream, response: &Response) {
        stream
            .write_all(&encode_frame(response).expect("response encodes"))
            .await
            .expect("write response");
    }

    fn response(next_sequence: u64, cursor_sequence: u64) -> Response {
        let cells = (0..93 * 31)
            .map(|index| PaneSnapshotCell {
                text: if index == 0 { "K" } else { " " }.to_owned(),
                width: 1,
                padding: false,
                attributes: 0,
                fg: 8,
                bg: 8,
                us: 8,
                link: 0,
            })
            .collect();
        Response::PaneOutputRecovery(Box::new(PaneOutputRecoveryResponse {
            subscription_id: subscription_id(),
            target: target(),
            pane_id: PaneId::new(7),
            cursor: PaneOutputCursor {
                next_sequence: cursor_sequence,
                missed_events: 0,
            },
            snapshot: PaneSnapshotResponse {
                cols: 93,
                rows: 31,
                cells,
                cursor: PaneSnapshotCursor {
                    row: 4,
                    col: 7,
                    visible: true,
                    style: 5,
                },
                revision: 17,
            },
            keyframe: PaneOutputKeyframe {
                cols: 93,
                rows: 31,
                bytes: b"\x1b[2J\x1b[Hkeyframe\x1b[1;".to_vec(),
                alternate: true,
                next_sequence,
            },
        }))
    }

    #[tokio::test]
    async fn recovery_returns_keyframe_and_opaque_raw_stream_at_exact_boundary() {
        let (client, mut server) = tokio::io::duplex(16 * 1024);
        let pane = pane(TransportClient::spawn(client));
        let proto_target = PaneTargetRef::slot(target());
        let task = tokio::spawn(async move { recover(&pane, proto_target).await });

        let Request::PaneOutputRecovery(request) = read_request(&mut server).await else {
            panic!("expected pane-output-recovery request");
        };
        assert_eq!(request.target, PaneTargetRef::slot(target()));
        write_response(&mut server, &response(42, 42)).await;

        let mut recovery = task.await.expect("task").expect("recovery succeeds");
        assert_eq!((recovery.cols, recovery.rows), (93, 31));
        assert_eq!(recovery.next_sequence, 42);
        assert_eq!(recovery.snapshot.revision, 17);
        assert_eq!(
            (recovery.snapshot.cursor.row, recovery.snapshot.cursor.col),
            (4, 7)
        );
        assert_eq!(recovery.snapshot.cells[0].glyph.text, "K");
        assert!(recovery.alternate);
        assert!(recovery.keyframe.ends_with(b"\x1b[1;"));

        let next = tokio::spawn(async move {
            let item = recovery.output.next().await;
            (item, recovery.output)
        });
        let Request::PaneOutputCursor(PaneOutputCursorRequest {
            subscription_id: requested,
            ..
        }) = read_request(&mut server).await
        else {
            panic!("expected pane-output-cursor request");
        };
        assert_eq!(requested, subscription_id());
        let arbitrary = vec![0, 0xff, b'm', 0x80, b'X'];
        write_response(
            &mut server,
            &Response::PaneOutputCursor(PaneOutputCursorResponse {
                subscription_id: subscription_id(),
                cursor: PaneOutputCursor {
                    next_sequence: 43,
                    missed_events: 0,
                },
                events: vec![PaneOutputEvent {
                    sequence: 42,
                    bytes: arbitrary.clone(),
                }],
                limited: false,
            }),
        )
        .await;
        let (item, output) = next.await.expect("next task");
        assert_eq!(
            item.expect("poll succeeds"),
            Some(PaneOutputChunk::Bytes {
                sequence: 42,
                bytes: arbitrary,
            })
        );
        drop(output);
        let Request::UnsubscribePaneOutput(UnsubscribePaneOutputRequest {
            subscription_id: dropped,
        }) = read_request(&mut server).await
        else {
            panic!("dropping recovery stream must unsubscribe");
        };
        assert_eq!(dropped, subscription_id());
    }

    #[tokio::test]
    async fn recovery_rejects_cursor_that_does_not_match_keyframe_boundary() {
        let (client, mut server) = tokio::io::duplex(16 * 1024);
        let pane = pane(TransportClient::spawn(client));
        let task = tokio::spawn(async move { recover(&pane, PaneTargetRef::slot(target())).await });
        assert!(matches!(
            read_request(&mut server).await,
            Request::PaneOutputRecovery(_)
        ));
        write_response(&mut server, &response(42, 41)).await;
        let error = task
            .await
            .expect("task")
            .err()
            .expect("mismatch fails closed");
        assert!(error.to_string().contains("recovery cursor 41"));
        assert!(matches!(
            read_request(&mut server).await,
            Request::UnsubscribePaneOutput(UnsubscribePaneOutputRequest {
                subscription_id: dropped,
            }) if dropped == subscription_id()
        ));
    }

    #[tokio::test]
    async fn recovery_rejects_foreign_pane_and_unsubscribes_before_returning() {
        let (client, mut server) = tokio::io::duplex(16 * 1024);
        let pane = pane(TransportClient::spawn(client));
        let requested = PaneTargetRef::by_id(target().session_name().clone(), PaneId::new(7));
        let task = tokio::spawn(async move { recover(&pane, requested).await });
        assert!(matches!(
            read_request(&mut server).await,
            Request::PaneOutputRecovery(_)
        ));
        let mut foreign = response(42, 42);
        let Response::PaneOutputRecovery(recovery) = &mut foreign else {
            unreachable!("test response is pane recovery");
        };
        recovery.pane_id = PaneId::new(8);
        write_response(&mut server, &foreign).await;

        let error = task
            .await
            .expect("task")
            .err()
            .expect("foreign pane fails closed");
        assert!(error.to_string().contains("recovery identity"));
        assert!(matches!(
            read_request(&mut server).await,
            Request::UnsubscribePaneOutput(UnsubscribePaneOutputRequest {
                subscription_id: dropped,
            }) if dropped == subscription_id()
        ));
    }

    #[tokio::test]
    async fn recovery_validation_failure_unsubscribes_before_returning() {
        let (client, mut server) = tokio::io::duplex(16 * 1024);
        let pane = pane(TransportClient::spawn(client));
        let task = tokio::spawn(async move { recover(&pane, PaneTargetRef::slot(target())).await });
        assert!(matches!(
            read_request(&mut server).await,
            Request::PaneOutputRecovery(_)
        ));
        let mut malformed = response(42, 42);
        let Response::PaneOutputRecovery(recovery) = &mut malformed else {
            unreachable!("test response is pane recovery");
        };
        recovery.snapshot.cells.pop();
        write_response(&mut server, &malformed).await;

        let error = task
            .await
            .expect("task")
            .err()
            .expect("malformed snapshot fails closed");
        assert!(error.to_string().contains("malformed row-major cell shape"));
        assert!(matches!(
            read_request(&mut server).await,
            Request::UnsubscribePaneOutput(UnsubscribePaneOutputRequest {
                subscription_id: dropped,
            }) if dropped == subscription_id()
        ));
    }

    #[tokio::test]
    async fn recovered_stream_surfaces_bounded_lag_without_fabricating_bytes() {
        let (client, mut server) = tokio::io::duplex(16 * 1024);
        let pane = pane(TransportClient::spawn(client));
        let task = tokio::spawn(async move { recover(&pane, PaneTargetRef::slot(target())).await });
        assert!(matches!(
            read_request(&mut server).await,
            Request::PaneOutputRecovery(_)
        ));
        write_response(&mut server, &response(42, 42)).await;
        let mut recovery = task.await.expect("task").expect("recovery");

        let next = tokio::spawn(async move {
            let item = recovery.output.next().await;
            (item, recovery.output)
        });
        assert!(matches!(
            read_request(&mut server).await,
            Request::PaneOutputCursor(_)
        ));
        let recent = vec![0, 0xff, b'R', 0x80];
        write_response(
            &mut server,
            &Response::PaneOutputLag(Box::new(PaneOutputLagResponse {
                subscription_id: subscription_id(),
                cursor: PaneOutputCursor {
                    next_sequence: 49,
                    missed_events: 7,
                },
                lag: PaneOutputLagNotice {
                    expected_sequence: 42,
                    resume_sequence: 49,
                    missed_events: 7,
                    newest_sequence: 51,
                    recent: PaneRecentOutput {
                        bytes: recent.clone(),
                        oldest_sequence: Some(49),
                        newest_sequence: Some(51),
                    },
                },
            })),
        )
        .await;
        let (item, output) = next.await.expect("next task");
        let Some(PaneOutputChunk::Lag(lag)) = item.expect("poll succeeds") else {
            panic!("recovered stream must surface a typed lag notice");
        };
        assert_eq!(lag.expected_sequence, 42);
        assert_eq!(lag.resume_sequence, 49);
        assert_eq!(lag.missed_events, 7);
        assert_eq!(lag.recent.bytes, recent);
        drop(output);
        assert!(matches!(
            read_request(&mut server).await,
            Request::UnsubscribePaneOutput(_)
        ));
    }

    #[tokio::test]
    async fn public_recovery_requires_the_atomic_capability_before_resolution() {
        let (client, mut server) = tokio::io::duplex(16 * 1024);
        let pane = pane(TransportClient::spawn(client));
        let task = tokio::spawn(async move { pane.recover_output().await });
        assert!(matches!(
            read_request(&mut server).await,
            Request::Handshake(_)
        ));
        write_response(
            &mut server,
            &Response::Handshake(HandshakeResponse {
                wire_version: rmux_proto::RMUX_WIRE_VERSION,
                capabilities: vec![rmux_proto::CAPABILITY_HANDSHAKE.to_owned()],
            }),
        )
        .await;
        let error = task
            .await
            .expect("task")
            .err()
            .expect("missing capability fails");
        assert!(error
            .to_string()
            .contains(CAPABILITY_SDK_PANE_OUTPUT_RECOVERY));
    }
}
