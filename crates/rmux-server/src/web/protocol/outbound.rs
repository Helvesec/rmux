//! Outbound framing: binary terminal frames and the JSON server messages
//! (`ready` / `viewer_count` / `share_revoked`) sent back to the browser.

use std::io;
use std::time::SystemTime;

use rmux_proto::TerminalSize;

use super::{
    SERVER_CAPABILITIES, WEB_SHARE_PROTOCOL_VERSION, WS_OUTPUT_RAW, WS_PANE_RECOVERY_SNAPSHOT,
    WS_RESIZE_NOTIFY, WS_SESSION_PANE_FRAME, WS_SESSION_VIEW, WS_SNAPSHOT_FULL,
};
use crate::handler::{WebPaneSnapshot, WebSessionPaneFrame, WebSessionSnapshot, WebShareStream};
use crate::web::outbound::{OutboundQueueResult, WebSocketOutbound, WEB_OUTBOUND_BYTES_MAX};
use crate::web::stream_sanitizer::WebTerminalSanitizer;
use crate::web::{WebShareConnectionCounts, WebShareRevokeReason};

mod message;
use message::{PaneSize, ServerMessage};

pub(crate) fn queue_output(socket: &WebSocketOutbound, bytes: &[u8]) -> OutboundQueueResult {
    socket.queue_frame(binary_payload(WS_OUTPUT_RAW, bytes))
}

pub(crate) fn queue_snapshot(
    socket: &WebSocketOutbound,
    snapshot: &WebPaneSnapshot,
    sanitizer: &mut WebTerminalSanitizer,
    include_recovery_coverage: bool,
) -> OutboundQueueResult {
    let Some(frame) = pane_snapshot_payload(snapshot, sanitizer, include_recovery_coverage) else {
        return OutboundQueueResult::Closed;
    };
    socket.queue_snapshot(frame)
}

pub(crate) fn queue_session_view(
    socket: &WebSocketOutbound,
    snapshot: &WebSessionSnapshot,
) -> OutboundQueueResult {
    let Ok(frame) = session_view_payload(snapshot) else {
        return OutboundQueueResult::Closed;
    };
    socket.queue_frame(frame)
}

pub(crate) fn queue_session_keyframe(
    socket: &WebSocketOutbound,
    resize: Option<TerminalSize>,
    snapshot: &WebSessionSnapshot,
    sanitizer: &mut WebTerminalSanitizer,
) -> OutboundQueueResult {
    let Some(frames) = session_keyframe_payloads(resize, snapshot, sanitizer) else {
        return OutboundQueueResult::Closed;
    };
    socket.queue_keyframe(frames)
}

pub(crate) fn queue_session_pane_frame(
    socket: &WebSocketOutbound,
    frame: &WebSessionPaneFrame,
    sanitizer: &mut WebTerminalSanitizer,
) -> OutboundQueueResult {
    let Some(frame) = session_pane_frame_payload(frame, sanitizer) else {
        return OutboundQueueResult::Closed;
    };
    socket.queue_frame(frame)
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

fn pane_snapshot_payload(
    snapshot: &WebPaneSnapshot,
    sanitizer: &mut WebTerminalSanitizer,
    include_recovery_coverage: bool,
) -> Option<Vec<u8>> {
    let mut frame = Vec::with_capacity(if include_recovery_coverage { 18 } else { 1 });
    if include_recovery_coverage {
        frame.push(WS_PANE_RECOVERY_SNAPSHOT);
        frame.extend_from_slice(&snapshot.history_rows_total.to_be_bytes());
        frame.extend_from_slice(&snapshot.history_rows_included.to_be_bytes());
        frame.push(u8::from(snapshot.metadata_complete));
    } else {
        frame.push(WS_SNAPSHOT_FULL);
    }
    let mut raw = Vec::new();
    snapshot.append_ansi_bytes(&mut raw);
    sanitizer.reset();
    sanitizer.push(&raw, &mut frame);
    (frame.len() <= WEB_OUTBOUND_BYTES_MAX).then_some(frame)
}

fn session_snapshot_payload(
    snapshot: &WebSessionSnapshot,
    sanitizer: &mut WebTerminalSanitizer,
) -> Option<Vec<u8>> {
    let mut frame = Vec::with_capacity(1);
    frame.push(WS_SNAPSHOT_FULL);
    let mut raw = Vec::new();
    snapshot.append_ansi_bytes(&mut raw);
    sanitizer.reset();
    sanitizer.push(&raw, &mut frame);
    (frame.len() <= WEB_OUTBOUND_BYTES_MAX).then_some(frame)
}

fn session_view_payload(snapshot: &WebSessionSnapshot) -> serde_json::Result<Vec<u8>> {
    let mut frame = Vec::with_capacity(1);
    frame.push(WS_SESSION_VIEW);
    serde_json::to_writer(&mut frame, &snapshot.view)?;
    Ok(frame)
}

fn session_pane_frame_payload(
    frame: &WebSessionPaneFrame,
    sanitizer: &mut WebTerminalSanitizer,
) -> Option<Vec<u8>> {
    let mut body = Vec::with_capacity(25 + frame.frame.len());
    body.push(WS_SESSION_PANE_FRAME);
    body.extend_from_slice(&frame.pane.id.to_be_bytes());
    body.extend_from_slice(&frame.size.cols.to_be_bytes());
    body.extend_from_slice(&frame.size.rows.to_be_bytes());
    body.extend_from_slice(&frame.pane.x.to_be_bytes());
    body.extend_from_slice(&frame.pane.y.to_be_bytes());
    body.extend_from_slice(&frame.pane.cols.to_be_bytes());
    body.extend_from_slice(&frame.pane.rows.to_be_bytes());
    body.extend_from_slice(&saturating_u32(frame.pane.scroll_offset).to_be_bytes());
    body.extend_from_slice(&saturating_u32(frame.pane.history_size).to_be_bytes());
    sanitizer.reset();
    sanitizer.push(&frame.frame, &mut body);
    (body.len() <= WEB_OUTBOUND_BYTES_MAX).then_some(body)
}

fn session_keyframe_payloads(
    resize: Option<TerminalSize>,
    snapshot: &WebSessionSnapshot,
    sanitizer: &mut WebTerminalSanitizer,
) -> Option<Vec<Vec<u8>>> {
    let mut frames = Vec::with_capacity(if resize.is_some() { 3 } else { 2 });
    if let Some(size) = resize {
        frames.push(resize_payload(size));
    }
    frames.push(session_snapshot_payload(snapshot, sanitizer)?);
    frames.push(session_view_payload(snapshot).ok()?);
    let total = frames
        .iter()
        .try_fold(0_usize, |total, frame| total.checked_add(frame.len()));
    total
        .filter(|total| *total <= WEB_OUTBOUND_BYTES_MAX)
        .map(|_| frames)
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
#[path = "outbound_tests.rs"]
mod tests;
