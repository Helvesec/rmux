use super::cell::LayoutKind;
use super::{
    LayoutCell, LayoutDirection, LayoutGeometry, LayoutTree, PANE_BORDER_CELLS, PANE_MINIMUM,
};
use crate::PaneId;

impl LayoutTree {
    pub(crate) fn split_leaf(
        &mut self,
        pane_id: PaneId,
        new_id: PaneId,
        direction: LayoutDirection,
        insert_before_target: bool,
    ) -> bool {
        let Some(path) = self.root.leaf_path(pane_id) else {
            return false;
        };

        if path.is_empty() {
            let original = self.root.clone();
            let Some(split_root) =
                split_leaf_cell(original.clone(), new_id, direction, insert_before_target)
            else {
                self.root = original;
                return false;
            };
            self.root = split_root;
            self.root.fix_offsets(0, 0);
            return true;
        }

        let mut parent_path = path.clone();
        let child_index = parent_path.pop().expect("non-root path has a child index");
        let Some(parent) = cell_mut_at_path(&mut self.root, &parent_path) else {
            return false;
        };

        match &mut parent.kind {
            LayoutKind::Split(parent_direction, children) if *parent_direction == direction => {
                let Some(original) = children.get(child_index).cloned() else {
                    return false;
                };
                let Some((first, second)) =
                    split_leaf_into_children(original, new_id, direction, insert_before_target)
                else {
                    return false;
                };
                children.remove(child_index);
                if insert_before_target {
                    children.insert(child_index, second);
                    children.insert(child_index, first);
                } else {
                    children.insert(child_index, first);
                    children.insert(child_index + 1, second);
                }
            }
            _ => {
                let Some(target) = cell_mut_at_path(&mut self.root, &path) else {
                    return false;
                };
                let original = target.clone();
                let Some(split_cell) =
                    split_leaf_cell(original.clone(), new_id, direction, insert_before_target)
                else {
                    *target = original;
                    return false;
                };
                *target = split_cell;
            }
        }

        self.root.fix_offsets(0, 0);
        true
    }

    pub(crate) fn split_root(
        &mut self,
        new_id: PaneId,
        direction: LayoutDirection,
        insert_before_target: bool,
    ) -> bool {
        let geometry = self.root.geometry;
        let saved = match direction {
            LayoutDirection::LeftRight => geometry.width,
            LayoutDirection::TopBottom => geometry.height,
        };
        if saved < (PANE_MINIMUM * 2) + PANE_BORDER_CELLS {
            return false;
        };
        let mut second_size = ((saved + PANE_BORDER_CELLS) / 2).saturating_sub(PANE_BORDER_CELLS);
        second_size = second_size.clamp(
            PANE_MINIMUM,
            saved.saturating_sub(PANE_MINIMUM + PANE_BORDER_CELLS),
        );
        let first_size = saved
            .saturating_sub(PANE_BORDER_CELLS)
            .saturating_sub(second_size);

        let mut existing = self.root.clone();
        let existing_size = if insert_before_target {
            second_size
        } else {
            first_size
        };
        let current_size = axis_size(&existing, direction);
        existing.resize_adjust(direction, existing_size as i32 - current_size as i32);

        let (new_geometry, existing_offset) = match direction {
            LayoutDirection::LeftRight => {
                if insert_before_target {
                    (
                        LayoutGeometry::new(first_size, geometry.height, 0, 0),
                        (first_size + PANE_BORDER_CELLS, 0),
                    )
                } else {
                    (
                        LayoutGeometry::new(
                            second_size,
                            geometry.height,
                            first_size + PANE_BORDER_CELLS,
                            0,
                        ),
                        (0, 0),
                    )
                }
            }
            LayoutDirection::TopBottom => {
                if insert_before_target {
                    (
                        LayoutGeometry::new(geometry.width, first_size, 0, 0),
                        (0, first_size + PANE_BORDER_CELLS),
                    )
                } else {
                    (
                        LayoutGeometry::new(
                            geometry.width,
                            second_size,
                            0,
                            first_size + PANE_BORDER_CELLS,
                        ),
                        (0, 0),
                    )
                }
            }
        };
        existing.fix_offsets(existing_offset.0, existing_offset.1);
        let new_leaf = LayoutCell::pane(new_id, new_geometry);
        self.root = if insert_before_target {
            LayoutCell::split(direction, geometry, vec![new_leaf, existing])
        } else {
            LayoutCell::split(direction, geometry, vec![existing, new_leaf])
        };
        self.root.fix_offsets(0, 0);
        true
    }

    pub(crate) fn remove_leaf(&mut self, pane_id: PaneId) -> bool {
        let Some(path) = self.root.leaf_path(pane_id) else {
            return false;
        };
        self.remove_at_path(path)
    }

    fn remove_at_path(&mut self, path: Vec<usize>) -> bool {
        if path.is_empty() {
            return false;
        }

        let mut parent_path = path;
        let child_index = parent_path.pop().expect("non-root path has a child index");
        let Some(parent) = cell_mut_at_path(&mut self.root, &parent_path) else {
            return false;
        };
        let LayoutKind::Split(direction, children) = &mut parent.kind else {
            return false;
        };
        if child_index >= children.len() {
            return false;
        }

        let removed = children.remove(child_index);
        if let Some(other) = if child_index == 0 {
            children.get_mut(0)
        } else {
            children.get_mut(child_index - 1)
        } {
            let growth = match direction {
                LayoutDirection::LeftRight => removed.geometry.width + PANE_BORDER_CELLS,
                LayoutDirection::TopBottom => removed.geometry.height + PANE_BORDER_CELLS,
            };
            other.resize_adjust(*direction, growth as i32);
        }

        collapse_single_child(&mut self.root);
        self.root.fix_offsets(0, 0);
        true
    }

