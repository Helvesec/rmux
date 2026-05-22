use std::collections::HashSet;

use rmux_proto::{RmuxError, SessionName};

use super::{session_not_found, HandlerState};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct WindowLinkSlot {
    pub(super) session_name: SessionName,
    pub(super) window_index: u32,
}

impl WindowLinkSlot {
    fn new(session_name: SessionName, window_index: u32) -> Self {
        Self {
            session_name,
            window_index,
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct WindowLinkGroup {
    pub(super) runtime_session_name: SessionName,
    pub(super) slots: Vec<WindowLinkSlot>,
}

impl HandlerState {
    fn window_link_slot(&self, session_name: &SessionName, window_index: u32) -> WindowLinkSlot {
        WindowLinkSlot::new(session_name.clone(), window_index)
    }

    pub(crate) fn window_link_count(&self, session_name: &SessionName, window_index: u32) -> usize {
        let slot = self.window_link_slot(session_name, window_index);
        self.window_link_slots
            .get(&slot)
            .and_then(|group_id| self.window_link_groups.get(group_id))
            .map(|group| group.slots.len())
            .unwrap_or(1)
    }

    pub(crate) fn window_linked_session_count(
        &self,
        session_name: &SessionName,
        window_index: u32,
    ) -> usize {
        let slot = self.window_link_slot(session_name, window_index);
        self.window_link_slots
            .get(&slot)
            .and_then(|group_id| self.window_link_groups.get(group_id))
            .map(|group| {
                group
                    .slots
                    .iter()
                    .map(|slot| slot.session_name.clone())
                    .collect::<HashSet<_>>()
                    .len()
            })
            .unwrap_or(1)
    }

    pub(crate) fn window_linked_sessions_list(
        &self,
        session_name: &SessionName,
        window_index: u32,
    ) -> Vec<SessionName> {
        let slot = self.window_link_slot(session_name, window_index);
        self.window_link_slots
            .get(&slot)
            .and_then(|group_id| self.window_link_groups.get(group_id))
            .map(|group| {
                group
                    .slots
                    .iter()
                    .map(|slot| slot.session_name.clone())
                    .collect()
            })
            .unwrap_or_else(|| vec![session_name.clone()])
    }

    pub(in crate::pane_terminals) fn runtime_session_name_for_window(
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
        self.window_link_slots
            .get(&slot)
            .and_then(|group_id| self.window_link_groups.get(group_id))
            .map(|group| group.slots.clone())
            .unwrap_or_else(|| vec![slot])
    }

    fn window_link_group_id_for_slot_or_group_peer(
        &self,
        session_name: &SessionName,
        window_index: u32,
    ) -> Option<&u64> {
        let slot = self.window_link_slot(session_name, window_index);
        if let Some(group_id) = self.window_link_slots.get(&slot) {
            return Some(group_id);
        }

        self.sessions
            .session_group_members(session_name)
            .into_iter()
            .filter(|member| member != session_name)
            .find_map(|member| {
                let member_slot = self.window_link_slot(&member, window_index);
                self.window_link_slots.get(&member_slot)
            })
    }

    pub(in crate::pane_terminals) fn detach_window_link_slot(
        &mut self,
        session_name: &SessionName,
        window_index: u32,
    ) -> usize {
        let slot = self.window_link_slot(session_name, window_index);
        let Some(group_id) = self.window_link_slots.remove(&slot) else {
            return 1;
        };

        let remaining = if let Some(group) = self.window_link_groups.get_mut(&group_id) {
            group.slots.retain(|candidate| candidate != &slot);
            group.slots.len()
        } else {
            0
        };

        if remaining <= 1
            && let Some(group) = self.window_link_groups.remove(&group_id) {
                for group_slot in group.slots {
                    let _ = self.window_link_slots.remove(&group_slot);
                }
            }

        remaining.max(1)
    }

    pub(in crate::pane_terminals) fn attach_window_link_slot(
        &mut self,
        source_session_name: &SessionName,
        source_window_index: u32,
        target_session_name: &SessionName,
        target_window_index: u32,
    ) {
        let source_slot = self.window_link_slot(source_session_name, source_window_index);
        let target_slot = self.window_link_slot(target_session_name, target_window_index);
        let _ = self.detach_window_link_slot(target_session_name, target_window_index);

        let group_id = self
            .window_link_slots
            .get(&source_slot)
            .copied()
            .unwrap_or_else(|| {
                let group_id = self.next_window_link_group_id;
                self.next_window_link_group_id = self.next_window_link_group_id.wrapping_add(1);
                let _ = self.window_link_groups.insert(
                    group_id,
                    WindowLinkGroup {
                        runtime_session_name: self.runtime_session_name_for_window(
                            source_session_name,
                            source_window_index,
                        ),
                        slots: vec![source_slot.clone()],
                    },
                );
                let _ = self.window_link_slots.insert(source_slot, group_id);
                group_id
            });

        let group = self
            .window_link_groups
            .get_mut(&group_id)
            .expect("linked window group must exist");
        if !group.slots.contains(&target_slot) {
            group.slots.push(target_slot.clone());
        }
        let _ = self.window_link_slots.insert(target_slot, group_id);
    }

    pub(in crate::pane_terminals) fn swap_window_link_slots(
        &mut self,
        session_name: &SessionName,
        source_window_index: u32,
        target_window_index: u32,
    ) {
        if source_window_index == target_window_index {
            return;
        }

        let source_slot = self.window_link_slot(session_name, source_window_index);
        let target_slot = self.window_link_slot(session_name, target_window_index);
        let source_group = self.window_link_slots.remove(&source_slot);
        let target_group = self.window_link_slots.remove(&target_slot);

        for group_id in [source_group, target_group].into_iter().flatten() {
            if let Some(group) = self.window_link_groups.get_mut(&group_id) {
                for slot in &mut group.slots {
                    if *slot == source_slot {
                        *slot = target_slot.clone();
                    } else if *slot == target_slot {
                        *slot = source_slot.clone();
                    }
                }
            }
        }

        if let Some(group_id) = source_group {
            let _ = self.window_link_slots.insert(target_slot, group_id);
        }
        if let Some(group_id) = target_group {
            let _ = self.window_link_slots.insert(source_slot, group_id);
        }
    }

    pub(in crate::pane_terminals) fn swap_auto_named_window_slots(
        &mut self,
        source_session_name: &SessionName,
        source_window_index: u32,
        target_session_name: &SessionName,
        target_window_index: u32,
    ) {
        let source_key = self.auto_named_window_key(source_session_name, source_window_index);
        let target_key = self.auto_named_window_key(target_session_name, target_window_index);
        if source_key == target_key {
            return;
        }

        let source_tracked = self.auto_named_windows.remove(&source_key);
        let target_tracked = self.auto_named_windows.remove(&target_key);

        if source_tracked {
            let _ = self.auto_named_windows.insert(target_key);
        }
        if target_tracked {
            let _ = self.auto_named_windows.insert(source_key);
        }
    }

    pub(crate) fn synchronize_linked_window_from_slot(
        &mut self,
        session_name: &SessionName,
        window_index: u32,
    ) -> Result<(), RmuxError> {
        let source_slot = self.window_link_slot(session_name, window_index);
        let Some(group_id) = self.window_link_slots.get(&source_slot).copied() else {
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

    fn auto_named_window_key(
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
        let source_slot = self.window_link_slot(session_name, window_index);
        let mut slots = self
            .window_link_slots
            .get(&source_slot)
            .and_then(|group_id| self.window_link_groups.get(group_id))
            .map(|group| group.slots.clone())
            .unwrap_or_else(|| vec![source_slot]);
        for member in self.sessions.session_group_members(session_name) {
            slots.push(self.window_link_slot(&member, window_index));
        }
        for slot in slots.into_iter().collect::<HashSet<_>>() {
            self.clear_auto_named_window(&slot.session_name, slot.window_index);
        }
    }
}
