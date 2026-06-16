use super::pane_transfer_shared::{
    adjusted_insert_position, adjusted_insert_position_before, apply_swap_between_windows,
    validate_swap_destination, SwapPaneEntry,
};
use super::target_error::{invalid_pane_target, invalid_window_target};
use super::{PaneJoinOptions, PaneSwapOptions, Session, SessionPaneTarget};
use crate::{Pane, Window};
use rmux_proto::{PaneSplitSize, RmuxError, SplitDirection};

#[path = "pane_transfer/break_window.rs"]
mod break_window;

impl Session {
    /// Selects the previously active pane in the addressed window.
    pub fn last_pane_in_window(&mut self, window_index: u32) -> Result<u32, RmuxError> {
        self.last_pane_in_window_with_zoom(window_index, false)
    }

    /// Selects the previously active pane in the addressed window, preserving zoom when requested.
    pub fn last_pane_in_window_with_zoom(
        &mut self,
        window_index: u32,
        preserve_zoom: bool,
    ) -> Result<u32, RmuxError> {
        let last_pane = self
            .window_at(window_index)
            .ok_or_else(|| invalid_window_target(&self.name, window_index))?
            .last_pane_index()
            .ok_or_else(|| RmuxError::Server("no last pane".to_owned()))?;

        let window = self
            .window_at_mut(window_index)
            .expect("addressed session window must exist");
        if preserve_zoom {
            let _ = window.push_zoom(true);
        }
        let selected = window.select_pane(last_pane);
        if preserve_zoom {
            window.pop_zoom();
        }
        debug_assert!(selected, "validated last pane must be selectable");

        Ok(last_pane)
    }

    /// Swaps two panes within the same session without renumbering either pane.
    pub fn swap_panes(
        &mut self,
        source: SessionPaneTarget,
        target: SessionPaneTarget,
        options: PaneSwapOptions,
    ) -> Result<(), RmuxError> {
        if source.window_index == target.window_index {
            return self.swap_panes_within_window(
                source.window_index,
                source.pane_index,
                target.pane_index,
                options,
            );
        }

        let source_pane = self
            .window_at(source.window_index)
            .ok_or_else(|| invalid_window_target(&self.name, source.window_index))?
            .pane(source.pane_index)
            .cloned()
            .ok_or_else(|| {
                invalid_pane_target(
                    &self.name,
                    source.window_index,
                    source.pane_index,
                    "pane index does not exist in session",
                )
            })?;
        let target_pane = self
            .window_at(target.window_index)
            .ok_or_else(|| invalid_window_target(&self.name, target.window_index))?
            .pane(target.pane_index)
            .cloned()
            .ok_or_else(|| {
                invalid_pane_target(
                    &self.name,
                    target.window_index,
                    target.pane_index,
                    "pane index does not exist in session",
                )
            })?;

        validate_swap_destination(
            self.window_at(source.window_index)
                .expect("source window must exist"),
            &target_pane,
            source.pane_index,
        )?;
        validate_swap_destination(
            self.window_at(target.window_index)
                .expect("target window must exist"),
            &source_pane,
            target.pane_index,
        )?;

        let mut source_window = self
            .windows
            .remove(&source.window_index)
            .expect("source window must exist for swap");
        let mut target_window = self
            .windows
            .remove(&target.window_index)
            .expect("target window must exist for swap");
        apply_swap_between_windows(
            &mut source_window,
            SwapPaneEntry {
                index: source.pane_index,
                pane: source_pane,
            },
            &mut target_window,
            SwapPaneEntry {
                index: target.pane_index,
                pane: target_pane,
            },
            options,
        )?;
        self.windows.insert(source.window_index, source_window);
        self.windows.insert(target.window_index, target_window);

        Ok(())
    }

