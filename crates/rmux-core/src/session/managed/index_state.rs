use super::*;

pub(super) type Links = BTreeMap<u32, WindowId>;

/// Local occurrence changes for a runtime/event consumer. Maps describe surviving
/// occurrences, including identity mappings; removed slots are absent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedSessionIndexChange {
    /// Session whose links changed.
    pub session_id: SessionId,
    /// Complete before and after identity tables.
    pub before: BTreeMap<u32, WindowId>,
    /// Complete resulting identity table.
    pub after: BTreeMap<u32, WindowId>,
    /// Authoritative old-to-new occurrence mapping.
    pub index_map: BTreeMap<u32, u32>,
    /// Selection before the operation.
    pub previous_active: u32,
    /// Selection after the operation.
    pub active: u32,
    /// History before the operation.
    pub previous_last: Option<u32>,
    /// History after the operation.
    pub last: Option<u32>,
}

/// Committed structural change; final owners are transferred exactly once.
#[derive(Debug)]
#[must_use]
pub struct ManagedIndexChange {
    /// Affected sessions in stable name order.
    pub sessions: Vec<ManagedSessionIndexChange>,
    /// Owners whose final alias was removed, ready for later runtime cleanup.
    pub removed_windows: Vec<Box<Window>>,
}

#[derive(Clone)]
pub(super) struct LocalState {
    pub active: u32,
    pub last: Option<u32>,
    pub stack: Vec<u32>,
    pub alerts: BTreeMap<u32, AlertFlags>,
}

impl LocalState {
    pub fn from_session(session: &ManagedSession) -> Self {
        Self {
            active: session.active_window,
            last: session.last_window,
            stack: session.window_stack.clone(),
            alerts: session.winlink_alert_flags.clone(),
        }
    }

    /// Carries this session's history onto the slots an explicit occurrence map
    /// defines, through the one helper that owns those writes.
    pub fn carry_stack(&mut self, occurrences: &BTreeMap<u32, u32>, after: &Links) {
        self.stack = crate::session::visit_history::carry_occurrences(
            &self.stack,
            occurrences,
            self.active,
            |slot| after.contains_key(&slot),
        );
    }

    /// Shifts one slot to the next index, the way an insertion opens room.
    ///
    /// The same winlink keeps its window and only changes index
    /// (`window.c:1758-1788`), so the selection and the history follow it instead
    /// of being purged as a move would.
    pub fn shift_slot(&mut self, links: &Links, source: u32, destination: u32) {
        if self.active == source {
            self.active = destination;
        }
        if self.last == Some(source) {
            self.last = Some(destination);
        }
        let flags = self.alerts.remove(&source).unwrap_or_default();
        self.alerts.insert(destination, flags);
        let active = self.active;
        let mut shifted = Vec::with_capacity(self.stack.len());
        for visited in &self.stack {
            let slot = if *visited == source {
                destination
            } else {
                *visited
            };
            if slot != active
                && (links.contains_key(&slot) || slot == destination)
                && !shifted.contains(&slot)
            {
                shifted.push(slot);
            }
        }
        self.stack = shifted;
    }

    pub fn move_slot(&mut self, links: &Links, source: u32, destination: u32, select: bool) {
        let active = self.active;
        let previous_active = active;
        let last = self.last;
        let flags = self.alerts.remove(&source).unwrap_or_default();
        self.alerts.insert(destination, flags);
        self.active = if active == source {
            if select {
                destination
            } else {
                last.filter(|i| *i != source && links.contains_key(i))
                    .unwrap_or_else(|| cyclic_previous(links, source))
            }
        } else if select {
            destination
        } else {
            active
        };
        let preserved = last.filter(|i| *i != source && *i != destination && links.contains_key(i));
        self.last = if active == source {
            if select {
                preserved
            } else {
                None
            }
        } else if select {
            if active == destination {
                preserved
            } else {
                Some(active)
            }
        } else {
            last.filter(|i| *i != source && links.contains_key(i))
        };
        if self.last == Some(self.active) {
            self.last = None;
        }
        self.move_stack(links, source, destination, previous_active, select);
    }

