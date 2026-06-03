use std::collections::HashMap;
use std::io;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use rmux_core::events::OutputCursorItem;
use rmux_core::PaneId;
use tokio::time::{sleep, Instant};
use tracing::{debug, info};

use super::rate_limit::OperatorRateLimiter;
use crate::handler::{
    RequestHandler, WebPaneStream, WebSessionAttachEvent, WebSessionSnapshot, WebSessionStream,
};
use crate::web::crypto::EncryptedWebSocketReader;
use crate::web::outbound::{OutboundQueueResult, WebSocketOutbound};
use crate::web::protocol::{
    handle_pane_client_text, handle_pane_operator_binary_frame, handle_session_client_text,
    handle_session_operator_binary_frame, queue_output, queue_resize, queue_session_snapshot,
    queue_session_view, queue_snapshot, send_revoked, send_viewer_count,
    SessionOperatorBinaryOutcome, SessionScrollRequest,
};
use crate::web::websocket::WebSocketMessage;
use crate::web::{WebShareConnectionCounts, WebShareRevokeReason};

const SLOW_VIEWER_CLOSE_CODE: u16 = 4001;
const SESSION_SNAPSHOT_DEBOUNCE: Duration = Duration::from_millis(50);

pub(super) async fn serve_pane_loop(
    handler: Arc<RequestHandler>,
    mut socket: EncryptedWebSocketReader,
    outbound: WebSocketOutbound,
    share_id: String,
    mut pane: WebPaneStream,
) -> io::Result<()> {
    queue_or_close(
        &outbound,
        queue_snapshot(&outbound, &pane.snapshot),
        &share_id,
    )
    .await?;
    let mut rate_limiter = OperatorRateLimiter::new();
    let mut last_connection_counts = pane.connection_counts();
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
                        match queue_output(&outbound, event.bytes()) {
                            OutboundQueueResult::Queued => {}
                            OutboundQueueResult::Backpressure => {
                                debug!(share_id = %share_id, "web-share viewer backlog exceeded; resyncing");
                                queue_fresh_pane_snapshot(handler.as_ref(), &outbound, &mut pane, &share_id).await?;
                            }
                            result => {
                                close_slow_viewer(&outbound, &share_id, result).await?;
                                return Ok(());
                            }
                        }
                    }
                    OutputCursorItem::Gap(gap) => {
                        debug!(missed = gap.missed_events(), "web-share spectator resync");
                        queue_fresh_pane_snapshot(handler.as_ref(), &outbound, &mut pane, &share_id).await?;
                    }
                }
            }
            message = socket.read_message() => {
                match message? {
                    WebSocketMessage::Text(text) => {
                        if !rate_limiter.try_acquire() {
                            info!(share_id = %share_id, "web_share_client_text_rate_limit_hit");
                            continue;
                        }
                        handle_pane_client_text(&outbound, &mut pane, &text).await?;
                    }
                    WebSocketMessage::Binary(bytes) => {
                        if !pane.is_operator() {
                            let _ = outbound.write_close_code(4006, "spectator_no_binary").await;
                            return Ok(());
                        }
                        if !rate_limiter.try_acquire() {
                            info!(share_id = %share_id, "web_share_operator_rate_limit_hit");
                            continue;
                        }
                        handle_pane_operator_binary_frame(&handler, &outbound, &pane, &bytes).await?;
                    }
                    WebSocketMessage::Close => {
                        let _ = outbound.write_close().await;
                        return Ok(());
                    }
                    WebSocketMessage::Ping(payload) => {
                        outbound.write_pong(&payload).await?;
                    }
                    WebSocketMessage::Pong => {}
                }
            }
            changed = pane.revoke_rx.changed() => {
                if changed.is_ok() {
                    let reason = *pane.revoke_rx.borrow();
                    if let Some(reason) = reason {
                        notify_revoked_and_close(&outbound, reason).await?;
                        return Ok(());
                    }
                }
            }
            _ = ttl_sleep.as_mut() => {
                notify_revoked_and_close(&outbound, WebShareRevokeReason::TtlExpired).await?;
                return Ok(());
            }
            _ = alive_tick.tick() => {
                if !handler.web_target_alive(pane.target()).await {
                    notify_revoked_and_close(&outbound, WebShareRevokeReason::PaneGone).await?;
                    return Ok(());
                }
                send_viewer_count_if_changed(
                    &outbound,
                    &mut last_connection_counts,
                    pane.connection_counts(),
                )
                .await?;
            }
        }
    }
}