    pub(crate) fn spread_from_leaf(&mut self, pane_id: PaneId) -> bool {
        let Some(path) = self.root.leaf_path(pane_id) else {
            return false;
        };
        for depth in (0..path.len()).rev() {
            let prefix = &path[..depth];
            let Some(cell) = cell_mut_at_path(&mut self.root, prefix) else {
                continue;
            };
            if cell.spread() {
                self.root.fix_offsets(0, 0);
                return true;
            }
        }
        false
    }

    /// Split before removal to preserve the existing target geometry. The
    /// duplicate source ID exists only inside this transaction, never in a window.
    pub(crate) fn move_leaf(
        &mut self,
        source: PaneId,
        target: PaneId,
        direction: LayoutDirection,
        before: bool,
    ) -> bool {
        if source == target {
            return false;
        }
        let Some(mut source_path) = self.root.leaf_path(source) else {
            return false;
        };
        let Some(target_path) = self.root.leaf_path(target) else {
            return false;
        };
        // A split in an existing same-axis parent inserts a sibling; only
        // source paths after that child need their child coordinate shifted.
        if let Some((&target_child, parent_path)) = target_path.split_last() {
            if let Some(parent) = cell_at_path(&self.root, parent_path) {
                if matches!(&parent.kind, LayoutKind::Split(axis, _) if *axis == direction)
                    && source_path.starts_with(parent_path)
                    && source_path
                        .get(parent_path.len())
                        .is_some_and(|index| *index > target_child)
                {
                    source_path[parent_path.len()] += 1;
                }
            }
        }
        if !self.split_leaf(target, source, direction, before) {
            return false;
        }
        assert!(
            self.remove_at_path(source_path),
            "validated source leaf must survive split"
        );
        true
    }
}

fn split_leaf_into_children(
    original: LayoutCell,
    new_id: PaneId,
    direction: LayoutDirection,
    insert_before_target: bool,
) -> Option<(LayoutCell, LayoutCell)> {
    let LayoutKind::Pane(original_id) = original.kind else {
        return None;
    };
    let geometry = original.geometry;
    let (first_id, second_id) = if insert_before_target {
        (new_id, original_id)
    } else {
        (original_id, new_id)
    };
    let saved = match direction {
        LayoutDirection::LeftRight => geometry.width,
        LayoutDirection::TopBottom => geometry.height,
    };
    if saved < (PANE_MINIMUM * 2) + PANE_BORDER_CELLS {
        return None;
    }

    let mut second_size = ((saved + PANE_BORDER_CELLS) / 2).saturating_sub(PANE_BORDER_CELLS);
    second_size = second_size.clamp(
        PANE_MINIMUM,
        saved.saturating_sub(PANE_MINIMUM + PANE_BORDER_CELLS),
    );
    let first_size = saved
        .saturating_sub(PANE_BORDER_CELLS)
        .saturating_sub(second_size);

    let (target_geometry, inserted_geometry) = match direction {
        LayoutDirection::LeftRight => (
            LayoutGeometry::new(first_size, geometry.height, geometry.x, geometry.y),
            LayoutGeometry::new(
                second_size,
                geometry.height,
                geometry.x + first_size + PANE_BORDER_CELLS,
                geometry.y,
            ),
        ),
        LayoutDirection::TopBottom => (
            LayoutGeometry::new(geometry.width, first_size, geometry.x, geometry.y),
            LayoutGeometry::new(
                geometry.width,
                second_size,
                geometry.x,
                geometry.y + first_size + PANE_BORDER_CELLS,
            ),
        ),
    };

    Some((
        LayoutCell::pane(first_id, target_geometry),
        LayoutCell::pane(second_id, inserted_geometry),
    ))
}

fn split_leaf_cell(
    original: LayoutCell,
    new_id: PaneId,
    direction: LayoutDirection,
    insert_before_target: bool,
) -> Option<LayoutCell> {
    let geometry = original.geometry;
    let (first, second) =
        split_leaf_into_children(original, new_id, direction, insert_before_target)?;
    Some(LayoutCell::split(direction, geometry, vec![first, second]))
}

pub(super) fn cell_at_path<'a>(cell: &'a LayoutCell, path: &[usize]) -> Option<&'a LayoutCell> {
    if path.is_empty() {
        return Some(cell);
    }

    let LayoutKind::Split(_, children) = &cell.kind else {
        return None;
    };
    let (head, tail) = path.split_first()?;
    let child = children.get(*head)?;
    cell_at_path(child, tail)
}

pub(super) fn cell_mut_at_path<'a>(
    cell: &'a mut LayoutCell,
    path: &[usize],
) -> Option<&'a mut LayoutCell> {
    if path.is_empty() {
        return Some(cell);
    }

    let LayoutKind::Split(_, children) = &mut cell.kind else {
        return None;
    };
    let (head, tail) = path.split_first()?;
    let child = children.get_mut(*head)?;
    cell_mut_at_path(child, tail)
}

pub(super) fn axis_size(cell: &LayoutCell, direction: LayoutDirection) -> u32 {
    match direction {
        LayoutDirection::LeftRight => cell.geometry.width,
        LayoutDirection::TopBottom => cell.geometry.height,
    }
}

fn collapse_single_child(cell: &mut LayoutCell) {
    if let LayoutKind::Split(_, children) = &mut cell.kind {
        for child in children.iter_mut() {
            collapse_single_child(child);
        }
        if children.len() == 1 {
            let mut only_child = children.remove(0);
            only_child.geometry = cell.geometry;
            only_child.fix_offsets(cell.geometry.x, cell.geometry.y);
            *cell = only_child;
        }
    }
}
