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
        if adjustment == ResizePaneAdjustment::Zoom {
            return self.toggle_zoom_in_window(window_index, pane_index);
        }

        self.ensure_resize_target(window_index, pane_index)?;
        let window = self
            .window_at_mut(window_index)
            .expect("addressed session window must exist");

        match adjustment {
            ResizePaneAdjustment::NoOp => {}
            ResizePaneAdjustment::AbsoluteWidth { columns } => {
                let _ = window.resize_pane_width(pane_index, columns);
            }
            ResizePaneAdjustment::AbsoluteHeight { rows } => {
                let _ = window.resize_pane_height(pane_index, rows);
            }
            ResizePaneAdjustment::AbsoluteSize { columns, rows } => {
                let _ = window.resize_pane_width(pane_index, columns);
                let _ = window.resize_pane_height(pane_index, rows);
            }
            ResizePaneAdjustment::Up { cells } => {
                let _ = window.resize_pane_by(pane_index, ResizePaneAdjustment::Up { cells });
            }
            ResizePaneAdjustment::Down { cells } => {
                let _ = window.resize_pane_by(pane_index, ResizePaneAdjustment::Down { cells });
            }
            ResizePaneAdjustment::Left { cells } => {
                let _ = window.resize_pane_by(pane_index, ResizePaneAdjustment::Left { cells });
            }
            ResizePaneAdjustment::Right { cells } => {
                let _ = window.resize_pane_by(pane_index, ResizePaneAdjustment::Right { cells });
            }
            ResizePaneAdjustment::TrimBelow => {}
            ResizePaneAdjustment::Zoom => unreachable!("zoom returned early"),
        }
        Ok(())
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

    /// Toggles zoom for the addressed pane's window.
    pub fn toggle_zoom_in_window(
        &mut self,
        window_index: u32,
        pane_index: u32,
    ) -> Result<(), RmuxError> {
        self.ensure_resize_target(window_index, pane_index)?;
        self.window_at_mut(window_index)
            .expect("addressed session window must exist")
            .toggle_zoom(pane_index);
        Ok(())
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

        Ok(())
    }
}
