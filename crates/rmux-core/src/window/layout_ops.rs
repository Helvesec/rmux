use rmux_proto::{LayoutName, ResizePaneAdjustment, RmuxError, SplitDirection};

use super::Window;
use crate::layout::{LayoutDirection, LayoutOptions, LayoutTree};

impl Window {
    pub(crate) fn resize_main_pane(&mut self, adjustment: ResizePaneAdjustment) {
        match adjustment {
            ResizePaneAdjustment::NoOp => return,
            ResizePaneAdjustment::AbsoluteWidth { columns } => {
                self.auto_unzoom();
                if self.custom_layout {
                    let resized = self.layout_tree.as_mut().is_some_and(|tree| {
                        tree.resize_pane_to(0, LayoutDirection::LeftRight, u32::from(columns))
                    });
                    if resized {
                        self.apply_layout_tree();
                    }
                    return;
                }
                self.requested_main_width = Some(columns);
            }
            ResizePaneAdjustment::AbsoluteHeight { rows } => {
                self.auto_unzoom();
                if self.custom_layout {
                    let resized = self.layout_tree.as_mut().is_some_and(|tree| {
                        tree.resize_pane_to(0, LayoutDirection::TopBottom, u32::from(rows))
                    });
                    if resized {
                        self.apply_layout_tree();
                    }
                    return;
                }
                self.requested_main_height = Some(rows);
            }
            ResizePaneAdjustment::AbsoluteSize { columns, rows } => {
                self.resize_main_pane(ResizePaneAdjustment::AbsoluteWidth { columns });
                self.resize_main_pane(ResizePaneAdjustment::AbsoluteHeight { rows });
                return;
            }
            ResizePaneAdjustment::Composite {
                columns,
                rows,
                relative,
                cells,
            } => {
                if let Some(columns) = columns {
                    self.resize_main_pane(ResizePaneAdjustment::AbsoluteWidth { columns });
                }
                if let Some(rows) = rows {
                    self.resize_main_pane(ResizePaneAdjustment::AbsoluteHeight { rows });
                }
                if let Some(relative) = relative {
                    self.resize_main_pane(relative.to_adjustment(cells));
                }
                return;
            }
            ResizePaneAdjustment::Zoom => {
                self.toggle_zoom(self.active_pane);
                return;
            }
            ResizePaneAdjustment::TrimBelow => return,
            ResizePaneAdjustment::Up { cells } => {
                let rows = self.pane(0).map_or(1, |pane| {
                    pane.geometry().rows().saturating_sub(cells).max(1)
                });
                self.resize_main_pane(ResizePaneAdjustment::AbsoluteHeight { rows });
                return;
            }
            ResizePaneAdjustment::Down { cells } => {
                let rows = self.pane(0).map_or(1, |pane| {
                    pane.geometry().rows().saturating_add(cells).max(1)
                });
                self.resize_main_pane(ResizePaneAdjustment::AbsoluteHeight { rows });
                return;
            }
            ResizePaneAdjustment::Left { cells } => {
                let columns = self.pane(0).map_or(1, |pane| {
                    pane.geometry().cols().saturating_sub(cells).max(1)
                });
                self.resize_main_pane(ResizePaneAdjustment::AbsoluteWidth { columns });
                return;
            }
            ResizePaneAdjustment::Right { cells } => {
                let columns = self.pane(0).map_or(1, |pane| {
                    pane.geometry().cols().saturating_add(cells).max(1)
                });
                self.resize_main_pane(ResizePaneAdjustment::AbsoluteWidth { columns });
                return;
            }
        }

        self.rebuild_named_layout_tree(self.layout);
    }

    pub(crate) fn resize_pane_width(&mut self, pane_index: u32, columns: u16) -> bool {
        let Some(position) = self.pane_position(pane_index) else {
            return false;
        };

        if self.panes.len() == 1 {
            self.resize_main_pane(ResizePaneAdjustment::AbsoluteWidth { columns });
            return true;
        }

        self.auto_unzoom();
        let resized = self.layout_tree.as_mut().is_some_and(|tree| {
            tree.resize_pane_to(position, LayoutDirection::LeftRight, u32::from(columns))
        });
        if resized {
            self.requested_main_width = None;
            self.apply_layout_tree();
        }
        resized
    }

    pub(crate) fn resize_pane_height(&mut self, pane_index: u32, rows: u16) -> bool {
        let Some(position) = self.pane_position(pane_index) else {
            return false;
        };

        if self.panes.len() == 1 {
            self.resize_main_pane(ResizePaneAdjustment::AbsoluteHeight { rows });
            return true;
        }

        self.auto_unzoom();
        let resized = self.layout_tree.as_mut().is_some_and(|tree| {
            tree.resize_pane_to(position, LayoutDirection::TopBottom, u32::from(rows))
        });
        if resized {
            self.requested_main_height = None;
            self.apply_layout_tree();
        }
        resized
    }

    pub(crate) fn resize_pane_to(
        &mut self,
        pane_index: u32,
        direction: SplitDirection,
        new_size: u32,
    ) -> bool {
        let Some(position) = self.pane_position(pane_index) else {
            return false;
        };
        self.auto_unzoom();
        let resized = self.layout_tree.as_mut().is_some_and(|tree| {
            tree.resize_pane_to(
                position,
                LayoutDirection::from_split_direction(direction),
                new_size,
            )
        });
        if resized {
            self.apply_layout_tree();
        }
        resized
    }

