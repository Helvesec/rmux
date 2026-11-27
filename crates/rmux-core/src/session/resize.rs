use rmux_proto::{ResizePaneAdjustment, RmuxError, SplitDirection};

use super::target_error::{invalid_pane_target, invalid_window_target};
use super::Session;

impl Session {
    /// Applies the supported resize adjustment to the session layout.
    pub fn resize_pane(
        &mut self,
        pane_index: u32,
        adjustment: ResizePaneAdjustment,
    ) -> Result<(), RmuxError> {
        self.resize_pane_in_window(self.active_window, pane_index, adjustment)
    }

    /// Applies the supported resize adjustment to the addressed window.
    pub fn resize_pane_in_window(
        &mut self,
        window_index: u32,
        pane_index: u32,
        adjustment: ResizePaneAdjustment,
    ) -> Result<(), RmuxError> {
        self.window_layout_context_mut(window_index)?
            .resize_pane(pane_index, adjustment)
    }

    /// Resizes the addressed pane to an exact size along the split axis.
    pub fn resize_pane_to_in_window(
        &mut self,
        window_index: u32,
        pane_index: u32,
        direction: SplitDirection,
        size: u32,
    ) -> Result<(), RmuxError> {
        self.ensure_resize_target(window_index, pane_index)?;
        let window = self
            .window_at_mut(window_index)
            .expect("addressed session window must exist");
        let _ = window.resize_pane_to(pane_index, direction, size.max(1));
        Ok(())
    }

    /// Resizes the newly split pane while keeping the adjustment inside the
    /// original target pane cell. Plain `resize_pane_to` follows tmux's normal
    /// border-selection rules and may borrow size from the next sibling when a
    /// pane is not last. For `split-window -l`, tmux instead sizes the new pane
    /// against the pane it just split, leaving unrelated neighbours untouched.
    pub fn resize_new_split_pane_to_in_window(
        &mut self,
        window_index: u32,
        new_pane_index: u32,
        direction: SplitDirection,
        size: u32,
        inserted_before_target: bool,
    ) -> Result<(), RmuxError> {
        self.ensure_resize_target(window_index, new_pane_index)?;
        let window = self
            .window_at_mut(window_index)
            .expect("addressed session window must exist");
        let _ = window.resize_new_split_pane_to(
            new_pane_index,
            direction,
            size,
            inserted_before_target,
        );
        Ok(())
    }

    /// Toggles zoom for the addressed pane's window.
    pub fn toggle_zoom_in_window(
        &mut self,
        window_index: u32,
        pane_index: u32,
    ) -> Result<(), RmuxError> {
        self.window_layout_context_mut(window_index)?
            .toggle_zoom(pane_index)
    }

    fn ensure_resize_target(&self, window_index: u32, pane_index: u32) -> Result<(), RmuxError> {
        if self.window_at(window_index).is_none() {
            return Err(invalid_window_target(&self.name, window_index));
        }

        if self
            .window_at(window_index)
            .and_then(|window| window.pane(pane_index))
            .is_none()
        {
            return Err(invalid_pane_target(
                &self.name,
                window_index,
                pane_index,
                "pane index does not exist in session",
            ));
        }

        self.window_at(window_index)
            .expect("validated resize window exists")
            .ensure_tiled_resize_target(pane_index)?;

        Ok(())
    }
}
