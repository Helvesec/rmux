use super::{path_from_target, serve_connection, HttpRequest};
use crate::handler::RequestHandler;
use crate::web::protocol::WEB_SHARE_PROTOCOL_VERSION;
use rmux_proto::{
    CreateWebShareRequest, NewSessionRequest, PaneTarget, Request, Response, SessionName,
    StopWebShareRequest, TerminalSize, WebShareRequest, WebShareResponse, WebShareScope,
};
use serde_json::Value;
use std::collections::HashMap;
use std::io;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio::time::{timeout, Duration};

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
async fn non_get_head_methods_return_405() {
    let response = response_for("POST /share HTTP/1.1\r\nHost: local\r\n\r\n").await;
    assert!(response.starts_with("HTTP/1.1 405 Method Not Allowed"));
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
    let session_name = SessionName::new("websocket-e2e").expect("valid session");
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

    let created = handler
        .handle(Request::WebShare(WebShareRequest::Create(
            CreateWebShareRequest {
                scope: WebShareScope::Pane(PaneTarget::new(session_name, 0).into()),
                public_base_url: Some("https://terminal.example".to_owned()),
                frontend_url: None,
                ttl_seconds: Some(60),
                max_readers: Some(1),
                url_options: Default::default(),
                require_pin: false,
                terminal_palette: None,
                writable: true,
                controls: false,
            },
        )))
        .await;
    let Response::WebShare(WebShareResponse::Created(created)) = created else {
        panic!("expected web share creation");
    };
    let operator_url = created.operator_url.as_deref().expect("operator URL");
    let operator_token = token_from_url(operator_url);

    let (mut client, server_task) = websocket_client(Arc::clone(&handler)).await;
    let auth = format!(
        r#"{{"type":"auth","protocol_version":{},"capabilities":["token-auth","operator-release-ack","terminal-palette-v1"],"token":"{}"}}"#,
        WEB_SHARE_PROTOCOL_VERSION, operator_token
    );
    write_client_text_frame(&mut client, auth.as_bytes()).await;

    let ready = read_server_frame(&mut client).await;
    assert_eq!(ready.opcode, OPCODE_TEXT);
    let ready: Value = serde_json::from_slice(&ready.payload).expect("ready is json");
    assert_eq!(ready["type"], "ready");
    assert_eq!(
        ready["protocol_version"].as_u64(),
        Some(u64::from(WEB_SHARE_PROTOCOL_VERSION))
    );
    assert_eq!(ready["scope"], "pane");
    assert_eq!(ready["role"], "operator");
    assert_eq!(ready["writable"], true);
    assert!(ready["capabilities"]
        .as_array()
        .expect("capabilities array")
        .iter()
        .any(|capability| capability == "token-auth"));

    let snapshot = read_server_frame(&mut client).await;
    assert_eq!(snapshot.opcode, OPCODE_BINARY);
    assert_eq!(snapshot.payload.first(), Some(&0x10));

    write_client_binary_frame(&mut client, &[0x80, b'p', b'w', b'd', b'\n']).await;
    let stopped = handler
        .handle(Request::WebShare(WebShareRequest::Stop(
            StopWebShareRequest {
                share_id: created.share_id,
            },
        )))
        .await;
    assert!(matches!(
        stopped,
        Response::WebShare(WebShareResponse::Stopped(_))
    ));

    let revoked = read_until_text_or_close(&mut client).await;
    assert_eq!(revoked["type"], "share_revoked");
    assert_eq!(revoked["reason"], "stopped_by_owner");

    drop(client);
    let _ = server_task.await.expect("server task joins");
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
    let task = tokio::spawn(serve_connection(
        server,
        Arc::new(RequestHandler::new()),
        Arc::new(Semaphore::new(16)),
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
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test listener");
    let addr = listener.local_addr().expect("listener addr");
    let client = TcpStream::connect(addr);
    let server = listener.accept();
    let (client, server) = tokio::join!(client, server);
    let mut client = client.expect("client connects");
    let (server, _) = server.expect("server accepts");
    let task = tokio::spawn(serve_connection(
        server,
        handler,
        Arc::new(Semaphore::new(16)),
    ));
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

async fn read_until_text_or_close(stream: &mut TcpStream) -> Value {
    loop {
        let frame = read_server_frame(stream).await;
        match frame.opcode {
            OPCODE_TEXT => return serde_json::from_slice(&frame.payload).expect("text frame json"),
            OPCODE_BINARY => continue,
            OPCODE_CLOSE => panic!("websocket closed before text frame"),
            opcode => panic!("unexpected websocket opcode {opcode}"),
        }
    }
}

async fn read_server_frame(stream: &mut TcpStream) -> ServerFrame {
    timeout(Duration::from_secs(2), read_server_frame_inner(stream))
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
    url.split_once("token=")
        .map(|(_, token)| token.split('&').next().unwrap_or(token).to_owned())
        .expect("URL contains access token")
}

const OPCODE_TEXT: u8 = 0x1;
const OPCODE_BINARY: u8 = 0x2;
const OPCODE_CLOSE: u8 = 0x8;
