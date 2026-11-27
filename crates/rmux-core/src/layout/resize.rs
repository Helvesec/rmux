use crate::PaneId;
use rmux_proto::{RmuxError, TerminalSize};

use super::cell::LayoutKind;
use super::mutation::{axis_size, cell_at_path, cell_mut_at_path};
use super::{LayoutCell, LayoutDirection, LayoutTree};

type AttachedResize = (Option<LayoutTree>, Vec<(PaneId, u16, u16)>);

impl LayoutTree {
    pub(crate) fn resize(&mut self, size: TerminalSize) {
        self.resize_axis(
            LayoutDirection::LeftRight,
            i32::from(size.cols) - self.root.geometry.width as i32,
        );
        self.resize_axis(
            LayoutDirection::TopBottom,
            i32::from(size.rows) - self.root.geometry.height as i32,
        );
        self.root.fix_offsets(0, 0);
    }

    fn resize_axis(&mut self, direction: LayoutDirection, mut change: i32) {
        let limit = self.root.resize_check(direction) as i32;
        if change < 0 && change < -limit {
            change = -limit;
        }
        if limit == 0 && change < 0 {
            change = 0;
        }
        if change != 0 {
            self.root.resize_adjust(direction, change);
        }
    }

    pub(crate) fn resize_pane_to(
        &mut self,
        pane_id: PaneId,
        direction: LayoutDirection,
        new_size: u32,
    ) -> bool {
        let Some(mut cell_path) = self.root.leaf_path(pane_id) else {
            return false;
        };

        loop {
            let Some(&child_index) = cell_path.last() else {
                return false;
            };
            let parent_path = cell_path[..cell_path.len() - 1].to_vec();
            let Some(parent) = cell_at_path(&self.root, &parent_path) else {
                return false;
            };
            let LayoutKind::Split(parent_direction, children) = &parent.kind else {
                return false;
            };
            if *parent_direction == direction {
                let current_size = axis_size(&children[child_index], direction);
                let (handle_index, change) = if child_index + 1 == children.len() {
                    if child_index == 0 {
                        return false;
                    }
                    (
                        child_index - 1,
                        current_size as i32 - new_size.min(i32::MAX as u32) as i32,
                    )
                } else {
                    (
                        child_index,
                        new_size.min(i32::MAX as u32) as i32 - current_size as i32,
                    )
                };

                return self.resize_layout(&parent_path, handle_index, direction, change, true);
            }

            cell_path.pop();
        }
    }

    /// The cells `pane_id` shares with its next sibling along `direction`,
    /// their border included, in the nearest ancestor split that way: the
    /// cell a split inserted before its target divided. `None` when the pane
    /// has no next sibling along `direction`.
    pub(crate) fn cell_with_next_sibling(
        &self,
        pane_id: PaneId,
        direction: LayoutDirection,
    ) -> Option<u32> {
        let mut cell_path = self.root.leaf_path(pane_id)?;
        loop {
            let child_index = *cell_path.last()?;
            let parent_path = &cell_path[..cell_path.len() - 1];
            let parent = cell_at_path(&self.root, parent_path)?;
            if let LayoutKind::Split(parent_direction, children) = &parent.kind {
                if *parent_direction == direction {
                    let next = children.get(child_index + 1)?;
                    return Some(
                        axis_size(&children[child_index], direction)
                            .saturating_add(axis_size(next, direction))
                            .saturating_add(1),
                    );
                }
            }
            cell_path.pop();
        }
    }

    pub(crate) fn resize_pane_by(
        &mut self,
        pane_id: PaneId,
        direction: LayoutDirection,
        change: i32,
    ) -> bool {
        if change == 0 {
            return self.root.leaf_path(pane_id).is_some();
        }
        let Some((parent_path, handle_index)) = self.find_resize_handle(pane_id, direction) else {
            return false;
        };

        self.resize_layout(&parent_path, handle_index, direction, change, true)
    }

