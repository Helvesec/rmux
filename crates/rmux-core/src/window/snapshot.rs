use std::collections::{BTreeMap, BTreeSet};

use rmux_proto::{
    RmuxError, RmuxFloatingPaneSnapshot, RmuxWindowLayoutMetadata, RmuxWindowResizeTopology,
    RmuxWindowSnapshot, RmuxWindowSnapshotSplitDirection, RmuxWindowSnapshotV2, SessionId,
    WindowTarget, RMUX_WINDOW_SNAPSHOT_V2_VERSION, RMUX_WINDOW_SNAPSHOT_VERSION,
};

use super::Window;
use crate::layout::LayoutTree;
use crate::{FloatingPanePlacement, FloatingPaneRect, FloatingStack, Pane, PaneId};

impl Window {
    /// Captures a typed RMUX snapshot of this non-zoomed canonical window.
    pub fn export_rmux_window_snapshot(
        &self,
        session_id: SessionId,
        target: WindowTarget,
    ) -> Result<RmuxWindowSnapshot, RmuxError> {
        let snapshot = self.export_snapshot_fields(session_id, target)?;
        let tree = snapshot
            .tiled
            .as_ref()
            .map(|cell| LayoutTree::from_snapshot_cell(cell, snapshot.size))
            .transpose()
            .map_err(|_| {
                snapshot_error(
                    "state requires export-window-snapshot-v2 (content differs from viewport)",
                )
            })?;
        let topology = imported_topology(tree.as_ref(), &self.current_floating_stack());
        if topology != self.current_resize_topology() {
            return Err(snapshot_error(
                "state requires export-window-snapshot-v2 (attachment order/root would be lost)",
            ));
        }
        Ok(snapshot)
    }

    fn export_snapshot_fields(
        &self,
        session_id: SessionId,
        target: WindowTarget,
    ) -> Result<RmuxWindowSnapshot, RmuxError> {
        if self.is_zoomed() {
            return Err(snapshot_error("zoomed windows cannot be exported"));
        }
        let active_pane = self
            .active_pane()
            .map(Pane::id)
            .ok_or_else(|| snapshot_error("active pane is missing"))?;
        let last_pane = self
            .last_pane
            .and_then(|pane_index| self.pane_id(pane_index));
        let tiled = self
            .current_layout_tree()
            .map(|tree| tree.snapshot_cell())
            .transpose()?;
        Ok(RmuxWindowSnapshot {
            version: RMUX_WINDOW_SNAPSHOT_VERSION,
            session_id,
            target,
            window_id: self.id,
            size: self.size,
            zoomed: false,
            tiled,
            floating: self
                .current_floating_stack()
                .placements()
                .iter()
                .map(|placement| {
                    let rect = placement.rect();
                    RmuxFloatingPaneSnapshot {
                        pane_id: placement.pane_id(),
                        x: rect.x(),
                        y: rect.y(),
                        cols: rect.cols(),
                        rows: rect.rows(),
                        visible: placement.visible(),
                    }
                })
                .collect(),
            active_pane,
            last_pane,
            runtimes: Vec::new(),
            metadata: RmuxWindowLayoutMetadata {
                layout: self.layout,
                last_layout: self.last_layout,
                custom_layout: self.custom_layout,
                requested_main_width: self.requested_main_width,
                requested_main_height: self.requested_main_height,
            },
        })
    }

    /// Restores a typed RMUX snapshot into the same live canonical window.
    ///
    /// The returned pane ids are the only panes whose effective terminal size
    /// changed and therefore need a runtime PTY resize.
    pub fn restore_rmux_window_snapshot(
        &mut self,
        snapshot: &RmuxWindowSnapshot,
    ) -> Result<Vec<PaneId>, RmuxError> {
        let prepared = self.prepare_rmux_window_snapshot_restore(snapshot, None)?;
        self.commit_snapshot_restore(snapshot, prepared)
    }

