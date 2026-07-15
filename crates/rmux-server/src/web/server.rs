use std::future::Future;
use std::io;
use std::sync::Arc;
use std::time::Duration;

use tokio::net::{TcpListener, TcpStream};
use tokio::time::{sleep, timeout};
use tracing::{debug, info, warn};

mod http;
mod keepalive;
mod pre_auth;
mod rate_limit;
mod streams;

use super::outbound::WebSocketOutbound;
use super::protocol::{
    build_challenge, close_for_auth_error, read_auth_message, read_client_hello, send_ready,
    send_text, HANDSHAKE_REJECTED, PRE_AUTH_TIMEOUT, UNIFORM_AUTH_DELAY,
};
use super::websocket::{valid_client_key, WebSocket};
use super::{crypto, crypto::EncryptedWebSocketReader};
use crate::handler::{RequestHandler, WebShareStream};
use http::{read_http_request, write_response, HttpRequest};
use pre_auth::{PreAuthGuard, PreAuthQueue};
use streams::{serve_pane_loop, serve_session_loop};

const PRE_AUTH_SLOTS: usize = 64;
const PRE_AUTH_SLOTS_PER_IP: usize = 4;
const PRE_READY_TIMEOUT: Duration = Duration::from_secs(8);
const WEB_WRITE_TIMEOUT: Duration = Duration::from_secs(2);
const FD_EXHAUSTION_ACCEPT_BACKOFF: Duration = Duration::from_millis(250);

struct EstablishedWebShare {
    socket: EncryptedWebSocketReader,
    outbound: WebSocketOutbound,
    share_id: String,
    share: WebShareStream,
    supports_session_pane_frame: bool,
}

struct PreReadyWebShare {
    socket: WebSocket,
    origin: String,
    token_id: String,
    auth_pin: Option<String>,
    supports_session_pane_frame: bool,
    opener: crypto::FrameOpener,
    pre_auth_guard: PreAuthGuard,
    sealer: crypto::FrameSealer,
}

