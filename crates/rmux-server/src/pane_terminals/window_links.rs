use std::collections::{BTreeMap, HashMap};

use rmux_proto::SessionName;

use super::HandlerState;

#[path = "window_links/aliases.rs"]
mod aliases;
#[path = "window_links/occurrences.rs"]
mod occurrences;
pub(crate) use occurrences::WindowLinkOccurrenceId;

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

    pub(in crate::pane_terminals) fn detach_window_link_slot(
        &mut self,
        session_name: &SessionName,
        window_index: u32,
    ) -> usize {
        let slot = self.canonical_window_link_slot(session_name, window_index);
        self.detach_canonical_window_link_slot(slot)
    }

    fn detach_canonical_window_link_slot(&mut self, slot: WindowLinkSlot) -> usize {
        let _ = self.window_link_occurrences.remove(&slot);
        let Some(group_id) = self.window_link_slots.remove(&slot) else {
            return 1;
        };

        let remaining = if let Some(group) = self.window_link_groups.get_mut(&group_id) {
            group.slots.retain(|candidate| candidate != &slot);
            group.slots.len()
        } else {
            0
        };

        if remaining <= 1 {
            if let Some(group) = self.window_link_groups.remove(&group_id) {
                for group_slot in group.slots {
                    let _ = self.window_link_slots.remove(&group_slot);
                }
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
        let source_auto_named_key =
            self.auto_named_window_key_by_index(source_session_name, source_window_index);
        let source_auto_named = self.auto_named_windows.contains(&source_auto_named_key);
        let source_slot =
            self.canonical_window_link_slot_by_index(source_session_name, source_window_index);
        let target_slot =
            self.canonical_window_link_slot_by_index(target_session_name, target_window_index);
        let _ = self.ensure_window_link_occurrence(&source_slot);
        let _ = self.detach_canonical_window_link_slot(target_slot.clone());
        self.renew_window_link_occurrence(target_slot.clone());

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
        if source_auto_named {
            let target_key =
                self.auto_named_window_key_by_index(target_session_name, target_window_index);
            let _ = self.auto_named_windows.insert(target_key);
        }
    }

    pub(in crate::pane_terminals) fn swap_window_link_slots(
        &mut self,
        session_name: &SessionName,
        source_window_index: u32,
        target_window_index: u32,
    ) {
        self.swap_window_link_slots_between(
            session_name,
            source_window_index,
            session_name,
            target_window_index,
        );
    }

    pub(in crate::pane_terminals) fn swap_window_link_slots_between(
        &mut self,
        source_session_name: &SessionName,
        source_window_index: u32,
        target_session_name: &SessionName,
        target_window_index: u32,
    ) {
        if source_session_name == target_session_name && source_window_index == target_window_index
        {
            return;
        }

        let source_slot =
            self.canonical_window_link_slot_by_index(source_session_name, source_window_index);
        let target_slot =
            self.canonical_window_link_slot_by_index(target_session_name, target_window_index);
        let source_occurrence = self.window_link_occurrences.remove(&source_slot);
        let target_occurrence = self.window_link_occurrences.remove(&target_slot);
        let source_group = self.window_link_slots.remove(&source_slot);
        let target_group = self.window_link_slots.remove(&target_slot);
        let source_runtime = self.runtime_session_name(&source_slot.session_name);
        let target_runtime = self.runtime_session_name(&target_slot.session_name);

        for group_id in [source_group, target_group].into_iter().flatten() {
            if let Some(group) = self.window_link_groups.get_mut(&group_id) {
                for slot in &mut group.slots {
                    if *slot == source_slot {
                        *slot = target_slot.clone();
                    } else if *slot == target_slot {
                        *slot = source_slot.clone();
                    }
                }
                if group.runtime_session_name == source_runtime {
                    group.runtime_session_name = target_runtime.clone();
                } else if group.runtime_session_name == target_runtime {
                    group.runtime_session_name = source_runtime.clone();
                }
            }
        }

        if let Some(group_id) = source_group {
            let _ = self.window_link_slots.insert(target_slot.clone(), group_id);
        }
        if let Some(group_id) = target_group {
            let _ = self.window_link_slots.insert(source_slot.clone(), group_id);
        }
        if let Some(occurrence_id) = source_occurrence {
            let _ = self
                .window_link_occurrences
                .insert(target_slot, occurrence_id);
        }
        if let Some(occurrence_id) = target_occurrence {
            let _ = self
                .window_link_occurrences
                .insert(source_slot, occurrence_id);
        }
    }

    pub(in crate::pane_terminals) fn move_window_link_slot(
        &mut self,
        source_session_name: &SessionName,
        source_window_index: u32,
        target_session_name: &SessionName,
        target_window_index: u32,
    ) {
        if source_window_index == target_window_index && source_session_name == target_session_name
        {
            return;
        }

        let source_slot =
            self.canonical_window_link_slot_by_index(source_session_name, source_window_index);
        let target_slot =
            self.canonical_window_link_slot_by_index(target_session_name, target_window_index);
        let source_occurrence = self.window_link_occurrences.remove(&source_slot);
        let _ = self.window_link_occurrences.remove(&target_slot);
        let source_group = self.window_link_slots.get(&source_slot).copied();
        let target_group = self.window_link_slots.get(&target_slot).copied();
        let source_runtime = self.runtime_session_name(&source_slot.session_name);
        let target_runtime = self.runtime_session_name(&target_slot.session_name);

        match (source_group, target_group) {
            (None, Some(_)) => {
                let _ = self.detach_window_link_slot(target_session_name, target_window_index);
                if let Some(occurrence_id) = source_occurrence {
                    let _ = self
                        .window_link_occurrences
                        .insert(target_slot, occurrence_id);
                }
                return;
            }
            (None, None) => {
                if let Some(occurrence_id) = source_occurrence {
                    let _ = self
                        .window_link_occurrences
                        .insert(target_slot, occurrence_id);
                }
                return;
            }
            (Some(source_group), Some(target_group)) if source_group != target_group => {
                let _ = self.detach_window_link_slot(target_session_name, target_window_index);
            }
            (Some(group_id), Some(_)) => {
                let _ = self.window_link_slots.remove(&target_slot);
                if let Some(group) = self.window_link_groups.get_mut(&group_id) {
                    group.slots.retain(|slot| slot != &target_slot);
                }
            }
            (Some(_), None) => {}
        }

        let Some(group_id) = self.window_link_slots.remove(&source_slot) else {
            if let Some(occurrence_id) = source_occurrence {
                let _ = self
                    .window_link_occurrences
                    .insert(target_slot, occurrence_id);
            }
            return;
        };

        if let Some(group) = self.window_link_groups.get_mut(&group_id) {
            for slot in &mut group.slots {
                if *slot == source_slot {
                    *slot = target_slot.clone();
                }
            }
            if !group.slots.contains(&target_slot) {
                group.slots.push(target_slot.clone());
            }
            if group.runtime_session_name == source_runtime {
                group.runtime_session_name = target_runtime;
            }
        }

        let _ = self.window_link_slots.insert(target_slot.clone(), group_id);
        if let Some(occurrence_id) = source_occurrence {
            let _ = self
                .window_link_occurrences
                .insert(target_slot, occurrence_id);
        }
    }

    pub(in crate::pane_terminals) fn linked_runtime_transfer_slot_for_detached_window(
        &self,
        session_name: &SessionName,
        window_index: u32,
    ) -> Option<WindowLinkSlot> {
        let detached_slot = self.canonical_window_link_slot(session_name, window_index);
        let group_id = self.window_link_slots.get(&detached_slot)?;
        let group = self.window_link_groups.get(group_id)?;
        group
            .slots
            .iter()
            .filter(|slot| **slot != detached_slot)
            .filter(|slot| {
                self.sessions
                    .session(&slot.session_name)
                    .and_then(|session| session.window_at(slot.window_index))
                    .is_some()
            })
            .min_by(|left, right| {
                left.session_name
                    .as_str()
                    .cmp(right.session_name.as_str())
                    .then_with(|| left.window_index.cmp(&right.window_index))
            })
            .cloned()
    }

    pub(in crate::pane_terminals) fn move_auto_named_window_slot(
        &mut self,
        source_session_name: &SessionName,
        source_window_index: u32,
        target_session_name: &SessionName,
        target_window_index: u32,
    ) {
        let source_key =
            self.auto_named_window_key_by_index(source_session_name, source_window_index);
        let target_key =
            self.auto_named_window_key_by_index(target_session_name, target_window_index);
        if source_key == target_key {
            return;
        }

        let source_tracked = self.auto_named_windows.remove(&source_key);
        let _ = self.auto_named_windows.remove(&target_key);
        if source_tracked {
            let _ = self.auto_named_windows.insert(target_key);
        }
    }

    pub(in crate::pane_terminals) fn swap_auto_named_window_slots(
        &mut self,
        source_session_name: &SessionName,
        source_window_index: u32,
        target_session_name: &SessionName,
        target_window_index: u32,
    ) {
        let source_key =
            self.auto_named_window_key_by_index(source_session_name, source_window_index);
        let target_key =
            self.auto_named_window_key_by_index(target_session_name, target_window_index);
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

    pub(in crate::pane_terminals) fn remap_window_indexed_state(
        &mut self,
        session_name: &SessionName,
        index_map: &BTreeMap<u32, u32>,
    ) {
        self.auto_named_windows = self
            .auto_named_windows
            .iter()
            .map(|(name, window_index)| {
                let next_index = if name == session_name {
                    index_map
                        .get(window_index)
                        .copied()
                        .unwrap_or(*window_index)
                } else {
                    *window_index
                };
                (name.clone(), next_index)
            })
            .collect();

        let mut remapped_slots = HashMap::with_capacity(self.window_link_slots.len());
        for (slot, group_id) in &self.window_link_slots {
            let next_slot = remapped_window_link_slot(slot, session_name, index_map);
            remapped_slots.insert(next_slot, *group_id);
        }
        self.window_link_slots = remapped_slots;

        let mut remapped_occurrences = HashMap::with_capacity(self.window_link_occurrences.len());
        for (slot, occurrence_id) in &self.window_link_occurrences {
            let next_slot = remapped_window_link_slot(slot, session_name, index_map);
            remapped_occurrences.insert(next_slot, *occurrence_id);
        }
        self.window_link_occurrences = remapped_occurrences;

        for group in self.window_link_groups.values_mut() {
            group.slots = group
                .slots
                .iter()
                .map(|slot| remapped_window_link_slot(slot, session_name, index_map))
                .collect();
        }
    }

    pub(in crate::pane_terminals) fn rename_window_link_session(
        &mut self,
        session_name: &SessionName,
        new_name: &SessionName,
    ) {
        let mut renamed_slots = HashMap::with_capacity(self.window_link_slots.len());
        for (slot, group_id) in &self.window_link_slots {
            renamed_slots.insert(
                renamed_window_link_slot(slot, session_name, new_name),
                *group_id,
            );
        }
        self.window_link_slots = renamed_slots;

        let mut renamed_occurrences = HashMap::with_capacity(self.window_link_occurrences.len());
        for (slot, occurrence_id) in &self.window_link_occurrences {
            renamed_occurrences.insert(
                renamed_window_link_slot(slot, session_name, new_name),
                *occurrence_id,
            );
        }
        self.window_link_occurrences = renamed_occurrences;

        for group in self.window_link_groups.values_mut() {
            rename_window_link_runtime_session(group, session_name, new_name);
            group.slots = group
                .slots
                .iter()
                .map(|slot| renamed_window_link_slot(slot, session_name, new_name))
                .collect();
        }
    }

    pub(in crate::pane_terminals) fn linked_runtime_transfer_slots_for_removed_session(
        &self,
        session_name: &SessionName,
    ) -> Vec<WindowLinkSlot> {
        let mut slots = self
            .window_link_groups
            .values()
            .filter(|group| group.runtime_session_name == *session_name)
            .filter_map(|group| {
                group
                    .slots
                    .iter()
                    .filter(|slot| slot.session_name != *session_name)
                    .filter(|slot| {
                        self.sessions
                            .session(&slot.session_name)
                            .and_then(|session| session.window_at(slot.window_index))
                            .is_some()
                    })
                    .min_by(|left, right| {
                        left.session_name
                            .as_str()
                            .cmp(right.session_name.as_str())
                            .then_with(|| left.window_index.cmp(&right.window_index))
                    })
                    .cloned()
            })
            .collect::<Vec<_>>();
        slots.sort_by(|left, right| {
            left.session_name
                .as_str()
                .cmp(right.session_name.as_str())
                .then_with(|| left.window_index.cmp(&right.window_index))
        });
        slots
    }

    pub(in crate::pane_terminals) fn set_window_link_runtime_session_for_slot(
        &mut self,
        slot: &WindowLinkSlot,
        runtime_session_name: SessionName,
    ) {
        let Some(group_id) = self.window_link_slots.get(slot).copied() else {
            return;
        };
        if let Some(group) = self.window_link_groups.get_mut(&group_id) {
            group.runtime_session_name = runtime_session_name;
        }
    }

    pub(in crate::pane_terminals) fn remove_window_link_session_slots(
        &mut self,
        session_name: &SessionName,
    ) {
        let slots = self
            .window_link_slots
            .keys()
            .filter(|slot| slot.session_name == *session_name)
            .cloned()
            .collect::<Vec<_>>();
        for slot in slots {
            let _ = self.detach_window_link_slot(&slot.session_name, slot.window_index);
        }
        self.window_link_occurrences
            .retain(|slot, _| slot.session_name != *session_name);
    }
}

fn remapped_window_link_slot(
    slot: &WindowLinkSlot,
    session_name: &SessionName,
    index_map: &BTreeMap<u32, u32>,
) -> WindowLinkSlot {
    if &slot.session_name != session_name {
        return slot.clone();
    }
    WindowLinkSlot::new(
        slot.session_name.clone(),
        index_map
            .get(&slot.window_index)
            .copied()
            .unwrap_or(slot.window_index),
    )
}

fn renamed_window_link_slot(
    slot: &WindowLinkSlot,
    session_name: &SessionName,
    new_name: &SessionName,
) -> WindowLinkSlot {
    if &slot.session_name != session_name {
        return slot.clone();
    }
    WindowLinkSlot::new(new_name.clone(), slot.window_index)
}

fn rename_window_link_runtime_session(
    group: &mut WindowLinkGroup,
    session_name: &SessionName,
    new_name: &SessionName,
) {
    if group.runtime_session_name == *session_name {
        group.runtime_session_name = new_name.clone();
    }
}
