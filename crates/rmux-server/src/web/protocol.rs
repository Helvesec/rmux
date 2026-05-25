use std::io;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::time::timeout;

use rmux_proto::WebTerminalPalette;

use super::websocket::{WebSocket, WebSocketMessage};
use super::WebShareRevokeReason;
use crate::handler::{
    RequestHandler, WebPaneSnapshot, WebPaneStream, WebSessionStream, WebShareStream,
};

pub(crate) const PRE_AUTH_TIMEOUT: Duration = Duration::from_secs(5);
pub(crate) const UNIFORM_AUTH_DELAY: Duration = Duration::from_millis(50);

const OPERATOR_INPUT_FRAME_MAX: usize = 4 * 1024;
const WS_OUTPUT_RAW: u8 = 0x01;
const WS_RESIZE_NOTIFY: u8 = 0x02;
const WS_SNAPSHOT_FULL: u8 = 0x10;
const WS_INPUT_TEXT: u8 = 0x80;
const WS_INPUT_KEY: u8 = 0x81;
const WS_RESIZE_REQUEST: u8 = 0x82;
const WS_ATTACH_INPUT: u8 = 0x83;

#[derive(Debug)]
pub(crate) struct AuthMessage {
    pub(crate) token: String,
    pub(crate) pin: Option<String>,
}

pub(crate) async fn read_auth_message(
    socket: &mut WebSocket,
) -> Result<AuthMessage, (u16, &'static str)> {
    let message = timeout(PRE_AUTH_TIMEOUT, socket.read_message())
        .await
        .map_err(|_| (4000, "auth_timeout"))?
        .map_err(|_| (4006, "invalid_auth_frame"))?;
    let WebSocketMessage::Text(text) = message else {
        return Err((4006, "auth_must_be_text"));
    };
    let wire =
        serde_json::from_str::<AuthWireMessage>(&text).map_err(|_| (4006, "invalid_auth_json"))?;
    if wire.kind != "auth" {
        return Err((4006, "first_frame_must_auth"));
    }
    if wire.token.is_empty() {
        return Err((4000, "invalid_auth"));
    }
    Ok(AuthMessage {
        token: wire.token,
        pin: wire.pin,
    })
}

pub(crate) fn close_for_auth_error(error: &str) -> (u16, &'static str) {
    if error.contains("read limit") {
        return (4003, "read_cap_reached");
    }
    if error.contains("operator is already connected") {
        return (4007, "operator_already_connected");
    }
    if error.contains("not writable") {
        return (4006, "operator_on_read_only");
    }
    (4000, "invalid_auth")
}

pub(crate) async fn handle_pane_client_text(
    socket: &mut WebSocket,
    pane: &mut WebPaneStream,
    text: &str,
) -> io::Result<()> {
    let message = serde_json::from_str::<ClientMessage>(text)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    match message {
        ClientMessage::Release if pane.is_operator() => {
            if pane.release_operator() {
                send_released(socket).await
            } else {
                socket.write_close_code(4003, "read_cap_reached").await
            }
        }
        ClientMessage::Logout => {
            let _ = socket
                .write_close_code(4006, "logout_requires_session")
                .await;
            Ok(())
        }
        ClientMessage::Release => {
            let _ = socket.write_close_code(4006, "release_on_read").await;
            Ok(())
        }
    }
}

pub(crate) async fn handle_session_client_text(
    handler: &RequestHandler,
    socket: &mut WebSocket,
    session: &mut WebSessionStream,
    text: &str,
) -> io::Result<()> {
    let message = serde_json::from_str::<ClientMessage>(text)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    match message {
        ClientMessage::Release if session.is_operator() => {
            if session.release_operator() {
                send_released(socket).await
            } else {
                socket.write_close_code(4003, "read_cap_reached").await
            }
        }
        ClientMessage::Logout
            if session_logout_allowed(session.is_operator(), session.controls()) =>
        {
            handler
                .web_session_logout(session.session_name())
                .await
                .map_err(|error| io::Error::other(error.to_string()))?;
            socket.write_close_code(1000, "session_closed").await
        }
        ClientMessage::Logout if session.is_operator() => {
            let _ = socket
                .write_close_code(4006, "logout_requires_controls")
                .await;
            Ok(())
        }
        ClientMessage::Logout => {
            let _ = socket
                .write_close_code(4006, "logout_requires_operator")
                .await;
            Ok(())
        }
        ClientMessage::Release => {
            let _ = socket.write_close_code(4006, "release_on_read").await;
            Ok(())
        }
    }
}

pub(crate) async fn handle_pane_operator_binary_frame(
    handler: &RequestHandler,
    socket: &mut WebSocket,
    pane: &WebPaneStream,
    payload: &[u8],
) -> io::Result<()> {
    let Some((opcode, body)) = parse_operator_frame(socket, payload).await? else {
        return Ok(());
    };
    match opcode {
        WS_INPUT_TEXT => send_pane_text(handler, socket, pane, body).await?,
        WS_INPUT_KEY => send_pane_key(handler, socket, pane, body).await?,
        WS_RESIZE_REQUEST => resize_pane(handler, socket, pane, body).await?,
        _ => {
            let _ = socket
                .write_close_code(4006, "unknown_operator_opcode")
                .await;
        }
    }
    Ok(())
}

