use rmux_core::WindowId;
use rmux_proto::{SessionId, SessionName, WindowTarget};

use super::{HandlerState, WindowLinkSlot};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct WindowLinkOccurrenceId(u64);

impl WindowLinkOccurrenceId {
    pub(crate) const fn as_u64(self) -> u64 {
        self.0
    }

    #[cfg(test)]
    pub(crate) const fn new_for_test(value: u64) -> Self {
        Self(value)
    }
}

impl HandlerState {
    pub(crate) fn ensure_live_window_link_occurrences(&mut self) {
        let slots = self
            .sessions
            .iter()
            .flat_map(|(session_name, session)| {
                session
                    .windows()
                    .keys()
                    .copied()
                    .map(move |window_index| (session_name.clone(), window_index))
            })
            .collect::<Vec<_>>();
        for (session_name, window_index) in slots {
            let slot = self.canonical_window_link_slot_by_index(&session_name, window_index);
            let _ = self.ensure_window_link_occurrence(&slot);
        }
    }

    pub(crate) fn window_link_occurrence_id(
        &self,
        session_name: &SessionName,
        window_index: u32,
    ) -> Option<WindowLinkOccurrenceId> {
        let slot = self.canonical_window_link_slot_by_index(session_name, window_index);
        self.window_link_occurrences.get(&slot).copied()
    }

    pub(crate) fn ensure_live_window_link_occurrence_id(
        &mut self,
        session_name: &SessionName,
        window_index: u32,
    ) -> Option<WindowLinkOccurrenceId> {
        self.sessions
            .session(session_name)
            .and_then(|session| session.window_at(window_index))?;
        let slot = self.canonical_window_link_slot_by_index(session_name, window_index);
        Some(self.ensure_window_link_occurrence(&slot))
    }

    pub(crate) fn window_link_occurrence_target(
        &self,
        occurrence_id: WindowLinkOccurrenceId,
        preferred_session_id: SessionId,
        window_id: WindowId,
    ) -> Option<WindowTarget> {
        self.sessions
            .iter()
            .flat_map(|(session_name, session)| {
                session
                    .windows()
                    .iter()
                    .filter(move |(window_index, window)| {
                        window.id() == window_id
                            && self.window_link_occurrence_id(session_name, **window_index)
                                == Some(occurrence_id)
                    })
                    .map(move |(window_index, _)| {
                        (
                            session.id(),
                            WindowTarget::with_window(session_name.clone(), *window_index),
                        )
                    })
            })
            .min_by_key(|(session_id, target)| {
                (
                    *session_id != preferred_session_id,
                    std::cmp::Reverse(*session_id),
                    target.window_index(),
                )
            })
            .map(|(_, target)| target)
    }

    pub(in crate::pane_terminals) fn set_window_link_occurrence_id(
        &mut self,
        session_name: &SessionName,
        window_index: u32,
        occurrence_id: WindowLinkOccurrenceId,
    ) {
        let slot = self.canonical_window_link_slot_by_index(session_name, window_index);
        let _ = self.window_link_occurrences.insert(slot, occurrence_id);
    }

    fn allocate_window_link_occurrence_id(&mut self) -> WindowLinkOccurrenceId {
        loop {
            let occurrence_id = WindowLinkOccurrenceId(self.next_window_link_occurrence_id);
            self.next_window_link_occurrence_id =
                self.next_window_link_occurrence_id.wrapping_add(1);
            if !self
                .window_link_occurrences
                .values()
                .any(|candidate| *candidate == occurrence_id)
            {
                return occurrence_id;
            }
        }
    }

    pub(super) fn ensure_window_link_occurrence(
        &mut self,
        slot: &WindowLinkSlot,
    ) -> WindowLinkOccurrenceId {
        if let Some(occurrence_id) = self.window_link_occurrences.get(slot).copied() {
            return occurrence_id;
        }
        let occurrence_id = self.allocate_window_link_occurrence_id();
        let _ = self
            .window_link_occurrences
            .insert(slot.clone(), occurrence_id);
        occurrence_id
    }

    pub(super) fn renew_window_link_occurrence(&mut self, slot: WindowLinkSlot) {
        let occurrence_id = self.allocate_window_link_occurrence_id();
        let _ = self.window_link_occurrences.insert(slot, occurrence_id);
    }
}
