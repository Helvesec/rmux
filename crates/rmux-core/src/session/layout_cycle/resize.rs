use super::{ResizePaneAdjustment, RmuxError, WindowLayoutContext};

impl WindowLayoutContext<'_> {
    /// Applies a resize adjustment to one local pane index.
    ///
    /// Zoom returns early. Compound adjustments follow tmux's width, height,
    /// then relative order; the window owns split selection and size clamping.
    ///
    /// # Errors
    ///
    /// Returns [`RmuxError::InvalidTarget`] if the pane index does not exist.
    /// Returns [`RmuxError::Server`] for a relative floating resize or invalid geometry.
    pub fn resize_pane(
        &mut self,
        pane_index: u32,
        adjustment: ResizePaneAdjustment,
    ) -> Result<(), RmuxError> {
        if adjustment == ResizePaneAdjustment::Zoom {
            return self.toggle_zoom(pane_index);
        }

        self.ensure_pane_target(pane_index)?;
        if !self.window.floating_stack().is_empty() {
            let (columns, rows) = match adjustment {
                ResizePaneAdjustment::AbsoluteWidth { columns } => (Some(columns), None),
                ResizePaneAdjustment::AbsoluteHeight { rows } => (None, Some(rows)),
                ResizePaneAdjustment::AbsoluteSize { columns, rows } => (Some(columns), Some(rows)),
                ResizePaneAdjustment::Composite {
                    columns,
                    rows,
                    relative: None,
                    ..
                } => (columns, rows),
                _ => (None, None),
            };
            if columns.is_some() || rows.is_some() {
                let id = self
                    .window
                    .pane_id(pane_index)
                    .expect("validated resize target");
                return if self.window.is_floating_pane_id(id) {
                    self.window.resize_absolute(pane_index, columns, rows, true)
                } else {
                    self.window
                        .resize_canonical_absolute(pane_index, columns, rows)
                };
            }
        }
        if !matches!(
            adjustment,
            ResizePaneAdjustment::NoOp
                | ResizePaneAdjustment::TrimBelow
                | ResizePaneAdjustment::Composite {
                    columns: None,
                    rows: None,
                    relative: None,
                    ..
                }
        ) {
            self.window.ensure_tiled_resize_target(pane_index)?;
        }
        match adjustment {
            ResizePaneAdjustment::NoOp => {}
            ResizePaneAdjustment::AbsoluteWidth { columns } => {
                let _ = self.window.resize_pane_width(pane_index, columns);
            }
            ResizePaneAdjustment::AbsoluteHeight { rows } => {
                let _ = self.window.resize_pane_height(pane_index, rows);
            }
            ResizePaneAdjustment::AbsoluteSize { columns, rows } => {
                let _ = self.window.resize_pane_width(pane_index, columns);
                let _ = self.window.resize_pane_height(pane_index, rows);
            }
            ResizePaneAdjustment::Composite {
                columns,
                rows,
                relative,
                cells,
            } => {
                if let Some(columns) = columns {
                    let _ = self.window.resize_pane_width(pane_index, columns);
                }
                if let Some(rows) = rows {
                    let _ = self.window.resize_pane_height(pane_index, rows);
                }
                if let Some(relative) = relative {
                    let _ = self
                        .window
                        .resize_pane_by(pane_index, relative.to_adjustment(cells));
                }
            }
            ResizePaneAdjustment::Up { cells } => {
                let _ = self
                    .window
                    .resize_pane_by(pane_index, ResizePaneAdjustment::Up { cells });
            }
            ResizePaneAdjustment::Down { cells } => {
                let _ = self
                    .window
                    .resize_pane_by(pane_index, ResizePaneAdjustment::Down { cells });
            }
            ResizePaneAdjustment::Left { cells } => {
                let _ = self
                    .window
                    .resize_pane_by(pane_index, ResizePaneAdjustment::Left { cells });
            }
            ResizePaneAdjustment::Right { cells } => {
                let _ = self
                    .window
                    .resize_pane_by(pane_index, ResizePaneAdjustment::Right { cells });
            }
            ResizePaneAdjustment::TrimBelow => {}
            ResizePaneAdjustment::Zoom => unreachable!("zoom returned early"),
        }
        Ok(())
    }

    /// Toggles zoom around one local pane index.
    ///
    /// # Errors
    ///
    /// Returns [`RmuxError::InvalidTarget`] if the pane index does not exist.
    pub fn toggle_zoom(&mut self, pane_index: u32) -> Result<(), RmuxError> {
        self.ensure_pane_target(pane_index)?;
        let _ = self.window.toggle_zoom(pane_index);
        Ok(())
    }
}
