//! The order each window's links were made in: the plan a link-table
//! replacement states, its validation against the tables, and the group
//! synchronisation that rebuilds every peer's links at the tail.

use std::collections::{BTreeMap, BTreeSet};

use super::{SessionReshape, WindowLink, WindowStateStore};
use crate::{SessionId, WindowId};

/// What a wholesale link-table replacement really DID, for the order of the
/// windows it touches.
///
/// `replace_session_links` swaps complete tables, and the tables alone cannot
/// say what produced them: a chronology must never be deduced from how many
/// links turned up. So the caller states its plan, this store validates that
/// plan against the links before and after, and anything else - including a
/// replacement whose final tables are identical - leaves every window it
/// touched without an order its reader may trust.
///
/// Removing a link is not here: that is `unlink`, whose own semantics already
/// take the occurrence that went and leave the others where they are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LinkOrderPlan {
    /// Exactly this link was made in its session, and whatever window held
    /// that slot before, if any, lost it. That link goes at the TAIL of its
    /// window's order - which is where tmux puts a winlink it has just created
    /// (`window.c:183-193`).
    ///
    /// Then every peer of that session's group, in the order the peers joined
    /// it, is rebuilt from the session's new table: tmux synchronises the group
    /// from the session it has just changed, which empties each peer's winlink
    /// list and creates it again in index order (`session.c:612-626`,
    /// `:633-689`), so each recreated winlink is appended at the tail of its
    /// own window's order. A session in no group has no peers, and every other
    /// link keeps the position it had.
    LinkMade {
        made: WindowLink,
        peers: Vec<SessionId>,
    },
    /// One session's own links were made, moved, renamed in place or removed
    /// exactly as the reshape says, and its group was then synchronised from
    /// it, if the reshape names peers.
    Reshaped(SessionReshape),
    /// Anything else. The links of every window this replacement touches were
    /// rebuilt by something this store does not model - a renumber, for one -
    /// so their order becomes unknown and their reader falls back.
    Rebuilt,
}

impl WindowStateStore {
    /// The order this window's links were made in, when this store knows it.
    ///
    /// `None` is not "no links": it is an order this store cannot vouch for, and
    /// its reader must fall back to whatever it did before rather than invent a
    /// chronology.
    #[must_use]
    pub fn window_link_order(&self, id: WindowId) -> Option<&[WindowLink]> {
        self.order.get(&id).map(Vec::as_slice)
    }

    /// Forgets the order of these windows: their links were rebuilt by
    /// something this store does not model, so their reader must fall back
    /// instead of being handed a chronology that is no longer true.
    pub(crate) fn forget_link_order(&mut self, ids: impl IntoIterator<Item = WindowId>) {
        for id in ids {
            self.order.remove(&id);
        }
    }

    /// Applies the caller's plan to the windows this replacement touched, after
    /// validating it against the links before and after.
    ///
    /// A plan the tables do not bear out is not trusted: those windows lose
    /// their order, which is exactly what an unsupported replacement does
    /// anyway. Nothing is ever taken from the iteration order of the link map.
    pub(super) fn apply_link_order_plan(
        &mut self,
        before: &BTreeMap<WindowLink, WindowId>,
        touched: &BTreeSet<WindowId>,
        tables: &BTreeMap<SessionId, BTreeMap<u32, WindowId>>,
        plan: LinkOrderPlan,
    ) {
        if let LinkOrderPlan::Reshaped(reshape) = &plan {
            if self.reshape_is_borne_out(before, tables, reshape) {
                self.apply_reshape(before, touched, tables, reshape);
                return;
            }
            debug_assert!(false, "a reshape the replaced tables do not bear out");
        }
        if let LinkOrderPlan::LinkMade { made, peers } = plan {
            if self.link_made_is_borne_out(before, tables, made, &peers) {
                // The window that held this slot, if it was another one, loses
                // that name and keeps the rest where they are.
                if let Some(previous) = before.get(&made).copied() {
                    if let Some(order) = self.order.get_mut(&previous) {
                        if let Some(at) = order.iter().position(|entry| *entry == made) {
                            order.remove(at);
                        }
                    }
                }
                // The window that has it now takes it at the tail.
                let id = self.links[&made];
                if let Some(order) = self.order.get_mut(&id) {
                    order.push(made);
                }
                self.rebuild_peer_links_at_tail(touched, tables, &peers);
                return;
            }
            debug_assert!(
                false,
                "a link-made plan the replaced tables do not bear out"
            );
        }
        for id in touched {
            self.order.remove(id);
        }
    }

