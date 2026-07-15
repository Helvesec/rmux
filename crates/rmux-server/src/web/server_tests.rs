use super::http::{path_from_target, HttpRequest};
use super::pre_auth::PreAuthQueue;
use super::{is_fd_exhaustion, serve_admitted_connection, should_continue_accept_loop};
use crate::handler::RequestHandler;
use crate::web::protocol::{AUTH_FRAME_TIMEOUT, WEB_SHARE_PROTOCOL_VERSION};
use crate::web::SecretHashForCrypto;
use base64::Engine;
use rmux_proto::{
    CreateWebShareRequest, KillSessionRequest, ListSessionsRequest, ListWindowsRequest,
    NewSessionRequest, NewWindowRequest, PaneTarget, Request, Response, SessionName,
    SplitDirection, SplitWindowRequest, SplitWindowTarget, StopWebShareRequest, TerminalSize,
    WebShareCreatedResponse, WebShareRequest, WebShareResponse, WebShareScope,
};
use rmux_web_crypto::{derive_client_session, generate_ephemeral, Message, Opener, Sealer};
use serde_json::Value;
use std::collections::HashMap;
use std::io;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::{advance, pause, timeout, Duration};

#[cfg(windows)]
const WEBSOCKET_FRAME_TIMEOUT: Duration = Duration::from_secs(10);
#[cfg(not(windows))]
const WEBSOCKET_FRAME_TIMEOUT: Duration = Duration::from_secs(2);

#[test]
fn websocket_upgrade_requires_upgrade_token() {
    let request = request_with_headers([
        ("upgrade", "websocket"),
        ("connection", "keep-alive, Upgrade"),
    ]);
    assert!(request.is_websocket_upgrade());

    let request = request_with_headers([("upgrade", "websocket"), ("connection", "close")]);
    assert!(!request.is_websocket_upgrade());
}

#[test]
fn target_path_ignores_query_for_routing() {
    assert_eq!(path_from_target("/share?ignored=true"), "/share");
    assert_eq!(path_from_target("/assets/app.js"), "/assets/app.js");
}

#[test]
fn accept_loop_retries_transient_and_fd_exhaustion_errors() {
    let interrupted = io::Error::new(io::ErrorKind::Interrupted, "retry");
    assert!(should_continue_accept_loop(&interrupted));

    for code in [23, 24, 10024] {
        let error = io::Error::from_raw_os_error(code);
        assert!(is_fd_exhaustion(&error), "raw os error {code}");
    }

    let invalid = io::Error::new(io::ErrorKind::InvalidInput, "fatal");
    assert!(!should_continue_accept_loop(&invalid));
    assert!(!is_fd_exhaustion(&invalid));
}

#[tokio::test]
async fn non_websocket_http_paths_return_404() {
    for target in ["/", "/assets/app.js", "/index.html"] {
        let response = response_for(format!("GET {target} HTTP/1.1\r\nHost: local\r\n\r\n")).await;
        assert!(
            response.starts_with("HTTP/1.1 404 Not Found"),
            "{target}: {response}"
        );
    }
}

#[tokio::test]
async fn head_requests_return_headers_without_body() {
    let response = response_for("HEAD /missing HTTP/1.1\r\nHost: local\r\n\r\n").await;

    assert!(response.starts_with("HTTP/1.1 404 Not Found"));
    assert!(response.contains("Content-Length: 10\r\n"), "{response}");
    assert!(response.ends_with("\r\n\r\n"), "{response}");
    assert!(!response.contains("not found\n"), "{response}");
}

#[tokio::test]
async fn non_get_head_methods_return_405() {
    let response = response_for("POST /share HTTP/1.1\r\nHost: local\r\n\r\n").await;
    assert!(response.starts_with("HTTP/1.1 405 Method Not Allowed"));
}

#[tokio::test]
async fn pre_auth_full_queue_replaces_the_oldest_idle_connection() {
    let handler = Arc::new(RequestHandler::new());
    let queue = PreAuthQueue::new(1);
    let (mut first_client, first_task) = raw_connection(Arc::clone(&handler), queue.clone()).await;
    wait_for_pending_pre_auth(&queue, 1).await;

    let (mut second_client, second_task) = raw_peer_connection(handler, queue.clone())
        .await
        .expect("a new request replaces the oldest unproved connection");
    let mut byte = [0u8; 1];
    let read = timeout(Duration::from_secs(1), first_client.read(&mut byte))
        .await
        .expect("oldest connection should be cancelled")
        .expect("read oldest connection");
    assert_eq!(read, 0);
    first_task
        .await
        .expect("oldest connection task joins")
        .expect("load-shed connection exits cleanly");
    assert_eq!(queue.pending_count(), 1);

    second_client
        .write_all(b"GET / HTTP/1.1\r\nHost: local\r\n\r\n")
        .await
        .expect("write request");
    let response = read_http_response(&mut second_client).await;
    assert!(response.starts_with("HTTP/1.1 404 Not Found"));

    drop(first_client);
    drop(second_client);
    second_task
        .await
        .expect("replacement connection task joins")
        .expect("replacement request succeeds");
    assert_eq!(queue.pending_count(), 0);
}

#[tokio::test]
async fn incomplete_loopback_tunnel_peers_do_not_starve_complete_requests() {
    let handler = Arc::new(RequestHandler::new());
    let queue = PreAuthQueue::with_per_ip_capacity(8, 4);
    let mut idle_clients = Vec::new();
    let mut idle_tasks = Vec::new();
    for _ in 0..8 {
        let (client, task) = raw_peer_connection(Arc::clone(&handler), queue.clone())
            .await
            .expect("loopback abuse connection fits within the global queue");
        idle_clients.push(client);
        idle_tasks.push(task);
    }
    wait_for_pending_pre_auth(&queue, 8).await;

    let (mut viewer, viewer_task) = raw_peer_connection(Arc::clone(&handler), queue.clone())
        .await
        .expect("a complete tunnel viewer replaces the oldest idle peer");
    let mut oldest = idle_clients.remove(0);
    let mut byte = [0u8; 1];
    let read = timeout(Duration::from_secs(1), oldest.read(&mut byte))
        .await
        .expect("oldest loopback peer should be cancelled")
        .expect("read oldest loopback peer");
    assert_eq!(read, 0);
    idle_tasks
        .remove(0)
        .await
        .expect("oldest loopback task joins")
        .expect("oldest loopback task exits cleanly");

    viewer
        .write_all(b"GET / HTTP/1.1\r\nHost: local\r\n\r\n")
        .await
        .expect("write complete viewer request");
    let response = read_http_response(&mut viewer).await;
    assert!(response.starts_with("HTTP/1.1 404 Not Found"));
    drop(viewer);
    viewer_task
        .await
        .expect("complete viewer task joins")
        .expect("complete viewer request succeeds");
    assert_eq!(queue.pending_count(), 7);

    drop(oldest);
    drop(idle_clients);
    for task in idle_tasks {
        let _ = task.await.expect("idle connection task joins");
    }
    assert_eq!(queue.pending_count(), 0);
}

#[tokio::test]
async fn auth_frame_timeout_releases_pre_auth_slot() {
    let handler = Arc::new(RequestHandler::new());
    let session_name = create_session(&handler, "websocket-auth-timeout").await;
    let created = create_share(
        &handler,
        share_request(WebShareScope::Pane(PaneTarget::new(session_name, 0).into())),
    )
    .await;
    let token = token_from_url(created.spectator_url.as_deref().expect("spectator URL"));
    let token_id = SecretHashForCrypto::from_secret(&token).token_id();
    let queue = PreAuthQueue::new(1);
    let (mut stream, task) = websocket_client_with_queue(Arc::clone(&handler), queue.clone()).await;

    wait_for_pending_pre_auth(&queue, 1).await;
    write_client_hello(&mut stream, &token_id).await;
    let challenge = read_server_frame(&mut stream).await;
    assert_eq!(challenge.opcode, OPCODE_TEXT);
    assert_eq!(queue.pending_count(), 1);
    assert!(
        queue
            .admit_peer("127.0.0.1".parse().expect("loopback address"))
            .await
            .is_none(),
        "a connection that proved a non-enumerable token is not evicted"
    );

    timeout(AUTH_FRAME_TIMEOUT + Duration::from_secs(2), task)
        .await
        .expect("auth timeout should finish the connection task")
        .expect("connection task joins")
        .expect("connection task returns ok");
    assert_eq!(queue.pending_count(), 0);
    drop(stream);
}

