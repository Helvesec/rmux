use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use rmux_proto::{
    encode_frame, FrameDecoder, SourceFileInputDataV1Request, SourceFileInputV1Request,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Clone, Copy, Debug)]
enum Phase {
    Offered,
    Unused,
    Read,
}

async fn cancels(phase: Phase, shutdown: bool) {
    // Exercise both a plain pull offer and the relay-capable CLI offer. A relay
    // only outlives its caller when a background consumer has been handed off.
    for retained in [false, true] {
        let (server, mut client) = rmux_ipc::LocalStream::pair().unwrap();
        let (stop, mut stopped) = watch::channel(());
        let (entered_tx, entered_rx) = oneshot::channel();
        let (release, gate) = oneshot::channel();
        let mutated = Arc::new(AtomicBool::new(false));
        let marker = Arc::clone(&mutated);
        let mut task = tokio::spawn(async move {
            let mut conn = Connection::new(server);
            let registry = super::super::source_relay::PendingRelays::default();
            let user = rmux_os::identity::UserIdentity::Uid(10);
            let dispatch = async move {
                let input = SourceStdin::from_offer().unwrap();
                let held_input = match phase {
                    Phase::Offered => Some(input),
                    Phase::Unused => {
                        drop(input);
                        None
                    }
                    Phase::Read => {
                        assert_eq!(input.read().await.unwrap(), b"payload");
                        None
                    }
                };
                let _ = entered_tx.send(());
                let _ = gate.await;
                marker.store(true, Ordering::SeqCst);
                drop(held_input);
            };
            exchange(
                &mut conn,
                dispatch,
                &mut stopped,
                retained.then_some((&registry, &user)),
            )
            .await
        });
        if matches!(phase, Phase::Read) {
            let mut decoder = FrameDecoder::new();
            let mut bytes = [0; 1024];
            let response = tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    if let Some(response) = decoder.next_frame::<Response>().unwrap() {
                        break response;
                    }
                    let count = client.read(&mut bytes).await.unwrap();
                    assert_ne!(count, 0);
                    decoder.push_bytes(&bytes[..count]);
                }
            })
            .await
            .expect("source asks for stdin");
            assert!(matches!(response, Response::SourceFileReadStdinV1(_)));
            client
                .write_all(
                    &encode_frame(&Request::SourceFileInputDataV1(
                        SourceFileInputDataV1Request {
                            data: Ok(b"payload".to_vec()),
                        },
                    ))
                    .unwrap(),
                )
                .await
                .unwrap();
        }
        tokio::time::timeout(Duration::from_secs(2), entered_rx)
            .await
            .expect("source reaches its gate")
            .unwrap();
        if shutdown {
            // The real accept loop closes the watch sender on server shutdown.
            drop(stop);
        } else {
            drop(client);
        }
        let completed = tokio::time::timeout(Duration::from_secs(1), &mut task).await;
        let _ = release.send(()); // Unblock even the uncorrected fixture.
        if completed.is_err() {
            task.await.unwrap().unwrap();
        }
        assert!(
            completed.is_ok(),
            "source must cancel: {phase:?}, shutdown={shutdown}, retained={retained}"
        );
        assert!(completed.unwrap().unwrap().is_err());
        assert!(!mutated.load(Ordering::SeqCst));
    }
}

#[tokio::test]
async fn disconnect_before_stdin_demand_cancels() {
    cancels(Phase::Offered, false).await;
}

#[tokio::test]
async fn shutdown_before_stdin_demand_cancels() {
    cancels(Phase::Offered, true).await;
}

#[tokio::test]
async fn disconnect_after_stdin_demand_cancels() {
    cancels(Phase::Read, false).await;
}

#[tokio::test]
async fn shutdown_after_stdin_demand_cancels() {
    cancels(Phase::Read, true).await;
}

#[tokio::test]
async fn disconnect_after_unused_offer_cancels() {
    cancels(Phase::Unused, false).await;
}

#[tokio::test]
async fn shutdown_after_unused_offer_cancels() {
    cancels(Phase::Unused, true).await;
}

#[test]
fn every_source_input_envelope_preserves_cancellation_policy() {
    use super::super::{request_quiesce_behavior, RequestQuiesceBehavior};
    let arguments = || vec!["source-file".into(), "config".into()];
    for offer in [
        SourceFileInputV1Request::new(arguments(), None, None),
        SourceFileInputV1Request::new(arguments(), None, Some(b"payload".to_vec())),
        SourceFileInputV1Request::on_demand(arguments(), None),
        SourceFileInputV1Request::on_demand(arguments(), None).requiring_queue_status(),
    ] {
        assert_eq!(
            request_quiesce_behavior(&Request::SourceFileInputV1(Box::new(offer))),
            RequestQuiesceBehavior::CancelSafe
        );
    }
}
