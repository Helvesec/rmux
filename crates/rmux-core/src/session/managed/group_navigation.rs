use super::index_state::{identity_active, identity_map, Links};
use super::*;

/// Navigation identities before a compound group index operation. Contains no
/// window owners, pane state, or session mutation capability.
///
/// It is taken before the first mutation and restored exactly as it was taken:
/// a peer is resynchronised by its own indices, so no map of what the addressed
/// session moved in between belongs here.
pub struct ManagedGroupNavigation {
    peers: Vec<PeerNavigation>,
}

struct PeerNavigation {
    name: SessionName,
    id: SessionId,
    links: Links,
    active: u32,
    last: Option<u32>,
    stack: Vec<u32>,
}

impl ManagedSessionStore {
    /// Captures only the other group members' navigation before opening a
    /// relative insertion slot. Their final selection must resolve against the
    /// original identity table, not the temporary shifted table.
    pub fn group_peer_navigation(
        &self,
        source: &SessionName,
    ) -> Result<ManagedGroupNavigation, RmuxError> {
        let mut peers = Vec::new();
        for name in self.session_group_members(source) {
            if &name == source {
                continue;
            }
            let session = &self.sessions[&name];
            peers.push(PeerNavigation {
                links: self.index_links(&name)?,
                id: session.id,
                active: session.active_window,
                last: session.last_window,
                stack: session.window_stack.clone(),
                name,
            });
        }
        Ok(ManagedGroupNavigation { peers })
    }

    /// Resolves peer navigation against final links after a compound operation.
    /// Occurrence alerts already follow the composed moves and are untouched.
    pub fn reconcile_group_peer_navigation(
        &mut self,
        navigation: &ManagedGroupNavigation,
    ) -> Result<(), RmuxError> {
        let mut staged = Vec::new();
        for peer in &navigation.peers {
            let session = self
                .sessions
                .get(&peer.name)
                .filter(|session| session.id == peer.id)
                .ok_or_else(|| RmuxError::SessionNotFound(peer.name.to_string()))?;
            let links = self.index_links(&peer.name)?;
            let active = identity_active(&peer.links, &links, peer.active, peer.last);
            // The compound ends in the same resynchronisation as a single
            // mutation (`session.c:663-678`): the peer's visits stay on the slots
            // it knew, the ones those mutations removed go away, and the head of
            // what is left is its return.
            let stack = crate::session::visit_history::carry_occurrences(
                &peer.stack,
                &identity_map(&links),
                active,
                |slot| links.contains_key(&slot),
            );
            let last = crate::session::visit_history::live_head(&stack, active);
            staged.push((session.name.clone(), active, last, stack));
        }
        for (name, active, last, stack) in staged {
            let session = self.sessions.get_mut(&name).expect("validated peer");
            session.active_window = active;
            session.last_window = last;
            session.window_stack = stack;
        }
        Ok(())
    }
}
