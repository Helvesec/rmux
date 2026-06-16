//! Outbound framing: binary terminal frames and the JSON server messages
//! (`ready` / `viewer_count` / `share_revoked`) sent back to the browser.

use std::io;
use std::time::SystemTime;

use serde::Serialize;

use rmux_proto::{TerminalSize, WebTerminalPalette};

use super::{
    SERVER_CAPABILITIES, WEB_SHARE_PROTOCOL_VERSION, WS_OUTPUT_RAW, WS_RESIZE_NOTIFY,
    WS_SESSION_PANE_FRAME, WS_SESSION_VIEW, WS_SNAPSHOT_FULL,
};
use crate::handler::{WebPaneSnapshot, WebSessionPaneFrame, WebSessionSnapshot, WebShareStream};
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
        spectator_pairing_code: Option<&'a str>,
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

pub(crate) fn queue_session_view(
    socket: &WebSocketOutbound,
    snapshot: &WebSessionSnapshot,
) -> OutboundQueueResult {
    let Ok(view) = serde_json::to_vec(&snapshot.view) else {
        return OutboundQueueResult::Closed;
    };
    socket.queue_frame(binary_payload(WS_SESSION_VIEW, &view))
}

pub(crate) fn queue_session_keyframe(
    socket: &WebSocketOutbound,
    resize: Option<TerminalSize>,
    snapshot: &WebSessionSnapshot,
) -> OutboundQueueResult {
    let Ok(frames) = session_keyframe_payloads(resize, snapshot) else {
        return OutboundQueueResult::Closed;
    };
    socket.queue_keyframe(frames)
}

pub(crate) fn queue_session_pane_frame(
    socket: &WebSocketOutbound,
    frame: &WebSessionPaneFrame,
) -> OutboundQueueResult {
    socket.queue_frame(session_pane_frame_payload(frame))
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
        spectator_pairing_code: share.operator_visible_spectator_pairing_code(),
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

fn resize_payload(size: TerminalSize) -> Vec<u8> {
    binary_payload(
        WS_RESIZE_NOTIFY,
        &[
            (size.cols >> 8) as u8,
            size.cols as u8,
            (size.rows >> 8) as u8,
            size.rows as u8,
        ],
    )
}

fn session_snapshot_payload(snapshot: &WebSessionSnapshot) -> Vec<u8> {
    binary_payload(WS_SNAPSHOT_FULL, &snapshot.ansi_bytes())
}

fn session_view_payload(snapshot: &WebSessionSnapshot) -> serde_json::Result<Vec<u8>> {
    serde_json::to_vec(&snapshot.view).map(|view| binary_payload(WS_SESSION_VIEW, &view))
}

fn session_pane_frame_payload(frame: &WebSessionPaneFrame) -> Vec<u8> {
    let mut body = Vec::with_capacity(24 + frame.frame.len());
    body.extend_from_slice(&frame.pane.id.to_be_bytes());
    body.extend_from_slice(&frame.size.cols.to_be_bytes());
    body.extend_from_slice(&frame.size.rows.to_be_bytes());
    body.extend_from_slice(&frame.pane.x.to_be_bytes());
    body.extend_from_slice(&frame.pane.y.to_be_bytes());
    body.extend_from_slice(&frame.pane.cols.to_be_bytes());
    body.extend_from_slice(&frame.pane.rows.to_be_bytes());
    body.extend_from_slice(&saturating_u32(frame.pane.scroll_offset).to_be_bytes());
    body.extend_from_slice(&saturating_u32(frame.pane.history_size).to_be_bytes());
    body.extend_from_slice(&frame.frame);
    binary_payload(WS_SESSION_PANE_FRAME, &body)
}

fn session_keyframe_payloads(
    resize: Option<TerminalSize>,
    snapshot: &WebSessionSnapshot,
) -> serde_json::Result<Vec<Vec<u8>>> {
    let mut frames = Vec::with_capacity(if resize.is_some() { 3 } else { 2 });
    if let Some(size) = resize {
        frames.push(resize_payload(size));
    }
    frames.push(session_snapshot_payload(snapshot));
    frames.push(session_view_payload(snapshot)?);
    Ok(frames)
}

fn saturating_u32(value: usize) -> u32 {
    value.min(u32::MAX as usize) as u32
}

fn ttl_remaining_seconds(expires_at: Option<SystemTime>) -> Option<u64> {
    expires_at
        .and_then(|deadline| deadline.duration_since(SystemTime::now()).ok())
        .map(|duration| duration.as_secs())
}

#[cfg(test)]
mod tests {
    use rmux_proto::TerminalSize;

    use super::{
        session_keyframe_payloads, session_pane_frame_payload, WebSessionPaneFrame,
        WebSessionSnapshot, WS_RESIZE_NOTIFY, WS_SESSION_PANE_FRAME, WS_SESSION_VIEW,
        WS_SNAPSHOT_FULL,
    };
    use crate::handler::{TestWebSessionView, WebSessionPaneView};

    #[test]
    fn session_keyframe_keeps_resize_snapshot_and_view_atomic_order() {
        let size = TerminalSize { cols: 80, rows: 24 };
        let snapshot =
            WebSessionSnapshot::new(size, b"paint".to_vec(), TestWebSessionView::new(size), 0, 0);

        let frames = session_keyframe_payloads(Some(size), &snapshot).expect("view serializes");

        assert_eq!(frames.len(), 3);
        assert_eq!(frames[0][0], WS_RESIZE_NOTIFY);
        assert_eq!(frames[1][0], WS_SNAPSHOT_FULL);
        assert_eq!(frames[2][0], WS_SESSION_VIEW);
    }

    #[test]
    fn session_pane_frame_payload_uses_fixed_header_and_ansi_body() {
        let size = TerminalSize {
            cols: 120,
            rows: 40,
        };
        let frame = WebSessionPaneFrame::new(
            size,
            WebSessionPaneView {
                id: 7,
                x: 41,
                y: 2,
                cols: 39,
                rows: 20,
                active: true,
                history_size: 50_000,
                scroll_offset: 12,
                alternate_on: false,
                mouse_on: false,
            },
            b"\x1b[3;42Hrow".to_vec(),
        );

        let payload = session_pane_frame_payload(&frame);

        assert_eq!(payload[0], WS_SESSION_PANE_FRAME);
        assert_eq!(u32::from_be_bytes(payload[1..5].try_into().unwrap()), 7);
        assert_eq!(u16::from_be_bytes(payload[5..7].try_into().unwrap()), 120);
        assert_eq!(u16::from_be_bytes(payload[7..9].try_into().unwrap()), 40);
        assert_eq!(u16::from_be_bytes(payload[9..11].try_into().unwrap()), 41);
        assert_eq!(u16::from_be_bytes(payload[11..13].try_into().unwrap()), 2);
        assert_eq!(u16::from_be_bytes(payload[13..15].try_into().unwrap()), 39);
        assert_eq!(u16::from_be_bytes(payload[15..17].try_into().unwrap()), 20);
        assert_eq!(u32::from_be_bytes(payload[17..21].try_into().unwrap()), 12);
        assert_eq!(
            u32::from_be_bytes(payload[21..25].try_into().unwrap()),
            50_000
        );
        assert_eq!(&payload[25..], b"\x1b[3;42Hrow");
    }
}
