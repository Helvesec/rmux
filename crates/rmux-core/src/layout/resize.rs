use rmux_proto::TerminalSize;

use super::cell::LayoutKind;
use super::mutation::{axis_size, cell_at_path, cell_mut_at_path};
use super::{LayoutCell, LayoutDirection, LayoutTree};

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
        pane_position: usize,
        direction: LayoutDirection,
        new_size: u32,
    ) -> bool {
        let Some(leaf_index) = self
            .leaf_map
            .as_ref()
            .map_or(Some(pane_position), |map| map.get(pane_position).copied())
        else {
            return false;
        };

        self.resize_leaf_to(leaf_index, direction, new_size)
    }

    pub(crate) fn resize_pane_by(
        &mut self,
        pane_position: usize,
        direction: LayoutDirection,
        change: i32,
    ) -> bool {
        if change == 0 {
            return true;
        }

        let Some(leaf_index) = self
            .leaf_map
            .as_ref()
            .map_or(Some(pane_position), |map| map.get(pane_position).copied())
        else {
            return false;
        };

        self.resize_leaf_by(leaf_index, direction, change)
    }

    fn resize_leaf_to(
        &mut self,
        leaf_index: usize,
        direction: LayoutDirection,
        new_size: u32,
    ) -> bool {
        let paths = self.leaf_paths();
        let Some(mut cell_path) = paths.get(leaf_index).cloned() else {
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

    fn resize_leaf_by(
        &mut self,
        leaf_index: usize,
        direction: LayoutDirection,
        change: i32,
    ) -> bool {
        let Some((parent_path, handle_index)) = self.find_resize_handle(leaf_index, direction)
        else {
            return false;
        };

        self.resize_layout(&parent_path, handle_index, direction, change, true)
    }

    fn find_resize_handle(
        &self,
        leaf_index: usize,
        direction: LayoutDirection,
    ) -> Option<(Vec<usize>, usize)> {
        let paths = self.leaf_paths();
        let mut cell_path = paths.get(leaf_index)?.clone();

        loop {
            let child_index = *cell_path.last()?;
            let parent_path = cell_path[..cell_path.len() - 1].to_vec();
            let parent = cell_at_path(&self.root, &parent_path)?;
            if let LayoutKind::Split(parent_direction, children) = &parent.kind
                && *parent_direction == direction {
                    let handle_index = if child_index + 1 == children.len() {
                        child_index.checked_sub(1)
                    } else {
                        Some(child_index)
                    };
                    if let Some(handle_index) = handle_index {
                        return Some((parent_path, handle_index));
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
