mod resize;

use rmux_proto::{
    LayoutName, PaneId, ResizePaneAdjustment, ResizeWindowAdjustment, RmuxError,
    RmuxWindowSnapshot, RotateWindowDirection, SessionId, SessionName, SplitDirection, Target,
    TerminalSize, WindowTarget,
};

use super::{
    target_error::{invalid_pane_target, invalid_window_target},
    Session, SessionStore,
};
use crate::{FloatingPaneRect, FloatingStack, Window};

/// Layout operations over one exclusively borrowed stored window.
///
/// The window remains owned by its backing store. This context exposes only
/// layout and pane-geometry operations and owns no window state.
pub struct WindowLayoutContext<'a> {
    session_name: SessionName,
    window_index: u32,
    window: &'a mut Window,
}

impl<'a> WindowLayoutContext<'a> {
    pub(crate) fn new(
        session_name: SessionName,
        window_index: u32,
        window: &'a mut Window,
    ) -> Self {
        Self {
            session_name,
            window_index,
            window,
        }
    }

    /// Returns the window's selected named layout by value.
    #[must_use]
    pub fn layout(&self) -> LayoutName {
        self.window.layout()
    }

    /// Returns the serialized layout used for layout-change comparisons.
    #[must_use]
    pub fn layout_dump(&self) -> String {
        self.window.layout_dump()
    }

    /// Returns the resolved floating pane stack in rear-to-front order.
    #[must_use]
    pub fn floating_stack(&self) -> FloatingStack {
        self.window.floating_stack()
    }

    /// Returns whether this window is zoomed.
    #[must_use]
    pub fn is_zoomed(&self) -> bool {
        self.window.is_zoomed()
    }

    /// Returns this window's active local pane index.
    #[must_use]
    pub fn active_pane_index(&self) -> u32 {
        self.window.active_pane_index()
    }

    /// Returns this window's active stable pane identity.
    #[must_use]
    pub fn active_pane_id(&self) -> Option<PaneId> {
        self.window.active_pane().map(crate::Pane::id)
    }

    /// Returns this window's previously active stable pane identity.
    #[must_use]
    pub fn last_pane_id(&self) -> Option<PaneId> {
        self.window
            .last_pane_index()
            .and_then(|pane_index| self.window.pane_id(pane_index))
    }

    /// Restores explicit V2 canonical geometry and attachment semantics.
    pub fn restore_rmux_window_snapshot_v2(
        &mut self,
        snapshot: &rmux_proto::RmuxWindowSnapshotV2,
    ) -> Result<Vec<PaneId>, RmuxError> {
        self.window.restore_rmux_window_snapshot_v2(snapshot)
    }

    /// Captures a typed RMUX snapshot for this canonical window occurrence.
    pub fn export_rmux_window_snapshot(
        &self,
        session_id: SessionId,
    ) -> Result<RmuxWindowSnapshot, RmuxError> {
        self.window.export_rmux_window_snapshot(
            session_id,
            WindowTarget::with_window(self.session_name.clone(), self.window_index),
        )
    }

    /// Restores a typed RMUX snapshot and returns panes needing runtime resize.
    pub fn restore_rmux_window_snapshot(
        &mut self,
        snapshot: &RmuxWindowSnapshot,
    ) -> Result<Vec<PaneId>, RmuxError> {
        self.window.restore_rmux_window_snapshot(snapshot)
    }

    /// Saves the current serialized layout as the window's old layout.
    pub fn save_old_layout(&mut self) {
        self.window.save_old_layout();
    }

    /// Applies a named layout using tmux `main-pane-*` option values.
    pub fn select_layout_with_main_pane_size(
        &mut self,
        layout: LayoutName,
        main_width: Option<u16>,
        main_height: Option<u16>,
    ) {
        self.select_layout_with_tiled_max_columns(layout, main_width, main_height, None);
    }