async fn queue_fresh_pane_snapshot(
    handler: &RequestHandler,
    outbound: &WebSocketOutbound,
    pane: &mut WebPaneStream,
    share_id: &str,
) -> io::Result<()> {
    let target = pane.target().clone();
    let (snapshot, output) = handler
        .web_resnapshot(&target)
        .await
        .map_err(|error| io::Error::other(error.to_string()))?;
    pane.snapshot = snapshot;
    pane.output = output;
    queue_or_close(outbound, queue_snapshot(outbound, &pane.snapshot), share_id).await
}

pub(super) async fn serve_session_loop(
    handler: Arc<RequestHandler>,
    mut socket: EncryptedWebSocketReader,
    outbound: WebSocketOutbound,
    share_id: String,
    mut session: WebSessionStream,
) -> io::Result<()> {
    let mut scrolls = HashMap::new();
    queue_session_snapshot_and_view(&outbound, &session.snapshot, &share_id).await?;
    let mut attach_reader = session.take_attach_reader();
    let mut rate_limiter = OperatorRateLimiter::new();
    let mut last_connection_counts = session.connection_counts();
    let mut alive_tick = tokio::time::interval(Duration::from_millis(500));
    let ttl_delay = session
        .expires_at()
        .map(duration_until)
        .unwrap_or_else(|| Duration::from_secs(365 * 24 * 60 * 60));
    let ttl_sleep = sleep(ttl_delay);
    tokio::pin!(ttl_sleep);
    let snapshot_sleep = sleep(Duration::from_secs(365 * 24 * 60 * 60));
    tokio::pin!(snapshot_sleep);
    let mut snapshot_pending = false;
    let mut view_pending = false;

    loop {
        tokio::select! {
            output = attach_reader.read_event() => {
                match output? {
                    Some(WebSessionAttachEvent::Data(frame)) => match queue_output(&outbound, &frame) {
                        OutboundQueueResult::Queued => {
                            view_pending = true;
                            snapshot_sleep
                                .as_mut()
                                .reset(Instant::now() + SESSION_SNAPSHOT_DEBOUNCE);
                        }
                        OutboundQueueResult::Backpressure => {
                            debug!(share_id = %share_id, "web-share session viewer backlog exceeded; resyncing");
                            queue_fresh_session_snapshot(
                                handler.as_ref(),
                                &outbound,
                                &mut session,
                                &share_id,
                                &mut scrolls,
                            ).await?;
                        }
                        result => {
                            close_slow_viewer(&outbound, &share_id, result).await?;
                            return Ok(());
                        }
                    },
                    Some(WebSessionAttachEvent::Resize) => {
                        snapshot_pending = true;
                        view_pending = false;
                        snapshot_sleep
                            .as_mut()
                            .reset(Instant::now() + SESSION_SNAPSHOT_DEBOUNCE);
                    }
                    None => {
                        notify_revoked_and_close(&outbound, WebShareRevokeReason::SessionGone).await?;
                        return Ok(());
                    }
                }
            }
            message = socket.read_message() => {
                match message? {
                    WebSocketMessage::Text(text) => {
                        if !rate_limiter.try_acquire() {
                            info!(share_id = %share_id, "web_share_client_text_rate_limit_hit");
                            continue;
                        }
                        if let Some(request) = handle_session_client_text(
                            handler.as_ref(),
                            &outbound,
                            &mut session,
                            &text,
                        ).await? {
                            if !rate_limiter.try_acquire() {
                                info!(share_id = %share_id, "web_share_operator_rate_limit_hit");
                                continue;
                            }
                            apply_session_scroll(&mut scrolls, request);
                            queue_fresh_session_snapshot(
                                handler.as_ref(),
                                &outbound,
                                &mut session,
                                &share_id,
                                &mut scrolls,
                            ).await?;
                        }
                    }
                    WebSocketMessage::Binary(bytes) => {
                        if !session.is_operator() {
                            let _ = outbound.write_close_code(4006, "spectator_no_binary").await;
                            return Ok(());
                        }
                        if !rate_limiter.try_acquire() {
                            info!(share_id = %share_id, "web_share_operator_rate_limit_hit");
                            continue;
                        }
                        if !scrolls.is_empty() {
                            scrolls.clear();
                            queue_fresh_session_snapshot(
                                handler.as_ref(),
                                &outbound,
                                &mut session,
                                &share_id,
                                &mut scrolls,
                            ).await?;
                        }
                        if handle_session_operator_binary_frame(handler.as_ref(), &outbound, &mut session, &bytes).await?
                            == SessionOperatorBinaryOutcome::Snapshot
                        {
                            snapshot_pending = true;
                            view_pending = false;
                            snapshot_sleep
                                .as_mut()
                                .reset(Instant::now() + SESSION_SNAPSHOT_DEBOUNCE);
                        }
                    }
                    WebSocketMessage::Close => {
                        let _ = outbound.write_close().await;
                        return Ok(());
                    }
                    WebSocketMessage::Ping(payload) => {
                        outbound.write_pong(&payload).await?;
                    }
                    WebSocketMessage::Pong => {}
                }
            }
            changed = session.revoke_rx.changed() => {
                if changed.is_ok() {
                    let reason = *session.revoke_rx.borrow();
                    if let Some(reason) = reason {
                        notify_revoked_and_close(&outbound, reason).await?;
                        return Ok(());
                    }
                }
            }
            _ = ttl_sleep.as_mut() => {
                notify_revoked_and_close(&outbound, WebShareRevokeReason::TtlExpired).await?;
                return Ok(());
            }
            _ = snapshot_sleep.as_mut(), if snapshot_pending || view_pending => {
                if snapshot_pending {
                    snapshot_pending = false;
                    view_pending = false;
                    debug!(share_id = %share_id, "web-share session attach resized; sending coalesced snapshot");
                    queue_fresh_session_snapshot(
                        handler.as_ref(),
                        &outbound,
                        &mut session,
                        &share_id,
                        &mut scrolls,
                    ).await?;
                } else {
                    view_pending = false;
                    debug!(share_id = %share_id, "web-share session attach changed; refreshing view metadata");
                    queue_fresh_session_view(
                        handler.as_ref(),
                        &outbound,
                        &mut session,
                        &share_id,
                        &mut scrolls,
                    ).await?;
                }
            }
            _ = alive_tick.tick() => {
                if !handler.web_session_alive(session.target()).await {
                    notify_revoked_and_close(&outbound, WebShareRevokeReason::SessionGone).await?;
                    return Ok(());
                }
                send_viewer_count_if_changed(
                    &outbound,
                    &mut last_connection_counts,
                    session.connection_counts(),
                )
                .await?;
            }
        }
    }
}

