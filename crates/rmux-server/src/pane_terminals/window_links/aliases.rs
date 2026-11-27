use std::collections::HashSet;

use rmux_proto::{PaneTarget, RmuxError, SessionName, WindowTarget};

use super::super::session_not_found;
use super::{HandlerState, WindowLinkSlot};

impl HandlerState {
    pub(crate) fn window_link_count(&self, session_name: &SessionName, window_index: u32) -> usize {
        self.window_link_group_id_for_slot_or_group_peer(session_name, window_index)
            .and_then(|group_id| self.window_link_groups.get(group_id))
            .map(|group| group.slots.len())
            .unwrap_or(1)
    }

    /// How many winlinks hold this window, counted the way `#{window_linked}`
    /// counts them.
    ///
    /// tmux 3.7b answers 1 as soon as a second winlink, in any session, holds
    /// the same window (`format.c:2871-2890`). A session group gives every
    /// member its own winlink to each shared window, at the same index, so a
    /// grouped window is held more than once even when nothing links it
    /// explicitly. Each explicit link slot therefore stands for itself and for
    /// the same index in every member of its session's group.
    ///
    /// Presentation only: `window_link_count` keeps answering the explicit
    /// links that the kill, move and link paths reason about.
    pub(crate) fn window_winlink_count(
        &self,
        session_name: &SessionName,
        window_index: u32,
    ) -> usize {
        let mut winlinks = HashSet::new();
        for slot in self.window_link_slots_for(session_name, window_index) {
            for member in self.sessions.session_group_members(&slot.session_name) {
                winlinks.insert((member, slot.window_index));
            }
        }
        winlinks.len()
    }

    pub(crate) fn window_linked_session_count(
        &self,
        session_name: &SessionName,
        window_index: u32,
    ) -> usize {
        self.window_linked_session_family_list(session_name, window_index)
            .len()
    }

    /// The sessions this window is linked into, for the callers that walk them.
    ///
    /// `handler_window` reconciles, emits and refreshes the attached sessions in
    /// the order this returns, and the attach resize policy reads it too. It is
    /// exactly the body it has always had, and the presentation accessor below
    /// is what answers the format instead.
    pub(crate) fn window_linked_sessions_list(
        &self,
        session_name: &SessionName,
        window_index: u32,
    ) -> Vec<SessionName> {
        self.window_linked_session_family_list(session_name, window_index)
    }

    /// How many sessions `#{window_linked_sessions}` counts, and only that.
    ///
    /// tmux counts each session group that holds the window once, and each
    /// session in no group once (`format.c` `format_cb_window_linked_sessions`):
    /// a window shared by a group of three is held by one, and the same window
    /// linked into an outside session too is held by two. The kill paths keep
    /// reading `window_linked_session_count`, which counts the members.
    pub(crate) fn window_linked_session_group_count(
        &self,
        session_name: &SessionName,
        window_index: u32,
    ) -> usize {
        self.window_linked_session_family_list(session_name, window_index)
            .iter()
            .map(|member| {
                self.sessions
                    .session_group_name(member)
                    .map_or((false, member.clone()), |group| (true, group.clone()))
            })
            .collect::<HashSet<_>>()
            .len()
    }

    /// The sessions `#{window_linked_sessions_list}` names, and only that.
    ///
    /// Its one caller is `format_runtime/window.rs`; nothing that walks the
    /// links to do work reads this.
    ///
    /// tmux walks the WINDOW's own winlinks and prints one session name per
    /// winlink, in the order those links were made (`format.c:686-714`,
    /// `window.c:183-193`): a session that links the same window twice is named
    /// twice, an outsider linked between a creation and a JOIN is named between
    /// them, and a group synchronised from one member names that member before
    /// the peers it rebuilt. The window store keeps exactly that order, for a
    /// session group as for an explicit link; when it cannot vouch for it, this
    /// falls back to what it answered before.
    pub(crate) fn window_linked_sessions_presentation_list(
        &self,
        session_name: &SessionName,
        window_index: u32,
    ) -> Vec<SessionName> {
        if let Some(made) = self.sessions.window_link_order(&WindowTarget::with_window(
            session_name.clone(),
            window_index,
        )) {
            return made;
        }
        if self
            .window_link_group_id_for_slot_or_group_peer(session_name, window_index)
            .is_some()
        {
            return self.window_linked_session_family_list(session_name, window_index);
        }
        self.window_linked_sessions_without_explicit_links(session_name)
    }