    /// Applies a named layout using tmux `main-pane-*` and
    /// `tiled-layout-max-columns` option values.
    ///
    /// `tiled_max_columns` is the bound the caller resolved for this window at
    /// command time; `None` is unbounded, which is what the option's `0` means.
    pub fn select_layout_with_tiled_max_columns(
        &mut self,
        layout: LayoutName,
        main_width: Option<u16>,
        main_height: Option<u16>,
        tiled_max_columns: Option<u16>,
    ) {
        self.window.set_layout_with_tiled_max_columns(
            layout,
            main_width,
            main_height,
            tiled_max_columns,
        );
    }

    /// Refits a `window-size manual` window to its explicit size; see
    /// `Window::refit_to_manual_size`. Returns whether the size changed.
    pub fn refit_to_manual_size(&mut self) -> bool {
        self.window.refit_to_manual_size()
    }

    /// Applies a tmux custom layout string.
    pub fn select_custom_layout(&mut self, layout: &str) -> Result<(), RmuxError> {
        self.window.apply_custom_layout(layout)
    }

    /// Reapplies the previously saved tmux old layout.
    pub fn reapply_old_layout(&mut self) -> Result<bool, RmuxError> {
        self.window.reapply_old_layout()
    }

    /// Spreads the window around its active pane, matching tmux `select-layout -E`.
    pub fn spread_layout(&mut self) -> bool {
        self.window.spread_layout(self.window.active_pane_index())
    }

    /// Converts one tiled pane into a frontmost floating pane.
    ///
    /// # Errors
    ///
    /// Returns [`RmuxError::InvalidTarget`] if the pane index does not exist,
    /// or [`RmuxError::Server`] if the pane is already floating.
    pub fn float_pane(&mut self, pane_index: u32, rect: FloatingPaneRect) -> Result<(), RmuxError> {
        self.ensure_pane_target(pane_index)?;
        self.window.float_pane(pane_index, rect)
    }

    /// Converts a tiled pane using its canonical layout rectangle.
    pub fn float_pane_at_tiled_geometry(&mut self, pane_index: u32) -> Result<(), RmuxError> {
        self.ensure_pane_target(pane_index)?;
        self.window.float_pane_at_tiled_geometry(pane_index)
    }

    /// Converts one floating pane back into the tiled layout tree.
    ///
    /// # Errors
    ///
    /// Returns [`RmuxError::InvalidTarget`] when an addressed pane index does
    /// not exist, or [`RmuxError::Server`] when the source/target placement
    /// is not valid for tiling.
    pub fn tile_floating_pane(
        &mut self,
        pane_index: u32,
        target_pane_index: Option<u32>,
        direction: SplitDirection,
        before: bool,
    ) -> Result<(), RmuxError> {
        self.ensure_pane_target(pane_index)?;
        if let Some(target_pane_index) = target_pane_index {
            self.ensure_pane_target(target_pane_index)?;
        }
        self.window
            .tile_floating_pane(pane_index, target_pane_index, direction, before)
    }

    /// Moves one floating pane to an absolute signed content coordinate.
    ///
    /// # Errors
    ///
    /// Returns [`RmuxError::InvalidTarget`] if the pane index does not exist,
    /// or [`RmuxError::Server`] if the pane is not floating.
    pub fn move_floating_pane(&mut self, pane_index: u32, x: i32, y: i32) -> Result<(), RmuxError> {
        self.ensure_pane_target(pane_index)?;
        self.window.move_floating_pane(pane_index, x, y)
    }

    /// Changes selected canonical coordinates without changing zoom or size.
    pub fn translate_floating_pane(
        &mut self,
        pane_index: u32,
        x: Option<i32>,
        y: Option<i32>,
    ) -> Result<bool, RmuxError> {
        self.ensure_pane_target(pane_index)?;
        self.window.translate_floating_pane(pane_index, x, y)
    }

    /// Resizes one floating pane to an absolute signed content size.
    ///
    /// # Errors
    ///
    /// Returns [`RmuxError::InvalidTarget`] if the pane index does not exist,
    /// or [`RmuxError::Server`] if the pane is not floating or the size is
    /// not positive.
    pub fn resize_floating_pane(
        &mut self,
        pane_index: u32,
        cols: i32,
        rows: i32,
    ) -> Result<(), RmuxError> {
        self.ensure_pane_target(pane_index)?;
        self.window.resize_floating_pane(pane_index, cols, rows)
    }

