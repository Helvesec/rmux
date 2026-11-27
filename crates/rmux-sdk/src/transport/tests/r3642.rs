use super::*;

#[test]
fn r3642_admission_race_has_exactly_one_winner() {
    use crate::transport::admission::Admission;
    for _ in 0..1000 {
        let admission = Admission::default();
        let barrier = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            let actor = scope.spawn(|| {
                barrier.wait();
                admission.admit()
            });
            barrier.wait();
            let withdrawn = admission.withdraw();
            assert_ne!(actor.join().unwrap(), withdrawn);
            assert!(!admission.admit());
            assert!(!admission.withdraw());
        });
    }
}

#[tokio::test(start_paused = true)]
async fn r3642_dropped_plain_buffered_command_is_written_and_drained() {
    let StalledTransport {
        client,
        mut peer_stream,
        callers,
    } = stall_callers(65).await;
    let dropped = spawn_request(&client, numbered_list_sessions_request(1000));
    tokio::time::sleep(Duration::from_millis(1)).await;
    assert_eq!(client.commands.capacity(), 62);
    dropped.abort();
    assert!(dropped.await.unwrap_err().is_cancelled());
    let peer = tokio::spawn(async move {
        let mut stall = [0; STALL_BYTES];
        peer_stream.read_exact(&mut stall).await.unwrap();
        let mut decoder = rmux_proto::FrameDecoder::new();
        let mut buffer = [0; 256];
        for index in 0..67 {
            let request = loop {
                if let Some(request) = decoder.next_frame::<Request>().unwrap() {
                    break request;
                }
                let read = peer_stream.read(&mut buffer).await.unwrap();
                assert_ne!(read, 0);
                decoder.push_bytes(&buffer[..read]);
            };
            let Request::ListSessions(ListSessionsRequest {
                format: Some(id), ..
            }) = request
            else {
                panic!("unexpected request");
            };
            if index == 65 {
                assert_eq!(id, "1000", "dropped buffered call must still be written");
            }
            if index == 66 {
                assert_eq!(id, "1001");
            }
            write_response(
                &mut peer_stream,
                &list_sessions_response(&numbered_list_sessions_stdout(&id)),
            )
            .await;
        }
    });
    for (index, caller) in callers {
        assert_eq!(
            join_request(caller).await.unwrap(),
            list_sessions_response(&numbered_list_sessions_stdout(&index.to_string()))
        );
    }
    assert_eq!(
        client
            .request(numbered_list_sessions_request(1001))
            .await
            .unwrap(),
        list_sessions_response(&numbered_list_sessions_stdout("1001"))
    );
    peer.await.unwrap();
    client.abort();
}

#[tokio::test(start_paused = true)]
async fn r3642_expired_operation_before_admission_preserves_handle() {
    for armed in [false, true] {
        let StalledTransport {
            client,
            mut peer_stream,
            callers,
        } = stall_callers(65).await;
        let deadline = OperationDeadline::from_timeout(Some(Duration::from_millis(10)));
        tokio::time::advance(Duration::from_millis(20)).await;
        let expired = client.with_operation_deadline(deadline);
        if armed {
            expect_timed_out(
                expired
                    .armed_request(sdk_wait_request(SdkWaitId::new(99)))
                    .await,
            )
            .unwrap();
        } else {
            expect_timed_out(expired.request(has_session_request()).await).unwrap();
        }
        assert!(client.state.terminal_failure().is_none());
        let peer = tokio::spawn(async move {
            let mut stall = [0; STALL_BYTES];
            peer_stream.read_exact(&mut stall).await.unwrap();
            serve_requests_serially(peer_stream).await;
        });
        for (_, caller) in callers {
            assert!(join_request(caller).await.is_ok());
        }
        assert!(client
            .request(numbered_list_sessions_request(1001))
            .await
            .is_ok());
        client.abort();
        peer.abort();
        let _ = peer.await;
    }
}

