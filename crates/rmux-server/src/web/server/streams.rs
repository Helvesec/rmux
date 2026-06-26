use std::collections::HashMap;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use rmux_core::events::OutputCursorItem;
use rmux_core::PaneId;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{sleep, Instant, MissedTickBehavior, Sleep};
use tracing::{debug, info};

use super::rate_limit::OperatorRateLimiter;
use crate::handler::{
    RequestHandler, WebPaneStream, WebSessionAttachEvent, WebSessionSnapshot, WebSessionStream,
};
use crate::web::crypto::EncryptedWebSocketReader;
use crate::web::outbound::{OutboundQueueResult, WebSocketOutbound};
use crate::web::protocol::{
    handle_pane_client_text, handle_pane_operator_binary_frame, handle_session_client_text,
    handle_session_operator_binary_frame, queue_output, queue_session_keyframe,
    queue_session_pane_frame, queue_session_view, queue_snapshot, send_revoked, send_viewer_count,
    SessionClientTextOutcome, SessionOperatorBinaryOutcome, SessionScrollRequest,
};
use crate::web::websocket::WebSocketMessage;
use crate::web::{WebShareConnectionCounts, WebShareRevokeReason};

const SLOW_VIEWER_CLOSE_CODE: u16 = 4001;
const SESSION_SNAPSHOT_DEBOUNCE: Duration = Duration::from_millis(50);
const SESSION_SNAPSHOT_MAX_WAIT: Duration = Duration::from_millis(200);
const SESSION_INTERACTIVE_DEBOUNCE: Duration = Duration::from_millis(8);
const SESSION_INTERACTIVE_MAX_WAIT: Duration = Duration::from_millis(32);

