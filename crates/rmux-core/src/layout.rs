use rmux_proto::{
    LayoutName, RmuxError, RmuxWindowSnapshotCell, RmuxWindowSnapshotSplitDirection,
    SplitDirection, TerminalSize,
};

use crate::{Pane, PaneGeometry, PaneId};

mod cell;
mod mutation;
mod named;
mod parser;
mod resize;

use cell::{LayoutCell, LayoutGeometry};
use parser::LayoutParser;

const PANE_BORDER_CELLS: u32 = 1;
const PANE_MINIMUM: u32 = 1;
const DEFAULT_MAIN_PANE_WIDTH: u32 = 80;
const DEFAULT_MAIN_PANE_HEIGHT: u32 = 24;

pub(crate) const fn minimum_axis_size_for_panes(pane_count: usize) -> u32 {
    if pane_count == 0 {
        return 0;
    }
    (PANE_MINIMUM * pane_count as u32) + (PANE_BORDER_CELLS * pane_count.saturating_sub(1) as u32)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct LayoutOptions {
    requested_main_width: Option<u16>,
    requested_main_height: Option<u16>,
    tiled_max_columns: Option<u16>,
}

impl LayoutOptions {
    #[must_use]
    pub(crate) const fn with_requested_main_width(mut self, width: Option<u16>) -> Self {
        self.requested_main_width = width;
        self
    }

    #[must_use]
    pub(crate) const fn with_requested_main_height(mut self, height: Option<u16>) -> Self {
        self.requested_main_height = height;
        self
    }

    #[must_use]
    pub(crate) const fn with_tiled_max_columns(mut self, columns: Option<u16>) -> Self {
        self.tiled_max_columns = columns;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LayoutDirection {
    LeftRight,
    TopBottom,
}

impl LayoutDirection {
    #[must_use]
    pub(crate) const fn from_split_direction(direction: SplitDirection) -> Self {
        match direction {
            SplitDirection::Vertical => Self::LeftRight,
            SplitDirection::Horizontal => Self::TopBottom,
        }
    }

    #[must_use]
    const fn from_snapshot_direction(direction: RmuxWindowSnapshotSplitDirection) -> Self {
        match direction {
            RmuxWindowSnapshotSplitDirection::LeftRight => Self::LeftRight,
            RmuxWindowSnapshotSplitDirection::TopBottom => Self::TopBottom,
        }
    }

    #[must_use]
    const fn to_snapshot_direction(self) -> RmuxWindowSnapshotSplitDirection {
        match self {
            Self::LeftRight => RmuxWindowSnapshotSplitDirection::LeftRight,
            Self::TopBottom => RmuxWindowSnapshotSplitDirection::TopBottom,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LayoutTree {
    root: LayoutCell,
}

impl LayoutTree {
    #[must_use]
    pub(crate) fn single(size: TerminalSize, pane_id: PaneId) -> Self {
        Self {
            root: LayoutCell::pane(
                pane_id,
                LayoutGeometry::new(u32::from(size.cols), u32::from(size.rows), 0, 0),
            ),
        }
    }

    pub(crate) fn parse(layout: &str, pane_ids: &[PaneId]) -> Result<Self, RmuxError> {
        let pane_count = pane_ids.len();
        let (checksum, body) = layout
            .split_once(',')
            .ok_or_else(|| RmuxError::Server("invalid layout".to_owned()))?;
        if checksum.len() != 4 {
            return Err(RmuxError::Server("invalid layout".to_owned()));
        }
        let expected_checksum = u16::from_str_radix(checksum, 16)
            .map_err(|_| RmuxError::Server("invalid layout".to_owned()))?;
        if expected_checksum != layout_checksum(body) {
            return Err(RmuxError::Server("invalid layout".to_owned()));
        }

        let mut parser = LayoutParser::new(body);
        let root = parser.parse_cell()?;
        if !parser.is_eof() {
            return Err(RmuxError::Server("invalid layout".to_owned()));
        }

        let mut tree = Self { root };
        // Imported cells must be safe before pruning: removing a saved pane
        // resizes its siblings using signed deltas and recomputes offsets.
        if !tree.root.fix_root_size_if_needed() || !tree.root.import_dimensions_are_safe() {
            return Err(RmuxError::Server("invalid layout dimensions".to_owned()));
        }
        if !tree.root.check() {
            return Err(RmuxError::Server("invalid layout geometry".to_owned()));
        }
        tree.root.fix_offsets(0, 0);
        while tree.leaf_count() > pane_count {
            if !tree.remove_leaf(PaneId::new((tree.leaf_count() - 1) as u32)) {
                return Err(RmuxError::Server("invalid layout".to_owned()));
            }
        }
        if pane_count > tree.leaf_count() {
            return Err(RmuxError::Server(format!(
                "have {pane_count} panes but need {}",
                tree.leaf_count()
            )));
        }

        if !tree.root.fix_root_size_if_needed() || !tree.root.check() {
            return Err(RmuxError::Server(
                "size mismatch after applying layout".to_owned(),
            ));
        }
        tree.root.fix_offsets(0, 0);
        // Serialized IDs describe the exporting window, not ownership here.
        // Parsing assigns temporary traversal IDs; bind only surviving leaves.
        tree.root.map_ids(&mut |id| pane_ids[id.as_u32() as usize]);
        Ok(tree)
    }

    #[must_use]
    pub(crate) fn leaf_count(&self) -> usize {
        self.root.leaf_count()
    }

    #[must_use]
    pub(crate) fn contains_leaf(&self, pane_id: PaneId) -> bool {
        self.root.leaf_path(pane_id).is_some()
    }

    #[must_use]
    pub(crate) fn leaf_ids(&self) -> Vec<PaneId> {
        let mut leaves = Vec::new();
        self.root
            .visit_leaves(&mut |pane_id, _| leaves.push(pane_id));
        leaves
    }

    #[must_use]
    pub(crate) fn size(&self) -> TerminalSize {
        TerminalSize {
            cols: self.root.geometry.width.min(u32::from(u16::MAX)) as u16,
            rows: self.root.geometry.height.min(u32::from(u16::MAX)) as u16,
        }
    }

    pub(crate) fn apply_to_panes(&self, panes: &mut [Pane]) {
        debug_assert_eq!(self.leaf_count(), panes.len());
        self.apply_to_matching_panes(panes);
    }

    pub(crate) fn apply_to_matching_panes(&self, panes: &mut [Pane]) {
        let mut by_id: std::collections::HashMap<_, _> =
            panes.iter_mut().map(|pane| (pane.id(), pane)).collect();
        self.root.visit_leaves(&mut |id, geometry| {
            let pane = by_id
                .remove(&id)
                .expect("layout leaves must uniquely identify live panes");
            pane.set_geometry(PaneGeometry::new(
                geometry.x.min(u32::from(u16::MAX)) as u16,
                geometry.y.min(u32::from(u16::MAX)) as u16,
                geometry.width.min(u32::from(u16::MAX)) as u16,
                geometry.height.min(u32::from(u16::MAX)) as u16,
            ));
        });
    }

    /// Reads the occupied cell under its resolved pane identity.
    pub(crate) fn pane_geometry(&self, pane_id: PaneId) -> Option<PaneGeometry> {
        let mut result = None;
        self.root.visit_leaves(&mut |id, geometry| {
            if id == pane_id {
                result = Some(PaneGeometry::new(
                    geometry.x.min(u32::from(u16::MAX)) as u16,
                    geometry.y.min(u32::from(u16::MAX)) as u16,
                    geometry.width.min(u32::from(u16::MAX)) as u16,
                    geometry.height.min(u32::from(u16::MAX)) as u16,
                ));
            }
        });
        result
    }

    /// Reassign occupied cells for explicit swap/rotation operations.
    pub(crate) fn remap_ids(&mut self, replacements: &[(PaneId, PaneId)]) {
        self.root.map_ids(&mut |id| {
            replacements
                .iter()
                .find(|(old, _)| *old == id)
                .map_or(id, |(_, new)| *new)
        });
    }

    #[must_use]
    pub(crate) fn dump(&self) -> String {
        let body = self.root.dump_body();
        format!("{:04x},{}", layout_checksum(&body), body)
    }

    pub(crate) fn snapshot_cell(&self) -> Result<RmuxWindowSnapshotCell, RmuxError> {
        snapshot_cell_from_layout(&self.root)
    }

    pub(crate) fn from_snapshot_cell_v2(cell: &RmuxWindowSnapshotCell) -> Result<Self, RmuxError> {
        let root = layout_cell_from_snapshot(cell)?;
        if root.geometry.x != 0
            || root.geometry.y != 0
            || !root.check()
            || !snapshot_offsets_match(&root)
        {
            return Err(invalid_rmux_window_snapshot(
                "canonical tiled content is inconsistent",
            ));
        }
        Ok(Self { root })
    }

    pub(crate) fn from_snapshot_cell(
        cell: &RmuxWindowSnapshotCell,
        size: TerminalSize,
    ) -> Result<Self, RmuxError> {
        let root = layout_cell_from_snapshot(cell)?;
        if root.geometry.x != 0
            || root.geometry.y != 0
            || root.geometry.width != u32::from(size.cols)
            || root.geometry.height != u32::from(size.rows)
        {
            return Err(invalid_rmux_window_snapshot(
                "tiled root geometry does not match window size",
            ));
        }
        if !root.check() || !snapshot_offsets_match(&root) {
            return Err(invalid_rmux_window_snapshot(
                "tiled tree geometry is inconsistent",
            ));
        }
        Ok(Self { root })
    }
}

fn snapshot_cell_from_layout(cell: &LayoutCell) -> Result<RmuxWindowSnapshotCell, RmuxError> {
    let cols = snapshot_u16(cell.geometry.width, "layout width")?;
    let rows = snapshot_u16(cell.geometry.height, "layout height")?;
    let x = snapshot_u16(cell.geometry.x, "layout x")?;
    let y = snapshot_u16(cell.geometry.y, "layout y")?;
    match &cell.kind {
        cell::LayoutKind::Pane(pane_id) => Ok(RmuxWindowSnapshotCell::Pane {
            pane_id: *pane_id,
            cols,
            rows,
            x,
            y,
        }),
        cell::LayoutKind::Split(direction, children) => Ok(RmuxWindowSnapshotCell::Split {
            direction: direction.to_snapshot_direction(),
            cols,
            rows,
            x,
            y,
            children: children
                .iter()
                .map(snapshot_cell_from_layout)
                .collect::<Result<Vec<_>, _>>()?,
        }),
    }
}

fn snapshot_u16(value: u32, field: &str) -> Result<u16, RmuxError> {
    u16::try_from(value).map_err(|_| {
        invalid_rmux_window_snapshot(format!("{field} exceeds snapshot coordinate range"))
    })
}

fn layout_cell_from_snapshot(cell: &RmuxWindowSnapshotCell) -> Result<LayoutCell, RmuxError> {
    match cell {
        RmuxWindowSnapshotCell::Pane {
            pane_id,
            cols,
            rows,
            x,
            y,
        } => {
            let geometry = snapshot_geometry(*cols, *rows, *x, *y)?;
            Ok(LayoutCell::pane(*pane_id, geometry))
        }
        RmuxWindowSnapshotCell::Split {
            direction,
            cols,
            rows,
            x,
            y,
            children,
        } => {
            if children.is_empty() {
                return Err(invalid_rmux_window_snapshot(
                    "split cells must contain at least one child",
                ));
            }
            let geometry = snapshot_geometry(*cols, *rows, *x, *y)?;
            Ok(LayoutCell::split(
                LayoutDirection::from_snapshot_direction(*direction),
                geometry,
                children
                    .iter()
                    .map(layout_cell_from_snapshot)
                    .collect::<Result<Vec<_>, _>>()?,
            ))
        }
    }
}

fn snapshot_geometry(cols: u16, rows: u16, x: u16, y: u16) -> Result<LayoutGeometry, RmuxError> {
    if cols == 0 || rows == 0 {
        return Err(invalid_rmux_window_snapshot(
            "tiled cells must have positive dimensions",
        ));
    }
    Ok(LayoutGeometry::new(
        u32::from(cols),
        u32::from(rows),
        u32::from(x),
        u32::from(y),
    ))
}

fn snapshot_offsets_match(cell: &LayoutCell) -> bool {
    match &cell.kind {
        cell::LayoutKind::Pane(_) => true,
        cell::LayoutKind::Split(direction, children) => {
            let mut cursor = match direction {
                LayoutDirection::LeftRight => cell.geometry.x,
                LayoutDirection::TopBottom => cell.geometry.y,
            };
            for child in children {
                match direction {
                    LayoutDirection::LeftRight => {
                        if child.geometry.x != cursor || child.geometry.y != cell.geometry.y {
                            return false;
                        }
                        cursor = cursor
                            .saturating_add(child.geometry.width)
                            .saturating_add(PANE_BORDER_CELLS);
                    }
                    LayoutDirection::TopBottom => {
                        if child.geometry.x != cell.geometry.x || child.geometry.y != cursor {
                            return false;
                        }
                        cursor = cursor
                            .saturating_add(child.geometry.height)
                            .saturating_add(PANE_BORDER_CELLS);
                    }
                }
                if !snapshot_offsets_match(child) {
                    return false;
                }
            }
            true
        }
    }
}

fn invalid_rmux_window_snapshot(reason: impl Into<String>) -> RmuxError {
    RmuxError::Server(format!("invalid RMUX window snapshot: {}", reason.into()))
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn apply_layout(
    panes: &mut [Pane],
    layout: LayoutName,
    size: TerminalSize,
    requested_main_width: Option<u16>,
) -> LayoutTree {
    let tree = LayoutTree::named(
        layout,
        &panes.iter().map(Pane::id).collect::<Vec<_>>(),
        size,
        LayoutOptions::default().with_requested_main_width(requested_main_width),
    );
    tree.apply_to_panes(panes);
    tree
}

#[must_use]
pub(crate) fn layout_checksum(layout: &str) -> u16 {
    let mut checksum = 0_u16;
    for byte in layout.bytes() {
        checksum = (checksum >> 1) + ((checksum & 1) << 15);
        checksum = checksum.wrapping_add(u16::from(byte));
    }
    checksum
}

fn split_axis(total: u32, count: usize) -> Vec<u32> {
    if count == 0 {
        return Vec::new();
    }
    let usable = total.saturating_sub((count as u32).saturating_sub(1) * PANE_BORDER_CELLS);
    let each = usable / count as u32;
    let remainder = usable - each * count as u32;
    let can_satisfy_minimums = usable >= count as u32;
    let mut sizes = Vec::with_capacity(count);
    for index in 0..count {
        let mut size = each;
        if can_satisfy_minimums && (index as u32) < remainder {
            size += 1;
        } else if !can_satisfy_minimums && index + 1 == count {
            // Preserve the fail-soft geometry used for undersized windows.
            size += remainder;
        }
        sizes.push(size);
    }
    sizes
}

#[cfg(test)]
#[path = "layout/import_tests.rs"]
mod import_tests;

#[cfg(test)]
#[path = "layout/tests.rs"]
mod tests;

#[cfg(test)]
#[path = "layout/tiled_tests.rs"]
mod tiled_tests;
