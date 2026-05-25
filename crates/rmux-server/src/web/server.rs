use std::collections::HashMap;
use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine;
use rmux_core::events::OutputCursorItem;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::{debug, warn};

use super::origin::origin_matches;
use super::websocket::{WebSocket, WebSocketMessage};
use crate::handler::{RequestHandler, WebPaneSnapshot};

const HTTP_READ_LIMIT: usize = 8 * 1024;
const OPERATOR_RATE_LIMIT: u16 = 200;

pub(crate) fn spawn(handler: Arc<RequestHandler>) {
    tokio::spawn(async move {
        if let Err(error) = serve(handler).await {
            warn!("web-share listener unavailable: {error}");
        }
    });
}

async fn serve(handler: Arc<RequestHandler>) -> io::Result<()> {
    let listener_config = handler.web_listener();
    let bind_addr = format!("{}:{}", listener_config.host, listener_config.port);
    let listener = TcpListener::bind(&bind_addr).await?;
    debug!("web-share listener bound to {bind_addr}");
    loop {
        let (stream, _) = listener.accept().await?;
        let handler = Arc::clone(&handler);
        tokio::spawn(async move {
            if let Err(error) = serve_connection(stream, handler).await {
                debug!("web-share connection ended: {error}");
            }
        });
    }
}

async fn serve_connection(mut stream: TcpStream, handler: Arc<RequestHandler>) -> io::Result<()> {
    let request = read_http_request(&mut stream).await?;
    if request.method != "GET" {
        return write_response(
            &mut stream,
            405,
            "text/plain; charset=utf-8",
            b"unsupported method\n",
        )
        .await;
    }
    if request.path == "/health" {
        return write_response(&mut stream, 200, "text/plain; charset=utf-8", b"ok\n").await;
    }
    if request.path == "/" || request.path.starts_with("/s/") {
        return write_response(
            &mut stream,
            200,
            "text/html; charset=utf-8",
            INDEX_HTML.as_bytes(),
        )
        .await;
    }
    if request.path.starts_with("/ws/") {
        let share_id = request
            .path
            .strip_prefix("/ws/")
            .expect("prefix checked")
            .to_owned();
        return serve_websocket(stream, request, handler, share_id).await;
    }
    write_response(
        &mut stream,
        404,
        "text/plain; charset=utf-8",
        b"not found\n",
    )
    .await
}

async fn serve_websocket(
    mut stream: TcpStream,
    request: HttpRequest,
    handler: Arc<RequestHandler>,
    share_id: String,
) -> io::Result<()> {
    let Some(key) = request.headers.get("sec-websocket-key") else {
        return write_response(
            &mut stream,
            400,
            "text/plain; charset=utf-8",
            b"missing websocket key\n",
        )
        .await;
    };
    let share_key = request.query.get("key").map(String::as_str).unwrap_or("");
    let mut pane = match handler.open_web_share(&share_id, share_key).await {
        Ok(pane) => pane,
        Err(_) => {
            return write_response(
                &mut stream,
                403,
                "text/plain; charset=utf-8",
                b"invalid web-share credentials\n",
            )
            .await;
        }
    };
    if let Some(origin) = request.headers.get("origin") {
        if !origin_matches(origin, pane.expected_origin()) {
            return write_response(
                &mut stream,
                403,
                "text/plain; charset=utf-8",
                b"web-share origin is not allowed\n",
            )
            .await;
        }
    }
    let mut socket = WebSocket::accept(stream, key).await?;
    send_snapshot(&mut socket, &pane.snapshot, pane.is_operator()).await?;
    let mut rate_limiter = OperatorRateLimiter::new();

    loop {
        tokio::select! {
            item = pane.output.recv() => {
                match item {
                    OutputCursorItem::Event(event) => {
                        send_output(&mut socket, event.bytes()).await?;
                    }
                    OutputCursorItem::Gap(gap) => {
                        send_resync(&mut socket, gap.missed_events()).await?;
                        let (snapshot, output) = handler
                            .web_resnapshot(pane.target())
                            .await
                            .map_err(|error| io::Error::other(error.to_string()))?;
                        pane.snapshot = snapshot;
                        pane.output = output;
                        send_snapshot(&mut socket, &pane.snapshot, pane.is_operator()).await?;
                    }
                }
            }
            message = socket.read_message() => {
                match message? {
                    WebSocketMessage::Text(text) => {
                        if !pane.is_operator() {
                            let _ = socket.write_close().await;
                            return Ok(());
                        }
                        if !rate_limiter.try_acquire() {
                            debug!(share_id, "operator_rate_limit_hit");
                            continue;
                        }
                        handle_client_message(&handler, &mut pane, &text).await?;
                    }
                    WebSocketMessage::Binary(bytes) => {
                        if !pane.is_operator() {
                            let _ = socket.write_close().await;
                            return Ok(());
                        }
                        if !rate_limiter.try_acquire() {
                            debug!(share_id, "operator_rate_limit_hit");
                            continue;
                        }
                        let text = String::from_utf8_lossy(&bytes).into_owned();
                        handler.web_send_text(pane.target(), text).await
                            .map_err(|error| io::Error::other(error.to_string()))?;
                    }
                    WebSocketMessage::Close => {
                        let _ = socket.write_close().await;
                        return Ok(());
                    }
                }
            }
        }
    }
}