    /// Moves one pane next to another pane in the same session.
    pub fn join_pane(
        &mut self,
        source: SessionPaneTarget,
        target: SessionPaneTarget,
        options: PaneJoinOptions,
    ) -> Result<(), RmuxError> {
        if source.window_index == target.window_index {
            return self.join_pane_within_window(source, target, options);
        }

        let source_pane = self
            .window_at(source.window_index)
            .ok_or_else(|| invalid_window_target(&self.name, source.window_index))?
            .pane(source.pane_index)
            .cloned()
            .ok_or_else(|| {
                invalid_pane_target(
                    &self.name,
                    source.window_index,
                    source.pane_index,
                    "pane index does not exist in session",
                )
            })?;
        let target_position = self
            .window_at(target.window_index)
            .ok_or_else(|| invalid_window_target(&self.name, target.window_index))?
            .pane_position(target.pane_index)
            .ok_or_else(|| {
                invalid_pane_target(
                    &self.name,
                    target.window_index,
                    target.pane_index,
                    "pane index does not exist in session",
                )
            })?;
        let requested_size = join_requested_size(
            self.window_at(target.window_index)
                .expect("target window must exist"),
            target.pane_index,
            options.direction,
            options.full_size,
            options.size,
        )?;
        let transient_index = self
            .window_at(target.window_index)
            .expect("target window must exist")
            .panes()
            .iter()
            .map(|pane| pane.index())
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        let mut source_pane_for_validation = source_pane.clone();
        source_pane_for_validation.set_index(transient_index);

        self.window_at(target.window_index)
            .expect("target window must exist")
            .ensure_accepts_pane(&source_pane_for_validation, None)?;
        let target_active_before_id = self
            .window_at(target.window_index)
            .and_then(Window::active_pane)
            .map(Pane::id)
            .expect("validated target window must have an active pane");
        let target_last_before_id = self.window_at(target.window_index).and_then(|window| {
            window
                .last_pane_index()
                .and_then(|pane_index| window.pane(pane_index).map(Pane::id))
        });

        let mut source_window = self
            .windows
            .remove(&source.window_index)
            .expect("source window must exist for join");
        let mut target_window = self
            .windows
            .remove(&target.window_index)
            .expect("target window must exist for join");
        source_window.auto_unzoom();
        target_window.auto_unzoom();
        let mut moved_pane = source_window
            .extract_pane(source.pane_index)
            .expect("validated source pane must extract");
        let moved_pane_id = moved_pane.id();
        moved_pane.set_index(transient_index);
        if options.full_size {
            target_window.insert_pane_full_size(moved_pane, options.direction, options.before)?;
        } else {
            let insert_position = if options.before {
                target_position
            } else {
                target_position + 1
            };
            target_window.insert_pane_at_position(
                insert_position,
                moved_pane,
                options.direction,
            )?;
        }
        let (active_after_id, last_after_id) = if options.detached {
            (target_active_before_id, target_last_before_id)
        } else {
            (
                moved_pane_id,
                (target_active_before_id != moved_pane_id).then_some(target_active_before_id),
            )
        };
        target_window.renumber_panes_by_position(active_after_id, last_after_id);
        let moved_pane_index = target_window
            .panes()
            .iter()
            .find(|pane| pane.id() == moved_pane_id)
            .map(|pane| pane.index())
            .expect("moved pane must survive cross-window join");
        if let Some(requested_size) = requested_size {
            let _ =
                target_window.resize_pane_to(moved_pane_index, options.direction, requested_size);
        }

        let source_was_empty = source_window.pane_count() == 0;
        self.windows.insert(target.window_index, target_window);
        if source_was_empty {
            if self.windows.is_empty() {
                self.windows.insert(source.window_index, source_window);
                return Err(RmuxError::Server(format!(
                    "cannot kill the only window in session {}",
                    self.name
                )));
            }
            if self.active_window == source.window_index {
                self.active_window = self.next_active_window_after_removal(source.window_index);
            }
            if self.last_window == Some(source.window_index) {
                self.last_window = None;
            }
        } else {
            self.windows.insert(source.window_index, source_window);
        }

        if !options.detached {
            self.select_window(target.window_index)?;
        }

        Ok(())
    }

    fn swap_panes_within_window(
        &mut self,
        window_index: u32,
        source_pane_index: u32,
        target_pane_index: u32,
        options: PaneSwapOptions,
    ) -> Result<(), RmuxError> {
        if source_pane_index == target_pane_index {
            let window = self
                .window_at(window_index)
                .ok_or_else(|| invalid_window_target(&self.name, window_index))?;
            if window.pane(source_pane_index).is_none() {
                return Err(invalid_pane_target(
                    &self.name,
                    window_index,
                    source_pane_index,
                    "pane index does not exist in session",
                ));
            }
            return Ok(());
        }

        let window = self
            .window_at(window_index)
            .ok_or_else(|| invalid_window_target(&self.name, window_index))?;
        if window.pane(source_pane_index).is_none() {
            return Err(invalid_pane_target(
                &self.name,
                window_index,
                source_pane_index,
                "pane index does not exist in session",
            ));
        }
        if window.pane(target_pane_index).is_none() {
            return Err(invalid_pane_target(
                &self.name,
                window_index,
                target_pane_index,
                "pane index does not exist in session",
            ));
        }

        let window = self
            .window_at_mut(window_index)
            .expect("window must exist for in-window swap");
        window.push_zoom(options.preserve_zoom);
        let target_pane_id = window
            .pane(target_pane_index)
            .expect("validated target pane must exist before swap")
            .id();
        let swapped = window.swap_panes(source_pane_index, target_pane_index);
        debug_assert!(swapped, "validated in-window swap must succeed");

        if !options.detached {
            window.select_pane_by_id(target_pane_id);
        }
        window.pop_zoom();

        Ok(())
    }