pub(crate) async fn spawn(handler: Arc<RequestHandler>) -> io::Result<()> {
    let settings = handler.web_settings();
    let bind_addr = format!("{}:{}", settings.host, settings.port);
    let (listener, bind_addr) = match TcpListener::bind(&bind_addr).await {
        Ok(listener) => (listener, bind_addr),
        Err(error)
            if settings.allows_automatic_port_fallback()
                && error.kind() == io::ErrorKind::AddrInUse =>
        {
            let fallback_addr = format!("{}:0", settings.host);
            let listener = match TcpListener::bind(&fallback_addr).await {
                Ok(listener) => listener,
                Err(error) => {
                    handler.mark_web_listener_unavailable(error.to_string());
                    warn!("web-share listener unavailable: {error}");
                    return Err(error);
                }
            };
            let actual_port = listener.local_addr()?.port();
            handler.update_web_listener_port(actual_port);
            let bind_addr = format!("{}:{}", settings.host, actual_port);
            (listener, bind_addr)
        }
        Err(error) => {
            handler.mark_web_listener_unavailable(error.to_string());
            warn!("web-share listener unavailable: {error}");
            return Err(error);
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
    Ok(())
}

async fn serve(
    handler: Arc<RequestHandler>,
    listener: TcpListener,
    bind_addr: String,
) -> io::Result<()> {
    let pre_auth = PreAuthQueue::with_per_ip_capacity(PRE_AUTH_SLOTS, PRE_AUTH_SLOTS_PER_IP);
    debug!("web-share listener bound to {bind_addr}");
    loop {
        let (stream, peer_addr) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(error) if is_fd_exhaustion(&error) => {
                warn!(
                    ?error,
                    "web-share listener accept hit fd limit; backing off"
                );
                sleep(FD_EXHAUSTION_ACCEPT_BACKOFF).await;
                continue;
            }
            Err(error) if should_continue_accept_loop(&error) => {
                warn!(?error, "web-share listener accept failed; continuing");
                sleep(Duration::from_millis(50)).await;
                continue;
            }
            Err(error) => return Err(error),
        };
        if let Err(error) = stream.set_nodelay(true) {
            warn!(%peer_addr, ?error, "failed to enable TCP_NODELAY for web-share client");
        }
        let Some(pre_auth_guard) = pre_auth.try_register_peer(peer_addr.ip()) else {
            debug!(
                %peer_addr,
                "web-share pre-auth capacity reached; closing pending connection"
            );
            continue;
        };
        let handler = Arc::clone(&handler);
        tokio::spawn(async move {
            if let Err(error) = serve_connection(stream, handler, pre_auth_guard).await {
                debug!("web-share connection ended: {error}");
            }
        });
    }
}

async fn serve_connection(
    mut stream: TcpStream,
    handler: Arc<RequestHandler>,
    pre_auth_guard: PreAuthGuard,
) -> io::Result<()> {
    let request = match timeout(PRE_AUTH_TIMEOUT, read_http_request(&mut stream)).await {
        Ok(Ok(request)) => request,
        Ok(Err(error)) if error.kind() == io::ErrorKind::InvalidData => {
            return write_response(
                &mut stream,
                431,
                "text/plain; charset=utf-8",
                b"request headers too large or invalid\n",
                true,
            )
            .await;
        }
        Ok(Err(error)) => return Err(error),
        Err(_) => return Ok(()),
    };
    if request.method != "GET" && request.method != "HEAD" {
        return write_response(
            &mut stream,
            405,
            "text/plain; charset=utf-8",
            b"unsupported method\n",
            true,
        )
        .await;
    }
    if request.method == "GET" && request.path == "/share" && request.is_websocket_upgrade() {
        return serve_websocket(stream, request, handler, pre_auth_guard).await;
    }
    write_response(
        &mut stream,
        404,
        "text/plain; charset=utf-8",
        b"not found\n",
        request.method != "HEAD",
    )
    .await
}

async fn serve_websocket(
    mut stream: TcpStream,
    request: HttpRequest,
    handler: Arc<RequestHandler>,
    pre_auth_guard: PreAuthGuard,
) -> io::Result<()> {
    let Some(key) = request.headers.get("sec-websocket-key") else {
        return write_response(
            &mut stream,
            400,
            "text/plain; charset=utf-8",
            b"missing websocket key\n",
            true,
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
            true,
        )
        .await;
    }
    if !valid_client_key(key) {
        return write_response(
            &mut stream,
            400,
            "text/plain; charset=utf-8",
            b"invalid websocket key\n",
            true,
        )
        .await;
    }
    let key = key.to_owned();
    let established =
        match establish_web_share(stream, request, key, Arc::clone(&handler), pre_auth_guard).await
        {
            Ok(Some(established)) => established,
            Ok(None) => return Ok(()),
            Err(error) => return Err(error),
        };
    match established.share {
        WebShareStream::Pane(pane) => {
            serve_pane_loop(
                handler,
                established.socket,
                established.outbound,
                established.share_id,
                *pane,
            )
            .await
        }
        WebShareStream::Session(session) => {
            serve_session_loop(
                handler,
                established.socket,
                established.outbound,
                established.share_id,
                *session,
                established.supports_session_pane_frame,
            )
            .await
        }
    }
}

async fn establish_web_share(
    stream: TcpStream,
    request: HttpRequest,
    key: String,
    handler: Arc<RequestHandler>,
    pre_auth_guard: PreAuthGuard,
) -> io::Result<Option<EstablishedWebShare>> {
    let peer_ip = stream.peer_addr().ok().map(|address| address.ip());
    let pre_ready = match timeout(
        PRE_READY_TIMEOUT,
        complete_pre_ready_handshake(stream, request, key, Arc::clone(&handler), pre_auth_guard),
    )
    .await
    {
        Ok(Ok(Some(pre_ready))) => pre_ready,
        Ok(Ok(None)) => return Ok(None),
        Ok(Err(error)) => return Err(error),
        Err(_) => {
            debug!("web-share pre-ready handshake timed out");
            return Ok(None);
        }
    };
    let PreReadyWebShare {
        mut socket,
        origin,
        token_id,
        auth_pin,
        supports_session_pane_frame,
        opener,
        pre_auth_guard,
        sealer,
    } = pre_ready;

    // Authenticate against the registry outside PRE_READY_TIMEOUT. The registry
    // backoff may intentionally sleep longer than the pre-ready budget, and its
    // cancellation-safe guard owns in-flight accounting for that phase.
    // Transfer this socket atomically from the pre-auth queue to the bounded
    // post-handshake queue. If the latter is full, retain the pre-auth guard
    // through the uniform rejection delay so overflow tasks remain bounded too.
    let auth_wait = match handler.reserve_web_share_authentication(&token_id, peer_ip) {
        Ok(auth_wait) => {
            drop(pre_auth_guard);
            auth_wait
        }
        Err(error) => {
            let message = error.to_string();
            let close = close_for_auth_error(&message);
            reject_handshake_with_close(&mut socket, close.reason, close.wire_close).await?;
            return Ok(None);
        }
    };
    let share = match handler
        .open_web_share_token_id_with_wait(&token_id, auth_pin.as_deref(), &auth_wait)
        .await
    {
        Ok(pane) => pane,
        Err(error) => {
            let message = error.to_string();
            // A valid token that omitted the pairing code: signal it distinctly
            // so the client can prompt. Safe — this path is only reachable AFTER
            // the token-authenticated handshake (a peer without the token cannot
            // produce a decryptable auth frame and is rejected earlier), and a
            // *wrong* PIN still collapses to the generic rejection, so PIN
            // correctness is never disclosed.
            if message.contains("missing web-share pairing code") {
                sleep(UNIFORM_AUTH_DELAY).await;
                let _ = write_with_timeout(socket.write_close_code(4008, "pin_required")).await;
                return Ok(None);
            }
            let close = close_for_auth_error(&message);
            reject_handshake_with_close(&mut socket, close.reason, close.wire_close).await?;
            return Ok(None);
        }
    };
    let share_id = share.share_id().to_owned();
    if !share.origin_allowed(&origin) {
        reject_handshake(&mut socket, "origin_not_allowed").await?;
        return Ok(None);
    }
    sleep(UNIFORM_AUTH_DELAY).await;
    info!(
        share_id = %share_id,
        role = share.role(),
        "web_share_auth_ok"
    );
    let (reader, writer) = socket.split();
    let socket = EncryptedWebSocketReader::new(reader, opener);
    let outbound = WebSocketOutbound::spawn(writer, sealer);
    write_with_timeout(send_ready(&outbound, &share)).await?;
    Ok(Some(EstablishedWebShare {
        socket,
        outbound,
        share_id,
        share,
        supports_session_pane_frame,
    }))
}

async fn complete_pre_ready_handshake(
    stream: TcpStream,
    request: HttpRequest,
    key: String,
    handler: Arc<RequestHandler>,
    pre_auth_guard: PreAuthGuard,
) -> io::Result<Option<PreReadyWebShare>> {
    let mut socket = WebSocket::accept(stream, &key).await?;
    let Some(origin) = request.headers.get("origin").cloned() else {
        reject_handshake(&mut socket, "origin_required").await?;
        return Ok(None);
    };
    // 1. Read the v1 hello (exact raw text, client X25519 public key, and
    //    ML-KEM encapsulation key).
    let hello = match read_client_hello(&mut socket).await {
        Ok(hello) => hello,
        Err(reason) => {
            reject_handshake(&mut socket, reason).await?;
            return Ok(None);
        }
    };
    // 2. Pre-ready token lookup. The token id is a 128-bit non-enumerable
    // handle; keep this lookup single-pass and collapse every miss on the wire.
    let Some((secret, origin_allowed)) = handler.web_share_pre_auth_token(&hello.token_id, &origin)
    else {
        reject_handshake(&mut socket, "unknown_token").await?;
        return Ok(None);
    };
    if !origin_allowed {
        reject_handshake(&mut socket, "origin_not_allowed").await?;
        return Ok(None);
    }
    // 3. Use the token secret as the high-entropy PSK.
    // 4. Generate the ephemeral server X25519 key pair (forward secrecy).
    let server_eph = rmux_web_crypto::generate_ephemeral();
    let server_public = server_eph.public_bytes();
    // 4b. Post-quantum hybrid: encapsulate to the client's ML-KEM key. A malformed
    //     key is collapsed to the uniform pre-ready rejection (Ok(None)), never
    //     bypassed via `?` — only a server RNG failure propagates as an error.
    let Some((ml_kem_ct, ml_kem_ss)) = crypto::encapsulate_ml_kem(&hello.client_ml_kem_ek)? else {
        reject_handshake(&mut socket, "invalid_ml_kem_key").await?;
        return Ok(None);
    };
    // 5. Random server handshake nonce.
    let server_nonce = crypto::random_handshake_nonce()?;
    // 6. Serialize the exact challenge text that will be both bound and sent. It
    //    carries the ML-KEM ciphertext, so the transcript binds it automatically.
    let challenge_text = build_challenge(
        &server_nonce,
        &crypto::base64url(&server_public),
        &crypto::base64url(&ml_kem_ct),
    )?;
    // 7. Complete the DH (consumes the ephemeral secret).
    let dh = server_eph.into_shared_secret(&hello.client_public);
    // 8. Derive the hybrid session, binding the exact hello + challenge transcript
    //    bytes and mixing both the X25519 and ML-KEM shared secrets.
    let psk = zeroize::Zeroizing::new(secret.as_bytes());
    let (mut opener, sealer) = match crypto::derive_server_crypto(
        &psk[..],
        &dh,
        &ml_kem_ss,
        hello.raw.as_bytes(),
        challenge_text.as_bytes(),
    ) {
        Ok(pair) => pair,
        // A low-order client public key yields an all-zero DH, which
        // rmux-web-crypto rejects. Collapse it to the uniform pre-ready
        // rejection instead of bypassing the delay via `?`.
        Err(_) => {
            reject_handshake(&mut socket, "weak_shared_secret").await?;
            return Ok(None);
        }
    };
    // 9. Send the challenge (the same bytes that were bound above).
    write_with_timeout(send_text(&mut socket, &challenge_text)).await?;
    // 10. Read and decrypt the first (auth) frame.
    let auth = match read_auth_message(&mut socket, &mut opener).await {
        Ok(auth) => auth,
        Err(reason) => {
            reject_handshake(&mut socket, reason).await?;
            return Ok(None);
        }
    };
    Ok(Some(PreReadyWebShare {
        socket,
        origin,
        token_id: hello.token_id,
        auth_pin: auth.pin,
        supports_session_pane_frame: auth.supports_session_pane_frame,
        opener,
        pre_auth_guard,
        sealer,
    }))
}

fn should_continue_accept_loop(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::Interrupted
            | io::ErrorKind::TimedOut
            | io::ErrorKind::WouldBlock
    )
}

fn is_fd_exhaustion(error: &io::Error) -> bool {
    matches!(error.raw_os_error(), Some(23 | 24 | 10024))
}

/// Rejects a pre-ready handshake with the collapsed close pair.
///
/// Logs the PRECISE internal reason server-side, waits the uniform auth delay
/// (timing-side-channel mitigation), and sends the SINGLE collapsed wire close
/// pair so no close code can act as a token/PIN/identity oracle.
async fn reject_handshake(socket: &mut WebSocket, reason: &str) -> io::Result<()> {
    reject_handshake_with_close(socket, reason, HANDSHAKE_REJECTED).await
}

async fn reject_handshake_with_close(
    socket: &mut WebSocket,
    reason: &str,
    wire_close: (u16, &str),
) -> io::Result<()> {
    sleep(UNIFORM_AUTH_DELAY).await;
    let (code, wire_reason) = wire_close;
    info!(close_code = code, reason, "web_share_handshake_rejected");
    let _ = write_with_timeout(socket.write_close_code(code, wire_reason)).await;
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

#[cfg(test)]
#[path = "server_tests.rs"]
mod tests;
