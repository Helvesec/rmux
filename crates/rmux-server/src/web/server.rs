use std::collections::HashMap;
use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use rmux_core::events::OutputCursorItem;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::{sleep, timeout};
use tracing::{debug, warn};

use super::origin::origin_matches;
use super::protocol::{
    close_for_auth_error, handle_client_text, handle_operator_binary_frame, read_auth_message,
    send_output, send_ready, send_revoked, send_snapshot, PRE_AUTH_TIMEOUT, UNIFORM_AUTH_DELAY,
};
use super::websocket::{WebSocket, WebSocketMessage};
use super::WebShareRevokeReason;
use crate::handler::RequestHandler;

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
    let request = match timeout(PRE_AUTH_TIMEOUT, read_http_request(&mut stream)).await {
        Ok(Ok(request)) => request,
        Ok(Err(error)) if error.kind() == io::ErrorKind::InvalidData => {
            return write_response(
                &mut stream,
                431,
                "text/plain; charset=utf-8",
                b"request headers too large or invalid\n",
            )
            .await;
        }
        Ok(Err(error)) => return Err(error),
        Err(_) => return Ok(()),
    };
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
    if request.path == "/share" {
        return serve_websocket(stream, request, handler).await;
    }
    if request.path.starts_with("/ws/") {
        return write_response(
            &mut stream,
            410,
            "text/plain; charset=utf-8",
            b"legacy web-share websocket path is gone; use /share\n",
        )
        .await;
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
    let mut socket = WebSocket::accept(stream, key).await?;
    let auth = match read_auth_message(&mut socket).await {
        Ok(auth) => auth,
        Err((code, reason)) => {
            sleep(UNIFORM_AUTH_DELAY).await;
            let _ = socket.write_close_code(code, reason).await;
            return Ok(());
        }
    };
    let mut pane = match handler
        .open_web_share(&auth.id, &auth.key, auth.role.connect_role())
        .await
    {
        Ok(pane) => pane,
        Err(error) => {
            sleep(UNIFORM_AUTH_DELAY).await;
            let (code, reason) = close_for_auth_error(&error.to_string());
            let _ = socket.write_close_code(code, reason).await;
            return Ok(());
        }
    };
    if let Some(origin) = request.headers.get("origin") {
        if !origin_matches(origin, pane.expected_origin()) {
            sleep(UNIFORM_AUTH_DELAY).await;
            let _ = socket.write_close_code(4004, "origin_not_allowed").await;
            return Ok(());
        }
    }
    sleep(UNIFORM_AUTH_DELAY).await;
    send_ready(&mut socket, &pane, auth.role.as_str()).await?;
    send_snapshot(&mut socket, &pane.snapshot).await?;
    let mut rate_limiter = OperatorRateLimiter::new();
    let mut alive_tick = tokio::time::interval(Duration::from_millis(500));
    let ttl_delay = pane
        .expires_at()
        .map(duration_until)
        .unwrap_or_else(|| Duration::from_secs(365 * 24 * 60 * 60));
    let ttl_sleep = sleep(ttl_delay);
    tokio::pin!(ttl_sleep);

    loop {
        tokio::select! {
            item = pane.output.recv() => {
                match item {
                    OutputCursorItem::Event(event) => {
                        send_output(&mut socket, event.bytes()).await?;
                    }
                    OutputCursorItem::Gap(gap) => {
                        debug!(missed = gap.missed_events(), "web-share viewer resync");
                        let (snapshot, output) = handler
                            .web_resnapshot(pane.target())
                            .await
                            .map_err(|error| io::Error::other(error.to_string()))?;
                        pane.snapshot = snapshot;
                        pane.output = output;
                        send_snapshot(&mut socket, &pane.snapshot).await?;
                    }
                }
            }
            message = socket.read_message() => {
                match message? {
                    WebSocketMessage::Text(text) => {
                        handle_client_text(&mut socket, &mut pane, &text).await?;
                    }
                    WebSocketMessage::Binary(bytes) => {
                        if !pane.is_operator() {
                            let _ = socket.write_close_code(4006, "viewer_no_binary").await;
                            return Ok(());
                        }
                        if !rate_limiter.try_acquire() {
                            debug!(share_id = auth.id, "operator_rate_limit_hit");
                            continue;
                        }
                        handle_operator_binary_frame(&handler, &mut socket, &pane, &bytes).await?;
                    }
                    WebSocketMessage::Close => {
                        let _ = socket.write_close().await;
                        return Ok(());
                    }
                }
            }
            changed = pane.revoke_rx.changed() => {
                if changed.is_ok() {
                    let reason = *pane.revoke_rx.borrow();
                    if let Some(reason) = reason {
                        send_revoked(&mut socket, reason).await?;
                        let _ = socket.write_close_code(1000, reason.as_str()).await;
                        return Ok(());
                    }
                }
            }
            _ = ttl_sleep.as_mut() => {
                send_revoked(&mut socket, WebShareRevokeReason::TtlExpired).await?;
                let _ = socket.write_close_code(1000, "ttl_expired").await;
                return Ok(());
            }
            _ = alive_tick.tick() => {
                if !handler.web_target_alive(pane.target()).await {
                    send_revoked(&mut socket, WebShareRevokeReason::PaneGone).await?;
                    let _ = socket.write_close_code(1000, "pane_gone").await;
                    return Ok(());
                }
            }
        }
    }
}

fn duration_until(deadline: SystemTime) -> Duration {
    deadline
        .duration_since(SystemTime::now())
        .unwrap_or(Duration::ZERO)
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
        if buffer.len() > HTTP_READ_LIMIT {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "HTTP request headers exceed rmux web limit",
            ));
        }
        if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    parse_http_request(&buffer)
}

fn parse_http_request(buffer: &[u8]) -> io::Result<HttpRequest> {
    let mut headers = [httparse::EMPTY_HEADER; 64];
    let mut request = httparse::Request::new(&mut headers);
    let status = request
        .parse(buffer)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    if !status.is_complete() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "incomplete HTTP request",
        ));
    }
    let method = request.method.unwrap_or_default().to_owned();
    let target = request.path.unwrap_or_default();
    let (path, query) = split_target(target);
    let headers = request
        .headers
        .iter()
        .map(|header| {
            let value = String::from_utf8_lossy(header.value).trim().to_owned();
            (header.name.to_ascii_lowercase(), value)
        })
        .collect();
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
        410 => "Gone",
        431 => "Request Header Fields Too Large",
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
