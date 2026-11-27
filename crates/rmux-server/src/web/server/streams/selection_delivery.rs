use super::{
    io, log_recoverable_session_queue_result, normalize_session_scrolls, pane_top_line, HashMap,
    OutboundQueueResult, PaneId, RequestHandler, WebSessionSnapshot, WebSessionStream,
    WebSocketOutbound, WebTerminalSanitizer,
};
use crate::web::protocol::{queue_session_keyframe, queue_session_pane_frame, queue_session_view};

pub(super) async fn queue_selected_session_delivery(
    handler: &RequestHandler,
    session: &WebSessionStream,
    publish: impl FnOnce() -> OutboundQueueResult,
) -> io::Result<OutboundQueueResult> {
    handler
        .with_web_session_window_selection(session.target(), session.selected_window(), publish)
        .await
        .map_err(|error| io::Error::other(error.to_string()))
}

pub(super) async fn queue_fresh_session_snapshot(
    handler: &RequestHandler,
    outbound: &WebSocketOutbound,
    session: &mut WebSessionStream,
    share_id: &str,
    scrolls: &mut HashMap<PaneId, usize>,
    sanitizer: &mut WebTerminalSanitizer,
) -> io::Result<OutboundQueueResult> {
    let next = handler
        .web_session_snapshot_with_scrolls(session.target(), session.selected_window(), scrolls)
        .await
        .map_err(|error| io::Error::other(error.to_string()))?;
    normalize_session_scrolls(scrolls, &next);
    let resize = (next.size != session.size()).then_some(next.size);
    session.snapshot = next;
    #[cfg(test)]
    super::super::selection_delivery_pause::before_queue(share_id, true).await;
    let result = queue_selected_session_delivery(handler, session, || {
        queue_session_keyframe(outbound, resize, &session.snapshot, sanitizer)
    })
    .await?;
    log_recoverable_session_queue_result(share_id, result);
    Ok(result)
}

pub(super) async fn queue_session_scroll_patch(
    handler: &RequestHandler,
    outbound: &WebSocketOutbound,
    session: &mut WebSessionStream,
    share_id: &str,
    scrolls: &mut HashMap<PaneId, usize>,
    supports_session_pane_frame: bool,
    sanitizer: &mut WebTerminalSanitizer,
) -> io::Result<Option<OutboundQueueResult>> {
    if !supports_session_pane_frame || scrolls.len() != 1 {
        return Ok(None);
    }
    let Some((&pane_id, &top_line)) = scrolls.iter().next() else {
        return Ok(None);
    };
    let Some(frame) = handler
        .web_session_pane_scroll_frame(
            session.target(),
            pane_id,
            top_line,
            session.selected_window(),
        )
        .await
        .map_err(|error| io::Error::other(error.to_string()))?
    else {
        return Ok(None);
    };
    if !pane_frame_matches_snapshot(&session.snapshot, &frame) {
        return Ok(None);
    }
    let result = queue_selected_session_delivery(handler, session, || {
        queue_session_pane_frame(outbound, &frame, sanitizer)
    })
    .await?;
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
    snapshot.accepts_rectangular_patch(frame)
        && snapshot.size == frame.size
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

pub(super) fn normalize_session_scroll_from_pane_frame(
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

pub(super) async fn queue_fresh_session_view(
    handler: &RequestHandler,
    outbound: &WebSocketOutbound,
    session: &mut WebSessionStream,
    share_id: &str,
    scrolls: &mut HashMap<PaneId, usize>,
    sanitizer: &mut WebTerminalSanitizer,
) -> io::Result<OutboundQueueResult> {
    let next = handler
        .web_session_snapshot_with_scrolls(session.target(), session.selected_window(), scrolls)
        .await
        .map_err(|error| io::Error::other(error.to_string()))?;
    normalize_session_scrolls(scrolls, &next);
    let resize = (next.size != session.size()).then_some(next.size);
    let keyframe = session.snapshot.requires_keyframe_for(&next);
    session.snapshot = next;
    let result = queue_selected_session_delivery(handler, session, || {
        if keyframe {
            queue_session_keyframe(outbound, resize, &session.snapshot, sanitizer)
        } else {
            queue_session_view(outbound, &session.snapshot)
        }
    })
    .await?;
    log_recoverable_session_queue_result(share_id, result);
    Ok(result)
}
