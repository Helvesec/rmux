//! Pane web-share stream loop and its coalesced pane resnapshot.

use std::io;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
use tokio::time::{sleep, MissedTickBehavior};
use tracing::{debug, info};

use super::super::keepalive::{KeepaliveAction, WebSocketKeepalive, PAYLOAD as KEEPALIVE_PAYLOAD};
use super::super::rate_limit::OperatorRateLimiter;
use super::{
    close_slow_viewer, duration_until, is_recoverable_session_queue_pressure,
    notify_revoked_and_close, queue_or_close, schedule_session_refresh,
    send_viewer_count_if_changed, wait_for_normal_shutdown, WebSocketReadPump,
    KEEPALIVE_TIMEOUT_CLOSE_CODE, KEEPALIVE_TIMEOUT_REASON,
};
use crate::handler::{RequestHandler, WebPaneStream};
use crate::pane_io::PaneObservationItem;
use crate::web::crypto::EncryptedWebSocketReader;
use crate::web::outbound::{OutboundQueueResult, WebSocketOutbound};
use crate::web::protocol::{
    handle_pane_client_text, handle_pane_operator_binary_frame, queue_output, queue_snapshot,
};
use crate::web::stream_sanitizer::WebTerminalSanitizer;
use crate::web::websocket::WebSocketMessage;
use crate::web::WebShareRevokeReason;

pub(in crate::web::server) async fn serve_pane_loop(
    handler: Arc<RequestHandler>,
    socket: EncryptedWebSocketReader,
    outbound: WebSocketOutbound,
    share_id: String,
    mut pane: WebPaneStream,
    supports_pane_recovery_coverage: bool,
    mut shutdown: watch::Receiver<bool>,
) -> io::Result<()> {
    let mut sanitizer = WebTerminalSanitizer::for_role(pane.connect_role());
    let initial_snapshot = queue_snapshot(
        &outbound,
        &pane.snapshot,
        &mut sanitizer,
        supports_pane_recovery_coverage,
    );
    queue_or_close(&outbound, initial_snapshot, &share_id).await?;
    let mut rate_limiter = OperatorRateLimiter::new();
    let mut last_connection_counts = pane.connection_counts();
    let mut alive_tick = tokio::time::interval(Duration::from_millis(500));
    alive_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut keepalive = WebSocketKeepalive::new();
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
            biased;
            () = wait_for_normal_shutdown(&mut shutdown) => {
                // Dropping the outbound side aborts a blocked socket writer.
                // Shutdown must not wait on an unresponsive browser before it
                // can account for the stream and its attach cleanup.
                return Ok(());
            }
            item = pane.output.recv_observed() => {
                match item {
                    PaneObservationItem::Output(rmux_core::events::OutputCursorItem::Event(event)) => {
                        if snapshot_pending {
                            continue;
                        }
                        let mut safe = Vec::with_capacity(event.bytes().len());
                        sanitizer.push(event.bytes(), &mut safe);
                        if safe.is_empty() {
                            continue;
                        }
                        match queue_output(&outbound, &safe) {
                            OutboundQueueResult::Queued => {}
                            result if is_recoverable_session_queue_pressure(result) => {
                                debug!(share_id = %share_id, "web-share viewer backlog exceeded; resyncing");
                                sanitizer.reset();
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
                    PaneObservationItem::Output(rmux_core::events::OutputCursorItem::Gap(gap)) => {
                        debug!(missed = gap.missed_events(), "web-share spectator resync");
                        sanitizer.reset();
                        snapshot_pending = true;
                        schedule_session_refresh(
                            snapshot_sleep.as_mut(),
                            &mut pending_started_at,
                        );
                    }
                    PaneObservationItem::Invalidated(invalidation) => {
                        debug!(
                            reason = ?invalidation.reason,
                            revision = invalidation.boundary.invalidation_revision,
                            "web-share pane state invalidated; resyncing"
                        );
                        sanitizer.reset();
                        snapshot_pending = true;
                        schedule_session_refresh(
                            snapshot_sleep.as_mut(),
                            &mut pending_started_at,
                        );
                    }
                    PaneObservationItem::ProcessExited { .. } => {
                        // Child EOF is not logical pane removal. Kept panes
                        // remain attached and may later be respawned.
                        sanitizer.reset();
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
                    WebSocketMessage::Pong(payload) => keepalive.acknowledge_pong(&payload),
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
                match queue_fresh_pane_snapshot(
                    handler.as_ref(),
                    &outbound,
                    &mut pane,
                    &mut sanitizer,
                    supports_pane_recovery_coverage,
                ).await? {
                    OutboundQueueResult::Queued => {
                        snapshot_pending = false;
                        pending_started_at = None;
                    }
                    result if is_recoverable_session_queue_pressure(result) => {
                        sanitizer.reset();
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
                match handler
                    .current_web_pane_target(pane.session_id(), pane.target())
                    .await
                {
                    Ok(target) => pane.set_target(target),
                    Err(_) => {
                        notify_revoked_and_close(&outbound, WebShareRevokeReason::PaneGone).await?;
                        return Ok(());
                    }
                }
                send_viewer_count_if_changed(
                    &outbound,
                    &mut last_connection_counts,
                    pane.connection_counts(),
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

async fn queue_fresh_pane_snapshot(
    handler: &RequestHandler,
    outbound: &WebSocketOutbound,
    pane: &mut WebPaneStream,
    sanitizer: &mut WebTerminalSanitizer,
    supports_pane_recovery_coverage: bool,
) -> io::Result<OutboundQueueResult> {
    let target = handler
        .current_web_pane_target(pane.session_id(), pane.target())
        .await
        .map_err(|error| io::Error::other(error.to_string()))?;
    pane.set_target(target.clone());
    let (snapshot, output) = handler
        .web_resnapshot(&target)
        .await
        .map_err(|error| io::Error::other(error.to_string()))?;
    pane.snapshot = snapshot;
    pane.output = output;
    Ok(queue_snapshot(
        outbound,
        &pane.snapshot,
        sanitizer,
        supports_pane_recovery_coverage,
    ))
}
