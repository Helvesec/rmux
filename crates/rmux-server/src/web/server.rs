use std::collections::HashMap;
use std::future::Future;
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
    close_for_auth_error, handle_pane_client_text, handle_pane_operator_binary_frame,
    handle_session_client_text, handle_session_operator_binary_frame, read_auth_message,
    send_output, send_ready, send_revoked, send_snapshot, PRE_AUTH_TIMEOUT, UNIFORM_AUTH_DELAY,
};
use super::websocket::{valid_client_key, WebSocket, WebSocketMessage};
use super::WebShareRevokeReason;
use crate::handler::{RequestHandler, WebPaneStream, WebSessionStream, WebShareStream};

const HTTP_READ_LIMIT: usize = 8 * 1024;
const OPERATOR_RATE_LIMIT: u16 = 200;
const PRE_AUTH_SLOTS: usize = 16;
const WEB_WRITE_TIMEOUT: Duration = Duration::from_secs(2);

pub(crate) async fn spawn(handler: Arc<RequestHandler>) {
    let listener_config = handler.web_listener();
    let bind_addr = format!("{}:{}", listener_config.host, listener_config.port);
    let listener = match TcpListener::bind(&bind_addr).await {
        Ok(listener) => listener,
        Err(error) => {
            handler.mark_web_listener_unavailable(error.to_string());
            warn!("web-share listener unavailable: {error}");
            return;
        }
    };
    handler.mark_web_listener_available();
    let task_handler = Arc::clone(&handler);
    tokio::spawn(async move {
        if let Err(error) = serve(handler, listener, bind_addr).await {
            task_handler.mark_web_listener_unavailable(error.to_string());
            warn!("web-share listener stopped: {error}");
        }
    });
}

async fn serve(
    handler: Arc<RequestHandler>,
    listener: TcpListener,
    bind_addr: String,
) -> io::Result<()> {
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
    if request
        .headers
        .get("sec-websocket-version")
        .is_none_or(|version| version.trim() != "13")
    {
        return write_response(
            &mut stream,
            400,
            "text/plain; charset=utf-8",
            b"unsupported websocket version\n",
        )
        .await;
    }
    if !valid_client_key(key) {
        return write_response(
            &mut stream,
            400,
            "text/plain; charset=utf-8",
            b"invalid websocket key\n",
        )
        .await;
    }
    let mut socket = WebSocket::accept(stream, key).await?;
    let Some(origin) = request.headers.get("origin") else {
        sleep(UNIFORM_AUTH_DELAY).await;
        let _ = socket.write_close_code(4004, "origin_required").await;
        return Ok(());
    };
    let auth = match read_auth_message(&mut socket).await {
        Ok(auth) => auth,
        Err((code, reason)) => {
            sleep(UNIFORM_AUTH_DELAY).await;
            let _ = socket.write_close_code(code, reason).await;
            return Ok(());
        }
    };
    if handler
        .known_web_share_origin_allowed(&auth.token, origin)
        .is_some_and(|allowed| !allowed)
    {
        sleep(UNIFORM_AUTH_DELAY).await;
        let _ = socket.write_close_code(4004, "origin_not_allowed").await;
        return Ok(());
    }
    drop(pre_auth_permit);
    let share = match handler
        .open_web_share(&auth.token, auth.pin.as_deref())
        .await
    {
        Ok(pane) => pane,
        Err(error) => {
            sleep(UNIFORM_AUTH_DELAY).await;
            let (code, reason) = close_for_auth_error(&error.to_string());
            info!(close_code = code, reason, "web_share_auth_failed");
            let _ = socket.write_close_code(code, reason).await;
            return Ok(());
        }
    };
    let share_id = share.share_id().to_owned();
    if !share.origin_allowed(origin) {
        sleep(UNIFORM_AUTH_DELAY).await;
        let _ = socket.write_close_code(4004, "origin_not_allowed").await;
        return Ok(());
    }
    sleep(UNIFORM_AUTH_DELAY).await;
    info!(
        share_id = %share_id,
        role = share.role(),
        "web_share_auth_ok"
    );
    write_with_timeout(send_ready(&mut socket, &share)).await?;
    match share {
        WebShareStream::Pane(pane) => serve_pane_loop(handler, socket, share_id, *pane).await,
        WebShareStream::Session(session) => {
            serve_session_loop(handler, socket, share_id, *session).await
        }
    }
}