async fn queue_fresh_session_snapshot(
    handler: &RequestHandler,
    outbound: &WebSocketOutbound,
    session: &mut WebSessionStream,
    share_id: &str,
    scrolls: &mut HashMap<PaneId, usize>,
) -> io::Result<()> {
    let next = handler
        .web_session_snapshot_with_scrolls(session.target(), scrolls)
        .await
        .map_err(|error| io::Error::other(error.to_string()))?;
    normalize_session_scrolls(scrolls, &next);
    if next.size != session.size() {
        queue_or_close(outbound, queue_resize(outbound, next.size), share_id).await?;
    }
    session.snapshot = next;
    queue_session_snapshot_and_view(outbound, &session.snapshot, share_id).await
}

async fn queue_fresh_session_view(
    handler: &RequestHandler,
    outbound: &WebSocketOutbound,
    session: &mut WebSessionStream,
    share_id: &str,
    scrolls: &mut HashMap<PaneId, usize>,
) -> io::Result<()> {
    let next = handler
        .web_session_snapshot_with_scrolls(session.target(), scrolls)
        .await
        .map_err(|error| io::Error::other(error.to_string()))?;
    normalize_session_scrolls(scrolls, &next);
    if next.size != session.size() {
        queue_or_close(outbound, queue_resize(outbound, next.size), share_id).await?;
    }
    session.snapshot = next;
    queue_or_close(
        outbound,
        queue_session_view(outbound, &session.snapshot),
        share_id,
    )
    .await
}