#[tokio::test]
async fn share_websocket_upgrade_returns_101() {
    let request = concat!(
        "GET /share HTTP/1.1\r\n",
        "Host: local\r\n",
        "Connection: Upgrade\r\n",
        "Upgrade: websocket\r\n",
        "Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n",
        "Sec-WebSocket-Version: 13\r\n",
        "\r\n"
    );
    let response = response_for(request).await;
    assert!(response.starts_with("HTTP/1.1 101 Switching Protocols"));
}

#[tokio::test]
async fn share_websocket_upgrade_requires_version_13_and_valid_key() {
    let missing_version = concat!(
        "GET /share HTTP/1.1\r\n",
        "Host: local\r\n",
        "Connection: Upgrade\r\n",
        "Upgrade: websocket\r\n",
        "Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n",
        "\r\n"
    );
    let response = response_for(missing_version).await;
    assert!(response.starts_with("HTTP/1.1 400 Bad Request"));

    let invalid_key = concat!(
        "GET /share HTTP/1.1\r\n",
        "Host: local\r\n",
        "Connection: Upgrade\r\n",
        "Upgrade: websocket\r\n",
        "Sec-WebSocket-Key: Zm9v\r\n",
        "Sec-WebSocket-Version: 13\r\n",
        "\r\n"
    );
    let response = response_for(invalid_key).await;
    assert!(response.starts_with("HTTP/1.1 400 Bad Request"));
}

#[tokio::test]
async fn share_websocket_auth_ready_snapshot_operator_and_revoke_loop() {
    let handler = Arc::new(RequestHandler::new());
    let session_name = create_session(&handler, "websocket-e2e").await;
    let created = create_share(
        &handler,
        CreateWebShareRequest {
            operator: true,
            ..share_request(WebShareScope::Pane(PaneTarget::new(session_name, 0).into()))
        },
    )
    .await;
    let mut client = TestWebSocket::connect(
        Arc::clone(&handler),
        &token_from_url(created.operator_url.as_deref().expect("operator URL")),
    )
    .await;
    let ready = client.read_json().await;
    assert_eq!(ready["type"], "ready");
    assert_eq!(
        ready["protocol_version"].as_u64(),
        Some(u64::from(WEB_SHARE_PROTOCOL_VERSION))
    );
    assert_eq!(ready["scope"], "pane");
    assert_eq!(ready["role"], "operator");
    assert_eq!(ready["operator"], true);
    assert_eq!(ready["show_viewers"], true);
    assert_eq!(ready["spectators_active"], 0);
    assert_eq!(ready["spectators_max"], 1);
    assert_eq!(ready["operators_active"], 1);
    assert_eq!(ready["viewers_connected"], 1);
    assert!(ready["capabilities"]
        .as_array()
        .expect("capabilities array")
        .iter()
        .any(|capability| capability == "e2ee-token-auth"));

    client.read_binary_with_prefix(0x10, "snapshot").await;

    client.send_binary(&[0x80, b'p', b'w', b'd', b'\n']).await;
    let stopped = handler
        .handle(Request::WebShare(Box::new(WebShareRequest::Stop(
            StopWebShareRequest {
                share_id: created.share_id,
            },
        ))))
        .await;
    assert!(matches!(
        stopped,
        Response::WebShare(response) if matches!(response.as_ref(), WebShareResponse::Stopped(_))
    ));

    let revoked = client.read_json().await;
    assert_eq!(revoked["type"], "share_revoked");
    assert_eq!(revoked["reason"], "stopped_by_owner");

    client.close().await;
}

#[tokio::test]
async fn authenticated_idle_pane_and_session_shares_survive_with_matching_pongs() {
    let handler = Arc::new(RequestHandler::new());
    let session_name = create_session(&handler, "websocket-idle-keepalive").await;
    let pane_share = create_share(
        &handler,
        share_request(WebShareScope::Pane(
            PaneTarget::new(session_name.clone(), 0).into(),
        )),
    )
    .await;
    let session_share = create_share(
        &handler,
        share_request(WebShareScope::Session(session_name)),
    )
    .await;
    let mut pane = TestWebSocket::connect(
        Arc::clone(&handler),
        &token_from_url(
            pane_share
                .spectator_url
                .as_deref()
                .expect("pane spectator URL"),
        ),
    )
    .await;
    let mut session = TestWebSocket::connect(
        handler,
        &token_from_url(
            session_share
                .spectator_url
                .as_deref()
                .expect("session spectator URL"),
        ),
    )
    .await;
    assert_eq!(pane.read_json().await["scope"], "pane");
    pane.read_binary_with_prefix(0x10, "pane snapshot").await;
    assert_eq!(session.read_json().await["scope"], "session");
    session
        .read_binary_with_prefix(0x10, "session snapshot")
        .await;

    pause();
    for _ in 0..3 {
        advance(Duration::from_secs(2)).await;
        tokio::task::yield_now().await;
        for client in [&mut pane, &mut session] {
            let mut saw_ping = false;
            for _ in 0..MAX_INTERLEAVED_WEBSOCKET_FRAMES {
                let frame = timeout(
                    Duration::from_millis(100),
                    read_server_frame_inner(&mut client.stream),
                )
                .await
                .expect("keepalive frame timeout")
                .expect("read keepalive frame");
                assert_ne!(frame.opcode, OPCODE_CLOSE, "share closed while idle");
                if frame.opcode == OPCODE_PING {
                    assert_eq!(frame.payload, b"rmux");
                    write_client_frame(&mut client.stream, OPCODE_PONG, &frame.payload).await;
                    saw_ping = true;
                    break;
                }
            }
            assert!(saw_ping, "idle share did not emit its keepalive ping");
        }
    }

    assert!(
        !pane.task.is_finished() && !session.task.is_finished(),
        "ignoring WebSocket pings must not turn a live idle share into code 1006"
    );
    pane.close().await;
    session.close().await;
}

#[tokio::test]
async fn authenticated_idle_pane_and_session_shares_close_without_pongs() {
    let handler = Arc::new(RequestHandler::new());
    let session_name = create_session(&handler, "websocket-idle-pong-timeout").await;
    let pane_share = create_share(
        &handler,
        share_request(WebShareScope::Pane(
            PaneTarget::new(session_name.clone(), 0).into(),
        )),
    )
    .await;
    let session_share = create_share(
        &handler,
        share_request(WebShareScope::Session(session_name)),
    )
    .await;
    let mut pane = TestWebSocket::connect(
        Arc::clone(&handler),
        &token_from_url(
            pane_share
                .spectator_url
                .as_deref()
                .expect("pane spectator URL"),
        ),
    )
    .await;
    let mut session = TestWebSocket::connect(
        handler,
        &token_from_url(
            session_share
                .spectator_url
                .as_deref()
                .expect("session spectator URL"),
        ),
    )
    .await;
    assert_eq!(pane.read_json().await["scope"], "pane");
    pane.read_binary_with_prefix(0x10, "pane snapshot").await;
    assert_eq!(session.read_json().await["scope"], "session");
    session
        .read_binary_with_prefix(0x10, "session snapshot")
        .await;

    pause();
    for _ in 0..3 {
        advance(Duration::from_secs(2)).await;
        tokio::task::yield_now().await;
        for client in [&mut pane, &mut session] {
            let mut saw_ping = false;
            for _ in 0..MAX_INTERLEAVED_WEBSOCKET_FRAMES {
                let frame = read_server_frame_inner(&mut client.stream)
                    .await
                    .expect("read keepalive frame");
                assert_ne!(frame.opcode, OPCODE_CLOSE, "share closed before timeout");
                if frame.opcode == OPCODE_PING {
                    assert_eq!(frame.payload, b"rmux");
                    saw_ping = true;
                    break;
                }
            }
            assert!(saw_ping, "idle share did not emit its keepalive ping");
        }
    }

    advance(Duration::from_secs(2)).await;
    tokio::task::yield_now().await;
    pane.read_close(4009, "pong_timeout").await;
    session.read_close(4009, "pong_timeout").await;
    assert!(pane.task.is_finished() && session.task.is_finished());
}

