use super::pane_transfer_shared::{
    apply_swap_between_windows, validate_swap_destination, SwapPaneEntry,
};
use super::target_error::{invalid_pane_target, invalid_window_target};
use super::{PaneJoinOptions, PaneSwapOptions, Session, SessionPaneTarget, WindowSelection};
use rmux_proto::RmuxError;

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
        let window = self
            .windows
            .get_mut(&window_index)
            .ok_or_else(|| invalid_window_target(&self.name, window_index))?;
        WindowSelection::new(&self.name, window_index, window).select_last(preserve_zoom)
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
        let result = apply_swap_between_windows(
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
        );
        // Refusal can intentionally unzoom, but cannot drop either owner.
        self.windows.insert(source.window_index, source_window);
        self.windows.insert(target.window_index, target_window);

        result
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

        // Remove only after both targets are validated; restore both placements on errors.
        self.window_at(source.window_index)
            .ok_or_else(|| invalid_window_target(&self.name, source.window_index))?;
        self.window_at(target.window_index)
            .ok_or_else(|| invalid_window_target(&self.name, target.window_index))?;
        let mut source_window = self
            .windows
            .remove(&source.window_index)
            .expect("validated source");
        let mut target_window = self
            .windows
            .remove(&target.window_index)
            .expect("validated target");
        let result = super::pane_transfer_join::join_between(
            &mut source_window,
            &self.name,
            source,
            &mut target_window,
            &self.name,
            target,
            options,
        );
        if let Err(error) = result {
            self.windows.insert(source.window_index, source_window);
            self.windows.insert(target.window_index, target_window);
            return Err(error);
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
            self.winlink_alert_flags.remove(&source.window_index);
            if self.active_window == source.window_index {
                self.active_window = self.next_active_window_after_removal(source.window_index);
                self.group_initial_window_id = None;
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
        source: u32,
        target: u32,
        options: PaneSwapOptions,
    ) -> Result<(), RmuxError> {
        let window = self
            .windows
            .get_mut(&window_index)
            .ok_or_else(|| invalid_window_target(&self.name, window_index))?;
        super::pane_transfer_borrowed::swap_within(
            window,
            &self.name,
            window_index,
            source,
            target,
            options,
        )
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
            .windows
            .get_mut(&source.window_index)
            .ok_or_else(|| invalid_window_target(&self.name, source.window_index))?;
        super::pane_transfer_borrowed::join_within(window, &self.name, source, target, options)
    }
}
