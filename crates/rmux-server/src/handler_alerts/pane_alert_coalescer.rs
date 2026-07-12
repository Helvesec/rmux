use std::collections::HashMap;
use std::time::Duration;

use crate::pane_io::PaneAlertEvent;
use rmux_core::PaneId;

// Pane readers can emit one alert callback per PTY chunk. Coalescing keeps
// activity/bell/automatic-rename work from turning high-output panes into a
// global state-lock storm while staying below a perceptible UI delay.
pub(super) const PANE_ALERT_COALESCE_DELAY: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct PaneAlertKey {
    pane_id: PaneId,
    generation: Option<u64>,
}

#[derive(Debug, Default)]
pub(in crate::handler) struct PaneAlertCoalescer {
    pending: HashMap<PaneAlertKey, PaneAlertEvent>,
    flush_scheduled: bool,
}

impl PaneAlertKey {
    fn from_event(event: &PaneAlertEvent) -> Self {
        Self {
            pane_id: event.pane_id,
            generation: event.generation,
        }
    }
}

impl PaneAlertCoalescer {
    /// Queues an alert event and reports whether a flush task must be armed.
    pub(super) fn push(&mut self, event: PaneAlertEvent) -> bool {
        let key = PaneAlertKey::from_event(&event);
        self.pending
            .entry(key)
            .and_modify(|pending| {
                pending.bell_count = pending.bell_count.saturating_add(event.bell_count);
                pending.title_changed |= event.title_changed;
                pending.clipboard_set |= event.clipboard_set;
                pending
                    .clipboard_writes
                    .extend(event.clipboard_writes.iter().cloned());
                pending.mouse_mode_changed |= event.mouse_mode_changed;
                pending.queue_activity_alert |= event.queue_activity_alert;
            })
            .or_insert(event);

        if self.flush_scheduled {
            return false;
        }
        self.flush_scheduled = true;
        true
    }

    pub(super) fn take_pending(&mut self) -> Vec<PaneAlertEvent> {
        self.flush_scheduled = false;
        self.pending.drain().map(|(_, event)| event).collect()
    }

    pub(super) fn take_for_pane_generation(
        &mut self,
        pane_id: PaneId,
        generation: Option<u64>,
    ) -> Option<PaneAlertEvent> {
        self.pending.remove(&PaneAlertKey {
            pane_id,
            generation,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alert_event(pane_id: u32, generation: Option<u64>, bell_count: u64) -> PaneAlertEvent {
        PaneAlertEvent {
            session_name: rmux_proto::SessionName::new("alpha").expect("valid session name"),
            pane_id: PaneId::new(pane_id),
            bell_count,
            title_changed: false,
            title_change: None,
            clipboard_set: false,
            clipboard_writes: Vec::new(),
            mouse_mode_changed: false,
            queue_activity_alert: true,
            generation,
        }
    }

    fn title_event(pane_id: u32, generation: Option<u64>) -> PaneAlertEvent {
        PaneAlertEvent {
            title_changed: true,
            title_change: None,
            queue_activity_alert: false,
            ..alert_event(pane_id, generation, 0)
        }
    }

    fn clipboard_event(pane_id: u32, generation: Option<u64>) -> PaneAlertEvent {
        PaneAlertEvent {
            clipboard_set: true,
            queue_activity_alert: false,
            ..alert_event(pane_id, generation, 0)
        }
    }

    fn mouse_mode_event(pane_id: u32, generation: Option<u64>) -> PaneAlertEvent {
        PaneAlertEvent {
            mouse_mode_changed: true,
            queue_activity_alert: false,
            ..alert_event(pane_id, generation, 0)
        }
    }

    #[test]
    fn pane_alert_callback_state_coalesces_by_pane_generation() {
        let mut state = PaneAlertCoalescer::default();

        assert!(state.push(alert_event(1, Some(7), 1)));
        assert!(!state.push(alert_event(1, Some(7), 2)));
        assert!(!state.push(alert_event(2, Some(7), 4)));

        let events = state.take_pending();
        let first = events
            .iter()
            .find(|event| event.pane_id == PaneId::new(1))
            .expect("first pane event");
        let second = events
            .iter()
            .find(|event| event.pane_id == PaneId::new(2))
            .expect("second pane event");

        assert_eq!(events.len(), 2);
        assert_eq!(first.bell_count, 3);
        assert_eq!(second.bell_count, 4);
        assert!(state.push(alert_event(1, Some(8), 0)));
    }

    #[test]
    fn pane_alert_callback_state_preserves_coalesced_title_changes() {
        let mut state = PaneAlertCoalescer::default();

        assert!(state.push(alert_event(1, Some(7), 0)));
        assert!(!state.push(title_event(1, Some(7))));

        let events = state.take_pending();
        let first = events
            .iter()
            .find(|event| event.pane_id == PaneId::new(1))
            .expect("first pane event");
        assert!(first.title_changed);
        assert!(first.queue_activity_alert);
    }

    #[test]
    fn pane_alert_callback_state_preserves_coalesced_clipboard_events() {
        let mut state = PaneAlertCoalescer::default();

        assert!(state.push(alert_event(1, Some(7), 0)));
        assert!(!state.push(clipboard_event(1, Some(7))));

        let events = state.take_pending();
        let first = events
            .iter()
            .find(|event| event.pane_id == PaneId::new(1))
            .expect("first pane event");
        assert!(first.clipboard_set);
        assert!(first.queue_activity_alert);
    }

    #[test]
    fn pane_alert_callback_state_preserves_coalesced_mouse_mode_changes() {
        let mut state = PaneAlertCoalescer::default();

        assert!(state.push(alert_event(1, Some(7), 0)));
        assert!(!state.push(mouse_mode_event(1, Some(7))));

        let events = state.take_pending();
        let first = events
            .iter()
            .find(|event| event.pane_id == PaneId::new(1))
            .expect("first pane event");
        assert!(first.mouse_mode_changed);
        assert!(first.queue_activity_alert);
    }

    #[test]
    fn taking_exiting_pane_alert_preserves_other_pending_alerts_and_flush() {
        let mut state = PaneAlertCoalescer::default();

        assert!(state.push(alert_event(1, Some(7), 1)));
        assert!(!state.push(alert_event(2, Some(7), 2)));
        let exiting = state
            .take_for_pane_generation(PaneId::new(1), Some(7))
            .expect("exiting pane alert remains pending");

        assert_eq!(exiting.pane_id, PaneId::new(1));
        assert!(!state.push(alert_event(3, Some(7), 3)));
        let remaining = state.take_pending();
        assert_eq!(remaining.len(), 2);
        assert!(remaining
            .iter()
            .any(|event| event.pane_id == PaneId::new(2)));
        assert!(remaining
            .iter()
            .any(|event| event.pane_id == PaneId::new(3)));
    }
}