    /// A member of a group lost a link and survived: tmux synchronises the
    /// group from it (`session_detach`, `session.c:340-358`), so every peer's
    /// links leave their windows' order and come back at the tail, peer after
    /// peer in `peers` order, each in index order; `source`'s own links and
    /// every outsider's keep their positions.
    ///
    /// Each peer is rebuilt from its own CURRENT table, so the order never
    /// names a link the store does not have. A group detach that removes the
    /// peers' copies of the slot one call at a time therefore lands, after its
    /// last call, exactly where tmux's single synchronisation does: applying
    /// this again from the same source moves nothing.
    pub(crate) fn synchronise_group_from(&mut self, source: SessionId, peers: &[SessionId]) {
        let in_group = |session_id: SessionId| session_id == source || peers.contains(&session_id);
        let touched = self
            .links
            .iter()
            .filter(|(link, _)| in_group(link.session_id))
            .map(|(_, id)| *id)
            .collect::<BTreeSet<_>>();
        let tables = peers
            .iter()
            .map(|&peer| {
                let table = self
                    .links
                    .iter()
                    .filter(|(link, _)| link.session_id == peer)
                    .map(|(link, id)| (link.window_index, *id))
                    .collect::<BTreeMap<_, _>>();
                (peer, table)
            })
            .collect::<BTreeMap<_, _>>();
        self.rebuild_peer_links_at_tail(&touched, &tables, peers);
    }

    /// Every peer's links leave the order of the windows this replacement
    /// touched and come back at the tail, peer after peer, each in index order.
    /// A window whose order is not known stays not known.
    pub(super) fn rebuild_peer_links_at_tail(
        &mut self,
        touched: &BTreeSet<WindowId>,
        tables: &BTreeMap<SessionId, BTreeMap<u32, WindowId>>,
        peers: &[SessionId],
    ) {
        if peers.is_empty() {
            return;
        }
        for id in touched {
            if let Some(order) = self.order.get_mut(id) {
                order.retain(|link| !peers.contains(&link.session_id));
            }
        }
        for &session_id in peers {
            for (&window_index, id) in &tables[&session_id] {
                if let Some(order) = self.order.get_mut(id) {
                    order.push(WindowLink {
                        session_id,
                        window_index,
                    });
                }
            }
        }
    }

    /// Is `made` the ONE link its session's table gained or changed - whether
    /// it was vacant before or named another window - and is every peer, and
    /// nothing else, replaced with exactly that session's new table?
    fn link_made_is_borne_out(
        &self,
        before: &BTreeMap<WindowLink, WindowId>,
        tables: &BTreeMap<SessionId, BTreeMap<u32, WindowId>>,
        made: WindowLink,
        peers: &[SessionId],
    ) -> bool {
        let Some(source) = tables.get(&made.session_id) else {
            return false;
        };
        if !self.links.contains_key(&made) {
            return false;
        }
        let replaced_sessions = tables.len() == peers.len() + 1
            && peers.iter().all(|peer| {
                *peer != made.session_id && tables.get(peer).is_some_and(|table| table == source)
            });
        if !replaced_sessions {
            return false;
        }
        let scoped = |links: &BTreeMap<WindowLink, WindowId>| {
            links
                .iter()
                .filter(|(link, _)| link.session_id == made.session_id && **link != made)
                .map(|(link, id)| (*link, *id))
                .collect::<BTreeMap<_, _>>()
        };
        scoped(before) == scoped(&self.links)
    }
}