    fn commit_snapshot_restore(
        &mut self,
        snapshot: &RmuxWindowSnapshot,
        prepared: PreparedWindowSnapshotRestore,
    ) -> Result<Vec<PaneId>, RmuxError> {
        self.layout_tree = prepared.layout_tree;
        self.floating_stack = prepared.floating_stack;
        self.resize_topology = prepared.resize_topology;
        self.layout = snapshot.metadata.layout;
        self.last_layout = snapshot.metadata.last_layout;
        self.custom_layout = snapshot.metadata.custom_layout;
        self.requested_main_width = snapshot.metadata.requested_main_width;
        self.requested_main_height = snapshot.metadata.requested_main_height;
        self.apply_layout_tree();
        self.set_selection_by_pane_ids(prepared.active_pane, prepared.last_pane)?;
        Ok(prepared.resized_panes)
    }

    fn prepare_rmux_window_snapshot_restore(
        &mut self,
        snapshot: &RmuxWindowSnapshot,
        topology: Option<&RmuxWindowResizeTopology>,
    ) -> Result<PreparedWindowSnapshotRestore, RmuxError> {
        self.reconcile_borrowed_pane_owners();
        let expected_version = if topology.is_some() {
            RMUX_WINDOW_SNAPSHOT_V2_VERSION
        } else {
            RMUX_WINDOW_SNAPSHOT_VERSION
        };
        if snapshot.version != expected_version {
            return Err(snapshot_error(format!(
                "unsupported snapshot version {}",
                snapshot.version
            )));
        }
        if snapshot.zoomed || self.is_zoomed() {
            return Err(snapshot_error("zoomed windows cannot be restored"));
        }
        if snapshot.window_id != self.id {
            return Err(snapshot_error("snapshot window id does not match target"));
        }
        if snapshot.size != self.size {
            return Err(snapshot_error("snapshot size does not match target"));
        }

        let layout_tree = snapshot
            .tiled
            .as_ref()
            .map(|cell| {
                if topology.is_some() {
                    LayoutTree::from_snapshot_cell_v2(cell)
                } else {
                    LayoutTree::from_snapshot_cell(cell, self.size)
                }
            })
            .transpose()?;
        let tiled_pane_ids = layout_tree
            .as_ref()
            .map_or_else(Vec::new, LayoutTree::leaf_ids);
        let floating_stack = floating_stack_from_snapshot(&snapshot.floating)?;
        let floating_pane_ids = floating_stack
            .placements()
            .iter()
            .map(|placement| placement.pane_id())
            .collect::<Vec<_>>();

        validate_snapshot_membership(self, &tiled_pane_ids, &floating_pane_ids)?;
        validate_snapshot_selection(self, snapshot.active_pane, snapshot.last_pane)?;
        let resize_topology = match topology {
            None => imported_topology(layout_tree.as_ref(), &floating_stack),
            Some(topology) => {
                let ids = topology
                    .floating_attachment_order
                    .iter()
                    .copied()
                    .collect::<BTreeSet<_>>();
                let floats = floating_pane_ids.iter().copied().collect::<BTreeSet<_>>();
                if ids != floats
                    || ids.len() != topology.floating_attachment_order.len()
                    || topology.root_direction.is_some() == floating_stack.is_empty()
                {
                    return Err(snapshot_error(
                        "attachment topology is inconsistent with canonical membership",
                    ));
                }
                for placement in floating_stack.placements() {
                    let rect = placement.rect();
                    if rect.cols() > i32::from(u16::MAX)
                        || rect.rows() > i32::from(u16::MAX)
                        || rect.x().checked_add(rect.cols()).is_none()
                        || rect.y().checked_add(rect.rows()).is_none()
                    {
                        return Err(snapshot_error(
                            "floating geometry exceeds representable bounds",
                        ));
                    }
                }
                topology.clone()
            }
        };

        let before_sizes = pane_sizes_by_id(&self.panes);
        let after_sizes =
            snapshot_pane_sizes(self, layout_tree.as_ref(), &floating_stack, &tiled_pane_ids)?;
        let resized_panes = before_sizes
            .into_iter()
            .filter_map(|(pane_id, before)| {
                after_sizes
                    .get(&pane_id)
                    .is_some_and(|after| *after != before)
                    .then_some(pane_id)
            })
            .collect();

        Ok(PreparedWindowSnapshotRestore {
            layout_tree,
            floating_stack,
            resize_topology,
            active_pane: snapshot.active_pane,
            last_pane: snapshot.last_pane,
            resized_panes,
        })
    }

