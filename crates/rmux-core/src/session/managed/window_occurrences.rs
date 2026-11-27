use super::index_state::LocalState;
use super::*;

impl ManagedSessionStore {
    /// The plan for a link made at this slot: the link goes at the tail, and
    /// every peer of the session's group is rebuilt after it.
    ///
    /// In a group, tmux adds the winlink to the addressed session and then
    /// synchronises every other member, in the order they joined the group,
    /// which empties each member's winlink list and creates it again
    /// (`session.c:612-626`, `:633-689`).
    pub(super) fn link_made_plan(
        &self,
        target: &WindowTarget,
    ) -> crate::window_state_store::LinkOrderPlan {
        match self.group_synchronised_from(target.session_name()) {
            Some((session_id, peers)) => crate::window_state_store::LinkOrderPlan::LinkMade {
                made: WindowLink {
                    session_id,
                    window_index: target.window_index(),
                },
                peers,
            },
            None => crate::window_state_store::LinkOrderPlan::Rebuilt,
        }
    }

    /// The identity of `source` and those of the other members of its group in
    /// the order they joined it: the members tmux rebuilds when it synchronises
    /// the group from `source`. A member joins only when it is created, so the
    /// join order is the order of the session identities. `None` when a member
    /// cannot be resolved.
    pub(super) fn group_synchronised_from(
        &self,
        source: &SessionName,
    ) -> Option<(SessionId, Vec<SessionId>)> {
        let source_id = self.sessions.get(source)?.id;
        let mut peers = Vec::new();
        for member in self.session_group_members(source) {
            let id = self.sessions.get(&member)?.id;
            if id != source_id {
                peers.push(id);
            }
        }
        peers.sort_unstable();
        Some((source_id, peers))
    }

    /// Installs an alias, returning any displaced final owner to its caller.
    pub fn install_window_alias(
        &mut self,
        target: &WindowTarget,
        id: WindowId,
        replace: bool,
        select: bool,
    ) -> Result<ManagedIndexChange, RmuxError> {
        let mut links = self.index_links(target.session_name())?;
        if links.contains_key(&target.window_index()) && !replace {
            return Err(super::index_ops::occupied(
                target.session_name(),
                target.window_index(),
            ));
        }
        let mut occurrences = links
            .keys()
            .map(|&index| (index, index))
            .collect::<BTreeMap<_, _>>();
        occurrences.remove(&target.window_index());
        links.insert(target.window_index(), id);
        // The addressed session loses the visit of the link that is replaced,
        // handled below; a peer keeps the visit it made on that slot, which
        // still exists for it (`session.c:663-678`).
        let history = super::index_state::identity_map(&links);
        let mut local = LocalState::from_session(&self.sessions[target.session_name()]);
        local
            .alerts
            .insert(target.window_index(), AlertFlags::empty());
        let selection_changed = select && local.active != target.window_index();
        // The destination this link replaces is gone, so the visit that named it
        // goes with it and the new link arrives unvisited.
        crate::session::visit_history::forget_occurrence(&mut local.stack, target.window_index());
        if selection_changed {
            let left = local.active;
            local.last = Some(left);
            local.active = target.window_index();
            crate::session::visit_history::record_visit(&mut local.stack, left, local.active);
        } else if local.last == Some(target.window_index()) {
            // The window the return pointed at has just been replaced: the return
            // takes the live head of what the session really visited.
            local.last = crate::session::visit_history::live_head(&local.stack, local.active);
        }
        let change = self.commit_indices(
            target.session_name(),
            links,
            super::index_state::IndexMaps {
                occurrences: occurrences.clone(),
                history,
                link_order: self.link_made_plan(target),
            },
            local,
            None,
            None,
        )?;
        if selection_changed {
            self.metadata_mut(target.session_name())?
                .group_initial_window_id = None;
        }
        Ok(change)
    }

    /// Exchanges canonical references atomically across distinct session groups.
    pub fn swap_window_occurrences(
        &mut self,
        source: &WindowTarget,
        target: &WindowTarget,
    ) -> Result<(), RmuxError> {
        self.swap_window_occurrences_with_selection(source, target, false)
    }

