use super::{path_from_target, serve_connection, HttpRequest};
use crate::handler::RequestHandler;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;

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
