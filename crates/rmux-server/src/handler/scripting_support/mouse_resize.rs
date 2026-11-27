use super::{
    queue_action_from_response, QueueCommandAction, QueueExecutionContext, RequestHandler,
};
use crate::mouse::{AttachedMouseEvent, MouseLocation};
use rmux_core::PaneGeometry;
use rmux_proto::{PaneTarget, ResizePaneAdjustment, ResizePaneRequest, RmuxError};

impl RequestHandler {
    pub(super) async fn execute_queued_mouse_resize_pane(
        &self,
        requester_pid: u32,
        target: PaneTarget,
        context: &QueueExecutionContext,
    ) -> Result<QueueCommandAction, RmuxError> {
        let fallback_event;
        let event = if let Some(event) = context.mouse_event.as_ref() {
            event
        } else if context.mouse_target.is_some() {
            fallback_event = {
                let active_attach = self.active_attach.lock().await;
                active_attach
                    .by_pid
                    .get(&requester_pid)
                    .and_then(|active| active.mouse.current_event.clone())
            };
            let Some(event) = fallback_event.as_ref() else {
                return Ok(QueueCommandAction::Normal {
                    output: None,
                    error: None,
                    source_file_error: None,
                    exit_status: None,
                });
            };
            event
        } else {
            return Ok(QueueCommandAction::Normal {
                output: None,
                error: None,
                source_file_error: None,
                exit_status: None,
            });
        };
        if event.location != MouseLocation::Border {
            return Ok(QueueCommandAction::Normal {
                output: None,
                error: None,
                source_file_error: None,
                exit_status: None,
            });
        }

        if self
            .execute_floating_mouse_drag(requester_pid, &target, event)
            .await?
        {
            return Ok(QueueCommandAction::Normal {
                output: None,
                error: None,
                source_file_error: None,
                exit_status: None,
            });
        }

        let adjustment = {
            let state = self.state.lock().await;
            state
                .sessions
                .session(target.session_name())
                .and_then(|session| session.window_at(target.window_index()))
                .and_then(|window| window.pane(target.pane_index()))
                .map(|pane| mouse_resize_adjustment(pane.geometry(), event))
        }
        .unwrap_or(ResizePaneAdjustment::NoOp);

        if adjustment == ResizePaneAdjustment::NoOp {
            return Ok(QueueCommandAction::Normal {
                output: None,
                error: None,
                source_file_error: None,
                exit_status: None,
            });
        }

        queue_action_from_response(
            self.handle_resize_pane(ResizePaneRequest { target, adjustment })
                .await,
        )
    }
}

fn mouse_resize_adjustment(
    geometry: PaneGeometry,
    event: &AttachedMouseEvent,
) -> ResizePaneAdjustment {
    let x = event.raw.x;
    let y = adjusted_mouse_y(event);
    let start_x = event.raw.lx;
    let start_y = adjusted_mouse_y_value(event, event.raw.ly);
    let right_border = geometry.x().saturating_add(geometry.cols());
    let bottom_border = geometry.y().saturating_add(geometry.rows());
    let started_on_right_border = start_x == right_border
        && start_y >= geometry.y().saturating_sub(1)
        && start_y <= bottom_border;
    let started_on_bottom_border = start_y == bottom_border
        && start_x >= geometry.x().saturating_sub(1)
        && start_x <= right_border;

    let horizontal_delta = x.abs_diff(start_x);
    let vertical_delta = y.abs_diff(start_y);

    if started_on_right_border
        || (!started_on_bottom_border && horizontal_delta >= vertical_delta && horizontal_delta > 0)
    {
        return ResizePaneAdjustment::AbsoluteWidth {
            columns: x.saturating_sub(geometry.x()).max(1),
        };
    }
    if started_on_bottom_border || vertical_delta > 0 {
        return ResizePaneAdjustment::AbsoluteHeight {
            rows: y.saturating_sub(geometry.y()).max(1),
        };
    }

    ResizePaneAdjustment::NoOp
}

fn adjusted_mouse_y(event: &AttachedMouseEvent) -> u16 {
    adjusted_mouse_y_value(event, event.raw.y)
}

fn adjusted_mouse_y_value(event: &AttachedMouseEvent, y: u16) -> u16 {
    match event.status_at {
        Some(0) if y >= event.status_lines => y.saturating_sub(event.status_lines),
        _ => y,
    }
}