    /// Resizes a newly inserted split pane while keeping the size adjustment
    /// inside the original target pane cell. Plain `resize_pane_to` follows
    /// tmux's normal border-selection rules and can borrow from an unrelated
    /// next sibling when the target is not the last pane. `split-window -l` and
    /// `join-pane -l` instead size the inserted pane against the pane that was
    /// split.
    pub(crate) fn resize_new_split_pane_to(
        &mut self,
        new_pane_index: u32,
        direction: SplitDirection,
        new_size: u32,
        inserted_before_target: bool,
    ) -> bool {
        let new_size = new_size.max(1);
        if inserted_before_target {
            return self.resize_pane_to(new_pane_index, direction, new_size);
        }

        let Some(new_position) = self.pane_position(new_pane_index) else {
            return false;
        };
        let Some(target_position) = new_position.checked_sub(1) else {
            return self.resize_pane_to(new_pane_index, direction, new_size);
        };
        let target_pane_index = self.panes()[target_position].index();
        let target_geometry = self.panes()[target_position].geometry();
        let new_geometry = self.panes()[new_position].geometry();
        let target_size = match direction {
            SplitDirection::Vertical => u32::from(target_geometry.cols()),
            SplitDirection::Horizontal => u32::from(target_geometry.rows()),
        };
        let new_current_size = match direction {
            SplitDirection::Vertical => u32::from(new_geometry.cols()),
            SplitDirection::Horizontal => u32::from(new_geometry.rows()),
        };
        let combined_cells = target_size
            .saturating_add(new_current_size)
            .saturating_add(1);
        let target_size = combined_cells
            .saturating_sub(1)
            .saturating_sub(new_size)
            .max(1);
        self.resize_pane_to(target_pane_index, direction, target_size)
    }

    pub(crate) fn resize_pane_by(
        &mut self,
        pane_index: u32,
        adjustment: ResizePaneAdjustment,
    ) -> bool {
        let Some(position) = self.pane_position(pane_index) else {
            return false;
        };
        let (direction, change) = match adjustment {
            ResizePaneAdjustment::Up { cells } => (LayoutDirection::TopBottom, -(i32::from(cells))),
            ResizePaneAdjustment::Down { cells } => (LayoutDirection::TopBottom, i32::from(cells)),
            ResizePaneAdjustment::Left { cells } => {
                (LayoutDirection::LeftRight, -(i32::from(cells)))
            }
            ResizePaneAdjustment::Right { cells } => (LayoutDirection::LeftRight, i32::from(cells)),
            ResizePaneAdjustment::AbsoluteWidth { .. }
            | ResizePaneAdjustment::AbsoluteHeight { .. }
            | ResizePaneAdjustment::AbsoluteSize { .. }
            | ResizePaneAdjustment::Composite { .. }
            | ResizePaneAdjustment::TrimBelow
            | ResizePaneAdjustment::Zoom
            | ResizePaneAdjustment::NoOp => return false,
        };

        self.auto_unzoom();
        let resized = self
            .layout_tree
            .as_mut()
            .is_some_and(|tree| tree.resize_pane_by(position, direction, change));
        if resized {
            self.sync_requested_main_size_after_directional_resize();
            self.apply_layout_tree();
        }
        resized
    }

    pub(crate) fn apply_custom_layout(&mut self, layout: &str) -> Result<(), RmuxError> {
        if self.panes.is_empty() {
            return Err(RmuxError::Server("invalid layout".to_owned()));
        }
        let tree = LayoutTree::parse(layout, self.panes.len())?;
        self.size = tree.size();
        self.layout_tree = Some(tree);
        self.custom_layout = true;
        self.apply_layout_tree();
        Ok(())
    }

    pub(crate) fn reapply_old_layout(&mut self) -> Result<bool, RmuxError> {
        let previous_old_layout = self.old_layout.clone();
        self.old_layout = Some(self.layout_dump());
        let Some(old_layout) = previous_old_layout else {
            return Ok(false);
        };
        self.apply_custom_layout(&old_layout)?;
        Ok(true)
    }

    pub(crate) fn spread_layout(&mut self, pane_index: u32) -> bool {
        let Some(position) = self.pane_position(pane_index) else {
            return false;
        };
        let spread = self
            .layout_tree
            .as_mut()
            .is_some_and(|tree| tree.spread_from_leaf(position));
        if spread {
            self.apply_layout_tree();
        }
        spread
    }

    fn layout_options(&self) -> LayoutOptions {
        LayoutOptions::default()
            .with_requested_main_width(self.requested_main_width)
            .with_requested_main_height(self.requested_main_height)
            .with_tiled_max_columns(None)
    }

    pub(super) fn rebuild_named_layout_tree(&mut self, layout: LayoutName) {
        if self.panes.is_empty() {
            self.layout_tree = None;
            self.custom_layout = false;
            return;
        }
        let tree = LayoutTree::named(layout, self.panes.len(), self.size, self.layout_options());
        self.layout_tree = Some(tree);
        self.custom_layout = false;
        self.apply_layout_tree();
    }

    pub(super) fn apply_layout_tree(&mut self) {
        if let Some(tree) = &self.layout_tree {
            tree.apply_to_panes(&mut self.panes);
        }
    }

    fn sync_requested_main_size_after_directional_resize(&mut self) {
        if self.custom_layout {
            return;
        }

        match self.layout {
            LayoutName::MainVertical | LayoutName::MainVerticalMirrored => {
                self.requested_main_width = self.pane(0).map(|pane| pane.geometry().cols());
            }
            LayoutName::MainHorizontal | LayoutName::MainHorizontalMirrored => {
                self.requested_main_height = self.pane(0).map(|pane| pane.geometry().rows());
            }
            _ => {}
        }
    }
}