    /// Sets a floating pane's visibility without changing z-order.
    ///
    /// # Errors
    ///
    /// Returns [`RmuxError::InvalidTarget`] if the pane index does not exist,
    /// or [`RmuxError::Server`] if the pane is not floating.
    pub fn set_floating_pane_visible(
        &mut self,
        pane_index: u32,
        visible: bool,
    ) -> Result<(), RmuxError> {
        self.ensure_pane_target(pane_index)?;
        self.window.set_floating_pane_visible(pane_index, visible)
    }

    /// Raises a floating pane to the front without changing active selection.
    ///
    /// # Errors
    ///
    /// Returns [`RmuxError::InvalidTarget`] if the pane index does not exist,
    /// or [`RmuxError::Server`] if the pane is not floating.
    pub fn raise_floating_pane(&mut self, pane_index: u32) -> Result<(), RmuxError> {
        self.ensure_pane_target(pane_index)?;
        self.window.raise_floating_pane(pane_index)
    }

    /// Selects a floating pane and raises it to the front.
    ///
    /// # Errors
    ///
    /// Returns [`RmuxError::InvalidTarget`] if the pane index does not exist,
    /// or [`RmuxError::Server`] if the pane is not floating.
    pub fn select_floating_pane(&mut self, pane_index: u32) -> Result<(), RmuxError> {
        self.ensure_pane_target(pane_index)?;
        self.window.select_floating_pane(pane_index)
    }

    /// Selects the next tmux-standard layout using tmux `main-pane-*` option values.
    pub fn next_layout_with_main_pane_size(
        &mut self,
        main_width: Option<u16>,
        main_height: Option<u16>,
    ) -> LayoutName {
        self.next_layout_with_tiled_max_columns(main_width, main_height, None)
    }

    /// Selects the next tmux-standard layout, carrying the resolved
    /// `tiled-layout-max-columns` bound for when the cycle reaches tiled.
    pub fn next_layout_with_tiled_max_columns(
        &mut self,
        main_width: Option<u16>,
        main_height: Option<u16>,
        tiled_max_columns: Option<u16>,
    ) -> LayoutName {
        self.window
            .next_layout_with_tiled_max_columns(main_width, main_height, tiled_max_columns)
    }

    /// Selects the previous tmux-standard layout using tmux `main-pane-*` option values.
    pub fn previous_layout_with_main_pane_size(
        &mut self,
        main_width: Option<u16>,
        main_height: Option<u16>,
    ) -> LayoutName {
        self.previous_layout_with_tiled_max_columns(main_width, main_height, None)
    }

    /// Selects the previous tmux-standard layout, carrying the resolved
    /// `tiled-layout-max-columns` bound for when the cycle reaches tiled.
    pub fn previous_layout_with_tiled_max_columns(
        &mut self,
        main_width: Option<u16>,
        main_height: Option<u16>,
        tiled_max_columns: Option<u16>,
    ) -> LayoutName {
        self.window.previous_layout_with_tiled_max_columns(
            main_width,
            main_height,
            tiled_max_columns,
        )
    }

    /// Rotates pane positions without changing pane identities.
    ///
    /// The selected slot and last-pane tracking follow the window's rotation
    /// semantics. A zoomed window is restored after rotation only when
    /// `restore_zoom` is set.
    pub fn rotate(&mut self, direction: RotateWindowDirection, restore_zoom: bool) {
        if restore_zoom {
            self.window.rotate_panes_with_zoom(direction, true);
        } else {
            self.window.rotate_panes(direction);
        }
    }

