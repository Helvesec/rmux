use std::io;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::time::timeout;

use rmux_proto::WebTerminalPalette;

use super::websocket::{WebSocket, WebSocketMessage};
use super::{WebShareConnectRole, WebShareRevokeReason};
use crate::handler::{RequestHandler, WebPaneSnapshot, WebPaneStream};

pub(crate) const PRE_AUTH_TIMEOUT: Duration = Duration::from_secs(5);
pub(crate) const UNIFORM_AUTH_DELAY: Duration = Duration::from_millis(50);

const OPERATOR_INPUT_FRAME_MAX: usize = 4 * 1024;
const WS_OUTPUT_RAW: u8 = 0x01;
const WS_RESIZE_NOTIFY: u8 = 0x02;
const WS_SNAPSHOT_FULL: u8 = 0x10;
const WS_INPUT_TEXT: u8 = 0x80;
const WS_INPUT_KEY: u8 = 0x81;
const WS_RESIZE_REQUEST: u8 = 0x82;

#[derive(Debug)]
pub(crate) struct AuthMessage {
    pub(crate) id: String,
    pub(crate) key: String,
    pub(crate) pin: Option<String>,
    pub(crate) role: AuthRole,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum AuthRole {
    Operator,
    Read,
}

impl AuthRole {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Operator => "operator",
            Self::Read => "read",
        }
    }

    pub(crate) const fn connect_role(self) -> WebShareConnectRole {
        match self {
            Self::Operator => WebShareConnectRole::Operator,
            Self::Read => WebShareConnectRole::Read,
        }
    }
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
    let role = match wire.role.as_str() {
        "read" => AuthRole::Read,
        "operator" => AuthRole::Operator,
        _ => return Err((4006, "invalid_role")),
    };
    if wire.id.is_empty() || wire.key.is_empty() {
        return Err((4000, "invalid_auth"));
    }
    Ok(AuthMessage {
        id: wire.id,
        key: wire.key,
        pin: wire.pin,
        role,
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

pub(crate) async fn handle_client_text(
    socket: &mut WebSocket,
    pane: &mut WebPaneStream,
    text: &str,
) -> io::Result<()> {
    let message = serde_json::from_str::<ClientMessage>(text)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    match message {
        ClientMessage::Release if pane.is_operator() => {
            pane.release_operator();
            let text = serde_json::to_string(&ServerMessage::Released { role: "read" })
                .map_err(|error| io::Error::other(error.to_string()))?;
            socket.write_text(&text).await
        }
        ClientMessage::Release => {
            let _ = socket.write_close_code(4006, "release_on_read").await;
            Ok(())
        }
    }
}

pub(crate) async fn handle_operator_binary_frame(
    handler: &RequestHandler,
    socket: &mut WebSocket,
    pane: &WebPaneStream,
    payload: &[u8],
) -> io::Result<()> {
    if payload.is_empty() {
        let _ = socket.write_close_code(4006, "empty_operator_frame").await;
        return Ok(());
    }
    if payload.len() > OPERATOR_INPUT_FRAME_MAX {
        let _ = socket
            .write_close_code(4002, "operator_frame_too_large")
            .await;
        return Ok(());
    }
    let opcode = payload[0];
    let body = &payload[1..];
    match opcode {
        WS_INPUT_TEXT => send_text(handler, socket, pane, body).await?,
        WS_INPUT_KEY => send_key(handler, socket, pane, body).await?,
        WS_RESIZE_REQUEST => resize(handler, socket, pane, body).await?,
        _ => {
            let _ = socket
                .write_close_code(4006, "unknown_operator_opcode")
                .await;
        }
    }
    Ok(())
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

pub(crate) async fn send_ready(
    socket: &mut WebSocket,
    pane: &WebPaneStream,
    role: &str,
) -> io::Result<()> {
    let payload = ServerMessage::Ready {
        pane_size: PaneSize {
            cols: pane.snapshot.cols,
            rows: pane.snapshot.rows,
        },
        role,
        writable: pane.is_operator(),
        terminal_palette: pane.terminal_palette(),
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

async fn send_text(
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

async fn send_key(
    handler: &RequestHandler,
    socket: &mut WebSocket,
    pane: &WebPaneStream,
    body: &[u8],
) -> io::Result<()> {
    let Ok(key) = std::str::from_utf8(body) else {
        let _ = socket.write_close_code(4006, "invalid_key_utf8").await;
        return Ok(());
    };
    if key.len() > 64
        || !key
            .bytes()
            .all(|byte| byte.is_ascii_graphic() || byte == b' ')
    {
        let _ = socket.write_close_code(4006, "invalid_key_token").await;
        return Ok(());
    }
    handler
        .web_send_key(pane.target(), key.to_owned())
        .await
        .map_err(|error| io::Error::other(error.to_string()))
}

async fn resize(
    handler: &RequestHandler,
    socket: &mut WebSocket,
    pane: &WebPaneStream,
    body: &[u8],
) -> io::Result<()> {
    if body.len() != 4 {
        let _ = socket
            .write_close_code(4006, "invalid_resize_payload")
            .await;
        return Ok(());
    }
    let cols = u16::from_be_bytes([body[0], body[1]]);
    let rows = u16::from_be_bytes([body[2], body[3]]);
    if cols == 0 || rows == 0 || cols > 9999 || rows > 9999 {
        let _ = socket.write_close_code(4006, "invalid_resize_size").await;
        return Ok(());
    }
    handler
        .web_resize(pane.target(), cols, rows)
        .await
        .map_err(|error| io::Error::other(error.to_string()))?;
    send_resize_notify(socket, cols, rows).await
}

async fn send_resize_notify(socket: &mut WebSocket, cols: u16, rows: u16) -> io::Result<()> {
    let mut payload = Vec::with_capacity(4);
    payload.extend_from_slice(&cols.to_be_bytes());
    payload.extend_from_slice(&rows.to_be_bytes());
    send_binary(socket, WS_RESIZE_NOTIFY, &payload).await
}

async fn send_binary(socket: &mut WebSocket, opcode: u8, body: &[u8]) -> io::Result<()> {
    let mut frame = Vec::with_capacity(1 + body.len());
    frame.push(opcode);
    frame.extend_from_slice(body);
    socket.write_binary(&frame).await
}

#[derive(Debug, Deserialize)]
struct AuthWireMessage {
    #[serde(rename = "type")]
    kind: String,
    id: String,
    key: String,
    #[serde(default)]
    pin: Option<String>,
    role: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMessage {
    Release,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ServerMessage<'a> {
    Ready {
        pane_size: PaneSize,
        role: &'a str,
        writable: bool,
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