async fn handle_client_message(
    handler: &RequestHandler,
    pane: &mut crate::handler::WebPaneStream,
    text: &str,
) -> io::Result<()> {
    let message = serde_json::from_str::<ClientMessage>(text)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    match message {
        ClientMessage::Input { text } if pane.is_operator() => handler
            .web_send_text(pane.target(), text)
            .await
            .map_err(|error| io::Error::other(error.to_string())),
        ClientMessage::Resize { cols, rows } if pane.is_operator() => handler
            .web_resize(pane.target(), cols, rows)
            .await
            .map_err(|error| io::Error::other(error.to_string())),
        ClientMessage::Release if pane.is_operator() => {
            pane.release_operator();
            Ok(())
        }
        _ => Ok(()),
    }
}

async fn send_snapshot(
    socket: &mut WebSocket,
    snapshot: &WebPaneSnapshot,
    writable: bool,
) -> io::Result<()> {
    let payload = ServerMessage::Snapshot { snapshot, writable };
    let text =
        serde_json::to_string(&payload).map_err(|error| io::Error::other(error.to_string()))?;
    socket.write_text(&text).await
}

async fn send_output(socket: &mut WebSocket, bytes: &[u8]) -> io::Result<()> {
    let payload = ServerMessage::Output {
        data: base64::engine::general_purpose::STANDARD.encode(bytes),
    };
    let text =
        serde_json::to_string(&payload).map_err(|error| io::Error::other(error.to_string()))?;
    socket.write_text(&text).await
}

async fn send_resync(socket: &mut WebSocket, missed_events: u64) -> io::Result<()> {
    let payload = ServerMessage::Resync { missed_events };
    let text =
        serde_json::to_string(&payload).map_err(|error| io::Error::other(error.to_string()))?;
    socket.write_text(&text).await
}

async fn read_http_request(stream: &mut TcpStream) -> io::Result<HttpRequest> {
    let mut buffer = Vec::new();
    loop {
        let mut chunk = [0u8; 1024];
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "connection closed before request",
            ));
        }
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
        if buffer.len() > HTTP_READ_LIMIT {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "HTTP request headers exceed rmux web limit",
            ));
        }
    }
    parse_http_request(&buffer)
}

fn parse_http_request(buffer: &[u8]) -> io::Result<HttpRequest> {
    let text = std::str::from_utf8(buffer)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    let head = text
        .split_once("\r\n\r\n")
        .map(|(head, _)| head)
        .unwrap_or(text);
    let mut lines = head.lines();
    let request_line = lines.next().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "HTTP request line is missing")
    })?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let target = parts.next().unwrap_or_default();
    let (path, query) = split_target(target);
    let mut headers = HashMap::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
        }
    }
    Ok(HttpRequest {
        method,
        path: path.to_owned(),
        query,
        headers,
    })
}

fn split_target(target: &str) -> (&str, HashMap<String, String>) {
    let Some((path, query)) = target.split_once('?') else {
        return (target, HashMap::new());
    };
    (path, parse_query(query))
}

fn parse_query(query: &str) -> HashMap<String, String> {
    query
        .split('&')
        .filter_map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            Some((percent_decode(key)?, percent_decode(value)?))
        })
        .collect()
}

fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => output.push(b' '),
            b'%' if index + 2 < bytes.len() => {
                let hi = hex_value(bytes[index + 1])?;
                let lo = hex_value(bytes[index + 2])?;
                output.push((hi << 4) | lo);
                index += 2;
            }
            byte => output.push(byte),
        }
        index += 1;
    }
    String::from_utf8(output).ok()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

async fn write_response(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Error",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         X-Content-Type-Options: nosniff\r\n\
         Connection: close\r\n\
         \r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(body).await
}

#[derive(Debug)]
struct HttpRequest {
    method: String,
    path: String,
    query: HashMap<String, String>,
    headers: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMessage {
    Input { text: String },
    Release,
    Resize { cols: u16, rows: u16 },
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ServerMessage<'a> {
    Snapshot {
        snapshot: &'a WebPaneSnapshot,
        writable: bool,
    },
    Output {
        data: String,
    },
    Resync {
        missed_events: u64,
    },
}

const INDEX_HTML: &str = include_str!("frontend/index.html");

struct OperatorRateLimiter {
    remaining: u16,
    window_started: Instant,
}

impl OperatorRateLimiter {
    fn new() -> Self {
        Self {
            remaining: OPERATOR_RATE_LIMIT,
            window_started: Instant::now(),
        }
    }

    fn try_acquire(&mut self) -> bool {
        if self.window_started.elapsed() >= Duration::from_secs(1) {
            self.remaining = OPERATOR_RATE_LIMIT;
            self.window_started = Instant::now();
        }
        if self.remaining == 0 {
            return false;
        }
        self.remaining -= 1;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_query, percent_decode, split_target};

    #[test]
    fn query_parser_decodes_key_values() {
        let query = parse_query("key=a%2Fb+c&empty=");
        assert_eq!(query.get("key").map(String::as_str), Some("a/b c"));
        assert_eq!(query.get("empty").map(String::as_str), Some(""));
    }

    #[test]
    fn target_split_preserves_path_without_query() {
        let (path, query) = split_target("/ws/share");
        assert_eq!(path, "/ws/share");
        assert!(query.is_empty());
        assert_eq!(percent_decode("a%20b").as_deref(), Some("a b"));
    }
}