    /// `#{window_active_sessions_list}` when the window store cannot vouch for
    /// the order of this window's links: tmux names the sessions whose current
    /// winlink is one of the window's, in the same order as
    /// `#{window_linked_sessions_list}` (`format.c:718-767`), so this follows
    /// the fallback that list takes.
    ///
    /// With an explicit link group the current-session list keeps the answer
    /// it gave. With none, every member of the session group carries this
    /// window at this index, and the members are named in the order the group
    /// took them rather than starting from whichever member is asking.
    pub(crate) fn window_active_sessions_presentation_fallback(
        &self,
        session_name: &SessionName,
        window_index: u32,
    ) -> Vec<SessionName> {
        if self
            .window_link_group_id_for_slot_or_group_peer(session_name, window_index)
            .is_some()
        {
            return self.window_linked_current_sessions_list(session_name, window_index);
        }
        self.window_linked_sessions_without_explicit_links(session_name)
            .into_iter()
            .filter(|member| {
                self.sessions
                    .session(member)
                    .is_some_and(|session| session.active_window_index() == window_index)
            })
            .collect()
    }

    /// The fallback for a window nothing links explicitly and whose order the
    /// store no longer vouches for (after a renumber or a partial detach in its
    /// group): its session group's members, in the order the group took them.
    ///
    /// The group took them in the order their identities were minted, under the
    /// invariant this store maintains - the only ways into a group are a
    /// singleton that declares one, or an existing template joined by a session
    /// whose identity is minted after it. That is the same invariant
    /// `#{session_group_list}` reads, and it is narrower than the sentence it
    /// serves: a session's birth and its JOIN are the same event HERE.
    ///
    /// Two things this deliberately does NOT do. It does not dedup: tmux prints
    /// one name per winlink, so a session that links the same window twice is
    /// named twice, and no guard here pretends otherwise. And it does not know
    /// where an explicitly linked outsider would sit among these members: only
    /// the store's order above knows that interleaving.
    ///
    /// Presentation only: `window_linked_session_family_list` is untouched, so
    /// the runtime, resize, clipboard and navigation aggregators, the shared
    /// deduplication and `window_linked_session_count` keep what they had.
    /// The window index is not read: with no explicit link group every member of
    /// the session group carries the same window list, so the members ARE the
    /// links of any window of it.
    fn window_linked_sessions_without_explicit_links(
        &self,
        session_name: &SessionName,
    ) -> Vec<SessionName> {
        let mut members = self.sessions.session_group_members(session_name);
        if members.len() < 2 {
            return members;
        }
        // A stable sort, so a member this store cannot resolve keeps the name
        // order it arrived in rather than jumping to the front.
        members.sort_by_key(|member| {
            self.sessions
                .session(member)
                .map_or(u32::MAX, |session| session.id().as_u32())
        });
        members
    }

    pub(crate) fn window_linked_current_sessions_list(
        &self,
        session_name: &SessionName,
        window_index: u32,
    ) -> Vec<SessionName> {
        let mut seen = HashSet::new();
        let mut sessions = Vec::new();
        for slot in self.window_link_slots_for(session_name, window_index) {
            let mut candidates = vec![slot.session_name.clone()];
            candidates.extend(
                self.sessions
                    .session_group_members(&slot.session_name)
                    .into_iter()
                    .filter(|member| member != &slot.session_name),
            );
            for candidate in candidates {
                // The selection comes first and the deduplication after it. A
                // session reached through an earlier slot where it is not current
                // must not be marked seen there: it can hold a second alias of the
                // same window further down the slot order and really be current on
                // that one. Slot order is kept and a session still appears once.
                let is_current = self
                    .sessions
                    .session(&candidate)
                    .is_some_and(|session| session.active_window_index() == slot.window_index);
                if !is_current {
                    continue;
                }
                if !seen.insert(candidate.clone()) {
                    continue;
                }
                sessions.push(candidate);
            }
        }
        sessions
    }

    pub(crate) fn window_linked_session_family_list(
        &self,
        session_name: &SessionName,
        window_index: u32,
    ) -> Vec<SessionName> {
        let mut seen = HashSet::new();
        let mut sessions = Vec::new();
        for slot in self.window_link_slots_for(session_name, window_index) {
            if seen.insert(slot.session_name.clone()) {
                sessions.push(slot.session_name.clone());
            }
            for member in self
                .sessions
                .session_group_members(&slot.session_name)
                .into_iter()
                .filter(|member| member != &slot.session_name)
            {
                if seen.insert(member.clone()) {
                    sessions.push(member);
                }
            }
        }
        sessions
    }

