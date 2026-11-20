//! Session web-share stream loop: attach output, client input and coalesced refresh.

use std::collections::HashMap;
use std::io;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
use tokio::time::{sleep, MissedTickBehavior};
use tracing::{debug, info};

use super::super::keepalive::{KeepaliveAction, WebSocketKeepalive, PAYLOAD as KEEPALIVE_PAYLOAD};
use super::super::rate_limit::OperatorRateLimiter;
use super::selection_delivery::{
    queue_fresh_session_snapshot, queue_fresh_session_view, queue_selected_session_delivery,
    queue_session_scroll_patch,
};
use super::{
    apply_session_scroll, close_slow_viewer, duration_until, is_recoverable_session_queue_pressure,
    notify_revoked_and_close, queue_session_keyframe_or_close,
    schedule_interactive_session_refresh, schedule_session_refresh, send_viewer_count_if_changed,
    wait_for_normal_shutdown, WebSocketReadPump, KEEPALIVE_TIMEOUT_CLOSE_CODE,
    KEEPALIVE_TIMEOUT_REASON,
};
use crate::handler::{RequestHandler, WebSessionAttachEvent, WebSessionStream};
use crate::web::crypto::EncryptedWebSocketReader;
use crate::web::outbound::{OutboundQueueResult, WebSocketOutbound};
use crate::web::protocol::{
    handle_session_client_text, handle_session_operator_binary_frame, queue_output,
    SessionClientTextOutcome, SessionOperatorBinaryOutcome,
};
use crate::web::stream_sanitizer::WebTerminalSanitizer;
use crate::web::websocket::WebSocketMessage;
use crate::web::WebShareRevokeReason;

pub(in crate::web::server) async fn serve_session_loop(
    handler: Arc<RequestHandler>,
    socket: EncryptedWebSocketReader,
    outbound: WebSocketOutbound,
    share_id: String,
    mut session: WebSessionStream,
    supports_session_pane_frame: bool,
    mut shutdown: watch::Receiver<bool>,
) -> io::Result<()> {
    let mut scrolls = HashMap::new();
    let mut sanitizer = WebTerminalSanitizer::for_role(session.connect_role());
    queue_session_keyframe_or_close(
        &outbound,
        None,
        &session.snapshot,
        &share_id,
        &mut sanitizer,
    )
    .await?;
    let mut attach_reader = session.take_attach_reader();
    let mut rate_limiter = OperatorRateLimiter::new();
    let mut last_connection_counts = session.connection_counts();
    let mut alive_tick = tokio::time::interval(Duration::from_millis(500));
    alive_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut keepalive = WebSocketKeepalive::new();
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
            biased;
            () = wait_for_normal_shutdown(&mut shutdown) => {
                // See the pane loop: the transport is cancel-safe, while any
                // already-started mutation retains its independent Drain guard.
                return Ok(());
            }
            output = attach_reader.read_event() => {
                match output? {
                    Some(WebSessionAttachEvent::Data(frame)) => {
                        if snapshot_pending {
                            sanitizer.reset();
                            continue;
                        }
                        if let Some(selection) = session.selected_window() {
                            if let Err(error) = handler
                                .validate_web_session_window_selection(session.target(), selection)
                                .await
                            {
                                debug!(
                                    share_id = %share_id,
                                    %error,
                                    "web-share selected session window disappeared; closing before attach output"
                                );
                                sanitizer.reset();
                                return Ok(());
                            }
                        }
                        #[cfg(test)]
                        super::super::selection_delivery_pause::before_queue(&share_id, false).await;
                        if !scrolls.is_empty() && session.snapshot.has_floating_panes() {
                            // A live renderer delta belongs to the unscrolled
                            // presentation. Once a floating scene is scrolled,
                            // only a complete recapture can preserve image,
                            // view metadata, clipping, and occlusion together.
                            sanitizer.reset();
                            snapshot_pending = true;
                            view_pending = false;
                            schedule_session_refresh(
                                snapshot_sleep.as_mut(),
                                &mut pending_started_at,
                            );
                            continue;
                        }
                        let mut safe = Vec::with_capacity(frame.len());
                        sanitizer.push(&frame, &mut safe);
                        if safe.is_empty() {
                            continue;
                        }
                        match queue_selected_session_delivery(handler.as_ref(), &session, || {
                            queue_output(&outbound, &safe)
                        }).await? {
                            OutboundQueueResult::Queued => {
                                #[cfg(test)]
                                super::super::selection_delivery_pause::raw_queued(&share_id, &safe);
                                view_pending = true;
                                schedule_session_refresh(
                                    snapshot_sleep.as_mut(),
                                    &mut pending_started_at,
                                );
                            }
                            result if is_recoverable_session_queue_pressure(result) => {
                                debug!(share_id = %share_id, "web-share session viewer backlog exceeded; resyncing");
                                sanitizer.reset();
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
                        sanitizer.reset();
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
                                        &mut sanitizer,
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
                                &mut sanitizer,
                            ).await? {
                                OutboundQueueResult::Queued => sanitizer.reset(),
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
                        #[cfg(test)]
                        super::super::selection_delivery_pause::drain_before_pong(&share_id, &payload, &outbound).await;
                        outbound.write_pong(&payload).await?;
                    }
                    WebSocketMessage::Pong(payload) => keepalive.acknowledge_pong(&payload),
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
                        &mut sanitizer,
                    ).await? {
                        OutboundQueueResult::Queued => {
                            sanitizer.reset();
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
                        &mut sanitizer,
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
            action = keepalive.next_action() => {
                match action {
                    KeepaliveAction::Ping => outbound.write_ping(KEEPALIVE_PAYLOAD).await?,
                    KeepaliveAction::TimedOut => {
                        let _ = outbound
                            .write_close_code(
                                KEEPALIVE_TIMEOUT_CLOSE_CODE,
                                KEEPALIVE_TIMEOUT_REASON,
                            )
                            .await;
                        return Ok(());
                    }
                }
            }
        }
    }
}