    fn find_resize_handle(
        &self,
        pane_id: PaneId,
        direction: LayoutDirection,
    ) -> Option<(Vec<usize>, usize)> {
        let mut cell_path = self.root.leaf_path(pane_id)?;

        loop {
            let child_index = *cell_path.last()?;
            let parent_path = cell_path[..cell_path.len() - 1].to_vec();
            let parent = cell_at_path(&self.root, &parent_path)?;
            if let LayoutKind::Split(parent_direction, children) = &parent.kind {
                if *parent_direction == direction {
                    let handle_index = if child_index + 1 == children.len() {
                        child_index.checked_sub(1)
                    } else {
                        Some(child_index)
                    };
                    if let Some(handle_index) = handle_index {
                        return Some((parent_path, handle_index));
                    }
                }
            }

            cell_path.pop();
        }
    }

    fn resize_layout(
        &mut self,
        parent_path: &[usize],
        child_index: usize,
        direction: LayoutDirection,
        change: i32,
        opposite: bool,
    ) -> bool {
        let mut needed = change;
        let mut changed = false;

        while needed != 0 {
            let size = if change > 0 {
                self.resize_pane_grow(parent_path, child_index, direction, needed as u32, opposite)
            } else {
                self.resize_pane_shrink(parent_path, child_index, direction, (-needed) as u32)
            };
            if size == 0 {
                break;
            }

            changed = true;
            if change > 0 {
                needed -= size as i32;
            } else {
                needed += size as i32;
            }
        }

        if changed {
            self.root.fix_offsets(0, 0);
        }
        changed
    }

    fn resize_pane_grow(
        &mut self,
        parent_path: &[usize],
        child_index: usize,
        direction: LayoutDirection,
        needed: u32,
        opposite: bool,
    ) -> u32 {
        let Some(parent) = cell_mut_at_path(&mut self.root, parent_path) else {
            return 0;
        };
        let LayoutKind::Split(parent_direction, children) = &mut parent.kind else {
            return 0;
        };
        if *parent_direction != direction || child_index >= children.len() {
            return 0;
        }

        let mut remove_index = None;
        let mut size = 0_u32;

        for (index, child) in children.iter().enumerate().skip(child_index + 1) {
            let available = child.resize_check(direction);
            if available > 0 {
                remove_index = Some(index);
                size = available;
                break;
            }
        }

        if opposite && remove_index.is_none() {
            for (index, child) in children.iter().enumerate().take(child_index).rev() {
                let available = child.resize_check(direction);
                if available > 0 {
                    remove_index = Some(index);
                    size = available;
                    break;
                }
            }
        }

        let Some(remove_index) = remove_index else {
            return 0;
        };
        size = size.min(needed);
        resize_children(children, child_index, remove_index, direction, size);
        size
    }

    fn resize_pane_shrink(
        &mut self,
        parent_path: &[usize],
        child_index: usize,
        direction: LayoutDirection,
        needed: u32,
    ) -> u32 {
        let Some(parent) = cell_mut_at_path(&mut self.root, parent_path) else {
            return 0;
        };
        let LayoutKind::Split(parent_direction, children) = &mut parent.kind else {
            return 0;
        };
        if *parent_direction != direction || child_index + 1 >= children.len() {
            return 0;
        }

        let mut remove_index = None;
        let mut size = 0_u32;
        for (index, child) in children.iter().enumerate().take(child_index + 1).rev() {
            let available = child.resize_check(direction);
            if available > 0 {
                remove_index = Some(index);
                size = available;
                break;
            }
        }

        let Some(remove_index) = remove_index else {
            return 0;
        };
        size = size.min(needed);
        resize_children(children, child_index + 1, remove_index, direction, size);
        size
    }
}