    pub(crate) fn expand_with_active_window_linked_session_families(
        &self,
        session_names: &mut Vec<SessionName>,
    ) {
        let mut seen = session_names.iter().cloned().collect::<HashSet<_>>();
        let mut cursor = 0;
        while cursor < session_names.len() {
            let session_name = session_names[cursor].clone();
            cursor += 1;
            let Some(active_window_index) = self
                .sessions
                .session(&session_name)
                .map(|session| session.active_window_index())
            else {
                continue;
            };
            for linked_session in
                self.window_linked_session_family_list(&session_name, active_window_index)
            {
                if seen.insert(linked_session.clone()) {
                    session_names.push(linked_session);
                }
            }
        }
        session_names.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        session_names.dedup();
    }

    pub(crate) fn window_linked_window_targets(
        &self,
        session_name: &SessionName,
        window_index: u32,
    ) -> Vec<WindowTarget> {
        let mut targets = Vec::new();
        let mut seen = HashSet::new();
        for slot in self.window_link_slots_for(session_name, window_index) {
            for member in self.sessions.session_group_members(&slot.session_name) {
                let target = WindowTarget::with_window(member, slot.window_index);
                if seen.insert(target.clone()) {
                    targets.push(target);
                }
            }
        }
        targets.sort_by(|left, right| {
            left.session_name()
                .as_str()
                .cmp(right.session_name().as_str())
                .then_with(|| left.window_index().cmp(&right.window_index()))
        });
        targets
    }

    pub(crate) fn touch_linked_window_activity_for_pane(
        &mut self,
        target: &PaneTarget,
        pane_id: rmux_core::PaneId,
    ) -> bool {
        let targets =
            self.window_linked_window_targets(target.session_name(), target.window_index());
        let family_is_complete = targets.iter().all(|window_target| {
            self.sessions
                .session(window_target.session_name())
                .and_then(|session| session.window_at(window_target.window_index()))
                .is_some_and(|window| window.panes().iter().any(|pane| pane.id() == pane_id))
        });
        if !family_is_complete {
            return false;
        }

        self.sessions
            .touch_window_pane_activity(
                &WindowTarget::with_window(target.session_name().clone(), target.window_index()),
                target.pane_index(),
            )
            .unwrap_or(false)
    }

    pub(crate) fn runtime_session_name_for_window(
        &self,
        session_name: &SessionName,
        window_index: u32,
    ) -> SessionName {
        self.window_link_group_id_for_slot_or_group_peer(session_name, window_index)
            .and_then(|group_id| self.window_link_groups.get(group_id))
            .map(|group| group.runtime_session_name.clone())
            .unwrap_or_else(|| self.runtime_session_name(session_name))
    }

    pub(in crate::pane_terminals) fn window_link_slots_for(
        &self,
        session_name: &SessionName,
        window_index: u32,
    ) -> Vec<WindowLinkSlot> {
        let slot = self.window_link_slot(session_name, window_index);
        self.window_link_group_id_for_slot_or_group_peer(session_name, window_index)
            .and_then(|group_id| self.window_link_groups.get(group_id))
            .map(|group| group.slots.clone())
            .unwrap_or_else(|| vec![slot])
    }

    pub(crate) fn synchronize_linked_window_options_from_slot(
        &mut self,
        session_name: &SessionName,
        window_index: u32,
    ) {
        let source = WindowTarget::with_window(session_name.clone(), window_index);
        let targets = self.window_linked_window_targets(session_name, window_index);
        for target in targets {
            if target != source {
                self.options.copy_window_overrides(&source, &target);
            }
        }
    }

    pub(crate) fn synchronize_window_alias_options_from_name(
        &mut self,
        name: &SessionName,
    ) -> Result<(), RmuxError> {
        let indices = self
            .sessions
            .session(name)
            .ok_or_else(|| session_not_found(name))?
            .windows()
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        for index in indices {
            self.synchronize_linked_window_options_from_slot(name, index);
        }
        Ok(())
    }