pub(super) async fn serve_pane_loop(
    handler: Arc<RequestHandler>,
    socket: EncryptedWebSocketReader,
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
    alive_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let snapshot_sleep = sleep(Duration::from_secs(365 * 24 * 60 * 60));
    tokio::pin!(snapshot_sleep);
    let mut snapshot_pending = false;
    let mut pending_started_at = None;
    let ttl_delay = pane
        .expires_at()
        .map(duration_until)
        .unwrap_or_else(|| Duration::from_secs(365 * 24 * 60 * 60));
    let ttl_sleep = sleep(ttl_delay);
    tokio::pin!(ttl_sleep);
    let mut inbound = WebSocketReadPump::new(socket);

    loop {
        tokio::select! {
            item = pane.output.recv() => {
                match item {
                    OutputCursorItem::Event(event) => {
                        if snapshot_pending {
                            continue;
                        }
                        match queue_output(&outbound, event.bytes()) {
                            OutboundQueueResult::Queued => {}
                            result if is_recoverable_session_queue_pressure(result) => {
                                debug!(share_id = %share_id, "web-share viewer backlog exceeded; resyncing");
                                snapshot_pending = true;
                                schedule_session_refresh(
                                    snapshot_sleep.as_mut(),
                                    &mut pending_started_at,
                                );
                            }
                            result => {
                                close_slow_viewer(&outbound, &share_id, result).await?;
                                return Ok(());
                            }
                        }
                    }
                    OutputCursorItem::Gap(gap) => {
                        debug!(missed = gap.missed_events(), "web-share spectator resync");
                        snapshot_pending = true;
                        schedule_session_refresh(
                            snapshot_sleep.as_mut(),
                            &mut pending_started_at,
                        );
                    }
                }
            }
            message = inbound.recv() => {
                let Some(message) = message? else {
                    return Ok(());
                };
                match message {
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
            _ = snapshot_sleep.as_mut(), if snapshot_pending => {
                match queue_fresh_pane_snapshot(handler.as_ref(), &outbound, &mut pane).await? {
                    OutboundQueueResult::Queued => {
                        snapshot_pending = false;
                        pending_started_at = None;
                    }
                    result if is_recoverable_session_queue_pressure(result) => {
                        snapshot_pending = true;
                        pending_started_at = None;
                        schedule_session_refresh(
                            snapshot_sleep.as_mut(),
                            &mut pending_started_at,
                        );
                    }
                    result => {
                        close_slow_viewer(&outbound, &share_id, result).await?;
                        return Ok(());
                    }
                }
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

struct WebSocketReadPump {
    messages: mpsc::Receiver<io::Result<WebSocketMessage>>,
    task: JoinHandle<()>,
}

impl WebSocketReadPump {
    fn new(mut socket: EncryptedWebSocketReader) -> Self {
        let (tx, messages) = mpsc::channel(16);
        let task = tokio::spawn(async move {
            loop {
                let result = socket.read_message().await;
                let should_stop = matches!(result, Err(_) | Ok(WebSocketMessage::Close));
                if tx.send(result).await.is_err() {
                    break;
                }
                if should_stop {
                    break;
                }
            }
        });
        Self { messages, task }
    }

    async fn recv(&mut self) -> io::Result<Option<WebSocketMessage>> {
        match self.messages.recv().await {
            Some(result) => result.map(Some),
            None => Ok(None),
        }
    }
}

impl Drop for WebSocketReadPump {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn queue_fresh_pane_snapshot(
    handler: &RequestHandler,
    outbound: &WebSocketOutbound,
    pane: &mut WebPaneStream,
) -> io::Result<OutboundQueueResult> {
    let target = pane.target().clone();
    let (snapshot, output) = handler
        .web_resnapshot(&target)
        .await
        .map_err(|error| io::Error::other(error.to_string()))?;
    pane.snapshot = snapshot;
    pane.output = output;
    Ok(queue_snapshot(outbound, &pane.snapshot))
}

pub(super) async fn serve_session_loop(
    handler: Arc<RequestHandler>,
    socket: EncryptedWebSocketReader,
    outbound: WebSocketOutbound,
    share_id: String,
    mut session: WebSessionStream,
    supports_session_pane_frame: bool,
) -> io::Result<()> {
    let mut scrolls = HashMap::new();
    queue_session_keyframe_or_close(&outbound, None, &session.snapshot, &share_id).await?;
    let mut attach_reader = session.take_attach_reader();
    let mut rate_limiter = OperatorRateLimiter::new();
    let mut last_connection_counts = session.connection_counts();
    let mut alive_tick = tokio::time::interval(Duration::from_millis(500));
    alive_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
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
    let mut pending_started_at = None;
    let mut inbound = WebSocketReadPump::new(socket);

    loop {
        tokio::select! {
            output = attach_reader.read_event() => {
                match output? {
                    Some(WebSessionAttachEvent::Data(frame)) => {
                        if snapshot_pending {
                            continue;
                        }
                        match queue_output(&outbound, &frame) {
                            OutboundQueueResult::Queued => {
                                view_pending = true;
                                schedule_session_refresh(
                                    snapshot_sleep.as_mut(),
                                    &mut pending_started_at,
                                );
                            }
                            result if is_recoverable_session_queue_pressure(result) => {
                                debug!(share_id = %share_id, "web-share session viewer backlog exceeded; resyncing");
                                snapshot_pending = true;
                                view_pending = false;
                                schedule_session_refresh(
                                    snapshot_sleep.as_mut(),
                                    &mut pending_started_at,
                                );
                            }
                            result => {
                                close_slow_viewer(&outbound, &share_id, result).await?;
                                return Ok(());
                            }
                        }
                    },
                    Some(WebSessionAttachEvent::Resize) => {
                        snapshot_pending = true;
                        view_pending = false;
                        schedule_session_refresh(snapshot_sleep.as_mut(), &mut pending_started_at);
                    }
                    None => {
                        notify_revoked_and_close(&outbound, WebShareRevokeReason::SessionGone).await?;
                        return Ok(());
                    }
                }
            }
            message = inbound.recv() => {
                let Some(message) = message? else {
                    return Ok(());
                };
                match message {
                    WebSocketMessage::Text(text) => {
                        if !rate_limiter.try_acquire() {
                            info!(share_id = %share_id, "web_share_client_text_rate_limit_hit");
                            continue;
                        }
                        match handle_session_client_text(
                            handler.as_ref(),
                            &outbound,
                            &mut session,
                            &text,
                        ).await? {
                            SessionClientTextOutcome::None => {}
                            SessionClientTextOutcome::Scroll(request) => {
                                apply_session_scroll(&mut scrolls, request, &session.snapshot);
                                if !snapshot_pending && !view_pending {
                                    match queue_session_scroll_patch(
                                        handler.as_ref(),
                                        &outbound,
                                        &mut session,
                                        &share_id,
                                        &mut scrolls,
                                        supports_session_pane_frame,
                                    ).await? {
                                        Some(OutboundQueueResult::Queued) => {
                                            pending_started_at = None;
                                        }
                                        Some(result)
                                            if is_recoverable_session_queue_pressure(result) =>
                                        {
                                            snapshot_pending = true;
                                            view_pending = false;
                                            schedule_interactive_session_refresh(
                                                snapshot_sleep.as_mut(),
                                                &mut pending_started_at,
                                            );
                                        }
                                        Some(result) => {
                                            close_slow_viewer(&outbound, &share_id, result)
                                                .await?;
                                            return Ok(());
                                        }
                                        None => {
                                            snapshot_pending = true;
                                            view_pending = false;
                                            schedule_interactive_session_refresh(
                                                snapshot_sleep.as_mut(),
                                                &mut pending_started_at,
                                            );
                                        }
                                    }
                                } else {
                                    snapshot_pending = true;
                                    view_pending = false;
                                    schedule_interactive_session_refresh(
                                        snapshot_sleep.as_mut(),
                                        &mut pending_started_at,
                                    );
                                }
                            }
                            SessionClientTextOutcome::Snapshot => {
                                snapshot_pending = true;
                                view_pending = false;
                                schedule_session_refresh(
                                    snapshot_sleep.as_mut(),
                                    &mut pending_started_at,
                                );
                            }
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
                            match queue_fresh_session_snapshot(
                                handler.as_ref(),
                                &outbound,
                                &mut session,
                                &share_id,
                                &mut scrolls,
                            ).await? {
                                OutboundQueueResult::Queued => {}
                                result if is_recoverable_session_queue_pressure(result) => {
                                    snapshot_pending = true;
                                    view_pending = false;
                                    pending_started_at = None;
                                    schedule_session_refresh(
                                        snapshot_sleep.as_mut(),
                                        &mut pending_started_at,
                                    );
                                }
                                result => {
                                    close_slow_viewer(&outbound, &share_id, result).await?;
                                    return Ok(());
                                }
                            }
                        }
                        match handle_session_operator_binary_frame(handler.as_ref(), &outbound, &mut session, &bytes).await? {
                            SessionOperatorBinaryOutcome::None => {}
                            SessionOperatorBinaryOutcome::Resize => {
                                snapshot_pending = true;
                                view_pending = false;
                                schedule_session_refresh(
                                    snapshot_sleep.as_mut(),
                                    &mut pending_started_at,
                                );
                            }
                            SessionOperatorBinaryOutcome::Snapshot => {
                                snapshot_pending = true;
                                view_pending = false;
                                schedule_session_refresh(
                                    snapshot_sleep.as_mut(),
                                    &mut pending_started_at,
                                );
                            }
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
                    debug!(share_id = %share_id, "web-share session attach resized; sending coalesced snapshot");
                    match queue_fresh_session_snapshot(
                        handler.as_ref(),
                        &outbound,
                        &mut session,
                        &share_id,
                        &mut scrolls,
                    ).await? {
                        OutboundQueueResult::Queued => {
                            snapshot_pending = false;
                            view_pending = false;
                            pending_started_at = None;
                        }
                        result if is_recoverable_session_queue_pressure(result) => {
                            snapshot_pending = true;
                            view_pending = false;
                            pending_started_at = None;
                            schedule_session_refresh(
                                snapshot_sleep.as_mut(),
                                &mut pending_started_at,
                            );
                        }
                        result => {
                            close_slow_viewer(&outbound, &share_id, result).await?;
                            return Ok(());
                        }
                    }
                } else {
                    debug!(share_id = %share_id, "web-share session attach changed; refreshing view metadata");
                    match queue_fresh_session_view(
                        handler.as_ref(),
                        &outbound,
                        &mut session,
                        &share_id,
                        &mut scrolls,
                    ).await? {
                        OutboundQueueResult::Queued => {
                            view_pending = false;
                            if !snapshot_pending {
                                pending_started_at = None;
                            }
                        }
                        result if is_recoverable_session_queue_pressure(result) => {
                            snapshot_pending = true;
                            view_pending = false;
                            pending_started_at = None;
                            schedule_session_refresh(
                                snapshot_sleep.as_mut(),
                                &mut pending_started_at,
                            );
                        }
                        result => {
                            close_slow_viewer(&outbound, &share_id, result).await?;
                            return Ok(());
                        }
                    }
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
) -> io::Result<OutboundQueueResult> {
    let next = handler
        .web_session_snapshot_with_scrolls(
            session.target(),
            session.selected_window_index(),
            scrolls,
        )
        .await
        .map_err(|error| io::Error::other(error.to_string()))?;
    normalize_session_scrolls(scrolls, &next);
    let resize = (next.size != session.size()).then_some(next.size);
    session.snapshot = next;
    let result = queue_session_keyframe(outbound, resize, &session.snapshot);
    log_recoverable_session_queue_result(share_id, result);
    Ok(result)
}

async fn queue_session_scroll_patch(
    handler: &RequestHandler,
    outbound: &WebSocketOutbound,
    session: &mut WebSessionStream,
    share_id: &str,
    scrolls: &mut HashMap<PaneId, usize>,
    supports_session_pane_frame: bool,
) -> io::Result<Option<OutboundQueueResult>> {
    if !supports_session_pane_frame {
        return Ok(None);
    }
    let Some((&pane_id, &top_line)) = scrolls.iter().next() else {
        return Ok(None);
    };
    if scrolls.len() != 1 {
        return Ok(None);
    }
    let Some(frame) = handler
        .web_session_pane_scroll_frame(
            session.target(),
            pane_id,
            top_line,
            session.selected_window_index(),
        )
        .await
        .map_err(|error| io::Error::other(error.to_string()))?
    else {
        return Ok(None);
    };
    if !pane_frame_matches_snapshot(&session.snapshot, &frame) {
        return Ok(None);
    }
    let result = queue_session_pane_frame(outbound, &frame);
    log_recoverable_session_queue_result(share_id, result);
    if result == OutboundQueueResult::Queued {
        update_session_snapshot_pane(&mut session.snapshot, &frame);
        normalize_session_scroll_from_pane_frame(scrolls, &frame);
    }
    Ok(Some(result))
}

fn pane_frame_matches_snapshot(
    snapshot: &WebSessionSnapshot,
    frame: &crate::handler::WebSessionPaneFrame,
) -> bool {
    snapshot.size == frame.size
        && snapshot.view.size == frame.size
        && snapshot.view.panes.iter().any(|pane| {
            pane.id == frame.pane.id
                && pane.x == frame.pane.x
                && pane.y == frame.pane.y
                && pane.cols == frame.pane.cols
                && pane.rows == frame.pane.rows
        })
}

fn update_session_snapshot_pane(
    snapshot: &mut WebSessionSnapshot,
    frame: &crate::handler::WebSessionPaneFrame,
) {
    if let Some(pane) = snapshot
        .view
        .panes
        .iter_mut()
        .find(|pane| pane.id == frame.pane.id)
    {
        *pane = frame.pane.clone();
    }
}

fn normalize_session_scroll_from_pane_frame(
    scrolls: &mut HashMap<PaneId, usize>,
    frame: &crate::handler::WebSessionPaneFrame,
) {
    let pane_id = PaneId::new(frame.pane.id);
    if frame.pane.scroll_offset == 0 {
        scrolls.remove(&pane_id);
    } else {
        scrolls.insert(
            pane_id,
            pane_top_line(frame.pane.history_size, frame.pane.scroll_offset),
        );
    }
}

async fn queue_fresh_session_view(
    handler: &RequestHandler,
    outbound: &WebSocketOutbound,
    session: &mut WebSessionStream,
    share_id: &str,
    scrolls: &mut HashMap<PaneId, usize>,
) -> io::Result<OutboundQueueResult> {
    let next = handler
        .web_session_snapshot_with_scrolls(
            session.target(),
            session.selected_window_index(),
            scrolls,
        )
        .await
        .map_err(|error| io::Error::other(error.to_string()))?;
    normalize_session_scrolls(scrolls, &next);
    if next.size != session.size() {
        let resize = next.size;
        session.snapshot = next;
        let result = queue_session_keyframe(outbound, Some(resize), &session.snapshot);
        log_recoverable_session_queue_result(share_id, result);
        return Ok(result);
    }
    session.snapshot = next;
    let result = queue_session_view(outbound, &session.snapshot);
    log_recoverable_session_queue_result(share_id, result);
    Ok(result)
}

async fn queue_session_keyframe_or_close(
    outbound: &WebSocketOutbound,
    resize: Option<rmux_proto::TerminalSize>,
    snapshot: &WebSessionSnapshot,
    share_id: &str,
) -> io::Result<()> {
    queue_or_close(
        outbound,
        queue_session_keyframe(outbound, resize, snapshot),
        share_id,
    )
    .await
}

fn log_recoverable_session_queue_result(share_id: &str, result: OutboundQueueResult) {
    if is_recoverable_session_queue_pressure(result) {
        debug!(
            share_id = %share_id,
            ?result,
            "web-share session keyframe deferred by output pressure"
        );
    }
}

fn is_recoverable_session_queue_pressure(result: OutboundQueueResult) -> bool {
    matches!(
        result,
        OutboundQueueResult::Backpressure | OutboundQueueResult::Full
    )
}

fn apply_session_scroll(
    scrolls: &mut HashMap<PaneId, usize>,
    request: SessionScrollRequest,
    snapshot: &WebSessionSnapshot,
) {
    let pane_id = PaneId::new(request.pane_id);
    let Some(pane) = snapshot
        .view
        .panes
        .iter()
        .find(|pane| pane.id == request.pane_id)
    else {
        scrolls.remove(&pane_id);
        return;
    };
    if pane.history_size == 0 || pane.alternate_on {
        scrolls.remove(&pane_id);
        return;
    }

    let current_top = scrolls
        .get(&pane_id)
        .copied()
        .unwrap_or_else(|| pane_top_line(pane.history_size, pane.scroll_offset));
    let next_top = if request.delta < 0 {
        current_top.saturating_sub(request.delta.unsigned_abs() as usize)
    } else {
        current_top.saturating_add(request.delta as usize)
    };
    if next_top >= pane.history_size {
        scrolls.remove(&pane_id);
    } else {
        scrolls.insert(pane_id, next_top);
    }
}

fn normalize_session_scrolls(scrolls: &mut HashMap<PaneId, usize>, snapshot: &WebSessionSnapshot) {
    let current = snapshot
        .view
        .panes
        .iter()
        .map(|pane| (PaneId::new(pane.id), (pane.history_size, pane.alternate_on)))
        .collect::<HashMap<_, _>>();
    scrolls.retain(|pane_id, top_line| {
        let Some((history_size, alternate_on)) = current.get(pane_id).copied() else {
            return false;
        };
        if alternate_on || history_size == 0 {
            return false;
        }
        *top_line = (*top_line).min(history_size);
        *top_line < history_size
    });
}

fn pane_top_line(history_size: usize, scroll_offset: usize) -> usize {
    history_size.saturating_sub(scroll_offset.min(history_size))
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

fn schedule_session_refresh(
    snapshot_sleep: Pin<&mut Sleep>,
    pending_started_at: &mut Option<Instant>,
) {
    let now = Instant::now();
    let deadline = next_session_refresh_deadline(
        now,
        pending_started_at,
        SESSION_SNAPSHOT_DEBOUNCE,
        SESSION_SNAPSHOT_MAX_WAIT,
    );
    snapshot_sleep.reset(deadline);
}

fn schedule_interactive_session_refresh(
    snapshot_sleep: Pin<&mut Sleep>,
    pending_started_at: &mut Option<Instant>,
) {
    let now = Instant::now();
    let deadline = next_session_refresh_deadline(
        now,
        pending_started_at,
        SESSION_INTERACTIVE_DEBOUNCE,
        SESSION_INTERACTIVE_MAX_WAIT,
    );
    snapshot_sleep.reset(deadline);
}

fn next_session_refresh_deadline(
    now: Instant,
    pending_started_at: &mut Option<Instant>,
    debounce: Duration,
    max_wait: Duration,
) -> Instant {
    let started_at = *pending_started_at.get_or_insert(now);
    (now + debounce).min(started_at + max_wait)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use rmux_core::PaneId;
    use rmux_proto::TerminalSize;

    use crate::handler::{
        TestWebSessionView, WebSessionPaneFrame, WebSessionPaneView, WebSessionSnapshot,
    };
    use crate::web::outbound::OutboundQueueResult;

    use super::{
        apply_session_scroll, is_recoverable_session_queue_pressure, next_session_refresh_deadline,
        normalize_session_scroll_from_pane_frame, normalize_session_scrolls,
    };

    #[test]
    fn session_output_pressure_is_recoverable_until_writer_closes() {
        assert!(is_recoverable_session_queue_pressure(
            OutboundQueueResult::Backpressure
        ));
        assert!(is_recoverable_session_queue_pressure(
            OutboundQueueResult::Full
        ));
        assert!(!is_recoverable_session_queue_pressure(
            OutboundQueueResult::Closed
        ));
        assert!(!is_recoverable_session_queue_pressure(
            OutboundQueueResult::Queued
        ));
    }

    #[test]
    fn session_refresh_deadline_is_bounded_by_max_wait() {
        let started = tokio::time::Instant::now();
        let mut pending_started_at = None;

        assert_eq!(
            next_session_refresh_deadline(
                started,
                &mut pending_started_at,
                super::SESSION_SNAPSHOT_DEBOUNCE,
                super::SESSION_SNAPSHOT_MAX_WAIT,
            ),
            started + super::SESSION_SNAPSHOT_DEBOUNCE
        );
        assert_eq!(pending_started_at, Some(started));

        let later = started + super::SESSION_SNAPSHOT_MAX_WAIT - super::Duration::from_millis(10);
        assert_eq!(
            next_session_refresh_deadline(
                later,
                &mut pending_started_at,
                super::SESSION_SNAPSHOT_DEBOUNCE,
                super::SESSION_SNAPSHOT_MAX_WAIT,
            ),
            started + super::SESSION_SNAPSHOT_MAX_WAIT
        );
    }

    #[test]
    fn interactive_session_refresh_uses_short_latency_budget() {
        let started = tokio::time::Instant::now();
        let mut pending_started_at = None;

        assert_eq!(
            next_session_refresh_deadline(
                started,
                &mut pending_started_at,
                super::SESSION_INTERACTIVE_DEBOUNCE,
                super::SESSION_INTERACTIVE_MAX_WAIT,
            ),
            started + super::SESSION_INTERACTIVE_DEBOUNCE
        );

        let later = started + super::SESSION_INTERACTIVE_MAX_WAIT - super::Duration::from_millis(2);
        assert_eq!(
            next_session_refresh_deadline(
                later,
                &mut pending_started_at,
                super::SESSION_INTERACTIVE_DEBOUNCE,
                super::SESSION_INTERACTIVE_MAX_WAIT,
            ),
            started + super::SESSION_INTERACTIVE_MAX_WAIT
        );
    }

    #[test]
    fn pane_frame_scroll_normalization_keeps_stable_top_line() {
        let pane_id = PaneId::new(7);
        let mut scrolls = HashMap::from([(pane_id, 10_000)]);
        let frame = WebSessionPaneFrame::new(
            TerminalSize { cols: 80, rows: 24 },
            WebSessionPaneView {
                id: pane_id.as_u32(),
                x: 0,
                y: 0,
                cols: 80,
                rows: 23,
                active: true,
                history_size: 120,
                scroll_offset: 37,
                alternate_on: false,
                mouse_on: false,
            },
            Vec::new(),
        );

        normalize_session_scroll_from_pane_frame(&mut scrolls, &frame);

        assert_eq!(scrolls.get(&pane_id), Some(&83));
    }

    #[test]
    fn pane_frame_scroll_normalization_removes_live_offset() {
        let pane_id = PaneId::new(7);
        let mut scrolls = HashMap::from([(pane_id, 12)]);
        let frame = WebSessionPaneFrame::new(
            TerminalSize { cols: 80, rows: 24 },
            WebSessionPaneView {
                id: pane_id.as_u32(),
                x: 0,
                y: 0,
                cols: 80,
                rows: 23,
                active: true,
                history_size: 120,
                scroll_offset: 0,
                alternate_on: false,
                mouse_on: false,
            },
            Vec::new(),
        );

        normalize_session_scroll_from_pane_frame(&mut scrolls, &frame);

        assert!(!scrolls.contains_key(&pane_id));
    }

    #[test]
    fn session_scroll_anchor_survives_history_growth() {
        let pane_id = PaneId::new(7);
        let mut scrolls = HashMap::from([(pane_id, 83)]);
        let snapshot = WebSessionSnapshot::new(
            TerminalSize { cols: 80, rows: 24 },
            Vec::new(),
            TestWebSessionView {
                size: TerminalSize { cols: 80, rows: 24 },
                windows: Vec::new(),
                panes: vec![WebSessionPaneView {
                    id: pane_id.as_u32(),
                    x: 0,
                    y: 0,
                    cols: 80,
                    rows: 23,
                    active: true,
                    history_size: 220,
                    scroll_offset: 137,
                    alternate_on: false,
                    mouse_on: false,
                }],
            },
            0,
            0,
        );

        normalize_session_scrolls(&mut scrolls, &snapshot);

        assert_eq!(scrolls.get(&pane_id), Some(&83));
    }

    #[test]
    fn session_scroll_delta_uses_snapshot_top_line() {
        let pane_id = PaneId::new(7);
        let mut scrolls = HashMap::new();
        let snapshot = WebSessionSnapshot::new(
            TerminalSize { cols: 80, rows: 24 },
            Vec::new(),
            TestWebSessionView {
                size: TerminalSize { cols: 80, rows: 24 },
                windows: Vec::new(),
                panes: vec![WebSessionPaneView {
                    id: pane_id.as_u32(),
                    x: 0,
                    y: 0,
                    cols: 80,
                    rows: 23,
                    active: true,
                    history_size: 120,
                    scroll_offset: 0,
                    alternate_on: false,
                    mouse_on: false,
                }],
            },
            0,
            0,
        );

        apply_session_scroll(
            &mut scrolls,
            crate::web::protocol::SessionScrollRequest {
                pane_id: pane_id.as_u32(),
                delta: -20,
            },
            &snapshot,
        );

        assert_eq!(scrolls.get(&pane_id), Some(&100));
    }
}
