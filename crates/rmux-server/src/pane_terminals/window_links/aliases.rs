use std::collections::HashSet;

use rmux_core::Session;
use rmux_proto::{RmuxError, SessionName, WindowTarget};

use super::super::session_not_found;
use super::{HandlerState, WindowLinkSlot};

impl HandlerState {
    pub(crate) fn window_link_count(&self, session_name: &SessionName, window_index: u32) -> usize {
        self.window_link_group_id_for_slot_or_group_peer(session_name, window_index)
            .and_then(|group_id| self.window_link_groups.get(group_id))
            .map(|group| group.slots.len())
            .unwrap_or(1)
    }

    pub(crate) fn window_linked_session_count(
        &self,
        session_name: &SessionName,
        window_index: u32,
    ) -> usize {
        self.window_linked_session_family_list(session_name, window_index)
            .len()
    }

    pub(crate) fn window_linked_sessions_list(
        &self,
        session_name: &SessionName,
        window_index: u32,
    ) -> Vec<SessionName> {
        self.window_linked_session_family_list(session_name, window_index)
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
                if !seen.insert(candidate.clone()) {
                    continue;
                }
                let is_current = self
                    .sessions
                    .session(&candidate)
                    .is_some_and(|session| session.active_window_index() == slot.window_index);
                if is_current {
                    sessions.push(candidate);
                }
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
                .map(rmux_core::Session::active_window_index)
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

    pub(crate) fn synchronize_window_alias_options_from_session(&mut self, source: &Session) {
        for window_index in source.windows().keys().copied() {
            self.synchronize_linked_window_options_from_slot(source.name(), window_index);
        }
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
        let source_slot = self.window_link_slot(session_name, window_index);
        let Some(group_id) = self
            .window_link_group_id_for_slot_or_group_peer(session_name, window_index)
            .copied()
        else {
            return Ok(());
        };
        let Some(group) = self.window_link_groups.get(&group_id).cloned() else {
            return Ok(());
        };
        if group.slots.len() <= 1 {
            return Ok(());
        }

        let source_window = self
            .sessions
            .session(session_name)
            .and_then(|session| session.window_at(window_index))
            .cloned()
            .ok_or_else(|| {
                RmuxError::invalid_target(
                    format!("{session_name}:{window_index}"),
                    "window index does not exist in session",
                )
            })?;

        for slot in group.slots {
            if slot == source_slot {
                continue;
            }
            self.sessions
                .session_mut(&slot.session_name)
                .ok_or_else(|| session_not_found(&slot.session_name))?
                .replace_window(slot.window_index, source_window.clone())?;
        }

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
        let source = WindowTarget::with_window(session_name.clone(), window_index);
        let source_window = self
            .sessions
            .session(session_name)
            .and_then(|session| session.window_at(window_index))
            .cloned()
            .ok_or_else(|| {
                RmuxError::invalid_target(
                    source.to_string(),
                    "window index does not exist in session",
                )
            })?;
        let targets = self.window_linked_window_targets(session_name, window_index);
        let mut synchronized_sessions = Vec::new();
        let mut seen_sessions = HashSet::new();
        for target in targets {
            if seen_sessions.insert(target.session_name().clone()) {
                synchronized_sessions.push(target.session_name().clone());
            }
            if target == source {
                continue;
            }
            self.sessions
                .session_mut(target.session_name())
                .ok_or_else(|| session_not_found(target.session_name()))?
                .replace_window(target.window_index(), source_window.clone())?;
        }
        Ok(synchronized_sessions)
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
