use std::collections::HashMap;
use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use rmux_core::events::OutputCursorItem;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::{sleep, timeout};
use tracing::{debug, info, warn};

use super::protocol::{
    close_for_auth_error, handle_client_text, handle_operator_binary_frame, read_auth_message,
    send_output, send_ready, send_revoked, send_snapshot, PRE_AUTH_TIMEOUT, UNIFORM_AUTH_DELAY,
};
use super::websocket::{WebSocket, WebSocketMessage};
use super::WebShareRevokeReason;
use crate::handler::RequestHandler;

const HTTP_READ_LIMIT: usize = 8 * 1024;
const OPERATOR_RATE_LIMIT: u16 = 200;
const PRE_AUTH_SLOTS: usize = 16;

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
    let pre_auth = Arc::new(Semaphore::new(PRE_AUTH_SLOTS));
    debug!("web-share listener bound to {bind_addr}");
    loop {
        let (stream, _) = listener.accept().await?;
        let handler = Arc::clone(&handler);
        let pre_auth = Arc::clone(&pre_auth);
        tokio::spawn(async move {
            if let Err(error) = serve_connection(stream, handler, pre_auth).await {
                debug!("web-share connection ended: {error}");
            }
        });
    }
}

async fn serve_connection(
    mut stream: TcpStream,
    handler: Arc<RequestHandler>,
    pre_auth: Arc<Semaphore>,
) -> io::Result<()> {
    let pre_auth_permit = match pre_auth.try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            return write_response(
                &mut stream,
                503,
                "text/plain; charset=utf-8",
                b"too many pending web-share auth connections\n",
            )
            .await;
        }
    };
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
    if request.method != "GET" && request.method != "HEAD" {
        drop(pre_auth_permit);
        return write_response(
            &mut stream,
            405,
            "text/plain; charset=utf-8",
            b"unsupported method\n",
        )
        .await;
    }
    if request.method == "GET" && request.path == "/share" && request.is_websocket_upgrade() {
        return serve_websocket(stream, request, handler, pre_auth_permit).await;
    }
    drop(pre_auth_permit);
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
    pre_auth_permit: OwnedSemaphorePermit,
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
    drop(pre_auth_permit);
    let mut pane = match handler
        .open_web_share(
            &auth.id,
            &auth.key,
            auth.pin.as_deref(),
            auth.role.connect_role(),
        )
        .await
    {
        Ok(pane) => pane,
        Err(error) => {
            sleep(UNIFORM_AUTH_DELAY).await;
            let (code, reason) = close_for_auth_error(&error.to_string());
            info!(
                share_id = %auth.id,
                close_code = code,
                reason,
                "web_share_auth_failed"
            );
            let _ = socket.write_close_code(code, reason).await;
            return Ok(());
        }
    };
    if let Some(origin) = request.headers.get("origin") {
        if !pane.origin_allowed(origin) {
            sleep(UNIFORM_AUTH_DELAY).await;
            let _ = socket.write_close_code(4004, "origin_not_allowed").await;
            return Ok(());
        }
    }
    sleep(UNIFORM_AUTH_DELAY).await;
    info!(
        share_id = %auth.id,
        role = auth.role.as_str(),
        "web_share_auth_ok"
    );
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
                        debug!(missed = gap.missed_events(), "web-share read resync");
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
                            let _ = socket.write_close_code(4006, "read_no_binary").await;
                            return Ok(());
                        }
                        if !rate_limiter.try_acquire() {
                            info!(share_id = %auth.id, "web_share_operator_rate_limit_hit");
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
    let path = path_from_target(target);
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
        headers,
    })
}

fn path_from_target(target: &str) -> &str {
    target.split_once('?').map_or(target, |(path, _)| path)
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
        431 => "Request Header Fields Too Large",
        503 => "Service Unavailable",
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
    headers: HashMap<String, String>,
}

impl HttpRequest {
    fn is_websocket_upgrade(&self) -> bool {
        self.headers
            .get("upgrade")
            .is_some_and(|value| value.eq_ignore_ascii_case("websocket"))
            && self
                .headers
                .get("connection")
                .is_some_and(|value| has_header_token(value, "upgrade"))
    }
}

fn has_header_token(value: &str, expected: &str) -> bool {
    value
        .split(',')
        .any(|token| token.trim().eq_ignore_ascii_case(expected))
}

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
            let response =
                response_for(format!("GET {target} HTTP/1.1\r\nHost: local\r\n\r\n")).await;
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
            "\r\n"
        );
        let response = response_for(request).await;
        assert!(response.starts_with("HTTP/1.1 101 Switching Protocols"));
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
}