#[tokio::test(start_paused = true)]
async fn r3642_admitted_but_unwritten_still_aborts() {
    for mode in 0..3 {
        let (mut stream, _peer) = tokio::io::duplex(STALL_BYTES);
        stream.write_all(&[0; STALL_BYTES]).await.unwrap();
        let client = TransportClient::spawn(stream);
        let callers: Vec<_> = (0..63)
            .map(|i| spawn_request(&client, numbered_list_sessions_request(i)))
            .collect();
        tokio::time::sleep(Duration::from_millis(1)).await;
        assert_eq!(client.commands.capacity(), 64);
        let short = client.with_default_timeout(Some(Duration::from_millis(100)));
        match mode {
            0 => expect_timed_out(short.request(has_session_request()).await).unwrap(),
            1 => expect_timed_out(
                short
                    .armed_request(sdk_wait_request(SdkWaitId::new(99)))
                    .await,
            )
            .unwrap(),
            _ => {
                let arm = client.clone();
                let task = tokio::spawn(async move {
                    arm.armed_request(sdk_wait_request(SdkWaitId::new(99)))
                        .await
                });
                tokio::time::sleep(Duration::from_millis(1)).await;
                assert_eq!(
                    client.commands.capacity(),
                    64,
                    "actor took the armed command"
                );
                task.abort();
                assert!(matches!(task.await, Err(error) if error.is_cancelled()));
            }
        }
        assert!(client.state.terminal_failure().is_some());
        for caller in callers {
            assert!(join_request(caller).await.is_err());
        }
        assert!(client.request(has_session_request()).await.is_err());
        client.abort();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn r3642_already_expired_idle_transport_never_poisoned() {
    let deadline = OperationDeadline::from_timeout(Some(Duration::from_millis(1)));
    tokio::time::sleep(Duration::from_millis(5)).await;
    let mut poisoned = [0; 2];
    for index in 0..4000 {
        let (stream, _peer) = tokio::io::duplex(4096);
        let client = TransportClient::spawn(stream);
        let expired = client.with_operation_deadline(deadline);
        if index % 2 == 0 {
            expect_timed_out(expired.request(has_session_request()).await).unwrap();
        } else {
            expect_timed_out(
                expired
                    .armed_request(sdk_wait_request(SdkWaitId::new(99)))
                    .await,
            )
            .unwrap();
        }
        poisoned[index % 2] += usize::from(client.state.terminal_failure().is_some());
        client.abort();
    }
    println!(
        "expired before submission: plain={}/2000 armed={}/2000 poisoned",
        poisoned[0], poisoned[1]
    );
    assert_eq!(
        poisoned,
        [0, 0],
        "deadline was already expired before command submission"
    );
}

#[tokio::test(start_paused = true)]
async fn r3642_expired_request_is_not_enqueued_and_handle_is_reusable() {
    for armed in [false, true] {
        for operation_scope in [false, true] {
            let (stream, mut peer_stream) = tokio::io::duplex(4096);
            let client = TransportClient::spawn(stream);
            let expired = if operation_scope {
                let deadline = OperationDeadline::from_timeout(Some(Duration::from_millis(1)));
                tokio::time::advance(Duration::from_millis(2)).await;
                client.with_operation_deadline(deadline)
            } else {
                client.with_default_timeout(Some(Duration::ZERO))
            };
            if armed {
                expect_timed_out(
                    expired
                        .armed_request(sdk_wait_request(SdkWaitId::new(99)))
                        .await,
                )
                .unwrap();
            } else {
                expect_timed_out(expired.request(has_session_request()).await).unwrap();
            }
            assert!(client.state.terminal_failure().is_none());
            assert_eq!(
                client.commands.capacity(),
                CAPACITY,
                "no expired command queued"
            );
            let peer = tokio::spawn(async move {
                assert_eq!(
                    read_request(&mut peer_stream).await,
                    numbered_list_sessions_request(1001)
                );
                write_response(&mut peer_stream, &list_sessions_response(b"fresh")).await;
            });
            let response = client
                .with_default_timeout(Some(Duration::from_secs(1)))
                .request(numbered_list_sessions_request(1001))
                .await
                .unwrap();
            assert_eq!(response, list_sessions_response(b"fresh"));
            peer.await.unwrap();
            client.abort();
        }
    }
}

#[tokio::test(start_paused = true)]
async fn r3642_expired_shutdown_still_closes_the_transport() {
    let deadline = OperationDeadline::from_timeout(Some(Duration::from_millis(1)));
    tokio::time::advance(Duration::from_millis(2)).await;
    let (stream, _peer) = tokio::io::duplex(4096);
    let client = TransportClient::spawn(stream);
    expect_timed_out(client.with_operation_deadline(deadline).shutdown().await).unwrap();
    assert!(client.state.terminal_failure().is_some());
    assert!(client.request(has_session_request()).await.is_err());
    client.abort();
}
