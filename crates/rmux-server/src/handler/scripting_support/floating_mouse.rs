use rmux_core::{FloatingPaneRect, LifecycleEvent};
use rmux_proto::{PaneTarget, RmuxError, WindowTarget};

use super::super::{
    focus_support::prepare_server_focus_effects, prepare_lifecycle_event,
    require_expected_pane_identity, RequestHandler,
};
use crate::mouse::{AttachedMouseEvent, MouseLocation};
use crate::pane_terminals::{HandlerState, WindowRuntimeResizeScope};

impl RequestHandler {
    pub(super) async fn execute_floating_mouse_drag(
        &self,
        requester_pid: u32,
        target: &PaneTarget,
        event: &AttachedMouseEvent,
    ) -> Result<bool, RmuxError> {
        if event.location != MouseLocation::Border {
            return Ok(false);
        }
        if floating_rect(&*self.state.lock().await, target, event).is_none() {
            return Ok(false);
        }
        let authority = self
            .capture_floating_move_write_authority(requester_pid)
            .await?;
        let window_target =
            WindowTarget::with_window(target.session_name().clone(), target.window_index());
        let (notification, selection_event, sessions, focus_effects) = {
            let mut state = self.state.lock().await;
            let (changed, focus_changed, sessions) = self
                .commit_floating_move_with_authority(&authority, &mut state, |state| {
                    require_expected_pane_identity(state, target)?;
                    let Some((pane_id, before)) = floating_rect(state, target, event) else {
                        return Ok((false, false, Vec::new()));
                    };
                    let after = dragged_rect(before, event)?;
                    let window = state
                        .sessions
                        .session(target.session_name())
                        .unwrap()
                        .window_at(target.window_index())
                        .unwrap();
                    let focus_changed = window.active_pane_index() != target.pane_index();
                    let raised = window
                        .floating_stack()
                        .placements()
                        .last()
                        .is_some_and(|placement| placement.pane_id() != pane_id);
                    if after == before && !focus_changed && !raised {
                        return Ok((false, false, Vec::new()));
                    }
                    let size_changed =
                        (before.cols(), before.rows()) != (after.cols(), after.rows());
                    let (_, sessions) = state
                        .mutate_store_and_resize_window_terminal_with_family_scoped(
                            target.session_name(),
                            target.window_index(),
                            |store| {
                                store
                                    .window_selection_mut(&window_target)?
                                    .select(target.pane_index(), false)?;
                                let mut layout = store.window_layout_context_mut(&window_target)?;
                                layout.move_floating_pane(
                                    target.pane_index(),
                                    after.x(),
                                    after.y(),
                                )?;
                                if size_changed {
                                    layout.resize_floating_pane(
                                        target.pane_index(),
                                        after.cols(),
                                        after.rows(),
                                    )?;
                                }
                                Ok(())
                            },
                            |_, _| {
                                Ok(size_changed.then_some(WindowRuntimeResizeScope::Pane(pane_id)))
                            },
                        )
                        .map_err(RmuxError::from)?;
                    Ok((true, focus_changed, sessions))
                })
                .await?;
            // Reserve lifecycle order in the same order we publish below.
            // Focus delivery waits for its hooks, so it must precede the
            // selection and layout reservations held by this command.
            let focus_effects = if focus_changed {
                let mut attaches = self.active_attach.lock().await;
                Some(prepare_server_focus_effects(&mut state, &mut attaches))
            } else {
                None
            };
            let selection_event = focus_changed.then(|| {
                prepare_lifecycle_event(
                    &mut state,
                    &LifecycleEvent::WindowPaneChanged {
                        target: window_target.clone(),
                    },
                )
            });
            let notification = changed.then(|| {
                prepare_lifecycle_event(
                    &mut state,
                    &LifecycleEvent::WindowLayoutChanged {
                        target: window_target.clone(),
                    },
                )
            });
            (notification, selection_event, sessions, focus_effects)
        };
        if let Some(effects) = focus_effects {
            let _ = effects.execute(self).await;
        }
        if let Some(selection_event) = selection_event {
            self.emit_prepared(selection_event).await;
        }
        if let Some(notification) = notification {
            self.emit_prepared(notification).await;
            self.refresh_linked_window_sessions(sessions).await;
        }
        Ok(true)
    }
}

fn floating_rect(
    state: &HandlerState,
    target: &PaneTarget,
    event: &AttachedMouseEvent,
) -> Option<(rmux_core::PaneId, FloatingPaneRect)> {
    let session = state.sessions.session(target.session_name())?;
    let window = session.window_at(target.window_index())?;
    let pane = window.pane(target.pane_index())?;
    if window.is_zoomed()
        || event.pane_id != Some(pane.id())
        || event.window_id != Some(window.id().as_u32())
    {
        return None;
    }
    let placement = window.floating_stack().placement(pane.id())?;
    placement.visible().then_some((pane.id(), placement.rect()))
}

fn dragged_rect(
    rect: FloatingPaneRect,
    event: &AttachedMouseEvent,
) -> Result<FloatingPaneRect, RmuxError> {
    let position = |x, y| {
        let y = mouse_y(event, y);
        event
            .coordinates
            .map_or((i32::from(x), i32::from(y)), |coordinates| {
                coordinates.window_position(x, y)
            })
    };
    let (x, y) = position(event.raw.x, event.raw.y);
    let (lx, ly) = position(event.raw.lx, event.raw.ly);
    let left = rect.x().saturating_sub(1);
    let right = rect.x().saturating_add(rect.cols());
    let top = rect.y().saturating_sub(1);
    let bottom = rect.y().saturating_add(rect.rows());
    let at_left = lx == left || lx == left.saturating_add(1);
    let at_right = lx == right || lx == right.saturating_add(1);
    let mut nx = rect.x();
    let mut ny = rect.y();
    let mut cols = rect.cols();
    let mut rows = rect.rows();
    // Corners resize both axes in one window mutation and one native resize.
    if (at_left || at_right) && (ly == top || ly == bottom) {
        if at_left {
            cols = cols.saturating_add(lx.saturating_sub(x)).max(1);
            nx = x.saturating_add(1);
        } else {
            cols = x.saturating_sub(rect.x()).max(1);
        }
        if ly == top {
            rows = rows.saturating_add(ly.saturating_sub(y)).max(1);
            ny = y.saturating_add(1);
        } else {
            rows = y.saturating_sub(rect.y());
            if at_left && rows < 1 {
                return Ok(rect);
            }
            rows = rows.max(1);
        }
    } else if lx == right {
        cols = x.saturating_sub(rect.x());
    } else if lx == left {
        cols = cols.saturating_add(lx.saturating_sub(x));
        nx = x.saturating_add(1);
    } else if ly == bottom {
        rows = y.saturating_sub(rect.y());
    } else if ly == top {
        nx = nx.saturating_add(x.saturating_sub(lx));
        ny = y.saturating_add(1);
    }
    if cols < 1 || rows < 1 {
        return Ok(rect);
    }
    FloatingPaneRect::new(nx, ny, cols, rows)
}

fn mouse_y(event: &AttachedMouseEvent, y: u16) -> u16 {
    match event.status_at {
        Some(0) if y >= event.status_lines => y - event.status_lines,
        Some(status) if status > 0 && y >= status => status - 1,
        _ => y,
    }
}