pub(crate) async fn handle_session_operator_binary_frame(
    handler: &RequestHandler,
    socket: &mut WebSocket,
    session: &mut WebSessionStream,
    payload: &[u8],
) -> io::Result<()> {
    let Some((opcode, body)) = parse_operator_frame(socket, payload).await? else {
        return Ok(());
    };
    match opcode {
        WS_INPUT_TEXT => send_session_text(handler, socket, session, body).await?,
        WS_INPUT_KEY => send_session_key(handler, socket, session, body).await?,
        WS_RESIZE_REQUEST => resize_session(socket, session, body).await?,
        WS_ATTACH_INPUT => send_session_attach_input(socket, session, body).await?,
        _ => {
            let _ = socket
                .write_close_code(4006, "unknown_operator_opcode")
                .await;
        }
    }
    Ok(())
}

async fn parse_operator_frame<'a>(
    socket: &mut WebSocket,
    payload: &'a [u8],
) -> io::Result<Option<(u8, &'a [u8])>> {
    if payload.is_empty() {
        let _ = socket.write_close_code(4006, "empty_operator_frame").await;
        return Ok(None);
    }
    if payload.len() > OPERATOR_INPUT_FRAME_MAX {
        let _ = socket
            .write_close_code(4002, "operator_frame_too_large")
            .await;
        return Ok(None);
    }
    Ok(Some((payload[0], &payload[1..])))
}

pub(crate) async fn send_output(socket: &mut WebSocket, bytes: &[u8]) -> io::Result<()> {
    send_binary(socket, WS_OUTPUT_RAW, bytes).await
}

pub(crate) async fn send_snapshot(
    socket: &mut WebSocket,
    snapshot: &WebPaneSnapshot,
) -> io::Result<()> {
    send_binary(socket, WS_SNAPSHOT_FULL, &snapshot.ansi_bytes()).await
}

pub(crate) async fn send_ready(socket: &mut WebSocket, share: &WebShareStream) -> io::Result<()> {
    let pane_size = match share {
        WebShareStream::Pane(pane) => PaneSize {
            cols: pane.snapshot.cols,
            rows: pane.snapshot.rows,
        },
        WebShareStream::Session(session) => PaneSize {
            cols: session.initial_size().cols,
            rows: session.initial_size().rows,
        },
    };
    let scope = match share {
        WebShareStream::Pane(_) => "pane",
        WebShareStream::Session(_) => "session",
    };
    let payload = ServerMessage::Ready {
        pane_size,
        scope,
        role: share.role(),
        writable: share.is_operator(),
        controls: share.controls(),
        terminal_palette: share.terminal_palette(),
    };
    let text =
        serde_json::to_string(&payload).map_err(|error| io::Error::other(error.to_string()))?;
    socket.write_text(&text).await
}

pub(crate) async fn send_revoked(
    socket: &mut WebSocket,
    reason: WebShareRevokeReason,
) -> io::Result<()> {
    let payload = ServerMessage::ShareRevoked {
        reason: reason.as_str(),
    };
    let text =
        serde_json::to_string(&payload).map_err(|error| io::Error::other(error.to_string()))?;
    socket.write_text(&text).await
}

async fn send_pane_text(
    handler: &RequestHandler,
    socket: &mut WebSocket,
    pane: &WebPaneStream,
    body: &[u8],
) -> io::Result<()> {
    let Ok(text) = std::str::from_utf8(body) else {
        let _ = socket.write_close_code(4006, "invalid_utf8").await;
        return Ok(());
    };
    handler
        .web_send_text(pane.target(), text.to_owned())
        .await
        .map_err(|error| io::Error::other(error.to_string()))
}

async fn send_session_text(
    handler: &RequestHandler,
    socket: &mut WebSocket,
    session: &mut WebSessionStream,
    body: &[u8],
) -> io::Result<()> {
    let Ok(text) = std::str::from_utf8(body) else {
        let _ = socket.write_close_code(4006, "invalid_utf8").await;
        return Ok(());
    };
    handler
        .web_session_send_text(session.session_name(), text.to_owned())
        .await
        .map_err(|error| io::Error::other(error.to_string()))
}

async fn send_pane_key(
    handler: &RequestHandler,
    socket: &mut WebSocket,
    pane: &WebPaneStream,
    body: &[u8],
) -> io::Result<()> {
    let Some(key) = validate_key_token(socket, body).await? else {
        return Ok(());
    };
    handler
        .web_send_key(pane.target(), key.to_owned())
        .await
        .map_err(|error| io::Error::other(error.to_string()))
}

async fn send_session_key(
    handler: &RequestHandler,
    socket: &mut WebSocket,
    session: &mut WebSessionStream,
    body: &[u8],
) -> io::Result<()> {
    let Some(key) = validate_key_token(socket, body).await? else {
        return Ok(());
    };
    handler
        .web_session_send_key(session.session_name(), key.to_owned())
        .await
        .map_err(|error| io::Error::other(error.to_string()))
}

