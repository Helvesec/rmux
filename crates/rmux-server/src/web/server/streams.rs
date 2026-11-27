use std::collections::HashMap;
use std::io;
use std::pin::Pin;
use std::time::{Duration, SystemTime};

use rmux_core::PaneId;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::{Instant, Sleep};
use tracing::{debug, info};

use crate::handler::{RequestHandler, WebSessionSnapshot, WebSessionStream};
use crate::web::crypto::EncryptedWebSocketReader;
use crate::web::outbound::{OutboundQueueResult, WebSocketOutbound};
use crate::web::protocol::{
    queue_session_keyframe, send_revoked, send_viewer_count, SessionScrollRequest,
};
use crate::web::stream_sanitizer::WebTerminalSanitizer;
use crate::web::websocket::WebSocketMessage;
use crate::web::{WebShareConnectionCounts, WebShareRevokeReason};

const SLOW_VIEWER_CLOSE_CODE: u16 = 4001;
const SESSION_SNAPSHOT_DEBOUNCE: Duration = Duration::from_millis(50);
const SESSION_SNAPSHOT_MAX_WAIT: Duration = Duration::from_millis(200);
const SESSION_INTERACTIVE_DEBOUNCE: Duration = Duration::from_millis(8);
const SESSION_INTERACTIVE_MAX_WAIT: Duration = Duration::from_millis(32);
const KEEPALIVE_TIMEOUT_CLOSE_CODE: u16 = 4009;
const KEEPALIVE_TIMEOUT_REASON: &str = "pong_timeout";

mod pane_loop;
mod selection_delivery;
mod session_loop;

pub(super) use pane_loop::serve_pane_loop;
pub(super) use session_loop::serve_session_loop;

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

async fn wait_for_normal_shutdown(shutdown: &mut watch::Receiver<bool>) {
    while !*shutdown.borrow() {
        if shutdown.changed().await.is_err() {
            return;
        }
    }
}

async fn queue_session_keyframe_or_close(
    outbound: &WebSocketOutbound,
    resize: Option<rmux_proto::TerminalSize>,
    snapshot: &WebSessionSnapshot,
    share_id: &str,
    sanitizer: &mut WebTerminalSanitizer,
) -> io::Result<()> {
    queue_or_close(
        outbound,
        queue_session_keyframe(outbound, resize, snapshot, sanitizer),
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
    use super::selection_delivery::normalize_session_scroll_from_pane_frame;
    use std::collections::HashMap;

    use rmux_core::PaneId;
    use rmux_proto::TerminalSize;

    use crate::handler::{
        TestWebSessionView, WebSessionPaneFrame, WebSessionPaneView, WebSessionSnapshot,
    };
    use crate::web::outbound::OutboundQueueResult;

    use super::{
        apply_session_scroll, is_recoverable_session_queue_pressure, next_session_refresh_deadline,
        normalize_session_scrolls,
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
        )
        .expect("pane frame fits");

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
        )
        .expect("pane frame fits");

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
                scene: None,
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
                metadata_complete: true,
            },
            0,
            0,
        )
        .expect("snapshot fits");

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
                scene: None,
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
                metadata_complete: true,
            },
            0,
            0,
        )
        .expect("snapshot fits");

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
