//! Outbound framing: binary terminal frames and the JSON server messages
//! (`ready` / `viewer_count` / `share_revoked`) sent back to the browser.

use std::io;
use std::time::SystemTime;

use serde::Serialize;

use rmux_proto::{TerminalSize, WebTerminalPalette};

use super::{
    SERVER_CAPABILITIES, WEB_SHARE_PROTOCOL_VERSION, WS_OUTPUT_RAW, WS_RESIZE_NOTIFY,
    WS_SESSION_VIEW, WS_SNAPSHOT_FULL,
};
use crate::handler::{WebPaneSnapshot, WebSessionSnapshot, WebShareStream};
use crate::web::outbound::{OutboundQueueResult, WebSocketOutbound};
use crate::web::{WebShareConnectionCounts, WebShareRevokeReason};

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ServerMessage<'a> {
    Ready {
        protocol_version: u16,
        capabilities: &'static [&'static str],
        pane_size: PaneSize,
        scope: &'a str,
        share_id: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        session_name: Option<&'a str>,
        role: &'a str,
        operator: bool,
        operator_access: bool,
        spectator_access: bool,
        controls: bool,
        show_viewers: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        ttl_remaining_seconds: Option<u64>,
        #[serde(flatten)]
        connection_counts: WebShareConnectionCounts,
        #[serde(skip_serializing_if = "Option::is_none")]
        terminal_palette: Option<&'a WebTerminalPalette>,
    },
    ViewerCount {
        #[serde(flatten)]
        connection_counts: WebShareConnectionCounts,
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

pub(crate) fn queue_output(socket: &WebSocketOutbound, bytes: &[u8]) -> OutboundQueueResult {
    socket.queue_frame(binary_payload(WS_OUTPUT_RAW, bytes))
}

pub(crate) fn queue_snapshot(
    socket: &WebSocketOutbound,
    snapshot: &WebPaneSnapshot,
) -> OutboundQueueResult {
    socket.queue_snapshot(binary_payload(WS_SNAPSHOT_FULL, &snapshot.ansi_bytes()))
}

pub(crate) fn queue_resize(socket: &WebSocketOutbound, size: TerminalSize) -> OutboundQueueResult {
    socket.queue_frame(binary_payload(
        WS_RESIZE_NOTIFY,
        &[
            (size.cols >> 8) as u8,
            size.cols as u8,
            (size.rows >> 8) as u8,
            size.rows as u8,
        ],
    ))
}

pub(crate) fn queue_session_snapshot(
    socket: &WebSocketOutbound,
    snapshot: &WebSessionSnapshot,
) -> OutboundQueueResult {
    socket.queue_snapshot(binary_payload(WS_SNAPSHOT_FULL, &snapshot.ansi_bytes()))
}

pub(crate) fn queue_session_view(
    socket: &WebSocketOutbound,
    snapshot: &WebSessionSnapshot,
) -> OutboundQueueResult {
    let Ok(view) = serde_json::to_vec(&snapshot.view) else {
        return OutboundQueueResult::Closed;
    };
    socket.queue_frame(binary_payload(WS_SESSION_VIEW, &view))
}

pub(crate) async fn send_ready(
    socket: &WebSocketOutbound,
    share: &WebShareStream,
) -> io::Result<()> {
    let pane_size = match share {
        WebShareStream::Pane(pane) => PaneSize {
            cols: pane.snapshot.cols,
            rows: pane.snapshot.rows,
        },
        WebShareStream::Session(session) => PaneSize {
            cols: session.size().cols,
            rows: session.size().rows,
        },
    };
    let scope = match share {
        WebShareStream::Pane(_) => "pane",
        WebShareStream::Session(_) => "session",
    };
    let payload = ServerMessage::Ready {
        protocol_version: WEB_SHARE_PROTOCOL_VERSION,
        capabilities: SERVER_CAPABILITIES,
        pane_size,
        scope,
        share_id: share.share_id(),
        session_name: share.session_name(),
        role: share.role(),
        operator: share.is_operator(),
        operator_access: share.has_operator_access(),
        spectator_access: share.has_spectator_access(),
        controls: share.controls(),
        show_viewers: share.show_viewers(),
        ttl_remaining_seconds: ttl_remaining_seconds(share.expires_at()),
        connection_counts: share.connection_counts(),
        terminal_palette: share.terminal_palette(),
    };
    let text =
        serde_json::to_string(&payload).map_err(|error| io::Error::other(error.to_string()))?;
    socket.write_text(&text).await
}

pub(crate) async fn send_viewer_count(
    socket: &WebSocketOutbound,
    counts: WebShareConnectionCounts,
) -> io::Result<()> {
    let payload = ServerMessage::ViewerCount {
        connection_counts: counts,
    };
    let text =
        serde_json::to_string(&payload).map_err(|error| io::Error::other(error.to_string()))?;
    socket.write_text(&text).await
}

pub(crate) async fn send_revoked(
    socket: &WebSocketOutbound,
    reason: WebShareRevokeReason,
) -> io::Result<()> {
    let payload = ServerMessage::ShareRevoked {
        reason: reason.as_str(),
    };
    let text =
        serde_json::to_string(&payload).map_err(|error| io::Error::other(error.to_string()))?;
    socket.write_text(&text).await
}

fn binary_payload(opcode: u8, body: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(1 + body.len());
    frame.push(opcode);
    frame.extend_from_slice(body);
    frame
}

fn ttl_remaining_seconds(expires_at: Option<SystemTime>) -> Option<u64> {
    expires_at
        .and_then(|deadline| deadline.duration_since(SystemTime::now()).ok())
        .map(|duration| duration.as_secs())
}
