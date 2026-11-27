use super::*;
use crate::window_state_store::{LinkOrderPlan, SessionReshape};

impl ManagedSessionStore {
    /// The order plan of a command that reshaped `name`'s own links.
    ///
    /// When the command reached `session_group_synchronize_from(name)`, tmux
    /// then empties and recreates every other member's winlinks, in the order
    /// the members joined the group (`session.c:612-626`, `:633-689`). A member
    /// joins only when it is created, so that is the order of the identities.
    pub(super) fn reshape_plan(
        &self,
        name: &SessionName,
        renamed: BTreeMap<u32, u32>,
        made: Vec<u32>,
        gone: Vec<u32>,
        synchronised: bool,
    ) -> LinkOrderPlan {
        let Some(session) = self.sessions.get(name) else {
            return LinkOrderPlan::Rebuilt;
        };
        let mut peers = Vec::new();
        if synchronised {
            for member in self.session_group_members(name) {
                match self.sessions.get(&member) {
                    Some(peer) if peer.id == session.id => {}
                    Some(peer) => peers.push(peer.id),
                    None => return LinkOrderPlan::Rebuilt,
                }
            }
            peers.sort_unstable();
        }
        LinkOrderPlan::Reshaped(SessionReshape {
            session: session.id,
            renamed,
            made,
            gone,
            peers,
        })
    }
}