#[tokio::test]
async fn ready_exposes_spectator_pairing_code_only_to_operator() {
    let handler = Arc::new(RequestHandler::new());
    let session_name = create_session(&handler, "websocket-ready-pairing-code").await;
    let created = create_share(
        &handler,
        CreateWebShareRequest {
            require_pin: true,
            operator: true,
            spectator: true,
            ..share_request(WebShareScope::Pane(PaneTarget::new(session_name, 0).into()))
        },
    )
    .await;
    let operator_token = token_from_url(created.operator_url.as_deref().expect("operator URL"));
    let operator_pin = created
        .operator_pairing_code
        .as_deref()
        .expect("operator pin");
    let spectator_token = token_from_url(created.spectator_url.as_deref().expect("spectator URL"));
    let spectator_pin = created
        .spectator_pairing_code
        .as_deref()
        .expect("spectator pin");

    let mut operator =
        TestWebSocket::connect_with_pin(Arc::clone(&handler), &operator_token, operator_pin).await;
    let operator_ready = operator.read_json().await;
    assert_eq!(operator_ready["type"], "ready");
    assert_eq!(operator_ready["role"], "operator");
    assert_eq!(
        operator_ready["spectator_pairing_code"].as_str(),
        Some(spectator_pin)
    );

    let mut spectator =
        TestWebSocket::connect_with_pin(Arc::clone(&handler), &spectator_token, spectator_pin)
            .await;
    let spectator_ready = spectator.read_json().await;
    assert_eq!(spectator_ready["type"], "ready");
    assert_eq!(spectator_ready["role"], "spectator");
    assert!(
        spectator_ready.get("spectator_pairing_code").is_none(),
        "spectator clients must not receive the group pairing code"
    );

    operator.close().await;
    spectator.close().await;
}

#[tokio::test]
async fn pane_share_rejects_browser_resize() {
    let handler = Arc::new(RequestHandler::new());
    let session_name = create_session(&handler, "websocket-pane-no-browser-resize").await;
    let created = create_share(
        &handler,
        CreateWebShareRequest {
            operator: true,
            ..share_request(WebShareScope::Pane(PaneTarget::new(session_name, 0).into()))
        },
    )
    .await;
    let mut client = TestWebSocket::connect(
        Arc::clone(&handler),
        &token_from_url(created.operator_url.as_deref().expect("operator URL")),
    )
    .await;
    let ready = client.read_json().await;
    assert_eq!(ready["scope"], "pane");
    assert_eq!(ready["role"], "operator");

    client.read_binary_with_prefix(0x10, "snapshot").await;
    client.send_binary(&[0x82, 0x00, 0x2c, 0x00, 0x24]).await;
    client.read_close(4006, "web_resize_unsupported").await;
    client.close().await;
}

#[tokio::test]
async fn session_operator_prefix_w_is_not_web_filtered() {
    let handler = Arc::new(RequestHandler::new());
    let session_name = create_session(&handler, "websocket-session-prefix-w").await;
    let created = create_share(
        &handler,
        CreateWebShareRequest {
            operator: true,
            ..share_request(WebShareScope::Session(session_name))
        },
    )
    .await;
    let mut client = TestWebSocket::connect(
        Arc::clone(&handler),
        &token_from_url(created.operator_url.as_deref().expect("operator URL")),
    )
    .await;
    let ready = client.read_json().await;
    assert_eq!(ready["scope"], "session");
    assert_eq!(ready["role"], "operator");

    client.read_binary_with_prefix(0x10, "snapshot").await;
    client.send_binary(&[0x83, 0x02, b'w']).await;
    let redraw = client
        .read_binary_with_prefix_payload_containing(0x01, b"\x1b[s\x1b[?25l", "prefix w redraw")
        .await;
    let redraw = String::from_utf8_lossy(&redraw[1..]);
    assert!(
        !redraw.contains("command is not allowed through web controls"),
        "operator prefix commands should not be filtered, got {redraw:?}"
    );
    assert!(
        redraw.contains("\x1b[s\x1b[?25l"),
        "operator prefix overlays should be forwarded to the browser, got {redraw:?}"
    );

    client.close().await;
}

#[tokio::test]
async fn session_operator_prefix_q_overlay_reaches_browser() {
    let handler = Arc::new(RequestHandler::new());
    let session_name = create_session(&handler, "websocket-session-prefix-q").await;
    let created = create_share(
        &handler,
        CreateWebShareRequest {
            operator: true,
            ..share_request(WebShareScope::Session(session_name))
        },
    )
    .await;
    let mut client = TestWebSocket::connect(
        Arc::clone(&handler),
        &token_from_url(created.operator_url.as_deref().expect("operator URL")),
    )
    .await;
    let ready = client.read_json().await;
    assert_eq!(ready["scope"], "session");
    assert_eq!(ready["role"], "operator");

    client.read_binary_with_prefix(0x10, "snapshot").await;
    client.send_binary(&[0x83, 0x02, b'q']).await;
    let redraw = client
        .read_binary_with_prefix_payload_containing(0x01, b"\x1b[?25l", "prefix q redraw")
        .await;
    let redraw = String::from_utf8_lossy(&redraw[1..]);
    assert!(
        redraw.contains("\x1b[?25l"),
        "display-panes overlay should be forwarded to the browser, got {redraw:?}"
    );

    client.close().await;
}

#[tokio::test]
async fn session_operator_command_prompt_rename_keeps_share_alive() {
    let handler = Arc::new(RequestHandler::new());
    let session_name = create_session(&handler, "websocket-session-rename").await;
    let created = create_share(
        &handler,
        CreateWebShareRequest {
            operator: true,
            ..share_request(WebShareScope::Session(session_name))
        },
    )
    .await;
    let mut client = TestWebSocket::connect(
        Arc::clone(&handler),
        &token_from_url(created.operator_url.as_deref().expect("operator URL")),
    )
    .await;
    let ready = client.read_json().await;
    assert_eq!(ready["scope"], "session");
    assert_eq!(ready["role"], "operator");

    client.read_binary_with_prefix(0x10, "snapshot").await;
    for bytes in [&b"\x02"[..], b":", b"rename-session renamed", b"\r"] {
        let mut frame = Vec::with_capacity(bytes.len() + 1);
        frame.push(0x83);
        frame.extend_from_slice(bytes);
        client.send_binary(&frame).await;
    }
    wait_for_session_name(&handler, "renamed").await;
    let mut seen = Vec::new();
    let output = loop {
        let payload = client.read_binary_payload("rename refresh").await;
        let summary = String::from_utf8_lossy(&payload).into_owned();
        seen.push(summary);
        if payload.first() == Some(&0x01)
            && payload
                .windows(b"[renamed]".len())
                .any(|w| w == b"[renamed]")
        {
            break payload;
        }
        assert!(
            seen.len() < 80,
            "did not receive renamed status frame; seen {seen:#?}"
        );
    };
    let output = String::from_utf8_lossy(&output[1..]);
    assert!(
        !output.contains("[websocket-session-rename]"),
        "renamed session status must not keep old name: {output:?}"
    );

    client.close().await;
}

#[tokio::test]
async fn session_share_sends_revoked_before_closing_when_session_is_killed() {
    let handler = Arc::new(RequestHandler::new());
    let session_name = create_session(&handler, "websocket-session-gone").await;
    let created = create_share(
        &handler,
        share_request(WebShareScope::Session(session_name.clone())),
    )
    .await;
    let mut client = TestWebSocket::connect(
        Arc::clone(&handler),
        &token_from_url(created.spectator_url.as_deref().expect("spectator URL")),
    )
    .await;
    let ready = client.read_json().await;
    assert_eq!(ready["type"], "ready");
    assert_eq!(ready["scope"], "session");

    let killed = handler
        .handle(Request::KillSession(KillSessionRequest {
            target: session_name,
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        }))
        .await;
    assert!(matches!(killed, Response::KillSession(_)));

    let revoked = client.read_json().await;
    assert_eq!(revoked["type"], "share_revoked");
    assert_eq!(revoked["reason"], "session_gone");

    client.close().await;
}

#[tokio::test]
async fn session_share_streams_attach_output_without_replacing_snapshot() {
    let handler = Arc::new(RequestHandler::new());
    let session_name = create_session(&handler, "websocket-session-snapshot").await;
    let created = create_share(
        &handler,
        share_request(WebShareScope::Session(session_name.clone())),
    )
    .await;
    let mut client = TestWebSocket::connect(
        Arc::clone(&handler),
        &token_from_url(created.spectator_url.as_deref().expect("spectator URL")),
    )
    .await;
    let ready = client.read_json().await;
    assert_eq!(ready["scope"], "session");

    let first = client
        .read_binary_with_prefix_payload(0x10, "initial session snapshot")
        .await;
    let first = String::from_utf8_lossy(&first[1..]);
    assert!(
        first.contains("[websocket"),
        "initial snapshot should contain the rendered session status, got {first:?}"
    );

    let redraw = client
        .read_binary_with_prefix_payload(0x01, "session attach output")
        .await;
    assert!(
        redraw.len() > 1,
        "session attach output should be streamed as raw terminal bytes"
    );
    client.close().await;
}

