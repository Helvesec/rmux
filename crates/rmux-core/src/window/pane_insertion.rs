//! Where an incoming pane lands in a tiled layout.
//!
//! A transfer needs three facts that the old positional helper conflated:
//! which tiled cell is split, where the moved pane sits in the window's pane
//! list, and which side of the split it takes. tmux 3.7b decides them
//! separately — the destination names the split, `-b` names only the
//! geometric side, and the pane list always reinserts immediately after the
//! destination — so they are three parameters here, never derived from one
//! another.
//!
//! Planning is separated from committing for the same reason a transfer must
//! be atomic: the caller may hold a second window and is about to unzoom and
//! extract. Producing a [`PaneInsertionPlan`] proves the split is possible, so
//! a refusal reaches the caller while both windows are still untouched, and a
//! failed split is never approximated by rebuilding a named layout.

use super::{panes::layout_for_split, Window};
use crate::layout::{LayoutDirection, LayoutTree};
use crate::{Pane, PaneId};
use rmux_proto::{RmuxError, SplitDirection};

/// The tiled cell an incoming pane splits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PaneInsertionTarget {
    /// Split the named tiled leaf: `join-pane`/`move-pane` without `-f`.
    Leaf(PaneId),
    /// Split the window root: `join-pane`/`move-pane` with `-f`.
    WindowRoot,
    /// The window owns no tiled leaf, so the incoming pane becomes the only one.
    /// A transfer never reaches this case — it refuses a destination that is not
    /// a tiled leaf — so only the window-level insertion tests construct it.
    #[cfg_attr(not(test), allow(dead_code))]
    SoleLeaf,
}

/// A validated insertion into one window.
///
/// The plan carries no geometry: it records the decision and the proof that
/// the split succeeds, and the tree is recomputed at commit time from whatever
/// the window owns then. That matters for a same-window full-size transfer,
/// where the source leaves the tree between the two steps.
#[derive(Debug, Clone, Copy)]
#[must_use]
pub(crate) struct PaneInsertionPlan {
    target: PaneInsertionTarget,
    direction: SplitDirection,
    before: bool,
}

impl Window {
    /// Validates an insertion without changing any window state.
    ///
    /// `WindowRoot` is exact even when the caller extracts a pane from this
    /// same window afterwards: `LayoutTree::split_root` reads only the root
    /// rectangle, and removing a leaf preserves it — `collapse_single_child`
    /// copies the parent geometry into the surviving child, and a window
    /// reduced to a single pane is rebuilt at `self.size`.
    /// Only the incoming identity is needed: uniqueness of the pane id and
    /// index belongs to the caller, because a same-window full-size transfer
    /// validates while its own source is still in the window.
    pub(crate) fn plan_pane_insertion(
        &self,
        incoming: PaneId,
        target: PaneInsertionTarget,
        direction: SplitDirection,
        before: bool,
    ) -> Result<PaneInsertionPlan, RmuxError> {
        let plan = PaneInsertionPlan {
            target,
            direction,
            before,
        };
        let layout_direction = LayoutDirection::from_split_direction(direction);
        match target {
            PaneInsertionTarget::SoleLeaf => {
                if !self.tiled_pane_ids().is_empty() {
                    return Err(RmuxError::Server(
                        "cannot insert a sole tiled pane into a window that already has one"
                            .to_owned(),
                    ));
                }
            }
            PaneInsertionTarget::Leaf(target_pane_id) => {
                if !self.is_tiled_pane_id(target_pane_id) {
                    return Err(RmuxError::Server(
                        "cannot split a floating target pane".to_owned(),
                    ));
                }
                let mut next = self
                    .current_layout_tree()
                    .ok_or_else(|| {
                        RmuxError::Server("cannot split a missing layout tree".to_owned())
                    })?
                    .into_owned();
                if !next.split_leaf(target_pane_id, incoming, layout_direction, before) {
                    return Err(RmuxError::Server(format!(
                        "cannot split target pane id {}",
                        target_pane_id.as_u32()
                    )));
                }
            }
            PaneInsertionTarget::WindowRoot => {
                let Some(tree) = self.current_layout_tree() else {
                    // No tiled tree to split; the commit installs a sole leaf.
                    return Ok(plan);
                };
                let mut next = tree.into_owned();
                if !next.split_root(incoming, layout_direction, before) {
                    return Err(RmuxError::Server(
                        "cannot split the window: no room for another pane".to_owned(),
                    ));
                }
            }
        }
        Ok(plan)
    }

    /// Installs a validated plan at an explicit list position.
    ///
    /// The list position and the split side are independent: `-b` moves the
    /// pane to the other side of the same destination, never to another index.
    pub(crate) fn commit_pane_insertion(
        &mut self,
        plan: PaneInsertionPlan,
        list_position: usize,
        pane: Pane,
    ) -> Result<(), RmuxError> {
        self.reconcile_borrowed_pane_owners();
        if list_position > self.panes.len() {
            return Err(RmuxError::Server(format!(
                "cannot insert pane at position {list_position} in a {}-pane window",
                self.panes.len()
            )));
        }
        self.ensure_accepts_pane(&pane, None)?;
        self.auto_unzoom();
        self.layout = layout_for_split(plan.direction);
        self.bump_next_pane_index(pane.index());
        let inserted_index = pane.index();
        let inserted_id = pane.id();
        self.panes.insert(list_position, pane);

        let layout_direction = LayoutDirection::from_split_direction(plan.direction);
        let sole_leaf = matches!(plan.target, PaneInsertionTarget::SoleLeaf);
        let installed = if sole_leaf || self.layout_tree.is_none() {
            self.layout_tree = Some(LayoutTree::single(self.size, inserted_id));
            if sole_leaf {
                self.active_pane = inserted_index;
                self.last_pane = None;
            }
            true
        } else {
            match plan.target {
                PaneInsertionTarget::SoleLeaf => unreachable!("handled above"),
                PaneInsertionTarget::Leaf(target_pane_id) => {
                    self.layout_tree.as_mut().is_some_and(|tree| {
                        tree.split_leaf(target_pane_id, inserted_id, layout_direction, plan.before)
                    })
                }
                PaneInsertionTarget::WindowRoot => self.layout_tree.as_mut().is_some_and(|tree| {
                    tree.split_root(inserted_id, layout_direction, plan.before)
                }),
            }
        };
        if !installed {
            // A transfer never falls back to a rebuilt named layout: the plan
            // proved this split against the same rectangle, so reaching here
            // means the window changed underneath and the pane must not be
            // left half-inserted.
            self.panes.remove(list_position);
            return Err(RmuxError::Server(
                "validated pane insertion no longer applies to this window".to_owned(),
            ));
        }
        self.apply_layout_tree();
        Ok(())
    }

    /// Position immediately after `target_pane_id` in the pane list.
    ///
    /// tmux reinserts a joined or moved pane right after its destination in the
    /// window's pane list, whatever `-b` and `-f` say, so this is the one list
    /// rule both transfer directions use.
    pub(crate) fn list_position_after(&self, target_pane_id: PaneId) -> Option<usize> {
        self.panes
            .iter()
            .position(|pane| pane.id() == target_pane_id)
            .map(|position| position + 1)
    }
}