    /// Applies an explicit `resize-window` request to this window.
    ///
    /// Explicit dimensions are applied before a saturating relative
    /// adjustment, and the requested dimensions are then clamped to one.
    /// Largest/smallest linked-session policies are resolved by the caller
    /// before this window-local operation.
    pub fn resize_window(
        &mut self,
        width: Option<u16>,
        height: Option<u16>,
        adjustment: Option<ResizeWindowAdjustment>,
    ) {
        let current_size = self.window.size();
        let mut cols = width.unwrap_or(current_size.cols);
        let mut rows = height.unwrap_or(current_size.rows);

        if let Some(adjustment) = adjustment {
            match adjustment {
                ResizeWindowAdjustment::Left(amount) => {
                    cols = cols.saturating_sub(amount);
                }
                ResizeWindowAdjustment::Right(amount) => {
                    cols = cols.saturating_add(amount);
                }
                ResizeWindowAdjustment::Up(amount) => {
                    rows = rows.saturating_sub(amount);
                }
                ResizeWindowAdjustment::Down(amount) => {
                    rows = rows.saturating_add(amount);
                }
                ResizeWindowAdjustment::LargestLinkedSession
                | ResizeWindowAdjustment::SmallestLinkedSession => {}
            }
        }

        self.window.set_explicit_size(TerminalSize {
            cols: cols.max(1),
            rows: rows.max(1),
        });
    }

    pub(super) fn set_size(&mut self, size: TerminalSize) {
        self.window.set_explicit_size(size);
    }

    /// Caps the visible geometry without changing the last explicit size.
    pub fn constrain_size(&mut self, maximum: TerminalSize) {
        self.window.mark_manual_size_constrained();
        let size = self.window.size();
        self.window.set_size(TerminalSize {
            cols: size.cols.min(maximum.cols).max(1),
            rows: size.rows.min(maximum.rows).max(1),
        });
    }

    fn ensure_pane_target(&self, pane_index: u32) -> Result<(), RmuxError> {
        if self.window.pane(pane_index).is_none() {
            return Err(invalid_pane_target(
                &self.session_name,
                self.window_index,
                pane_index,
                "pane index does not exist in session",
            ));
        }
        Ok(())
    }
}

impl Session {
    /// Applies a layout to the session's active window.
    pub fn select_layout(&mut self, layout: LayoutName) {
        self.select_layout_in_window(self.active_window, layout)
            .expect("active session window must exist");
    }

    /// Applies a layout to the addressed window.
    pub fn select_layout_in_window(
        &mut self,
        window_index: u32,
        layout: LayoutName,
    ) -> Result<(), RmuxError> {
        self.select_layout_in_window_with_main_pane_size(window_index, layout, None, None)
    }

    /// Applies a layout to the addressed window using tmux `main-pane-*` option values.
    pub fn select_layout_in_window_with_main_pane_size(
        &mut self,
        window_index: u32,
        layout: LayoutName,
        main_width: Option<u16>,
        main_height: Option<u16>,
    ) -> Result<(), RmuxError> {
        self.select_layout_in_window_with_tiled_max_columns(
            window_index,
            layout,
            main_width,
            main_height,
            None,
        )
    }

    /// Applies a named layout to the addressed window using tmux `main-pane-*`
    /// and `tiled-layout-max-columns` option values.
    pub fn select_layout_in_window_with_tiled_max_columns(
        &mut self,
        window_index: u32,
        layout: LayoutName,
        main_width: Option<u16>,
        main_height: Option<u16>,
        tiled_max_columns: Option<u16>,
    ) -> Result<(), RmuxError> {
        self.window_layout_context_mut(window_index)?
            .select_layout_with_tiled_max_columns(
                layout,
                main_width,
                main_height,
                tiled_max_columns,
            );
        Ok(())
    }

    /// Applies a tmux custom layout string to the addressed window.
    pub fn select_custom_layout_in_window(
        &mut self,
        window_index: u32,
        layout: &str,
    ) -> Result<(), RmuxError> {
        self.window_layout_context_mut(window_index)?
            .select_custom_layout(layout)
    }

    /// Reapplies the previously saved tmux old layout to the addressed window.
    pub fn reapply_old_layout_in_window(&mut self, window_index: u32) -> Result<bool, RmuxError> {
        self.window_layout_context_mut(window_index)?
            .reapply_old_layout()
    }

