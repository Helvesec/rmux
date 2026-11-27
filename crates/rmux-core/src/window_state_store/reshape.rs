//! The order of a window's links after a command reshaped ONE session's table
//! and tmux then synchronised that session's group from it.

use std::collections::{BTreeMap, BTreeSet};

use super::{WindowLink, WindowStateStore};
use crate::{SessionId, WindowId};

/// What a command did to one session's own links, in the terms tmux does it,
/// and which peers the group synchronisation then rebuilt from that session.
///
/// Every link of the session that is not named here kept its index, its window
/// and its place in that window's order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionReshape {
    /// The session the command addressed.
    pub session: SessionId,
    /// Links moved to another index WITHOUT being recreated, old index to new:
    /// `winlink_shuffle_up` only re-keys the session's tree (`window.c`), so
    /// each keeps its place in its window's order under the new index.
    pub renamed: BTreeMap<u32, u32>,
    /// Links (by final index) taken out of the window they held, if any, and
    /// appended at the tail of the window they hold now, in this order: a new
    /// winlink, or one whose window `cmd-swap-window.c` replaced.
    pub made: Vec<u32>,
    /// Links (by index before) removed and not made again.
    pub gone: Vec<u32>,
    /// The other members of the group, in the order they joined it, when tmux
    /// synchronised the group from `session` (`session.c:612-626`, `:633-689`);
    /// empty when the command never reached that synchronisation.
    pub peers: Vec<SessionId>,
}

impl SessionReshape {
    /// The session's table this reshape must produce from `before`, or `None`
    /// when the reshape contradicts itself (a link named twice, a rename onto a
    /// slot something else keeps, a made link the final table does not have).
    fn expected_table(
        &self,
        before: &BTreeMap<u32, WindowId>,
        after: &BTreeMap<u32, WindowId>,
    ) -> Option<BTreeMap<u32, WindowId>> {
        let mut seen = BTreeSet::new();
        if !self
            .renamed
            .keys()
            .chain(&self.gone)
            .all(|i| seen.insert(*i))
            || !self
                .renamed
                .keys()
                .chain(&self.gone)
                .all(|i| before.contains_key(i))
        {
            return None;
        }
        let mut expected = before
            .iter()
            .filter(|(i, _)| !seen.contains(i))
            .map(|(i, id)| (*i, *id))
            .collect::<BTreeMap<_, _>>();
        let mut renamed_to = BTreeSet::new();
        for (old, new) in &self.renamed {
            if !renamed_to.insert(*new) || expected.insert(*new, before[old]).is_some() {
                return None;
            }
        }
        let mut made = BTreeSet::new();
        for index in &self.made {
            if !made.insert(*index) || renamed_to.contains(index) {
                return None;
            }
            // A made link may take over a slot whose link was kept: that link
            // is the one it replaced.
            expected.insert(*index, *after.get(index)?);
        }
        Some(expected)
    }
}

impl WindowStateStore {
    /// Is this reshape what the replacement really did: the session's table
    /// is exactly the one it describes, and every other replaced table is that
    /// same table, with `peers` naming all of them or none?
    pub(super) fn reshape_is_borne_out(
        &self,
        before: &BTreeMap<WindowLink, WindowId>,
        tables: &BTreeMap<SessionId, BTreeMap<u32, WindowId>>,
        reshape: &SessionReshape,
    ) -> bool {
        let Some(after) = tables.get(&reshape.session) else {
            return false;
        };
        let others = tables
            .keys()
            .filter(|id| **id != reshape.session)
            .copied()
            .collect::<BTreeSet<_>>();
        let peers = reshape.peers.iter().copied().collect::<BTreeSet<_>>();
        if peers.len() != reshape.peers.len() || !(peers.is_empty() || peers == others) {
            return false;
        }
        if others.iter().any(|other| tables[other] != *after) {
            return false;
        }
        let old = session_table(before, reshape.session);
        reshape.expected_table(&old, after).as_ref() == Some(after)
    }

    /// Applies a validated reshape to the order of every window it touched:
    /// the gone and replaced links leave, the renamed ones keep their place
    /// under their new index, the made ones go at the tail, and then every
    /// peer is rebuilt at the tail. A window whose order is not known stays
    /// not known.
    pub(super) fn apply_reshape(
        &mut self,
        before: &BTreeMap<WindowLink, WindowId>,
        touched: &BTreeSet<WindowId>,
        tables: &BTreeMap<SessionId, BTreeMap<u32, WindowId>>,
        reshape: &SessionReshape,
    ) {
        let session_id = reshape.session;
        let old = session_table(before, session_id);
        let left = reshape.gone.iter().chain(
            reshape
                .made
                .iter()
                .filter(|index| !reshape.renamed.contains_key(index)),
        );
        for &window_index in left {
            let link = WindowLink {
                session_id,
                window_index,
            };
            if let Some(order) = old.get(&window_index).and_then(|id| self.order.get_mut(id)) {
                if let Some(at) = order.iter().position(|entry| *entry == link) {
                    order.remove(at);
                }
            }
        }
        for id in touched {
            if let Some(order) = self.order.get_mut(id) {
                for entry in order.iter_mut() {
                    if entry.session_id == session_id {
                        if let Some(new) = reshape.renamed.get(&entry.window_index) {
                            entry.window_index = *new;
                        }
                    }
                }
            }
        }
        let after = &tables[&session_id];
        for &window_index in &reshape.made {
            if let Some(order) = self.order.get_mut(&after[&window_index]) {
                order.push(WindowLink {
                    session_id,
                    window_index,
                });
            }
        }
        self.rebuild_peer_links_at_tail(touched, tables, &reshape.peers);
    }
}

fn session_table(
    links: &BTreeMap<WindowLink, WindowId>,
    session_id: SessionId,
) -> BTreeMap<u32, WindowId> {
    links
        .iter()
        .filter(|(link, _)| link.session_id == session_id)
        .map(|(link, id)| (link.window_index, *id))
        .collect()
}