async fn validate_key_token<'a>(
    socket: &mut WebSocket,
    body: &'a [u8],
) -> io::Result<Option<&'a str>> {
    let Ok(key) = std::str::from_utf8(body) else {
        let _ = socket.write_close_code(4006, "invalid_key_utf8").await;
        return Ok(None);
    };
    if key.len() > 64
        || !key
            .bytes()
            .all(|byte| byte.is_ascii_graphic() || byte == b' ')
    {
        let _ = socket.write_close_code(4006, "invalid_key_token").await;
        return Ok(None);
    }
    Ok(Some(key))
}

async fn resize_pane(
    handler: &RequestHandler,
    socket: &mut WebSocket,
    pane: &WebPaneStream,
    body: &[u8],
) -> io::Result<()> {
    let Some((cols, rows)) = parse_resize(socket, body).await? else {
        return Ok(());
    };
    handler
        .web_resize(pane.target(), cols, rows)
        .await
        .map_err(|error| io::Error::other(error.to_string()))?;
    send_resize_notify(socket, cols, rows).await
}

async fn resize_session(
    socket: &mut WebSocket,
    session: &mut WebSessionStream,
    body: &[u8],
) -> io::Result<()> {
    if !session.controls() {
        let _ = socket
            .write_close_code(4006, "resize_requires_controls")
            .await;
        return Ok(());
    }
    let Some((cols, rows)) = parse_resize(socket, body).await? else {
        return Ok(());
    };
    session.send_resize(cols, rows).await?;
    send_resize_notify(socket, cols, rows).await
}

async fn parse_resize(socket: &mut WebSocket, body: &[u8]) -> io::Result<Option<(u16, u16)>> {
    if body.len() != 4 {
        let _ = socket
            .write_close_code(4006, "invalid_resize_payload")
            .await;
        return Ok(None);
    }
    let cols = u16::from_be_bytes([body[0], body[1]]);
    let rows = u16::from_be_bytes([body[2], body[3]]);
    if cols == 0 || rows == 0 || cols > 9999 || rows > 9999 {
        let _ = socket.write_close_code(4006, "invalid_resize_size").await;
        return Ok(None);
    }
    Ok(Some((cols, rows)))
}

async fn send_session_attach_input(
    socket: &mut WebSocket,
    session: &mut WebSessionStream,
    body: &[u8],
) -> io::Result<()> {
    if !session.controls() {
        let _ = socket.write_close_code(4006, "controls_not_enabled").await;
        return Ok(());
    }
    session.send_attach_keystroke(body.to_vec()).await
}

async fn send_resize_notify(socket: &mut WebSocket, cols: u16, rows: u16) -> io::Result<()> {
    let mut payload = Vec::with_capacity(4);
    payload.extend_from_slice(&cols.to_be_bytes());
    payload.extend_from_slice(&rows.to_be_bytes());
    send_binary(socket, WS_RESIZE_NOTIFY, &payload).await
}

fn session_logout_allowed(is_operator: bool, controls: bool) -> bool {
    is_operator && controls
}

async fn send_released(socket: &mut WebSocket) -> io::Result<()> {
    let text = serde_json::to_string(&ServerMessage::Released { role: "read" })
        .map_err(|error| io::Error::other(error.to_string()))?;
    socket.write_text(&text).await
}

#[cfg(test)]
mod tests {
    use super::{session_logout_allowed, AuthWireMessage};

    #[test]
    fn session_logout_requires_operator_controls() {
        assert!(!session_logout_allowed(false, false));
        assert!(!session_logout_allowed(false, true));
        assert!(!session_logout_allowed(true, false));
        assert!(session_logout_allowed(true, true));
    }

    #[test]
    fn auth_wire_rejects_legacy_role_and_key_fields() {
        let message = r#"{"type":"auth","id":"s","key":"k","role":"operator","token":"t"}"#;

        assert!(serde_json::from_str::<AuthWireMessage>(message).is_err());
    }
}

async fn send_binary(socket: &mut WebSocket, opcode: u8, body: &[u8]) -> io::Result<()> {
    let mut frame = Vec::with_capacity(1 + body.len());
    frame.push(opcode);
    frame.extend_from_slice(body);
    socket.write_binary(&frame).await
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthWireMessage {
    #[serde(rename = "type")]
    kind: String,
    token: String,
    #[serde(default)]
    pin: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMessage {
    Release,
    Logout,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ServerMessage<'a> {
    Ready {
        pane_size: PaneSize,
        scope: &'a str,
        role: &'a str,
        writable: bool,
        controls: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        terminal_palette: Option<&'a WebTerminalPalette>,
    },
    Released {
        role: &'a str,
    },
    ShareRevoked {
        reason: &'a str,
    },
}

#[derive(Debug, Serialize)]
struct PaneSize {
    cols: u16,
    rows: u16,
}
