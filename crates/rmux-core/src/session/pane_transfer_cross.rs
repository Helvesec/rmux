use super::pane_transfer_shared::{
    apply_swap_between_windows, resolve_break_destination_index, validate_swap_destination,
    SwapPaneEntry,
};
use super::target_error::{invalid_pane_target, invalid_window_target};
use super::{BreakPaneOptions, PaneJoinOptions, PaneSwapOptions, Session, SessionPaneTarget};
use crate::Window;
use rmux_proto::RmuxError;

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
            .windows
            .get_mut(&source.window_index)
            .ok_or_else(|| invalid_window_target(&source_session.name, source.window_index))?;
        let target_window = self
            .windows
            .get_mut(&target.window_index)
            .ok_or_else(|| invalid_window_target(&self.name, target.window_index))?;
        let moved_index = super::pane_transfer_join::join_between(
            source_window,
            &source_session.name,
            source,
            target_window,
            &self.name,
            target,
            options,
        )?;
        let empty = source_window.pane_count() == 0;
        if !options.detached {
            target_window.select_pane(moved_index);
            self.select_window(target.window_index)?;
        }
        if empty {
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
        let source_pane_id = source_window
            .pane(source.pane_index)
            .expect("validated source")
            .id();
        if source_window.pane_count() > 1 && source_window.is_floating_pane_id(source_pane_id) {
            return Err(RmuxError::Server(
                "cannot break a floating pane out of a multi-pane window".to_owned(),
            ));
        }
        let destination_index = prepare_break_destination(
            destination_session,
            options.target_window_index,
            options.after,
            options.before,
        )?;

        if source_window.pane_count() == 1 {
            let source_alert_flags = self.winlink_alert_flags(source.window_index);
            let mut moved_window = self.remove_window_allowing_empty(source.window_index)?;
            moved_window.renumber_single_pane_to_zero();
            destination_session.insert_existing_window(destination_index, moved_window)?;
            if let Some(name) = options.name {
                destination_session.rename_window(destination_index, name)?;
            }
            let _ = destination_session.clear_all_winlink_alert_flags(destination_index);
            if !source_alert_flags.is_empty() {
                let _ = destination_session
                    .add_winlink_alert_flags(destination_index, source_alert_flags);
            }
            if !options.detached {
                destination_session.select_window(destination_index)?;
            }
            return Ok(destination_index);
        }

        let size = source_window.size();
        let pane_id = source_window
            .pane(source.pane_index)
            .expect("validated source")
            .id();
        let mut new_window =
            Window::new_with_initial_pane(size, pane_id, destination_session.allocate_window_id());
        super::pane_transfer_shared::apply_break_into_window(
            self.window_at_mut(source.window_index)
                .expect("validated source"),
            source.pane_index,
            &new_window,
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
