use super::super::pane_transfer_shared::resolve_break_destination_index;
use super::super::target_error::{invalid_pane_target, invalid_window_target};
use super::super::{BreakPaneOptions, Session, SessionPaneTarget};
use crate::Window;
use rmux_proto::RmuxError;

impl Session {
    /// Breaks one pane out into another window in the same session.
    pub fn break_pane(
        &mut self,
        source: SessionPaneTarget,
        options: BreakPaneOptions,
    ) -> Result<u32, RmuxError> {
        let original_source_window = self
            .window_at(source.window_index)
            .ok_or_else(|| invalid_window_target(&self.name, source.window_index))?;
        if original_source_window.pane(source.pane_index).is_none() {
            return Err(invalid_pane_target(
                &self.name,
                source.window_index,
                source.pane_index,
                "pane index does not exist in session",
            ));
        }
        let source_is_single_pane = original_source_window.pane_count() == 1;
        let source_pane_id = original_source_window
            .pane(source.pane_index)
            .expect("validated source pane")
            .id();
        if !source_is_single_pane && original_source_window.is_floating_pane_id(source_pane_id) {
            return Err(RmuxError::Server(
                "cannot break a floating pane out of a multi-pane window".to_owned(),
            ));
        }
        let (destination_index, source_window_index) = prepare_break_destination(
            self,
            source.window_index,
            options.target_window_index,
            options.after,
            options.before,
        )?;

        if source_is_single_pane {
            self.window_at_mut(source_window_index)
                .expect("prepared source window must exist")
                .renumber_single_pane_to_zero();
            self.move_window(
                source_window_index,
                destination_index,
                false,
                !options.detached,
            )?;
            if let Some(name) = options.name {
                self.rename_window(destination_index, name)?;
            }
            return Ok(destination_index);
        }

        let source_window = self
            .window_at(source_window_index)
            .expect("prepared source");
        let size = source_window.size();
        let pane_id = source_window
            .pane(source.pane_index)
            .expect("validated source")
            .id();
        let mut new_window =
            Window::new_with_initial_pane(size, pane_id, self.allocate_window_id());
        super::super::pane_transfer_shared::apply_break_into_window(
            self.window_at_mut(source_window_index)
                .expect("prepared source"),
            source.pane_index,
            &new_window,
        );
        if let Some(name) = options.name {
            new_window.set_name(name);
        }
        self.insert_existing_window(destination_index, new_window)?;
        if !options.detached {
            self.select_window(destination_index)?;
        }

        Ok(destination_index)
    }
}

fn prepare_break_destination(
    session: &mut Session,
    source_window_index: u32,
    target_window_index: Option<u32>,
    after: bool,
    before: bool,
) -> Result<(u32, u32), RmuxError> {
    if !(after || before) {
        let destination_index = resolve_break_destination_index(
            session,
            target_window_index,
            Some(source_window_index),
        )?;
        return Ok((destination_index, source_window_index));
    }

    let anchor_index = target_window_index.unwrap_or(session.active_window_index());
    if target_window_index.is_some() && session.window_at(anchor_index).is_none() {
        return Err(invalid_window_target(&session.name, anchor_index));
    }
    let destination_index = if before {
        anchor_index
    } else {
        anchor_index
            .checked_add(1)
            .ok_or_else(|| RmuxError::Server("window index space exhausted".to_owned()))?
    };
    shift_windows_up_from(session, destination_index)?;
    let shifted_source = if source_window_index >= destination_index {
        source_window_index
            .checked_add(1)
            .ok_or_else(|| RmuxError::Server("window index space exhausted".to_owned()))?
    } else {
        source_window_index
    };
    Ok((destination_index, shifted_source))
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
    // The shuffle moves the same occurrences (`window.c:1758-1785`), so the
    // visits travel with them instead of staying on indices another link now
    // holds, and the return is the head of the history that results.
    for visited in &mut session.window_stack {
        if *visited >= start_index {
            *visited = visited.saturating_add(1);
        }
    }
    if session.active_window >= start_index {
        session.active_window = session.active_window.saturating_add(1);
    }
    session.last_window =
        crate::session::visit_history::live_head(&session.window_stack, session.active_window);
    Ok(())
}