    fn window_link_group_id_for_slot_or_group_peer(
        &self,
        session_name: &SessionName,
        window_index: u32,
    ) -> Option<&u64> {
        self.window_link_group_slot_for_slot_or_group_peer(session_name, window_index)
            .and_then(|slot| self.window_link_slots.get(&slot))
    }

    fn window_link_group_slot_for_slot_or_group_peer(
        &self,
        session_name: &SessionName,
        window_index: u32,
    ) -> Option<WindowLinkSlot> {
        let slot = self.window_link_slot(session_name, window_index);
        if self.window_link_slots.contains_key(&slot) {
            return Some(slot);
        }

        self.sessions
            .session_group_members(session_name)
            .into_iter()
            .map(|member| self.window_link_slot(&member, window_index))
            .find(|member_slot| self.window_link_slots.contains_key(member_slot))
    }

    pub(super) fn canonical_window_link_slot(
        &self,
        session_name: &SessionName,
        window_index: u32,
    ) -> WindowLinkSlot {
        self.window_link_group_slot_for_slot_or_group_peer(session_name, window_index)
            .unwrap_or_else(|| {
                self.window_link_slot(&self.runtime_session_name(session_name), window_index)
            })
    }

    pub(super) fn canonical_window_link_slot_by_index(
        &self,
        session_name: &SessionName,
        window_index: u32,
    ) -> WindowLinkSlot {
        // Structural mutations rekey canonical metadata before grouped peer
        // models are synchronized, so these callers deliberately preserve
        // the already-remapped index instead of consulting a peer model.
        let runtime_owner = self.runtime_session_name(session_name);
        self.window_link_slot(&runtime_owner, window_index)
    }

    pub(crate) fn synchronize_linked_window_from_slot(
        &mut self,
        session_name: &SessionName,
        window_index: u32,
    ) -> Result<(), RmuxError> {
        self.sessions.window(&WindowTarget::with_window(
            session_name.clone(),
            window_index,
        ))?;
        Ok(())
    }

    pub(crate) fn synchronize_linked_window_family_from_slot(
        &mut self,
        session_name: &SessionName,
        window_index: u32,
    ) -> Result<Vec<SessionName>, RmuxError> {
        let linked_slots = self.window_link_slots_for(session_name, window_index);
        self.synchronize_linked_window_from_slot(session_name, window_index)?;
        let mut synchronized = HashSet::new();
        for slot in linked_slots {
            if synchronized.insert(slot.session_name.clone()) {
                self.synchronize_session_group_from(&slot.session_name)?;
            }
        }
        Ok(self.window_linked_session_family_list(session_name, window_index))
    }

    pub(crate) fn synchronize_window_alias_family_from_slot(
        &mut self,
        session_name: &SessionName,
        window_index: u32,
    ) -> Result<Vec<SessionName>, RmuxError> {
        self.sessions.window(&WindowTarget::with_window(
            session_name.clone(),
            window_index,
        ))?;
        Ok(self.window_linked_session_family_list(session_name, window_index))
    }

    pub(super) fn auto_named_window_key(
        &self,
        session_name: &SessionName,
        window_index: u32,
    ) -> (SessionName, u32) {
        (self.runtime_session_name(session_name), window_index)
    }

    pub(super) fn auto_named_window_key_by_index(
        &self,
        session_name: &SessionName,
        window_index: u32,
    ) -> (SessionName, u32) {
        (self.runtime_session_name(session_name), window_index)
    }

    pub(crate) fn tracks_auto_named_window(
        &self,
        session_name: &SessionName,
        window_index: u32,
    ) -> bool {
        self.auto_named_windows
            .contains(&self.auto_named_window_key(session_name, window_index))
    }

    pub(crate) fn mark_auto_named_window(&mut self, session_name: &SessionName, window_index: u32) {
        let key = self.auto_named_window_key(session_name, window_index);
        let _ = self.auto_named_windows.insert(key);
    }

    pub(in crate::pane_terminals) fn clear_auto_named_window(
        &mut self,
        session_name: &SessionName,
        window_index: u32,
    ) {
        let key = self.auto_named_window_key(session_name, window_index);
        let _ = self.auto_named_windows.remove(&key);
    }

    pub(in crate::pane_terminals) fn clear_auto_named_window_family(
        &mut self,
        session_name: &SessionName,
        window_index: u32,
    ) {
        for target in self.window_linked_window_targets(session_name, window_index) {
            self.clear_auto_named_window(target.session_name(), target.window_index());
        }
    }
}