    /// Moving a window is a new link and an unlink, not a renumbering
    /// (`cmd-move-window.c:103-109`, `server-fn.c:278-298`, `session.c:350`).
    ///
    /// So the occurrence that leaves takes its visit with it, the destination it
    /// replaces loses the visit that named it, and the link that appears has none
    /// of its own: nothing is invented at the new slot. A selection that really
    /// happens is recorded as the visit it is.
    fn move_stack(
        &mut self,
        links: &Links,
        source: u32,
        destination: u32,
        previous_active: u32,
        select: bool,
    ) {
        if source == destination {
            // Nothing was unlinked and nothing created: the history is untouched
            // and only a real selection could change it.
            if select && previous_active != self.active {
                crate::session::visit_history::record_visit(
                    &mut self.stack,
                    previous_active,
                    self.active,
                );
            }
            return;
        }
        crate::session::visit_history::forget_occurrence(&mut self.stack, source);
        crate::session::visit_history::forget_occurrence(&mut self.stack, destination);
        self.stack
            .retain(|visited| links.contains_key(visited) || *visited == source);
        if select && previous_active != self.active && links.contains_key(&previous_active) {
            crate::session::visit_history::record_visit(
                &mut self.stack,
                previous_active,
                self.active,
            );
        }
        let active = self.active;
        self.stack.retain(|visited| *visited != active);
        // The return is the head of the history that is left, whatever the index
        // rules computed on the way: never a link this mutation has just created.
        self.last = crate::session::visit_history::live_head(&self.stack, active);
    }
}

pub(super) fn cyclic_previous(links: &Links, index: u32) -> u32 {
    *links
        .range(..index)
        .next_back()
        .or_else(|| links.last_key_value())
        .expect("nonempty managed session")
        .0
}

fn identity_index(links: &Links, id: WindowId, preferred: u32) -> Option<u32> {
    if links.get(&preferred) == Some(&id) {
        return Some(preferred);
    }
    let mut matches = links
        .iter()
        .filter_map(|(&i, &candidate)| (candidate == id).then_some(i));
    let first = matches.next()?;
    matches.next().is_none().then_some(first)
}

/// Every slot of a link table, mapped to itself.
pub(super) fn identity_map(links: &Links) -> BTreeMap<u32, u32> {
    links.keys().map(|&index| (index, index)).collect()
}

/// The slot a peer's selection keeps: a question about the window it holds, and
/// not about the visits that peer made.
pub(super) fn identity_active(
    before: &Links,
    after: &Links,
    previous_active: u32,
    previous_last: Option<u32>,
) -> u32 {
    before
        .get(&previous_active)
        .and_then(|id| identity_index(after, *id, previous_active))
        .unwrap_or_else(|| {
            if after.contains_key(&previous_active) {
                previous_active
            } else {
                previous_last
                    .filter(|i| after.contains_key(i))
                    .unwrap_or_else(|| cyclic_previous(after, previous_active))
            }
        })
}

/// The two maps an index commit carries.
///
/// `occurrences` is the canonical map the runtime, the alerts and the selection
/// already use: where a window went. `history` says where a PEER's own visits
/// go, which is not the same question. A group synchronisation rebuilds every
/// peer from the indices it already had, whatever occupies them afterwards
/// (`session.c:663-678`), so that map is the identity of the final slots; a
/// renumber instead moves each member's own occurrences (`session.c:724-731`)
/// and its visits travel with them.
pub(super) struct IndexMaps {
    pub occurrences: BTreeMap<u32, u32>,
    pub history: BTreeMap<u32, u32>,
    /// What this commit does to the ORDER the touched windows' links were made
    /// in. It belongs here, with the other maps that say what the commit did:
    /// the final tables alone cannot tell an appended alias from a wholesale
    /// rebuild, and nothing may deduce one from the other.
    pub link_order: crate::window_state_store::LinkOrderPlan,
}