    /// Exchanges references while staging peer alerts for the caller's subsequent
    /// addressed flag exchange and optional selection of both swapped slots.
    pub fn swap_window_occurrences_with_selection(
        &mut self,
        source: &WindowTarget,
        target: &WindowTarget,
        select: bool,
    ) -> Result<(), RmuxError> {
        let source_id = self.window(source)?.id();
        let target_id = self.window(target)?.id();
        let source_members = self.session_group_members(source.session_name());
        let target_members = self.session_group_members(target.session_name());
        if source_members
            .iter()
            .any(|name| target_members.contains(name))
        {
            return Err(RmuxError::Server("sessions are grouped".to_owned()));
        }
        let mut tables = BTreeMap::new();
        let mut peers = Vec::new();
        for (slot, incoming, replacement, members) in [
            (source, target, target_id, source_members),
            (target, source, source_id, target_members),
        ] {
            let before = self.index_links(slot.session_name())?;
            let mut incoming_flags = self.sessions[incoming.session_name()]
                .winlink_alert_flags
                .get(&incoming.window_index())
                .copied()
                .unwrap_or_default();
            if select && self.sessions[slot.session_name()].active_window != slot.window_index() {
                incoming_flags.remove(crate::WINLINK_ALERTFLAGS);
            }
            for name in members {
                let mut links = self.index_links(&name)?;
                if links != before {
                    return Err(RmuxError::Server(
                        "managed group link tables differ".to_owned(),
                    ));
                }
                links.insert(slot.window_index(), replacement);
                if &name != slot.session_name() {
                    // Only exact surviving occurrences retain alerts. Navigation
                    // follows a surviving identity, as in standalone group sync.
                    let occurrences = before
                        .iter()
                        .filter_map(|(&index, id)| {
                            (links.get(&index) == Some(id)).then_some((index, index))
                        })
                        .collect();
                    // Alerts follow the surviving occurrences, but this peer's
                    // own visits stay on the slots it knew, replaced or not.
                    let history = super::index_state::identity_map(&links);
                    let mut local = super::index_state::peer_state(
                        &self.sessions[&name],
                        &before,
                        &links,
                        &occurrences,
                        &history,
                        None,
                    );
                    if before.get(&slot.window_index()) != Some(&replacement) {
                        // Standalone/server sync seeds replacement peers after
                        // exchanging addressed alerts. Surviving slots stay local.
                        local.alerts.insert(slot.window_index(), incoming_flags);
                    }
                    peers.push((name.clone(), local));
                }
                tables.insert(self.sessions[&name].id, links);
            }
        }
        let removed = self
            .windows
            .replace_session_links(
                &tables,
                None,
                crate::window_state_store::LinkOrderPlan::Rebuilt,
            )
            .map_err(|error| RmuxError::Server(error.to_string()))?;
        debug_assert!(removed.is_empty());
        for (name, local) in peers {
            let session = self.sessions.get_mut(&name).expect("validated group peer");
            session.active_window = local.active;
            session.last_window = local.last;
            session.window_stack = local.stack;
            session.winlink_alert_flags = local.alerts;
        }
        Ok(())
    }

    /// Removes one occurrence while retaining an empty session until the caller's
    /// existing runtime teardown reaches session destruction. Group callers must
    /// explicitly walk their prevalidated occurrence plan before publication.
    pub fn remove_window_occurrence_for_teardown(
        &mut self,
        target: &WindowTarget,
    ) -> Result<Option<Box<Window>>, RmuxError> {
        let link = self.target_link(target)?;
        if self.windows.session_windows(link.session_id).count() == 1 {
            return self
                .windows
                .unlink(link)
                .map_err(|error| RmuxError::Server(error.to_string()));
        }
        // Only a destroyed window reaches this: its group detaches it from the
        // first member by name, whichever occurrence the caller walks first.
        Ok(self
            .remove_occurrence(target, link, DetachSource::FirstByName)
            .0)
    }

    /// Replaces alert state on the addressed occurrence only.
    pub fn replace_winlink_alert_flags(
        &mut self,
        target: &WindowTarget,
        flags: AlertFlags,
    ) -> Result<(), RmuxError> {
        self.target_link(target)?;
        self.metadata_mut(target.session_name())?
            .winlink_alert_flags
            .insert(target.window_index(), flags);
        Ok(())
    }

    /// Restores the historical previous-slot fallback after active unlink.
    pub fn restore_last_window_after_active_unlink(
        &mut self,
        name: &SessionName,
    ) -> Result<(), RmuxError> {
        let links = self.index_links(name)?;
        let active = self.sessions[name].active_window;
        if self.sessions[name].last_window.is_some() {
            return Ok(());
        }
        let last = links
            .keys()
            .copied()
            .find(|index| *index > active)
            .or_else(|| links.keys().copied().find(|index| *index < active));
        self.metadata_mut(name)?.last_window = last;
        Ok(())
    }
}