fn resize_children(
    children: &mut [LayoutCell],
    add_index: usize,
    remove_index: usize,
    direction: LayoutDirection,
    size: u32,
) {
    if add_index == remove_index || size == 0 {
        return;
    }

    if add_index < remove_index {
        let (left, right) = children.split_at_mut(remove_index);
        left[add_index].resize_adjust(direction, size as i32);
        right[0].resize_adjust(direction, -(size as i32));
    } else {
        let (left, right) = children.split_at_mut(add_index);
        right[0].resize_adjust(direction, size as i32);
        left[remove_index].resize_adjust(direction, -(size as i32));
    }
}

impl LayoutTree {
    pub(crate) fn root_direction(&self) -> Option<LayoutDirection> {
        match self.root.kind {
            LayoutKind::Pane(_) => None,
            LayoutKind::Split(direction, _) => Some(direction),
        }
    }

    /// One transient operation over canonical owners. No combined tree is stored.
    pub(crate) fn resize_attached(
        tiled: Option<Self>,
        floats: &[(PaneId, u16, u16)],
        root_direction: LayoutDirection,
        pane_id: PaneId,
        columns: Option<u16>,
        rows: Option<u16>,
    ) -> Result<AttachedResize, RmuxError> {
        use super::cell::LayoutGeometry;
        let error = || RmuxError::Server("canonical resize exceeds representable geometry".into());
        let mut children = match tiled {
            None => Vec::new(),
            Some(tree) => match tree.root.kind {
                LayoutKind::Split(direction, children) if direction == root_direction => children,
                kind => vec![LayoutCell {
                    geometry: tree.root.geometry,
                    kind,
                }],
            },
        };
        let tiled_count = children.len();
        for &(id, cols, rows) in floats {
            let geometry = LayoutGeometry::new(cols.into(), rows.into(), 0, 0);
            children.push(LayoutCell::pane(id, geometry));
        }
        if children.is_empty() {
            return Err(error());
        }
        let mut plan = Self {
            root: LayoutCell::split(root_direction, LayoutGeometry::new(1, 1, 0, 0), children),
        };
        if let Some(columns) = columns {
            plan.resize_pane_to(pane_id, LayoutDirection::LeftRight, u32::from(columns));
        }
        if let Some(rows) = rows {
            plan.resize_pane_to(pane_id, LayoutDirection::TopBottom, u32::from(rows));
        }
        let LayoutKind::Split(_, mut children) = plan.root.kind else {
            unreachable!()
        };
        let floating_sizes = children
            .drain(tiled_count..)
            .map(|cell| {
                let LayoutKind::Pane(id) = cell.kind else {
                    unreachable!()
                };
                Ok((
                    id,
                    u16::try_from(cell.geometry.width).map_err(|_| error())?,
                    u16::try_from(cell.geometry.height).map_err(|_| error())?,
                ))
            })
            .collect::<Result<Vec<_>, RmuxError>>()?;
        let root = match children.len() {
            0 => None,
            1 => children.pop(),
            _ => {
                let mut span = u32::try_from(children.len() - 1).map_err(|_| error())?;
                let mut cross_span = 0;
                for child in &children {
                    let (along, across) = match root_direction {
                        LayoutDirection::LeftRight => (child.geometry.width, child.geometry.height),
                        LayoutDirection::TopBottom => (child.geometry.height, child.geometry.width),
                    };
                    span = span.checked_add(along).ok_or_else(error)?;
                    cross_span = cross_span.max(across);
                }
                let (width, height) = match root_direction {
                    LayoutDirection::LeftRight => (span, cross_span),
                    LayoutDirection::TopBottom => (cross_span, span),
                };
                Some(LayoutCell::split(
                    root_direction,
                    LayoutGeometry::new(width, height, 0, 0),
                    children,
                ))
            }
        };
        let tiled = root
            .map(|mut root| {
                root.fix_offsets(0, 0);
                if !root.check() {
                    return Err(error());
                }
                let tree = Self { root };
                tree.snapshot_cell()?; // Checked u16 bounds on every canonical rectangle.
                Ok(tree)
            })
            .transpose()?;
        Ok((tiled, floating_sizes))
    }
}
