use super::index_state::{identity_map, IndexMaps, Links, LocalState};
use super::*;

pub(super) fn exhausted(name: &SessionName) -> RmuxError {
    RmuxError::Server(format!("window index space exhausted for session {name}"))
}

pub(super) fn occupied(name: &SessionName, index: u32) -> RmuxError {
    super::super::target_error::invalid_window_target_with_reason(
        name,
        index,
        "window index already exists in session",
    )
}

pub(super) fn first_gap(links: &Links, name: &SessionName, minimum: u32) -> Result<u32, RmuxError> {
    let mut index = minimum;
    while links.contains_key(&index) {
        index = index.checked_add(1).ok_or_else(|| exhausted(name))?;
    }
    Ok(index)
}

impl ManagedSessionStore {
    /// Opens an insertion slot atomically, using descending standalone move
    /// tracking for the addressed session and explicit occurrence maps for peers.
    pub fn make_room_for_window(
        &mut self,
        name: &SessionName,
        index: u32,
    ) -> Result<ManagedIndexChange, RmuxError> {
        let mut links = self.index_links(name)?;
        let gap = first_gap(&links, name, index)?;
        let mut occurrences = identity_map(&links);
        let mut local = LocalState::from_session(&self.sessions[name]);
        for source in (index..gap).rev() {
            let destination = source + 1; // bounded by the checked gap
            let id = links.remove(&source).expect("contiguous occupied range");
            links.insert(destination, id);
            occurrences.insert(source, destination);
            local.shift_slot(&links, source, destination);
        }
        // The shift moves the addressed session's own links, but a peer is
        // resynchronised by its indices alone (`session.c:663-678`), so its
        // visits stay on the slots it knew.
        let history = identity_map(&links);
        self.commit_indices(
            name,
            links,
            IndexMaps {
                occurrences: occurrences.clone(),
                history,
                // Opening a slot moves the links above it, which tmux does by
                // recreating them (`session.c:691-746` for the renumber this
                // serves, `window.c:183-193` for where each recreated winlink
                // goes). No order survives that here.
                link_order: crate::window_state_store::LinkOrderPlan::Rebuilt,
            },
            local,
            Some(&occurrences),
            None,
        )
    }

    /// Moves one occurrence within a session/group. Source selection follows
    /// standalone detach/select rules; peers follow the existing identity rules.
    pub fn move_window(
        &mut self,
        name: &SessionName,
        source: u32,
        destination: u32,
        kill_destination: bool,
        select_destination: bool,
    ) -> Result<ManagedIndexChange, RmuxError> {
        let mut links = self.index_links(name)?;
        let id = *links
            .get(&source)
            .ok_or_else(|| invalid_window_target(name, source))?;
        let mut occurrences = identity_map(&links);
        let mut local = LocalState::from_session(&self.sessions[name]);
        if source != destination {
            if links.contains_key(&destination) && !kill_destination {
                return Err(occupied(name, destination));
            }
            links.remove(&source);
            links.insert(destination, id);
            occurrences.remove(&destination);
            occurrences.insert(source, destination);
            local.move_slot(&links, source, destination, select_destination);
        }
        // For a peer the move is only a resynchronisation: the emptied slot
        // disappears with its visit, and the slot a replacement took over keeps
        // the one the peer made there.
        let history = identity_map(&links);
        self.commit_indices(
            name,
            links,
            IndexMaps {
                occurrences,
                history,
                // A move takes a winlink away and makes another; the list it lands
                // in is not the one it left.
                link_order: crate::window_state_store::LinkOrderPlan::Rebuilt,
            },
            local,
            None,
            None,
        )
    }

    /// Swaps occurrence identities and alerts while retaining local selected slots.
    pub fn swap_windows(
        &mut self,
        name: &SessionName,
        source: u32,
        destination: u32,
    ) -> Result<ManagedIndexChange, RmuxError> {
        let mut links = self.index_links(name)?;
        let source_id = *links
            .get(&source)
            .ok_or_else(|| invalid_window_target(name, source))?;
        let destination_id = *links
            .get(&destination)
            .ok_or_else(|| invalid_window_target(name, destination))?;
        let mut occurrences = identity_map(&links);
        // A swap keeps both winlinks and their slots and exchanges the windows
        // they hold (`cmd-swap-window.c:75-85`), so the canonical map swaps while
        // every session's visits stay exactly where they are.
        let history = identity_map(&links);
        links.insert(source, destination_id);
        links.insert(destination, source_id);
        occurrences.insert(source, destination);
        occurrences.insert(destination, source);
        let mut local = LocalState::from_session(&self.sessions[name]);
        let a = local.alerts[&source];
        let b = local.alerts[&destination];
        local.alerts.insert(source, b);
        local.alerts.insert(destination, a);
        self.commit_indices(
            name,
            links,
            IndexMaps {
                occurrences,
                history,
                // A swap keeps both winlinks and exchanges the windows they hold,
                // so each window's list of names is a different one afterwards.
                link_order: crate::window_state_store::LinkOrderPlan::Rebuilt,
            },
            local,
            Some(&BTreeMap::new()),
            None,
        )
    }

    /// Compacts indices from zero without replacing canonical owners.
    pub fn reindex_windows(&mut self, name: &SessionName) -> Result<ManagedIndexChange, RmuxError> {
        self.reindex_windows_from(name, 0)
    }

    /// Compacts sparse indices, rejecting overflow before any mutation.
    pub fn reindex_windows_from(
        &mut self,
        name: &SessionName,
        first: u32,
    ) -> Result<ManagedIndexChange, RmuxError> {
        let before = self.index_links(name)?;
        let mut links = Links::new();
        let mut occurrences = BTreeMap::new();
        for (offset, (&old, &id)) in before.iter().enumerate() {
            let offset = u32::try_from(offset).map_err(|_| exhausted(name))?;
            let new = first.checked_add(offset).ok_or_else(|| exhausted(name))?;
            links.insert(new, id);
            occurrences.insert(old, new);
        }
        let mut local = LocalState::from_session(&self.sessions[name]);
        local.active = occurrences[&local.active];
        local.last = local.last.and_then(|i| occurrences.get(&i).copied());
        // A renumber keeps the occurrences and only moves their indices
        // (`session.c:724-731`): the history follows by that same explicit map.
        local.carry_stack(&occurrences, &links);
        local.alerts = local
            .alerts
            .iter()
            .map(|(i, flags)| (occurrences[i], *flags))
            .collect();
        // A renumber is applied to every member of the group in turn
        // (`server-fn.c:229-235`) and keeps each one's own occurrences
        // (`session.c:724-731`), so here a peer's visits do travel by the map.
        let history = occurrences.clone();
        self.commit_indices(
            name,
            links,
            IndexMaps {
                occurrences,
                history,
                // A renumber saves the session's winlink list, empties it and
                // creates every winlink again (`session.c:691-746`), each appended
                // at the tail of its own window's list (`window.c:183-193`); the
                // same is then applied to every member of the group
                // (`server-fn.c:229-235`). That order is not the one held here and
                // this store does not model it, so it stops claiming one - even
                // when the final tables are identical.
                link_order: crate::window_state_store::LinkOrderPlan::Rebuilt,
            },
            local,
            None,
            None,
        )
    }
}