    fn set_selection_by_pane_ids(
        &mut self,
        active_pane: PaneId,
        last_pane: Option<PaneId>,
    ) -> Result<(), RmuxError> {
        self.active_pane = self.pane_index_for_id(active_pane).ok_or_else(|| {
            snapshot_error(format!(
                "active pane id {} does not exist in target",
                active_pane.as_u32()
            ))
        })?;
        self.last_pane = last_pane
            .map(|pane_id| {
                self.pane_index_for_id(pane_id).ok_or_else(|| {
                    snapshot_error(format!(
                        "last pane id {} does not exist in target",
                        pane_id.as_u32()
                    ))
                })
            })
            .transpose()?
            .filter(|pane_index| *pane_index != self.active_pane);
        Ok(())
    }
}

struct PreparedWindowSnapshotRestore {
    layout_tree: Option<LayoutTree>,
    floating_stack: FloatingStack,
    resize_topology: RmuxWindowResizeTopology,
    active_pane: PaneId,
    last_pane: Option<PaneId>,
    resized_panes: Vec<PaneId>,
}

fn floating_stack_from_snapshot(
    floating: &[RmuxFloatingPaneSnapshot],
) -> Result<FloatingStack, RmuxError> {
    FloatingStack::from_placements(
        floating
            .iter()
            .map(|placement| {
                let rect = FloatingPaneRect::new(
                    placement.x,
                    placement.y,
                    placement.cols,
                    placement.rows,
                )?;
                Ok(FloatingPanePlacement::new_with_visibility(
                    placement.pane_id,
                    rect,
                    placement.visible,
                ))
            })
            .collect::<Result<Vec<_>, RmuxError>>()?,
    )
}

fn validate_snapshot_membership(
    window: &Window,
    tiled_pane_ids: &[PaneId],
    floating_pane_ids: &[PaneId],
) -> Result<(), RmuxError> {
    if tiled_pane_ids.is_empty() && floating_pane_ids.is_empty() {
        return Err(snapshot_error("snapshot contains no panes"));
    }

    let mut seen = BTreeSet::new();
    for pane_id in tiled_pane_ids.iter().chain(floating_pane_ids) {
        if !seen.insert(*pane_id) {
            return Err(snapshot_error(format!(
                "pane id {} appears more than once",
                pane_id.as_u32()
            )));
        }
    }
    let current = window.panes.iter().map(Pane::id).collect::<BTreeSet<_>>();
    if seen != current {
        return Err(snapshot_error("snapshot pane set does not match target"));
    }
    Ok(())
}

fn validate_snapshot_selection(
    window: &Window,
    active_pane: PaneId,
    last_pane: Option<PaneId>,
) -> Result<(), RmuxError> {
    let current = window.panes.iter().map(Pane::id).collect::<BTreeSet<_>>();
    if !current.contains(&active_pane) {
        return Err(snapshot_error(
            "active pane is not in the snapshot pane set",
        ));
    }
    if last_pane == Some(active_pane) {
        return Err(snapshot_error("active and last pane must be distinct"));
    }
    if last_pane.is_some_and(|pane_id| !current.contains(&pane_id)) {
        return Err(snapshot_error("last pane is not in the snapshot pane set"));
    }
    Ok(())
}

fn pane_sizes_by_id(panes: &[Pane]) -> BTreeMap<PaneId, (u16, u16)> {
    panes
        .iter()
        .map(|pane| {
            let geometry = pane.geometry();
            (pane.id(), (geometry.cols(), geometry.rows()))
        })
        .collect()
}