#[tokio::test]
async fn spectator_session_share_rejects_binary_frames() {
    let handler = Arc::new(RequestHandler::new());
    let session_name = create_session(&handler, "websocket-spectator-binary").await;
    let created = create_share(
        &handler,
        share_request(WebShareScope::Session(session_name)),
    )
    .await;
    let mut client = TestWebSocket::connect(
        Arc::clone(&handler),
        &token_from_url(created.spectator_url.as_deref().expect("spectator URL")),
    )
    .await;
    let ready = client.read_json().await;
    assert_eq!(ready["scope"], "session");
    assert_eq!(ready["role"], "spectator");

    client.send_binary(&[0x82, 0x00, 0x64, 0x00, 0x28]).await;
    client.read_close(4006, "spectator_no_binary").await;
    client.close().await;
}

#[tokio::test]
async fn spectator_session_share_allows_scroll_text_frames() {
    let handler = Arc::new(RequestHandler::new());
    let session_name = create_session(&handler, "websocket-spectator-scroll").await;
    let created = create_share(
        &handler,
        share_request(WebShareScope::Session(session_name)),
    )
    .await;
    let mut client = TestWebSocket::connect(
        Arc::clone(&handler),
        &token_from_url(created.spectator_url.as_deref().expect("spectator URL")),
    )
    .await;
    let ready = client.read_json().await;
    assert_eq!(ready["scope"], "session");
    assert_eq!(ready["role"], "spectator");

    client.read_binary_with_prefix(0x10, "snapshot").await;
    client
        .send_json(r#"{"type":"pane_scroll","pane_id":0,"delta":-1}"#)
        .await;
    read_session_view_until(&mut client, "spectator scroll session view", |view| {
        view["panes"]
            .as_array()
            .is_some_and(|panes| !panes.is_empty())
    })
    .await;
    client.close().await;
}

#[tokio::test]
async fn session_operator_browser_resize_queues_fresh_snapshot() {
    let handler = Arc::new(RequestHandler::new());
    let session_name = create_session(&handler, "websocket-session-browser-resize").await;
    let created = create_share(
        &handler,
        CreateWebShareRequest {
            operator: true,
            ..share_request(WebShareScope::Session(session_name))
        },
    )
    .await;
    let mut client = TestWebSocket::connect(
        Arc::clone(&handler),
        &token_from_url(created.operator_url.as_deref().expect("operator URL")),
    )
    .await;
    let ready = client.read_json().await;
    assert_eq!(ready["scope"], "session");
    assert_eq!(ready["role"], "operator");

    client
        .read_binary_with_prefix(0x10, "initial snapshot")
        .await;
    client
        .read_binary_with_prefix(0x11, "initial session view")
        .await;

    client.send_binary(&[0x82, 0x00, 0x78, 0x00, 0x28]).await;

    let resized_snapshot = client
        .read_binary_with_prefix_payload(0x10, "browser resize snapshot")
        .await;
    assert!(
        resized_snapshot.len() > 1,
        "browser resize should produce a full session snapshot"
    );
    let resized_view = client
        .read_binary_with_prefix_payload(0x11, "browser resize session view")
        .await;
    let resized_view = parse_session_view(&resized_view);
    assert_eq!(resized_view["size"]["cols"], 120);

    client.close().await;
}

#[tokio::test]
async fn session_operator_can_resize_pane_by_id() {
    let handler = Arc::new(RequestHandler::new());
    let session_name = create_session(&handler, "websocket-session-pane-resize").await;
    assert!(matches!(
        handler
            .handle(Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Session(session_name.clone()),
                direction: SplitDirection::Horizontal,
                before: false,
                environment: None,
            }))
            .await,
        Response::SplitWindow(_)
    ));

    let created = create_share(
        &handler,
        CreateWebShareRequest {
            operator: true,
            ..share_request(WebShareScope::Session(session_name.clone()))
        },
    )
    .await;
    let mut client = TestWebSocket::connect(
        Arc::clone(&handler),
        &token_from_url(created.operator_url.as_deref().expect("operator URL")),
    )
    .await;
    let ready = client.read_json().await;
    assert_eq!(ready["scope"], "session");
    assert_eq!(ready["role"], "operator");

    client.read_binary_with_prefix(0x10, "snapshot").await;
    let initial_view = client
        .read_binary_with_prefix_payload(0x11, "initial session view")
        .await;
    let initial_view = parse_session_view(&initial_view);
    let (pane_id, initial_width) = first_pane_id_and_width(&initial_view);

    let mut frame = vec![0x84];
    frame.extend_from_slice(&pane_id.to_be_bytes());
    frame.push(1);
    frame.extend_from_slice(&5u16.to_be_bytes());
    client.send_binary(&frame).await;

    let resized_view = read_session_view_until(&mut client, "resized session view", |view| {
        pane_width(view, pane_id) > initial_width
    })
    .await;
    assert!(
        pane_width(&resized_view, pane_id) > initial_width,
        "operator pane resize should update the target pane"
    );

    client.close().await;
}

