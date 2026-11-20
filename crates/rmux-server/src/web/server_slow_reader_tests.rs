use super::*;
use crate::listener::connection_test_support::finish_connection;
use crate::listener::connection_test_support::slow_reader_tests::{
    attach_peer, read_until, set_title_option,
};

fn bounded_socket_buffers(stream: TcpStream) -> TcpStream {
    let stream = stream.into_std().expect("native TCP stream");
    let socket = tokio::net::TcpSocket::from_std_stream(stream.try_clone().unwrap());
    socket.set_send_buffer_size(4096).unwrap();
    socket.set_recv_buffer_size(4096).unwrap();
    assert!(socket.send_buffer_size().unwrap() < 64 * 1024);
    assert!(socket.recv_buffer_size().unwrap() < 64 * 1024);
    drop(socket);
    TcpStream::from_std(stream).expect("restore async TCP stream")
}

async fn bounded_websocket(handler: Arc<RequestHandler>, token: &str) -> TestWebSocket {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (client, server) = tokio::join!(
        TcpStream::connect(listener.local_addr().unwrap()),
        listener.accept(),
    );
    let client = bounded_socket_buffers(client.unwrap());
    let server = bounded_socket_buffers(server.unwrap().0);
    let pre_auth = PreAuthQueue::new(1).try_register().expect("pre-auth slot");
    let shutdown = handler.normal_request_shutdown_receiver();
    let admission = handler
        .try_begin_normal_request(false)
        .expect("web admission");
    let task = tokio::spawn(serve_admitted_connection(
        server, handler, pre_auth, shutdown, admission,
    ));
    let (stream, task) = upgrade_test_websocket(client, task).await;
    let authenticated = authenticate_test_websocket(
        stream,
        task,
        token,
        &SecretHashForCrypto::from_secret(token).token_id(),
        &auth_text_with_pane_recovery_coverage(),
    )
    .await;
    TestWebSocket {
        stream: authenticated.stream,
        task: authenticated.task,
        opener: authenticated.opener,
        sealer: authenticated.sealer,
    }
}

#[tokio::test]
async fn slow_reader_isolation_web_writer_keeps_a_native_attached_client_live() {
    let handler = Arc::new(RequestHandler::new());
    let session = create_quiet_session(&handler, "web-isolation-live").await;
    let web_session = create_quiet_session(&handler, "web-isolation-slow").await;
    let target = PaneTarget::new(session.clone(), 0);
    let web_target = PaneTarget::new(web_session.clone(), 0);
    handler
        .wait_for_pane_startup_to_finish_for_test(&target)
        .await;
    handler
        .wait_for_pane_startup_to_finish_for_test(&web_target)
        .await;
    set_title_option(&handler, OptionName::SetTitlesString, "ISOLATION_READY").await;
    set_title_option(&handler, OptionName::SetTitles, "on").await;
    set_title_option(&handler, OptionName::StatusInterval, "0").await;
    let (mut attached, attach_shutdown, attach_task) =
        attach_peer(&handler, &target, "web-isolation-attached").await;
    let share = create_share(
        &handler,
        share_request(WebShareScope::Pane(web_target.clone().into())),
    )
    .await;
    let token = token_from_url(share.spectator_url.as_deref().expect("spectator URL"));
    let mut web = bounded_websocket(Arc::clone(&handler), &token).await;
    assert_eq!(web.read_json().await["type"], "ready");
    web.read_binary_with_prefix(0x13, "initial pane keyframe")
        .await;
    handler
        .publish_web_pane_bytes_for_test(&web_target.into(), vec![b'W'; 256 * 1024])
        .await
        .expect("publish a frame larger than both TCP buffers");
    let mut prefix = [0_u8; 2];
    timeout(Duration::from_secs(2), web.stream.read_exact(&mut prefix))
        .await
        .expect("Web response starts")
        .unwrap();
    assert_eq!(prefix[0] & 0x0f, OPCODE_BINARY);
    assert_eq!(
        prefix[1] & 0x7f,
        127,
        "the pending frame exceeds 65535 bytes"
    );
    // Leave that frame unread. Neither kernel buffer can hold its payload,
    // so the real encrypted Web writer is stalled throughout attached progress.
    for title in ["WEB_ISOLATION_ONE", "WEB_ISOLATION_TWO"] {
        timeout(
            Duration::from_secs(2),
            set_title_option(&handler, OptionName::SetTitlesString, title),
        )
        .await
        .expect("attached mutation is independent of a blocked Web writer");
        read_until(&mut attached, title.as_bytes()).await;
        assert!(
            !web.task.is_finished(),
            "Web peer remains active during native progress"
        );
    }
    timeout(Duration::from_secs(5), web.close())
        .await
        .expect("Web connection retires");
    drop(attached);
    let _ = attach_shutdown.send(());
    timeout(Duration::from_secs(6), finish_connection(attach_task))
        .await
        .expect("native attached connection retires")
        .unwrap();
    for target in [session, web_session] {
        let result = handler
            .handle(Request::KillSession(KillSessionRequest {
                target,
                kill_all_except_target: false,
                clear_alerts: false,
                kill_group: false,
            }))
            .await;
        assert!(matches!(result, Response::KillSession(_)), "{result:?}");
    }
}