    fn join_pane_within_window(
        &mut self,
        source: SessionPaneTarget,
        target: SessionPaneTarget,
        options: PaneJoinOptions,
    ) -> Result<(), RmuxError> {
        if source.pane_index == target.pane_index {
            return Err(RmuxError::Server(
                "source and target panes must be different".to_owned(),
            ));
        }

        let window = self
            .window_at(source.window_index)
            .ok_or_else(|| invalid_window_target(&self.name, source.window_index))?;
        let source_position = window.pane_position(source.pane_index).ok_or_else(|| {
            invalid_pane_target(
                &self.name,
                source.window_index,
                source.pane_index,
                "pane index does not exist in session",
            )
        })?;
        let target_position = window.pane_position(target.pane_index).ok_or_else(|| {
            invalid_pane_target(
                &self.name,
                target.window_index,
                target.pane_index,
                "pane index does not exist in session",
            )
        })?;
        let source_pane = window
            .pane(source.pane_index)
            .cloned()
            .expect("validated source pane must exist");
        let source_pane_id = source_pane.id();
        let target_pane_id = window
            .pane(target.pane_index)
            .expect("validated target pane must exist")
            .id();
        window.ensure_accepts_pane(&source_pane, Some(source_position))?;
        let requested_size = join_requested_size(
            window,
            target.pane_index,
            options.direction,
            options.full_size,
            options.size,
        )?;
        let active_before_id = window
            .active_pane()
            .expect("validated window must have an active pane")
            .id();
        let last_before_id = window
            .last_pane_index()
            .and_then(|pane_index| window.pane(pane_index).map(|pane| pane.id()));
        let detached_active_after_removal_id = if active_before_id == source_pane_id {
            last_before_id
                .filter(|pane_id| *pane_id != source_pane_id)
                .or_else(|| {
                    if source_position > 0 {
                        window.panes().get(source_position - 1).map(Pane::id)
                    } else {
                        window.panes().get(source_position + 1).map(Pane::id)
                    }
                })
        } else {
            Some(active_before_id)
        };
        let detached_last_after_removal_id = if active_before_id == source_pane_id {
            detached_active_after_removal_id
                .is_some_and(|pane_id| pane_id != target_pane_id)
                .then_some(target_pane_id)
        } else {
            last_before_id.filter(|pane_id| *pane_id != source_pane_id)
        };

        let insert_position = if options.full_size {
            usize::from(!options.before)
        } else if options.before {
            adjusted_insert_position_before(source_position, target_position)
        } else {
            adjusted_insert_position(source_position, target_position)
        };
        let window = self
            .window_at_mut(source.window_index)
            .expect("window must exist for in-window join");
        let moved_pane_id = if options.full_size {
            window.auto_unzoom();
            let mut moved_pane = window
                .extract_pane(source.pane_index)
                .expect("validated source pane must extract");
            let moved_pane_id = moved_pane.id();
            let transient_index = window
                .panes()
                .iter()
                .map(|pane| pane.index())
                .max()
                .unwrap_or(0)
                .saturating_add(1);
            moved_pane.set_index(transient_index);
            window.insert_pane_full_size(moved_pane, options.direction, options.before)?;
            moved_pane_id
        } else {
            window.move_pane_by_splitting_target(
                source_position,
                target_position,
                insert_position,
                options.direction,
                options.before,
            )?
        };
        let (active_after_id, last_after_id) = if options.detached {
            (
                detached_active_after_removal_id
                    .expect("detached pane move should leave an active pane"),
                detached_last_after_removal_id,
            )
        } else {
            (
                moved_pane_id,
                (active_before_id != moved_pane_id).then_some(active_before_id),
            )
        };
        window.renumber_panes_by_position(active_after_id, last_after_id);
        let moved_pane_index = window
            .panes()
            .iter()
            .find(|pane| pane.id() == moved_pane_id)
            .map(|pane| pane.index())
            .expect("moved pane must survive in-window join");
        if let Some(requested_size) = requested_size {
            let _ = window.resize_pane_to(moved_pane_index, options.direction, requested_size);
        }

        Ok(())
    }
}

fn join_requested_size(
    target_window: &Window,
    target_pane_index: u32,
    direction: SplitDirection,
    full_size: bool,
    size: Option<PaneSplitSize>,
) -> Result<Option<u32>, RmuxError> {
    let Some(size) = size else {
        return Ok(None);
    };
    let base = if full_size {
        join_axis_for_size(target_window.size(), direction)
    } else {
        let pane = target_window.pane(target_pane_index).ok_or_else(|| {
            RmuxError::Server(format!(
                "cannot size missing target pane index {target_pane_index}"
            ))
        })?;
        join_axis_for_size(
            rmux_proto::TerminalSize {
                cols: pane.geometry().cols(),
                rows: pane.geometry().rows(),
            },
            direction,
        )
    };

    Ok(Some(match size {
        PaneSplitSize::Absolute(value) => value.max(1),
        PaneSplitSize::Percentage(value) => {
            let scaled = (base.saturating_mul(u32::from(value))) / 100;
            scaled.max(1)
        }
    }))
}

fn join_axis_for_size(size: rmux_proto::TerminalSize, direction: SplitDirection) -> u32 {
    match direction {
        SplitDirection::Vertical => u32::from(size.cols),
        SplitDirection::Horizontal => u32::from(size.rows),
    }
}