async fn queue_session_snapshot_and_view(
    outbound: &WebSocketOutbound,
    snapshot: &WebSessionSnapshot,
    share_id: &str,
) -> io::Result<()> {
    queue_or_close(
        outbound,
        queue_session_snapshot(outbound, snapshot),
        share_id,
    )
    .await?;
    queue_or_close(outbound, queue_session_view(outbound, snapshot), share_id).await
}

fn apply_session_scroll(scrolls: &mut HashMap<PaneId, usize>, request: SessionScrollRequest) {
    let pane_id = PaneId::new(request.pane_id);
    let current = scrolls.get(&pane_id).copied().unwrap_or_default();
    let next = if request.delta < 0 {
        current.saturating_add(request.delta.unsigned_abs() as usize)
    } else {
        current.saturating_sub(request.delta as usize)
    };
    if next == 0 {
        scrolls.remove(&pane_id);
    } else {
        scrolls.insert(pane_id, next);
    }
}

fn normalize_session_scrolls(scrolls: &mut HashMap<PaneId, usize>, snapshot: &WebSessionSnapshot) {
    let current = snapshot
        .view
        .panes
        .iter()
        .map(|pane| (PaneId::new(pane.id), pane.scroll_offset))
        .collect::<HashMap<_, _>>();
    scrolls.retain(|pane_id, offset| {
        let Some(clamped) = current.get(pane_id).copied() else {
            return false;
        };
        *offset = clamped;
        clamped > 0
    });
}

async fn send_viewer_count_if_changed(
    socket: &WebSocketOutbound,
    last: &mut WebShareConnectionCounts,
    current: WebShareConnectionCounts,
) -> io::Result<()> {
    if *last == current {
        return Ok(());
    }
    send_viewer_count(socket, current).await?;
    *last = current;
    Ok(())
}

async fn notify_revoked_and_close(
    socket: &WebSocketOutbound,
    reason: WebShareRevokeReason,
) -> io::Result<()> {
    let _ = send_revoked(socket, reason).await;
    let _ = socket.write_close_code(1000, reason.as_str()).await;
    Ok(())
}

async fn queue_or_close(
    socket: &WebSocketOutbound,
    result: OutboundQueueResult,
    share_id: &str,
) -> io::Result<()> {
    match result {
        OutboundQueueResult::Queued => Ok(()),
        other => close_slow_viewer(socket, share_id, other).await,
    }
}

async fn close_slow_viewer(
    socket: &WebSocketOutbound,
    share_id: &str,
    result: OutboundQueueResult,
) -> io::Result<()> {
    info!(
        share_id = %share_id,
        ?result,
        "web-share viewer output queue closed"
    );
    let _ = socket
        .write_close_code(SLOW_VIEWER_CLOSE_CODE, "viewer_backpressure")
        .await;
    Ok(())
}

fn duration_until(deadline: SystemTime) -> Duration {
    deadline
        .duration_since(SystemTime::now())
        .unwrap_or(Duration::ZERO)
}