fn snapshot_pane_sizes(
    window: &Window,
    layout_tree: Option<&LayoutTree>,
    floating_stack: &FloatingStack,
    tiled_pane_ids: &[PaneId],
) -> Result<BTreeMap<PaneId, (u16, u16)>, RmuxError> {
    let mut sizes = BTreeMap::new();
    for pane_id in tiled_pane_ids {
        let geometry = layout_tree
            .and_then(|tree| tree.pane_geometry(*pane_id))
            .ok_or_else(|| snapshot_error("tiled pane is missing geometry"))?;
        sizes.insert(*pane_id, (geometry.cols(), geometry.rows()));
    }
    for placement in floating_stack.placements() {
        let geometry = placement.rect().pane_geometry();
        sizes.insert(placement.pane_id(), (geometry.cols(), geometry.rows()));
    }
    if sizes.len() != window.panes.len() {
        return Err(snapshot_error("snapshot pane size map is incomplete"));
    }
    Ok(sizes)
}

fn snapshot_error(reason: impl Into<String>) -> RmuxError {
    RmuxError::Server(format!("invalid RMUX window snapshot: {}", reason.into()))
}

#[cfg(test)]
pub(super) fn snapshot_pane_ids(cell: &rmux_proto::RmuxWindowSnapshotCell) -> Vec<PaneId> {
    match cell {
        rmux_proto::RmuxWindowSnapshotCell::Pane { pane_id, .. } => vec![*pane_id],
        rmux_proto::RmuxWindowSnapshotCell::Split { children, .. } => {
            children.iter().flat_map(snapshot_pane_ids).collect()
        }
    }
}

fn imported_topology(tree: Option<&LayoutTree>, stack: &FloatingStack) -> RmuxWindowResizeTopology {
    RmuxWindowResizeTopology {
        root_direction: (!stack.is_empty()).then(|| {
            match tree.and_then(LayoutTree::root_direction) {
                Some(crate::layout::LayoutDirection::LeftRight) => {
                    RmuxWindowSnapshotSplitDirection::LeftRight
                }
                _ => RmuxWindowSnapshotSplitDirection::TopBottom,
            }
        }),
        floating_attachment_order: stack.placements().iter().map(|p| p.pane_id()).collect(),
    }
}
impl Window {
    /// Export the canonical content, viewport and attachment behavior explicitly.
    pub fn export_rmux_window_snapshot_v2(
        &self,
        session_id: SessionId,
        target: WindowTarget,
    ) -> Result<RmuxWindowSnapshotV2, RmuxError> {
        let snapshot = self.export_snapshot_fields(session_id, target)?;
        Ok(RmuxWindowSnapshotV2 {
            version: RMUX_WINDOW_SNAPSHOT_V2_VERSION,
            session_id: snapshot.session_id,
            target: snapshot.target,
            window_id: snapshot.window_id,
            size: snapshot.size,
            zoomed: snapshot.zoomed,
            tiled: snapshot.tiled,
            floating: snapshot.floating,
            active_pane: snapshot.active_pane,
            last_pane: snapshot.last_pane,
            runtimes: snapshot.runtimes,
            metadata: snapshot.metadata,
            resize_topology: self.current_resize_topology(),
        })
    }
    /// Restore V2 after complete geometry, membership and topology validation.
    /// Legacy restore instead imports attachment order from serialized z order.
    pub fn restore_rmux_window_snapshot_v2(
        &mut self,
        snapshot: &RmuxWindowSnapshotV2,
    ) -> Result<Vec<PaneId>, RmuxError> {
        let fields = RmuxWindowSnapshot {
            version: snapshot.version,
            session_id: snapshot.session_id,
            target: snapshot.target.clone(),
            window_id: snapshot.window_id,
            size: snapshot.size,
            zoomed: snapshot.zoomed,
            tiled: snapshot.tiled.clone(),
            floating: snapshot.floating.clone(),
            active_pane: snapshot.active_pane,
            last_pane: snapshot.last_pane,
            runtimes: snapshot.runtimes.clone(),
            metadata: snapshot.metadata.clone(),
        };
        let prepared =
            self.prepare_rmux_window_snapshot_restore(&fields, Some(&snapshot.resize_topology))?;
        self.commit_snapshot_restore(&fields, prepared)
    }
}