pub(super) fn peer_state(
    session: &ManagedSession,
    before: &Links,
    after: &Links,
    occurrences: &BTreeMap<u32, u32>,
    history: &BTreeMap<u32, u32>,
    selection: Option<&BTreeMap<u32, u32>>,
) -> LocalState {
    let mut state = LocalState::from_session(session);
    let previous_active = state.active;
    let previous_last = state.last;
    let fallback = || {
        if after.contains_key(&previous_active) {
            previous_active
        } else {
            previous_last
                .filter(|i| after.contains_key(i))
                .unwrap_or_else(|| cyclic_previous(after, previous_active))
        }
    };
    if let Some(map) = selection {
        let remap = |i| map.get(&i).copied().unwrap_or(i);
        state.active = Some(remap(previous_active))
            .filter(|i| after.contains_key(i))
            .unwrap_or_else(fallback);
    } else {
        state.active = identity_active(before, after, previous_active, previous_last);
    }
    // A peer is resynchronised, never navigated: `session.c:663-678` rebuilds its
    // stack from the slots it already knew and drops only the ones that went
    // away. So no visit is recorded here and nothing follows the source's
    // shuffle; what its own map leaves then decides the return.
    state.carry_stack(history, after);
    state.last = crate::session::visit_history::live_head(&state.stack, state.active);
    state.alerts = after.keys().map(|&i| (i, AlertFlags::empty())).collect();
    for (&old, &new) in occurrences {
        if let Some(flags) = session.winlink_alert_flags.get(&old) {
            state.alerts.insert(new, *flags);
        }
    }
    state
}

impl ManagedSessionStore {
    pub(super) fn index_links(&self, name: &SessionName) -> Result<Links, RmuxError> {
        let session = self
            .sessions
            .get(name)
            .ok_or_else(|| RmuxError::SessionNotFound(name.to_string()))?;
        Ok(self
            .windows
            .session_windows(session.id)
            .map(|(i, w)| (i, w.id()))
            .collect())
    }

    /// Stage all peers before touching either canonical owners or local metadata.
    pub(super) fn commit_indices(
        &mut self,
        name: &SessionName,
        after: Links,
        maps: IndexMaps,
        source: LocalState,
        selection: Option<&BTreeMap<u32, u32>>,
        introduced: Option<Window>,
    ) -> Result<ManagedIndexChange, RmuxError> {
        let IndexMaps {
            occurrences,
            history,
            link_order,
        } = maps;
        let before = self.index_links(name)?;
        let mut tables = BTreeMap::new();
        let mut staged = Vec::new();
        let mut changes = Vec::new();
        for member in self.session_group_members(name) {
            let old = self.index_links(&member)?;
            // Defensively reject inconsistent internal state before changing
            // any peer or consuming an identity.
            if old != before {
                return Err(RmuxError::Server(
                    "managed group link tables differ".to_owned(),
                ));
            }
            let session = &self.sessions[&member];
            let local = if &member == name {
                source.clone()
            } else {
                peer_state(session, &old, &after, &occurrences, &history, selection)
            };
            if !after.contains_key(&local.active)
                || local.last.is_some_and(|i| !after.contains_key(&i))
                || local.alerts.keys().ne(after.keys())
            {
                return Err(RmuxError::Server("invalid managed index state".to_owned()));
            }
            changes.push(ManagedSessionIndexChange {
                session_id: session.id,
                before: old,
                after: after.clone(),
                index_map: occurrences.clone(),
                previous_active: session.active_window,
                active: local.active,
                previous_last: session.last_window,
                last: local.last,
            });
            tables.insert(session.id, after.clone());
            staged.push((member, local));
        }
        let removed_windows = self
            .windows
            .replace_session_links(&tables, introduced, link_order)
            .map_err(|e| RmuxError::Message(e.to_string()))?;
        for (member, local) in staged {
            let session = self.sessions.get_mut(&member).expect("validated member");
            session.active_window = local.active;
            session.last_window = local.last;
            session.window_stack = local.stack;
            session.winlink_alert_flags = local.alerts;
            // Group-initial selection is an identity, so no index rewrite is needed.
        }
        Ok(ManagedIndexChange {
            sessions: changes,
            removed_windows,
        })
    }
}
