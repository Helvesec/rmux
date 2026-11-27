use super::Window;
use crate::layout::{LayoutDirection, LayoutTree};
use crate::FloatingPaneRect;
use rmux_proto::{RmuxError, RmuxWindowResizeTopology, RmuxWindowSnapshotSplitDirection};

impl Window {
    pub(super) fn current_resize_topology(&self) -> RmuxWindowResizeTopology {
        let mut topology = self.resize_topology.clone();
        let replacements = self.borrowed_pane_replacements();
        for id in &mut topology.floating_attachment_order {
            if let Some((_, new)) = replacements.iter().find(|(old, _)| old == id) {
                *id = *new;
            }
        }
        let stack = self.current_floating_stack();
        topology
            .floating_attachment_order
            .retain(|id| stack.contains(*id));
        for placement in stack.placements() {
            let id = placement.pane_id();
            if !topology.floating_attachment_order.contains(&id) {
                topology.floating_attachment_order.push(id);
            }
        }
        if stack.is_empty() {
            topology.root_direction = None;
        } else if topology.root_direction.is_none() {
            let direction = self
                .current_layout_tree()
                .and_then(|tree| tree.root_direction());
            topology.root_direction = Some(match direction {
                Some(LayoutDirection::LeftRight) => RmuxWindowSnapshotSplitDirection::LeftRight,
                _ => RmuxWindowSnapshotSplitDirection::TopBottom,
            });
        }
        topology
    }

    // Reconcile insertions before mutation; snapshots resolve them without changing state.
    pub(super) fn sync_resize_topology(&mut self) {
        self.resize_topology = self.current_resize_topology();
    }

    pub(crate) fn resize_canonical_absolute(
        &mut self,
        pane_index: u32,
        columns: Option<u16>,
        rows: Option<u16>,
    ) -> Result<(), RmuxError> {
        self.resize_absolute(pane_index, columns, rows, false)
    }

    // Floating CLI axes normalize zero to one; incompatible axes resize only the target.
    pub(crate) fn resize_absolute(
        &mut self,
        pane_index: u32,
        columns: Option<u16>,
        rows: Option<u16>,
        floating_dimensions: bool,
    ) -> Result<(), RmuxError> {
        let id = self.require_pane_id(pane_index)?;
        let topology = self.current_resize_topology();
        let current_stack = self.current_floating_stack();
        let dimension = |value: i32| u16::try_from(value).map_err(|_| bounds());
        let floats = topology
            .floating_attachment_order
            .iter()
            .map(|id| {
                let rect = current_stack.placement(*id).expect("attached float").rect();
                Ok((*id, dimension(rect.cols())?, dimension(rect.rows())?))
            })
            .collect::<Result<Vec<_>, RmuxError>>()?;
        let direction = match topology.root_direction {
            Some(RmuxWindowSnapshotSplitDirection::LeftRight) => LayoutDirection::LeftRight,
            _ => LayoutDirection::TopBottom,
        };
        let (tree, sizes) = LayoutTree::resize_attached(
            self.current_layout_tree().map(|tree| tree.into_owned()),
            &floats,
            direction,
            id,
            columns,
            rows,
        )?;
        let mut stack = current_stack.into_owned();
        for (float_id, cols, height) in sizes {
            let (cols, rows) = match (floating_dimensions && float_id == id, direction) {
                (true, _) if tree.is_none() && floats.len() == 1 => (
                    columns.unwrap_or(cols).max(1),
                    rows.unwrap_or(height).max(1),
                ),
                (true, LayoutDirection::TopBottom) => (columns.unwrap_or(cols).max(1), height),
                (true, LayoutDirection::LeftRight) => (cols, rows.unwrap_or(height).max(1)),
                _ => (cols, height),
            };
            let rect = stack.placement(float_id).expect("planned float").rect();
            // Validate the complete plan before changing any window state.
            rect.x().checked_add(i32::from(cols)).ok_or_else(bounds)?;
            rect.y().checked_add(i32::from(rows)).ok_or_else(bounds)?;
            stack.set_rect(
                float_id,
                FloatingPaneRect::new(rect.x(), rect.y(), i32::from(cols), i32::from(rows))?,
            );
        }
        self.reconcile_borrowed_pane_owners();
        self.resize_topology = topology;
        self.auto_unzoom();
        self.layout_tree = tree;
        self.floating_stack = stack;
        self.requested_main_width = None;
        self.requested_main_height = None;
        self.apply_layout_tree();
        Ok(())
    }
}
fn bounds() -> RmuxError {
    RmuxError::Server("canonical resize exceeds representable geometry".into())
}