    /// Saves the current serialized layout as the window old layout.
    pub fn save_old_layout_in_window(&mut self, window_index: u32) -> Result<(), RmuxError> {
        self.window_layout_context_mut(window_index)?
            .save_old_layout();
        Ok(())
    }

    /// Spreads the addressed window around its active pane, matching tmux `select-layout -E`.
    pub fn spread_layout_in_window(&mut self, window_index: u32) -> Result<bool, RmuxError> {
        Ok(self
            .window_layout_context_mut(window_index)?
            .spread_layout())
    }

    /// Selects the next tmux-standard layout for the addressed window.
    pub fn next_layout_in_window(&mut self, window_index: u32) -> Result<LayoutName, RmuxError> {
        self.next_layout_in_window_with_main_pane_size(window_index, None, None)
    }

    /// Selects the next tmux-standard layout using tmux `main-pane-*` option values.
    pub fn next_layout_in_window_with_main_pane_size(
        &mut self,
        window_index: u32,
        main_width: Option<u16>,
        main_height: Option<u16>,
    ) -> Result<LayoutName, RmuxError> {
        self.next_layout_in_window_with_tiled_max_columns(
            window_index,
            main_width,
            main_height,
            None,
        )
    }

    /// Selects the next tmux-standard layout for the addressed window, carrying
    /// the resolved `tiled-layout-max-columns` bound.
    pub fn next_layout_in_window_with_tiled_max_columns(
        &mut self,
        window_index: u32,
        main_width: Option<u16>,
        main_height: Option<u16>,
        tiled_max_columns: Option<u16>,
    ) -> Result<LayoutName, RmuxError> {
        Ok(self
            .window_layout_context_mut(window_index)?
            .next_layout_with_tiled_max_columns(main_width, main_height, tiled_max_columns))
    }

    /// Selects the previous tmux-standard layout for the addressed window.
    pub fn previous_layout_in_window(
        &mut self,
        window_index: u32,
    ) -> Result<LayoutName, RmuxError> {
        self.previous_layout_in_window_with_main_pane_size(window_index, None, None)
    }

    /// Selects the previous tmux-standard layout using tmux `main-pane-*` option values.
    pub fn previous_layout_in_window_with_main_pane_size(
        &mut self,
        window_index: u32,
        main_width: Option<u16>,
        main_height: Option<u16>,
    ) -> Result<LayoutName, RmuxError> {
        self.previous_layout_in_window_with_tiled_max_columns(
            window_index,
            main_width,
            main_height,
            None,
        )
    }

    /// Selects the previous tmux-standard layout for the addressed window,
    /// carrying the resolved `tiled-layout-max-columns` bound.
    pub fn previous_layout_in_window_with_tiled_max_columns(
        &mut self,
        window_index: u32,
        main_width: Option<u16>,
        main_height: Option<u16>,
        tiled_max_columns: Option<u16>,
    ) -> Result<LayoutName, RmuxError> {
        Ok(self
            .window_layout_context_mut(window_index)?
            .previous_layout_with_tiled_max_columns(main_width, main_height, tiled_max_columns))
    }

    pub(super) fn window_layout_context_mut(
        &mut self,
        window_index: u32,
    ) -> Result<WindowLayoutContext<'_>, RmuxError> {
        let window = self
            .windows
            .get_mut(&window_index)
            .ok_or_else(|| invalid_window_target(&self.name, window_index))?;
        Ok(WindowLayoutContext::new(
            self.name.clone(),
            window_index,
            window,
        ))
    }
}

impl SessionStore {
    /// Borrows the addressed window for layout operations without exposing its owner.
    ///
    /// # Errors
    ///
    /// Returns [`RmuxError::SessionNotFound`] if the session does not exist, or
    /// [`RmuxError::InvalidTarget`] if the window index does not exist.
    pub fn window_layout_context_mut(
        &mut self,
        target: &WindowTarget,
    ) -> Result<WindowLayoutContext<'_>, RmuxError> {
        let window = self.resolve_window_mut(&Target::Window(target.clone()))?;
        Ok(WindowLayoutContext::new(
            target.session_name().clone(),
            target.window_index(),
            window,
        ))
    }
}
