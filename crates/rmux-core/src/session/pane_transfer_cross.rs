use super::pane_transfer_shared::{
    apply_swap_between_windows, resolve_break_destination_index, validate_swap_destination,
    SwapPaneEntry,
};
use super::target_error::{invalid_pane_target, invalid_window_target};
use super::{BreakPaneOptions, PaneJoinOptions, PaneSwapOptions, Session, SessionPaneTarget};
use crate::{Pane, Window};
use rmux_proto::{PaneSplitSize, RmuxError, SplitDirection};

impl Session {
    /// Swaps one pane in this session with one pane in another session.
    pub fn swap_panes_with_session(
        &mut self,
        source: SessionPaneTarget,
        target_session: &mut Session,
        target: SessionPaneTarget,
        options: PaneSwapOptions,
    ) -> Result<(), RmuxError> {
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
        let target_pane = target_session
            .window_at(target.window_index)
            .ok_or_else(|| invalid_window_target(&target_session.name, target.window_index))?
            .pane(target.pane_index)
            .cloned()
            .ok_or_else(|| {
                invalid_pane_target(
                    &target_session.name,
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
            target_session
                .window_at(target.window_index)
                .expect("target window must exist"),
            &source_pane,
            target.pane_index,
        )?;

        let source_window = self
            .window_at_mut(source.window_index)
            .expect("source window must exist");
        let target_window = target_session
            .window_at_mut(target.window_index)
            .expect("target window must exist");
        apply_swap_between_windows(
            source_window,
            SwapPaneEntry {
                index: source.pane_index,
                pane: source_pane,
            },
            target_window,
            SwapPaneEntry {
                index: target.pane_index,
                pane: target_pane,
            },
            options,
        )
    }

    /// Moves one pane from another session into this session.
    pub fn join_pane_from_session(
        &mut self,
        target: SessionPaneTarget,
        source_session: &mut Session,
        source: SessionPaneTarget,
        options: PaneJoinOptions,
    ) -> Result<(), RmuxError> {
        let source_window = source_session
            .window_at(source.window_index)
            .ok_or_else(|| invalid_window_target(&source_session.name, source.window_index))?;
        let source_pane = source_window
            .pane(source.pane_index)
            .cloned()
            .ok_or_else(|| {
                invalid_pane_target(
                    &source_session.name,
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
            .map(Pane::index)
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

        let target_window = self
            .window_at_mut(target.window_index)
            .expect("target window must exist");
        target_window.auto_unzoom();
        let source_window = source_session
            .window_at_mut(source.window_index)
            .expect("source window must exist");
        source_window.auto_unzoom();
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
            .map(Pane::index)
            .expect("moved pane must survive cross-session join");
        if let Some(requested_size) = requested_size {
            if options.full_size {
                let _ = target_window.resize_pane_to(
                    moved_pane_index,
                    options.direction,
                    requested_size,
                );
            } else {
                let _ = target_window.resize_new_split_pane_to(
                    moved_pane_index,
                    options.direction,
                    requested_size,
                    options.before,
                );
            }
        }
        if !options.detached {
            target_window.select_pane(moved_pane_index);
            self.select_window(target.window_index)?;
        }

        if source_window.pane_count() == 0 {
            source_session.remove_window_allowing_empty(source.window_index)?;
        }

        Ok(())
    }

    /// Breaks one pane out into another session as its own window.
    pub fn break_pane_to_session(
        &mut self,
        source: SessionPaneTarget,
        destination_session: &mut Session,
        options: BreakPaneOptions,
    ) -> Result<u32, RmuxError> {
        let source_window = self
            .window_at(source.window_index)
            .ok_or_else(|| invalid_window_target(&self.name, source.window_index))?;
        if source_window.pane(source.pane_index).is_none() {
            return Err(invalid_pane_target(
                &self.name,
                source.window_index,
                source.pane_index,
                "pane index does not exist in session",
            ));
        }
        let destination_index = prepare_break_destination(
            destination_session,
            options.target_window_index,
            options.after,
            options.before,
        )?;

        if source_window.pane_count() == 1 {
            let mut moved_window = self.remove_window_allowing_empty(source.window_index)?;
            moved_window.renumber_single_pane_to_zero();
            destination_session.insert_existing_window(destination_index, moved_window)?;
            if let Some(name) = options.name {
                destination_session.rename_window(destination_index, name)?;
            }
            if !options.detached {
                destination_session.select_window(destination_index)?;
            }
            return Ok(destination_index);
        }

        let source_size = source_window.size();
        let source_window = self
            .window_at_mut(source.window_index)
            .expect("source window must exist");
        source_window.auto_unzoom();
        let moved_pane = source_window
            .extract_pane(source.pane_index)
            .expect("validated source pane must extract");
        let mut new_window = Window::new_with_initial_pane(
            source_size,
            moved_pane.id(),
            destination_session.allocate_window_id(),
        );
        if let Some(name) = options.name {
            new_window.set_name(name);
        }
        destination_session.insert_existing_window(destination_index, new_window)?;
        if !options.detached {
            destination_session.select_window(destination_index)?;
        }

        Ok(destination_index)
    }
}

fn prepare_break_destination(
    destination_session: &mut Session,
    target_window_index: Option<u32>,
    after: bool,
    before: bool,
) -> Result<u32, RmuxError> {
    if !(after || before) {
        return resolve_break_destination_index(destination_session, target_window_index, None);
    }

    let anchor_index = target_window_index.unwrap_or(destination_session.active_window_index());
    if target_window_index.is_some() && destination_session.window_at(anchor_index).is_none() {
        return Err(invalid_window_target(
            &destination_session.name,
            anchor_index,
        ));
    }
    let destination_index = if before {
        anchor_index
    } else {
        anchor_index
            .checked_add(1)
            .ok_or_else(|| RmuxError::Server("window index space exhausted".to_owned()))?
    };
    shift_windows_up_from(destination_session, destination_index)?;
    Ok(destination_index)
}

fn shift_windows_up_from(session: &mut Session, start_index: u32) -> Result<(), RmuxError> {
    if session
        .windows
        .keys()
        .next_back()
        .is_some_and(|window_index| *window_index == u32::MAX)
    {
        return Err(RmuxError::Server("window index space exhausted".to_owned()));
    }

    let shifted_windows = session
        .windows
        .range(start_index..)
        .map(|(window_index, _)| *window_index)
        .collect::<Vec<_>>();
    for window_index in shifted_windows.into_iter().rev() {
        let new_index = window_index
            .checked_add(1)
            .ok_or_else(|| RmuxError::Server("window index space exhausted".to_owned()))?;
        let window = session
            .windows
            .remove(&window_index)
            .expect("shifted window must exist");
        let flags = session
            .winlink_alert_flags
            .remove(&window_index)
            .unwrap_or_else(crate::AlertFlags::empty);
        let replaced_window = session.windows.insert(new_index, window);
        debug_assert!(replaced_window.is_none());
        let replaced_flags = session.winlink_alert_flags.insert(new_index, flags);
        debug_assert!(replaced_flags.is_none());
    }
    if session.active_window >= start_index {
        session.active_window = session.active_window.saturating_add(1);
    }
    if let Some(last_window) = session.last_window.filter(|index| *index >= start_index) {
        session.last_window = Some(last_window.saturating_add(1));
    }
    Ok(())
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