async fn serve_pane_loop(
    handler: Arc<RequestHandler>,
    mut socket: WebSocket,
    share_id: String,
    mut pane: WebPaneStream,
) -> io::Result<()> {
    write_with_timeout(send_snapshot(&mut socket, &pane.snapshot)).await?;
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
                        write_with_timeout(send_output(&mut socket, event.bytes())).await?;
                    }
                    OutputCursorItem::Gap(gap) => {
                        debug!(missed = gap.missed_events(), "web-share read resync");
                        let (snapshot, output) = handler
                            .web_resnapshot(pane.target())
                            .await
                            .map_err(|error| io::Error::other(error.to_string()))?;
                        pane.snapshot = snapshot;
                        pane.output = output;
                        write_with_timeout(send_snapshot(&mut socket, &pane.snapshot)).await?;
                    }
                }
            }
            message = socket.read_message() => {
                match message? {
                    WebSocketMessage::Text(text) => {
                        handle_pane_client_text(&mut socket, &mut pane, &text).await?;
                    }
                    WebSocketMessage::Binary(bytes) => {
                        if !pane.is_operator() {
                            let _ = socket.write_close_code(4006, "read_no_binary").await;
                            return Ok(());
                        }
                        if !rate_limiter.try_acquire() {
                            info!(share_id = %share_id, "web_share_operator_rate_limit_hit");
                            continue;
                        }
                        handle_pane_operator_binary_frame(&handler, &mut socket, &pane, &bytes).await?;
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
                        notify_revoked_and_close(&mut socket, reason).await?;
                        return Ok(());
                    }
                }
            }
            _ = ttl_sleep.as_mut() => {
                notify_revoked_and_close(&mut socket, WebShareRevokeReason::TtlExpired).await?;
                return Ok(());
            }
            _ = alive_tick.tick() => {
                if !handler.web_target_alive(pane.target()).await {
                    notify_revoked_and_close(&mut socket, WebShareRevokeReason::PaneGone).await?;
                    return Ok(());
                }
            }
        }
    }
}

async fn serve_session_loop(
    handler: Arc<RequestHandler>,
    mut socket: WebSocket,
    share_id: String,
    mut session: WebSessionStream,
) -> io::Result<()> {
    let mut attach_reader = session.take_attach_reader();
    let mut rate_limiter = OperatorRateLimiter::new();
    let mut alive_tick = tokio::time::interval(Duration::from_millis(500));
    let ttl_delay = session
        .expires_at()
        .map(duration_until)
        .unwrap_or_else(|| Duration::from_secs(365 * 24 * 60 * 60));
    let ttl_sleep = sleep(ttl_delay);
    tokio::pin!(ttl_sleep);

    loop {
        tokio::select! {
            output = attach_reader.read_attach_bytes() => {
                match output? {
                    Some(bytes) => write_with_timeout(send_output(&mut socket, &bytes)).await?,
                    None => return Ok(()),
                }
            }
            message = socket.read_message() => {
                match message? {
                    WebSocketMessage::Text(text) => {
                        handle_session_client_text(handler.as_ref(), &mut socket, &mut session, &text).await?;
                    }
                    WebSocketMessage::Binary(bytes) => {
                        if !session.is_operator() {
                            let _ = socket.write_close_code(4006, "read_no_binary").await;
                            return Ok(());
                        }
                        if !rate_limiter.try_acquire() {
                            info!(share_id = %share_id, "web_share_operator_rate_limit_hit");
                            continue;
                        }
                        handle_session_operator_binary_frame(&handler, &mut socket, &mut session, &bytes).await?;
                    }
                    WebSocketMessage::Close => {
                        let _ = socket.write_close().await;
                        return Ok(());
                    }
                }
            }
            changed = session.revoke_rx.changed() => {
                if changed.is_ok() {
                    let reason = *session.revoke_rx.borrow();
                    if let Some(reason) = reason {
                        notify_revoked_and_close(&mut socket, reason).await?;
                        return Ok(());
                    }
                }
            }
            _ = ttl_sleep.as_mut() => {
                notify_revoked_and_close(&mut socket, WebShareRevokeReason::TtlExpired).await?;
                return Ok(());
            }
            _ = alive_tick.tick() => {
                if !handler.web_session_alive(session.target()).await {
                    notify_revoked_and_close(&mut socket, WebShareRevokeReason::SessionGone).await?;
                    return Ok(());
                }
            }
        }
    }
}

async fn notify_revoked_and_close(
    socket: &mut WebSocket,
    reason: WebShareRevokeReason,
) -> io::Result<()> {
    let _ = write_with_timeout(send_revoked(socket, reason)).await;
    let _ = write_with_timeout(socket.write_close_code(1000, reason.as_str())).await;
    Ok(())
}

async fn write_with_timeout<F>(operation: F) -> io::Result<()>
where
    F: Future<Output = io::Result<()>>,
{
    match timeout(WEB_WRITE_TIMEOUT, operation).await {
        Ok(result) => result,
        Err(_) => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "web-share client write timed out",
        )),
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
#[path = "server_tests.rs"]
mod tests;