#[tokio::test]
async fn session_operator_can_run_typed_window_actions() {
    let handler = Arc::new(RequestHandler::new());
    let session_name = create_session(&handler, "websocket-session-window-actions").await;
    let created = create_share(
        &handler,
        CreateWebShareRequest {
            operator: true,
            ..share_request(WebShareScope::Session(session_name))
        },
    )
    .await;
    let mut client = TestWebSocket::connect(
        Arc::clone(&handler),
        &token_from_url(created.operator_url.as_deref().expect("operator URL")),
    )
    .await;
    let ready = client.read_json().await;
    assert_eq!(ready["scope"], "session");
    assert_eq!(ready["role"], "operator");

    client.read_binary_with_prefix(0x10, "snapshot").await;
    let initial = read_session_view_until(&mut client, "initial session view", |view| {
        window_count(view) == 1 && active_window_index(view) == Some(0)
    })
    .await;
    let initial_panes = pane_count(&initial);
    assert_eq!(
        active_pane_count(&initial),
        1,
        "session view marks exactly one active pane"
    );

    client.send_json(r#"{"type":"new_window"}"#).await;
    read_session_view_until(&mut client, "new window view", |view| {
        window_count(view) == 2
    })
    .await;

    client
        .send_json(r#"{"type":"rename_window","window_index":1,"name":"logs"}"#)
        .await;
    read_session_view_until(&mut client, "renamed window view", |view| {
        window_named(view, 1, "logs")
    })
    .await;

    client
        .send_json(r#"{"type":"select_window","window_index":0}"#)
        .await;
    read_session_view_until(&mut client, "selected window view", |view| {
        active_window_index(view) == Some(0)
    })
    .await;

    client
        .send_json(r#"{"type":"split_pane","direction":"horizontal"}"#)
        .await;
    read_session_view_until(&mut client, "split pane view", |view| {
        pane_count(view) > initial_panes
    })
    .await;

    client.close().await;
}

#[tokio::test]
async fn session_spectator_can_select_windows_without_operator_access() {
    let handler = Arc::new(RequestHandler::new());
    let session_name = create_session(&handler, "websocket-spectator-window-select").await;
    assert!(matches!(
        handler
            .handle(Request::NewWindow(Box::new(NewWindowRequest {
                target: session_name.clone(),
                name: Some("logs".to_owned()),
                detached: true,
                environment: None,
                command: None,
                start_directory: None,
                target_window_index: None,
                insert_at_target: false,
                process_command: None,
            })))
            .await,
        Response::NewWindow(_)
    ));
    let created = create_share(
        &handler,
        share_request(WebShareScope::Session(session_name.clone())),
    )
    .await;
    let mut client = TestWebSocket::connect(
        Arc::clone(&handler),
        &token_from_url(created.spectator_url.as_deref().expect("spectator URL")),
    )
    .await;
    let ready = client.read_json().await;
    assert_eq!(ready["scope"], "session");
    assert_eq!(ready["role"], "spectator");

    client.read_binary_with_prefix(0x10, "snapshot").await;
    read_session_view_until(&mut client, "initial spectator session view", |view| {
        window_count(view) == 2 && active_window_index(view) == Some(0)
    })
    .await;

    client
        .send_json(r#"{"type":"select_window","window_index":1}"#)
        .await;
    read_session_view_until(&mut client, "spectator selected window view", |view| {
        active_window_index(view) == Some(1)
    })
    .await;
    let Response::ListWindows(listed) = handler
        .handle(Request::ListWindows(Box::new(ListWindowsRequest {
            target: session_name,
            format: None,
            filter: None,
            sort_order: None,
            reversed: false,
        })))
        .await
    else {
        panic!("expected list-windows response");
    };
    assert!(listed
        .windows
        .iter()
        .any(|window| window.target.window_index() == 0 && window.active));
    assert!(listed
        .windows
        .iter()
        .any(|window| window.target.window_index() == 1 && !window.active));

    client.close().await;
}

#[tokio::test]
async fn handshake_rejects_unknown_token_with_collapsed_close() {
    // An unknown token_id has no registered share, so the pre-ready token lookup
    // returns None and the server collapses to the single wire pair BEFORE it
    // ever emits a challenge.
    let handler = Arc::new(RequestHandler::new());
    let unknown_token_id = SecretHashForCrypto::from_secret("no-such-token").token_id();
    let (mut stream, task) = send_hello_only(Arc::clone(&handler), &unknown_token_id).await;

    assert_close(&mut stream, 4000, "handshake_rejected").await;

    drop(stream);
    let _ = task.await.expect("server task joins");
}

#[tokio::test]
async fn handshake_rejects_wrong_pin_with_same_collapsed_close() {
    // A PIN-required share has a KNOWN token, so the full DH handshake runs and
    // the encrypted auth frame carries a wrong PIN. The auth failure must
    // surface the IDENTICAL (4000, "handshake_rejected") pair as the unknown
    // token above, proving the close code is not a PIN oracle.
    let handler = Arc::new(RequestHandler::new());
    let session_name = create_session(&handler, "websocket-wrong-pin").await;
    let created = create_share(
        &handler,
        CreateWebShareRequest {
            require_pin: true,
            ..share_request(WebShareScope::Pane(PaneTarget::new(session_name, 0).into()))
        },
    )
    .await;
    let pairing_code = created
        .spectator_pairing_code
        .as_deref()
        .expect("pin-enabled spectator share returns pairing code");
    let wrong_pin = if pairing_code == "000000" {
        "111111"
    } else {
        "000000"
    };

    let token = token_from_url(created.spectator_url.as_deref().expect("spectator URL"));
    let token_id = SecretHashForCrypto::from_secret(&token).token_id();
    let HandshakeSession {
        mut stream, task, ..
    } = drive_handshake_through_auth(
        Arc::clone(&handler),
        &token,
        &token_id,
        &auth_text_with_pin(wrong_pin),
    )
    .await;

    assert_close(&mut stream, 4000, "handshake_rejected").await;

    drop(stream);
    let _ = task.await.expect("server task joins");
}

#[tokio::test]
async fn loopback_backoff_waiters_do_not_block_another_share_over_websocket() {
    let handler = Arc::new(RequestHandler::new_with_web_authentication_limits(
        1, 8, 8, 4,
    ));
    let protected_session = create_session(&handler, "websocket-protected-wait").await;
    let protected = create_share(
        &handler,
        CreateWebShareRequest {
            require_pin: true,
            ..share_request(WebShareScope::Pane(
                PaneTarget::new(protected_session, 0).into(),
            ))
        },
    )
    .await;
    let unrelated_session = create_session(&handler, "websocket-unrelated-wait").await;
    let unrelated = create_share(
        &handler,
        share_request(WebShareScope::Pane(
            PaneTarget::new(unrelated_session, 0).into(),
        )),
    )
    .await;
    let protected_token =
        token_from_url(protected.spectator_url.as_deref().expect("protected URL"));
    let protected_pin = protected
        .spectator_pairing_code
        .as_deref()
        .expect("protected share has a PIN");
    let wrong_pin = if protected_pin == "000000" {
        "111111"
    } else {
        "000000"
    };
    let protected_token_id = SecretHashForCrypto::from_secret(&protected_token).token_id();
    let unrelated_token =
        token_from_url(unrelated.spectator_url.as_deref().expect("unrelated URL"));

    // Four settled failures make the next attempt wait 800 ms, leaving enough
    // time to complete a real encrypted handshake for the unrelated share.
    for _ in 0..4 {
        let HandshakeSession {
            mut stream, task, ..
        } = drive_handshake_through_auth(
            Arc::clone(&handler),
            &protected_token,
            &protected_token_id,
            &auth_text_with_pin(wrong_pin),
        )
        .await;
        assert_close(&mut stream, 4000, "handshake_rejected").await;
        drop(stream);
        let _ = task.await.expect("failed PIN task joins");
    }

    let mut waiters = Vec::with_capacity(4);
    for _ in 0..4 {
        waiters.push(
            drive_handshake_through_auth(
                Arc::clone(&handler),
                &protected_token,
                &protected_token_id,
                &auth_text_with_pin(protected_pin),
            )
            .await,
        );
    }
    tokio::time::timeout(Duration::from_millis(100), async {
        while handler.web_authentication_wait_count() != 4 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("four loopback connections enter authentication backoff");

    let mut unrelated = TestWebSocket::connect(Arc::clone(&handler), &unrelated_token).await;
    let ready = tokio::time::timeout(Duration::from_millis(600), unrelated.read_json())
        .await
        .expect("unrelated share reaches ready while protected share waits");
    assert_eq!(ready["type"], "ready");

    for waiter in waiters {
        let HandshakeSession { stream, task, .. } = waiter;
        drop(stream);
        task.abort();
        assert!(
            task.await
                .expect_err("backoff task was cancelled")
                .is_cancelled(),
            "cancelling a backoff task must stop it"
        );
    }
    unrelated.close().await;
    assert_eq!(handler.web_authentication_wait_count(), 0);
}

#[tokio::test]
async fn handshake_rejects_capacity_reached_with_collapsed_close() {
    // The share caps spectators at 1. Once that slot is held by a live viewer,
    // a second spectator hits the capacity-reached path after token auth. Keep
    // the wire close collapsed so PIN-protected shares do not expose an oracle.
    let handler = Arc::new(RequestHandler::new());
    let session_name = create_session(&handler, "websocket-capacity").await;
    let created = create_share(
        &handler,
        share_request(WebShareScope::Session(session_name)),
    )
    .await;
    let token = token_from_url(created.spectator_url.as_deref().expect("spectator URL"));

    // First spectator occupies the only slot and stays connected.
    let mut first = TestWebSocket::connect(Arc::clone(&handler), &token).await;
    let ready = first.read_json().await;
    assert_eq!(ready["type"], "ready");

    // Second spectator must be rejected with the collapsed pair.
    let token_id = SecretHashForCrypto::from_secret(&token).token_id();
    let HandshakeSession {
        mut stream, task, ..
    } = drive_handshake_through_auth(Arc::clone(&handler), &token, &token_id, &auth_text()).await;

    assert_close(&mut stream, 4000, "handshake_rejected").await;

    drop(stream);
    let _ = task.await.expect("server task joins");
    first.close().await;
}

#[tokio::test]
async fn handshake_rejects_pin_protected_capacity_after_valid_pin_with_collapsed_close() {
    let handler = Arc::new(RequestHandler::new());
    let session_name = create_session(&handler, "websocket-pin-capacity").await;
    let created = create_share(
        &handler,
        CreateWebShareRequest {
            require_pin: true,
            ..share_request(WebShareScope::Session(session_name))
        },
    )
    .await;
    let token = token_from_url(created.spectator_url.as_deref().expect("spectator URL"));
    let pin = created
        .spectator_pairing_code
        .as_deref()
        .expect("pin-enabled spectator share returns pairing code");

    let first = TestWebSocket::connect_with_pin(Arc::clone(&handler), &token, pin).await;

    let token_id = SecretHashForCrypto::from_secret(&token).token_id();
    let HandshakeSession {
        mut stream, task, ..
    } = drive_handshake_through_auth(
        Arc::clone(&handler),
        &token,
        &token_id,
        &auth_text_with_pin(pin),
    )
    .await;

    assert_close(&mut stream, 4000, "handshake_rejected").await;

    drop(stream);
    let _ = task.await.expect("server task joins");
    first.close().await;
}

#[tokio::test]
async fn handshake_rejects_wrong_pin_before_capacity_with_collapsed_close() {
    let handler = Arc::new(RequestHandler::new());
    let session_name = create_session(&handler, "websocket-wrong-pin-capacity").await;
    let created = create_share(
        &handler,
        CreateWebShareRequest {
            require_pin: true,
            ..share_request(WebShareScope::Session(session_name))
        },
    )
    .await;
    let token = token_from_url(created.spectator_url.as_deref().expect("spectator URL"));
    let pin = created
        .spectator_pairing_code
        .as_deref()
        .expect("pin-enabled spectator share returns pairing code");
    let wrong_pin = if pin == "000000" { "111111" } else { "000000" };

    let first = TestWebSocket::connect_with_pin(Arc::clone(&handler), &token, pin).await;

    let token_id = SecretHashForCrypto::from_secret(&token).token_id();
    let HandshakeSession {
        mut stream, task, ..
    } = drive_handshake_through_auth(
        Arc::clone(&handler),
        &token,
        &token_id,
        &auth_text_with_pin(wrong_pin),
    )
    .await;

    assert_close(&mut stream, 4000, "handshake_rejected").await;

    drop(stream);
    let _ = task.await.expect("server task joins");
    first.close().await;
}

#[tokio::test]
async fn handshake_rejects_missing_pin_with_pin_required_close() {
    let handler = Arc::new(RequestHandler::new());
    let session_name = create_session(&handler, "websocket-missing-pin").await;
    let created = create_share(
        &handler,
        CreateWebShareRequest {
            require_pin: true,
            ..share_request(WebShareScope::Session(session_name))
        },
    )
    .await;
    let token = token_from_url(created.spectator_url.as_deref().expect("spectator URL"));
    let token_id = SecretHashForCrypto::from_secret(&token).token_id();

    let HandshakeSession {
        mut stream, task, ..
    } = drive_handshake_through_auth(Arc::clone(&handler), &token, &token_id, &auth_text()).await;

    assert_close(&mut stream, 4008, "pin_required").await;

    drop(stream);
    let _ = task.await.expect("server task joins");
}

fn request_with_headers<const N: usize>(headers: [(&str, &str); N]) -> HttpRequest {
    HttpRequest {
        method: "GET".to_owned(),
        path: "/share".to_owned(),
        headers: headers
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value.to_owned()))
            .collect::<HashMap<_, _>>(),
    }
}

#[test]
fn transient_accept_errors_keep_listener_alive() {
    for kind in [
        io::ErrorKind::ConnectionAborted,
        io::ErrorKind::ConnectionReset,
        io::ErrorKind::Interrupted,
        io::ErrorKind::TimedOut,
        io::ErrorKind::WouldBlock,
    ] {
        let error = io::Error::from(kind);
        assert!(should_continue_accept_loop(&error), "{kind:?}");
    }

    let error = io::Error::from(io::ErrorKind::PermissionDenied);
    assert!(!should_continue_accept_loop(&error));
}

async fn create_session(handler: &RequestHandler, name: &str) -> SessionName {
    let session_name = SessionName::new(name).expect("valid session");
    assert!(matches!(
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session_name.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
        Response::NewSession(_)
    ));
    session_name
}

async fn create_share(
    handler: &RequestHandler,
    request: CreateWebShareRequest,
) -> WebShareCreatedResponse {
    handler.mark_web_listener_available();
    let response = handler
        .handle(Request::WebShare(Box::new(WebShareRequest::Create(
            request,
        ))))
        .await;
    let Response::WebShare(response) = response else {
        panic!("expected web share creation");
    };
    let WebShareResponse::Created(created) = *response else {
        panic!("expected web share creation");
    };
    created
}

async fn wait_for_session_name(handler: &RequestHandler, name: &str) {
    timeout(Duration::from_secs(10), async {
        loop {
            let Response::ListSessions(listed) = handler
                .handle(Request::ListSessions(ListSessionsRequest {
                    format: Some("#{session_name}".to_owned()),
                    filter: None,
                    sort_order: None,
                    reversed: false,
                }))
                .await
            else {
                panic!("list-sessions should succeed");
            };
            let stdout = String::from_utf8_lossy(listed.output.stdout());
            if stdout.lines().any(|line| line == name) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("session {name:?} was not created"));
}

fn parse_session_view(frame: &[u8]) -> Value {
    serde_json::from_slice(&frame[1..]).expect("session view json")
}

async fn read_session_view_until(
    client: &mut TestWebSocket,
    label: &str,
    matches: impl Fn(&Value) -> bool,
) -> Value {
    for _ in 0..40 {
        let frame = client.read_binary_with_prefix_payload(0x11, label).await;
        let view = parse_session_view(&frame);
        if matches(&view) {
            return view;
        }
    }
    panic!("did not receive matching {label}");
}

fn pane_count(view: &Value) -> usize {
    view["panes"].as_array().expect("panes array").len()
}

fn window_count(view: &Value) -> usize {
    view["windows"].as_array().expect("windows array").len()
}

fn active_pane_count(view: &Value) -> usize {
    view["panes"]
        .as_array()
        .expect("panes array")
        .iter()
        .filter(|pane| pane["active"].as_bool() == Some(true))
        .count()
}

fn active_window_index(view: &Value) -> Option<u64> {
    view["windows"]
        .as_array()
        .expect("windows array")
        .iter()
        .find(|window| window["active"].as_bool() == Some(true))
        .and_then(|window| window["index"].as_u64())
}

fn window_named(view: &Value, index: u32, name: &str) -> bool {
    view["windows"]
        .as_array()
        .expect("windows array")
        .iter()
        .any(|window| {
            window["index"].as_u64() == Some(u64::from(index))
                && window["name"].as_str() == Some(name)
        })
}

fn first_pane_id_and_width(view: &Value) -> (u32, u64) {
    let pane = view["panes"]
        .as_array()
        .expect("panes array")
        .iter()
        .min_by_key(|pane| {
            (
                pane["y"].as_u64().expect("pane y"),
                pane["x"].as_u64().expect("pane x"),
            )
        })
        .expect("first pane");
    (
        pane["id"].as_u64().expect("pane id") as u32,
        pane["cols"].as_u64().expect("pane cols"),
    )
}

fn pane_width(view: &Value, pane_id: u32) -> u64 {
    view["panes"]
        .as_array()
        .expect("panes array")
        .iter()
        .find(|pane| pane["id"].as_u64() == Some(u64::from(pane_id)))
        .expect("pane exists")
        .get("cols")
        .and_then(Value::as_u64)
        .expect("pane cols")
}

fn share_request(scope: WebShareScope) -> CreateWebShareRequest {
    CreateWebShareRequest {
        scope,
        public_base_url: Some("https://terminal.example".to_owned()),
        tunnel_provider: None,
        frontend_url: None,
        ttl_seconds: Some(60),
        expires_at_unix: None,
        max_spectators: Some(1),
        max_operators: None,
        url_options: Default::default(),
        require_pin: false,
        operator_pin: None,
        spectator_pin: None,
        terminal_palette: None,
        operator: false,
        spectator: true,
        controls: false,
        kill_session_on_expire: false,
    }
}

async fn response_for(request: impl AsRef<[u8]>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test listener");
    let addr = listener.local_addr().expect("listener addr");
    let client = TcpStream::connect(addr);
    let server = listener.accept();
    let (client, server) = tokio::join!(client, server);
    let mut client = client.expect("client connects");
    let (server, _) = server.expect("server accepts");
    let pre_auth = PreAuthQueue::new(16);
    let pre_auth_admission = pre_auth.try_register().expect("pre-auth slot");
    let task = tokio::spawn(serve_admitted_connection(
        server,
        Arc::new(RequestHandler::new()),
        pre_auth_admission,
    ));

    client
        .write_all(request.as_ref())
        .await
        .expect("write request");
    let mut buffer = [0u8; 4096];
    let read = client.read(&mut buffer).await.expect("read response");
    drop(client);
    let _ = task.await.expect("connection task joins");
    String::from_utf8_lossy(&buffer[..read]).into_owned()
}

async fn websocket_client(
    handler: Arc<RequestHandler>,
) -> (TcpStream, tokio::task::JoinHandle<io::Result<()>>) {
    websocket_client_with_queue(handler, PreAuthQueue::new(16)).await
}

async fn websocket_client_with_queue(
    handler: Arc<RequestHandler>,
    pre_auth: PreAuthQueue,
) -> (TcpStream, tokio::task::JoinHandle<io::Result<()>>) {
    let (mut client, task) = raw_connection(handler, pre_auth).await;
    client
        .write_all(
            concat!(
                "GET /share HTTP/1.1\r\n",
                "Host: local\r\n",
                "Connection: Upgrade\r\n",
                "Upgrade: websocket\r\n",
                "Origin: https://share.rmux.io\r\n",
                "Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n",
                "Sec-WebSocket-Version: 13\r\n",
                "\r\n"
            )
            .as_bytes(),
        )
        .await
        .expect("write upgrade request");
    let response = read_http_response(&mut client).await;
    assert!(
        response.starts_with("HTTP/1.1 101 Switching Protocols"),
        "{response}"
    );
    (client, task)
}

struct TestWebSocket {
    stream: TcpStream,
    task: tokio::task::JoinHandle<io::Result<()>>,
    opener: Opener,
    sealer: Sealer,
}

impl TestWebSocket {
    async fn connect(handler: Arc<RequestHandler>, token: &str) -> Self {
        let auth = auth_text();
        Self::connect_with_auth(handler, token, &auth).await
    }

    async fn connect_with_pin(handler: Arc<RequestHandler>, token: &str, pin: &str) -> Self {
        let auth = auth_text_with_pin(pin);
        Self::connect_with_auth(handler, token, &auth).await
    }

    async fn connect_with_auth(handler: Arc<RequestHandler>, token: &str, auth: &str) -> Self {
        let token_id = SecretHashForCrypto::from_secret(token).token_id();
        let HandshakeSession {
            stream,
            task,
            opener,
            sealer,
        } = drive_handshake_through_auth(handler, token, &token_id, auth).await;
        Self {
            stream,
            task,
            opener,
            sealer,
        }
    }

    async fn read_json(&mut self) -> Value {
        serde_json::from_str(&read_encrypted_text(&mut self.stream, &mut self.opener).await)
            .expect("encrypted text frame json")
    }

    async fn read_binary_with_prefix(&mut self, prefix: u8, label: &str) {
        self.read_binary_with_prefix_payload(prefix, label).await;
    }

    async fn read_binary_with_prefix_payload(&mut self, prefix: u8, label: &str) -> Vec<u8> {
        read_encrypted_binary_frame_with_prefix(&mut self.stream, &mut self.opener, prefix, label)
            .await
    }

    async fn read_binary_payload(&mut self, label: &str) -> Vec<u8> {
        read_encrypted_binary_frame(&mut self.stream, &mut self.opener, label).await
    }

    async fn read_binary_with_prefix_payload_containing(
        &mut self,
        prefix: u8,
        needle: &[u8],
        label: &str,
    ) -> Vec<u8> {
        read_encrypted_binary_frame_with_prefix_containing(
            &mut self.stream,
            &mut self.opener,
            prefix,
            needle,
            label,
        )
        .await
    }

    async fn send_binary(&mut self, payload: &[u8]) {
        write_client_binary_frame(
            &mut self.stream,
            &self.sealer.seal_binary(payload).expect("seal binary"),
        )
        .await;
    }

    async fn send_json(&mut self, payload: &str) {
        write_client_binary_frame(
            &mut self.stream,
            &self.sealer.seal_text(payload).expect("seal text"),
        )
        .await;
    }

    async fn read_close(&mut self, code: u16, reason: &str) {
        for _ in 0..8 {
            let frame = read_server_frame(&mut self.stream).await;
            if frame.opcode != OPCODE_CLOSE {
                continue;
            }
            assert!(
                frame.payload.len() >= 2,
                "close frame should include a status code"
            );
            assert_eq!(
                u16::from_be_bytes([frame.payload[0], frame.payload[1]]),
                code
            );
            assert_eq!(String::from_utf8_lossy(&frame.payload[2..]), reason);
            return;
        }
        panic!("websocket did not close with {code} {reason}");
    }

    async fn close(self) {
        drop(self.stream);
        let _ = self.task.await.expect("server task joins");
    }
}

struct HandshakeSession {
    stream: TcpStream,
    task: tokio::task::JoinHandle<io::Result<()>>,
    opener: Opener,
    sealer: Sealer,
}

/// Drives a real v1 handshake all the way through sending the encrypted auth
/// frame and returns the live client session.
///
/// `token` derives the PSK and `token_id` is sent on the wire; passing a
/// mismatching pair (or a wrong PIN inside `auth`) exercises the rejection
/// paths. The caller decides whether to expect `ready` or a close frame.
async fn drive_handshake_through_auth(
    handler: Arc<RequestHandler>,
    token: &str,
    token_id: &str,
    auth: &str,
) -> HandshakeSession {
    let psk = SecretHashForCrypto::from_secret(token).as_bytes();
    let (mut stream, task) = websocket_client(handler).await;

    // Generate the client ephemeral X25519 key and the ML-KEM keypair, and
    // advertise the X25519 public key + ML-KEM encapsulation key.
    let client_eph = generate_ephemeral();
    let client_public = client_eph.public_bytes();
    let ml_kem = rmux_web_crypto::ml_kem::KeyPair::generate([0x21u8; 64]);
    let ml_kem_ek = ml_kem.encapsulation_key();
    let hello = format!(
        r#"{{"type":"hello","protocol_version":{},"capabilities":["e2ee-token-auth","terminal-palette-v1"],"token_id":"{}","client_nonce":"{}","client_public":"{}","client_ml_kem_ek":"{}"}}"#,
        WEB_SHARE_PROTOCOL_VERSION,
        token_id,
        TEST_CLIENT_NONCE,
        b64url(&client_public),
        b64url(&ml_kem_ek),
    );
    write_client_text_frame(&mut stream, hello.as_bytes()).await;

    // The server binds the exact challenge bytes it sends, so we must bind
    // the exact challenge bytes we received.
    let challenge = read_server_frame(&mut stream).await;
    assert_eq!(challenge.opcode, OPCODE_TEXT);
    let challenge_value: Value =
        serde_json::from_slice(&challenge.payload).expect("challenge is json");
    assert_eq!(challenge_value["type"], "challenge");
    assert_eq!(
        challenge_value["protocol_version"].as_u64(),
        Some(u64::from(WEB_SHARE_PROTOCOL_VERSION))
    );
    assert!(challenge_value["server_nonce"].as_str().is_some());
    let server_public = decode_public(
        challenge_value["server_public"]
            .as_str()
            .expect("challenge has server public"),
    );
    // Decapsulate the server ML-KEM ciphertext into the hybrid shared secret.
    let ml_kem_ct = decode_ml_kem_ct(
        challenge_value["server_ml_kem_ct"]
            .as_str()
            .expect("challenge has ml-kem ciphertext"),
    );
    let ml_kem_ss = ml_kem.decapsulate(&ml_kem_ct);

    // Complete the DH and derive the hybrid client session over the EXACT
    // hello + challenge transcript bytes.
    let dh = client_eph.into_shared_secret(&server_public);
    let (mut sealer, opener) =
        derive_client_session(&psk, &dh, &ml_kem_ss, hello.as_bytes(), &challenge.payload)
            .expect("client crypto");
    write_client_binary_frame(&mut stream, &sealer.seal_text(auth).expect("seal auth")).await;

    HandshakeSession {
        stream,
        task,
        opener,
        sealer,
    }
}

/// Sends a v1 hello carrying `token_id` and returns the raw stream after the
/// upgrade. Used to exercise pre-challenge rejection paths (e.g. unknown token)
/// where the server collapses to `handshake_rejected` BEFORE emitting a
/// challenge.
async fn send_hello_only(
    handler: Arc<RequestHandler>,
    token_id: &str,
) -> (TcpStream, tokio::task::JoinHandle<io::Result<()>>) {
    let (mut stream, task) = websocket_client(handler).await;
    write_client_hello(&mut stream, token_id).await;
    (stream, task)
}

async fn write_client_hello(stream: &mut TcpStream, token_id: &str) {
    let client_public = generate_ephemeral().public_bytes();
    let ml_kem_ek = rmux_web_crypto::ml_kem::KeyPair::generate([0x33u8; 64]).encapsulation_key();
    let hello = format!(
        r#"{{"type":"hello","protocol_version":{},"capabilities":["e2ee-token-auth","terminal-palette-v1"],"token_id":"{}","client_nonce":"{}","client_public":"{}","client_ml_kem_ek":"{}"}}"#,
        WEB_SHARE_PROTOCOL_VERSION,
        token_id,
        TEST_CLIENT_NONCE,
        b64url(&client_public),
        b64url(&ml_kem_ek),
    );
    write_client_text_frame(stream, hello.as_bytes()).await;
}

/// Reads frames until a close frame is found and asserts its (code, reason).
async fn assert_close(stream: &mut TcpStream, code: u16, reason: &str) {
    for _ in 0..8 {
        let frame = read_server_frame(stream).await;
        if frame.opcode != OPCODE_CLOSE {
            continue;
        }
        assert!(
            frame.payload.len() >= 2,
            "close frame should include a status code"
        );
        assert_eq!(
            u16::from_be_bytes([frame.payload[0], frame.payload[1]]),
            code,
            "unexpected close code"
        );
        assert_eq!(
            String::from_utf8_lossy(&frame.payload[2..]),
            reason,
            "unexpected close reason"
        );
        return;
    }
    panic!("websocket did not close with {code} {reason}");
}

fn auth_text() -> String {
    format!(
        r#"{{"type":"auth","protocol_version":{},"capabilities":["e2ee-token-auth","terminal-palette-v1"]}}"#,
        WEB_SHARE_PROTOCOL_VERSION
    )
}

fn auth_text_with_pin(pin: &str) -> String {
    format!(
        r#"{{"type":"auth","protocol_version":{},"capabilities":["e2ee-token-auth","terminal-palette-v1"],"pin":"{}"}}"#,
        WEB_SHARE_PROTOCOL_VERSION, pin
    )
}

fn b64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn decode_public(value: &str) -> [u8; 32] {
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(value)
        .expect("server public is base64url");
    <[u8; 32]>::try_from(bytes.as_slice()).expect("server public is 32 bytes")
}

fn decode_ml_kem_ct(value: &str) -> [u8; rmux_web_crypto::ml_kem::CIPHERTEXT_LEN] {
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(value)
        .expect("ml-kem ciphertext is base64url");
    <[u8; rmux_web_crypto::ml_kem::CIPHERTEXT_LEN]>::try_from(bytes.as_slice())
        .expect("ml-kem ciphertext is 1088 bytes")
}

async fn raw_connection(
    handler: Arc<RequestHandler>,
    pre_auth: PreAuthQueue,
) -> (TcpStream, tokio::task::JoinHandle<io::Result<()>>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test listener");
    let addr = listener.local_addr().expect("listener addr");
    let client = TcpStream::connect(addr);
    let server = listener.accept();
    let (client, server) = tokio::join!(client, server);
    let client = client.expect("client connects");
    let (server, _) = server.expect("server accepts");
    let pre_auth_admission = pre_auth.try_register().expect("pre-auth slot");
    let task = tokio::spawn(serve_admitted_connection(
        server,
        handler,
        pre_auth_admission,
    ));
    (client, task)
}

async fn raw_peer_connection(
    handler: Arc<RequestHandler>,
    pre_auth: PreAuthQueue,
) -> Option<(TcpStream, tokio::task::JoinHandle<io::Result<()>>)> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test listener");
    let addr = listener.local_addr().expect("listener addr");
    let client = TcpStream::connect(addr);
    let server = listener.accept();
    let (client, server) = tokio::join!(client, server);
    let client = client.expect("client connects");
    let (server, peer_addr) = server.expect("server accepts");
    let pre_auth_admission = pre_auth.admit_peer(peer_addr.ip()).await?;
    let task = tokio::spawn(serve_admitted_connection(
        server,
        handler,
        pre_auth_admission,
    ));
    Some((client, task))
}

async fn wait_for_pending_pre_auth(queue: &PreAuthQueue, expected: usize) {
    timeout(Duration::from_secs(1), async {
        while queue.pending_count() != expected {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("pre-auth queue reached expected size");
}

async fn read_http_response(stream: &mut TcpStream) -> String {
    let mut buffer = Vec::new();
    loop {
        let mut byte = [0u8; 1];
        timeout(Duration::from_secs(2), stream.read_exact(&mut byte))
            .await
            .expect("HTTP response timeout")
            .expect("read HTTP response byte");
        buffer.push(byte[0]);
        if buffer.ends_with(b"\r\n\r\n") {
            return String::from_utf8_lossy(&buffer).into_owned();
        }
    }
}

async fn write_client_text_frame(stream: &mut TcpStream, payload: &[u8]) {
    write_client_frame(stream, OPCODE_TEXT, payload).await;
}

async fn write_client_binary_frame(stream: &mut TcpStream, payload: &[u8]) {
    write_client_frame(stream, OPCODE_BINARY, payload).await;
}

async fn write_client_frame(stream: &mut TcpStream, opcode: u8, payload: &[u8]) {
    let mask = [0x12, 0x34, 0x56, 0x78];
    let mut frame = Vec::with_capacity(14 + payload.len());
    frame.push(0x80 | opcode);
    push_client_frame_len(&mut frame, payload.len());
    frame.extend_from_slice(&mask);
    frame.extend(
        payload
            .iter()
            .enumerate()
            .map(|(index, byte)| byte ^ mask[index % mask.len()]),
    );
    stream
        .write_all(&frame)
        .await
        .expect("write websocket frame");
}

fn push_client_frame_len(frame: &mut Vec<u8>, len: usize) {
    if len < 126 {
        frame.push(0x80 | len as u8);
    } else if u16::try_from(len).is_ok() {
        frame.push(0x80 | 126);
        frame.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        frame.push(0x80 | 127);
        frame.extend_from_slice(&(len as u64).to_be_bytes());
    }
}

async fn read_encrypted_text(stream: &mut TcpStream, opener: &mut Opener) -> String {
    loop {
        let frame = read_server_frame(stream).await;
        match frame.opcode {
            OPCODE_BINARY => {
                if let Message::Text(text) = opener
                    .open(&frame.payload)
                    .expect("encrypted server text opens")
                {
                    return text;
                }
            }
            OPCODE_CLOSE => panic!("websocket closed before encrypted text frame"),
            opcode => panic!("unexpected websocket opcode {opcode} before encrypted text frame"),
        }
    }
}

async fn read_encrypted_binary_frame(
    stream: &mut TcpStream,
    opener: &mut Opener,
    label: &str,
) -> Vec<u8> {
    loop {
        let frame = read_server_frame(stream).await;
        match frame.opcode {
            OPCODE_BINARY => {
                if let Message::Binary(payload) = opener
                    .open(&frame.payload)
                    .expect("encrypted server binary opens")
                {
                    return payload;
                }
            }
            OPCODE_CLOSE => panic!("websocket closed before {label} frame"),
            opcode => panic!("unexpected websocket opcode {opcode} before {label} frame"),
        }
    }
}

async fn read_encrypted_binary_frame_with_prefix(
    stream: &mut TcpStream,
    opener: &mut Opener,
    prefix: u8,
    label: &str,
) -> Vec<u8> {
    for _ in 0..MAX_INTERLEAVED_WEBSOCKET_FRAMES {
        let frame = read_server_frame(stream).await;
        match frame.opcode {
            OPCODE_BINARY => {
                if let Message::Binary(payload) = opener
                    .open(&frame.payload)
                    .expect("encrypted server binary opens")
                {
                    if payload.first() == Some(&prefix) {
                        return payload;
                    }
                }
            }
            OPCODE_CLOSE => panic!("websocket closed before {label} frame"),
            opcode => panic!("unexpected websocket opcode {opcode} before {label} frame"),
        }
    }
    panic!("did not receive {label} frame");
}

async fn read_encrypted_binary_frame_with_prefix_containing(
    stream: &mut TcpStream,
    opener: &mut Opener,
    prefix: u8,
    needle: &[u8],
    label: &str,
) -> Vec<u8> {
    for _ in 0..MAX_INTERLEAVED_WEBSOCKET_FRAMES {
        let frame = read_server_frame(stream).await;
        match frame.opcode {
            OPCODE_BINARY => {
                if let Message::Binary(payload) = opener
                    .open(&frame.payload)
                    .expect("encrypted server binary opens")
                {
                    if payload.first() == Some(&prefix)
                        && payload.windows(needle.len()).any(|window| window == needle)
                    {
                        return payload;
                    }
                }
            }
            OPCODE_CLOSE => panic!("websocket closed before {label} frame"),
            opcode => panic!("unexpected websocket opcode {opcode} before {label} frame"),
        }
    }
    panic!("did not receive {label} frame");
}

async fn read_server_frame(stream: &mut TcpStream) -> ServerFrame {
    timeout(WEBSOCKET_FRAME_TIMEOUT, read_server_frame_inner(stream))
        .await
        .expect("websocket frame timeout")
        .expect("read websocket frame")
}

async fn read_server_frame_inner(stream: &mut TcpStream) -> io::Result<ServerFrame> {
    let mut head = [0u8; 2];
    stream.read_exact(&mut head).await?;
    let opcode = head[0] & 0x0f;
    let masked = head[1] & 0x80 != 0;
    assert!(!masked, "server frames must not be masked");
    let mut len = u64::from(head[1] & 0x7f);
    if len == 126 {
        let mut bytes = [0u8; 2];
        stream.read_exact(&mut bytes).await?;
        len = u64::from(u16::from_be_bytes(bytes));
    } else if len == 127 {
        let mut bytes = [0u8; 8];
        stream.read_exact(&mut bytes).await?;
        len = u64::from_be_bytes(bytes);
    }
    let mut payload = vec![0u8; len as usize];
    stream.read_exact(&mut payload).await?;
    Ok(ServerFrame { opcode, payload })
}

struct ServerFrame {
    opcode: u8,
    payload: Vec<u8>,
}

fn token_from_url(url: &str) -> String {
    url.split_once("#")
        .and_then(|(_, fragment)| {
            fragment.split('&').find_map(|param| {
                let (key, value) = param.split_once('=')?;
                (key == "t").then_some(value.to_owned())
            })
        })
        .expect("URL contains access token")
}

const OPCODE_TEXT: u8 = 0x1;
const OPCODE_BINARY: u8 = 0x2;
const OPCODE_CLOSE: u8 = 0x8;
const OPCODE_PING: u8 = 0x9;
const OPCODE_PONG: u8 = 0xa;
const TEST_CLIENT_NONCE: &str = "AQIDBAUGBwgJCgsMDQ4PEA";
const MAX_INTERLEAVED_WEBSOCKET_FRAMES: usize = 32;
