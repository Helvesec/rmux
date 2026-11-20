use std::collections::{HashMap, HashSet};

use rmux_proto::{format_continue_line, format_pause_line};

use super::ControlOutputQueue;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ControlPaneOutputActionKind {
    On,
    Off,
    Pause,
    Continue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ControlPaneOutputAction {
    pane_id: u32,
    kind: ControlPaneOutputActionKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ControlPaneOutputRefresh {
    None,
    Now,
}

impl ControlPaneOutputAction {
    pub(crate) const fn new(pane_id: u32, kind: ControlPaneOutputActionKind) -> Self {
        Self { pane_id, kind }
    }

    pub(crate) const fn pane_id(self) -> u32 {
        self.pane_id
    }

    pub(crate) const fn kind(self) -> ControlPaneOutputActionKind {
        self.kind
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct PaneOutputState {
    off: bool,
    paused: bool,
    epoch: u64,
}

#[derive(Debug, Default)]
pub(super) struct ControlPaneOutputStates {
    states: HashMap<u32, PaneOutputState>,
    next_epoch: u64,
}

impl ControlPaneOutputStates {
    pub(super) fn epoch_for(&self, pane_id: u32) -> u64 {
        self.states.get(&pane_id).map_or(0, |state| state.epoch)
    }

    pub(super) fn wants_subscription(&self, pane_id: u32) -> bool {
        self.states
            .get(&pane_id)
            .is_none_or(|state| !state.off && !state.paused)
    }

    pub(super) fn ensure_pane_epoch(&mut self, pane_id: u32) -> u64 {
        if let Some(state) = self.states.get(&pane_id) {
            return state.epoch;
        }
        let epoch = self.allocate_epoch();
        self.store_state(
            pane_id,
            PaneOutputState {
                epoch,
                ..Default::default()
            },
        );
        epoch
    }

    pub(super) fn is_paused(&self, pane_id: u32) -> bool {
        self.states.get(&pane_id).is_some_and(|state| state.paused)
    }

    pub(super) fn is_off(&self, pane_id: u32) -> bool {
        self.states.get(&pane_id).is_some_and(|state| state.off)
    }

    pub(super) fn blocks_reading(&self, pane_id: u32) -> bool {
        self.is_off(pane_id) && !self.is_paused(pane_id)
    }

    pub(super) fn read_blocked_panes(&self) -> HashSet<u32> {
        self.states
            .keys()
            .copied()
            .filter(|pane_id| self.blocks_reading(*pane_id))
            .collect()
    }

    pub(super) fn prune_missing(&mut self, live_pane_ids: &HashSet<u32>) {
        self.states
            .retain(|pane_id, _| live_pane_ids.contains(pane_id));
    }

    #[cfg(test)]
    pub(super) fn state_count(&self) -> usize {
        self.states.len()
    }

    pub(super) fn accepts_event(&self, pane_id: u32, epoch: u64) -> bool {
        self.states.get(&pane_id).map_or(epoch == 0, |state| {
            state.epoch == epoch && !state.off && !state.paused
        })
    }

    pub(super) fn apply_actions(
        &mut self,
        actions: &[ControlPaneOutputAction],
        output_queue: &mut ControlOutputQueue,
    ) -> Vec<ControlPaneOutputRefresh> {
        actions
            .iter()
            .map(|action| self.apply_action(*action, output_queue))
            .collect()
    }

    pub(super) fn apply_pause_after(
        &mut self,
        pane_id: u32,
        output_queue: &mut ControlOutputQueue,
    ) -> bool {
        let mut state = self.states.get(&pane_id).copied().unwrap_or_default();
        if state.paused {
            return false;
        }
        state.paused = true;
        state.epoch = self.allocate_epoch();
        self.store_state(pane_id, state);
        output_queue.discard_pane_output(pane_id);
        output_queue.enqueue_line(format_pause_line(pane_id).into_bytes(), false);
        true
    }

    fn apply_action(
        &mut self,
        action: ControlPaneOutputAction,
        output_queue: &mut ControlOutputQueue,
    ) -> ControlPaneOutputRefresh {
        let pane_id = action.pane_id();
        let mut state = self.states.get(&pane_id).copied().unwrap_or_default();
        let mut changed = false;
        let mut notification = None;

        match action.kind() {
            ControlPaneOutputActionKind::On if state.off => {
                state.off = false;
                changed = true;
            }
            ControlPaneOutputActionKind::Off if !state.off => {
                state.off = true;
                changed = true;
            }
            ControlPaneOutputActionKind::Pause if !state.paused => {
                state.paused = true;
                changed = true;
                notification = Some(format_pause_line(pane_id));
            }
            ControlPaneOutputActionKind::Continue if state.paused => {
                state.paused = false;
                changed = true;
                notification = Some(format_continue_line(pane_id));
            }
            _ => {}
        }

        if !changed {
            return ControlPaneOutputRefresh::None;
        }

        state.epoch = self.allocate_epoch();
        self.store_state(pane_id, state);
        output_queue.discard_pane_output(pane_id);
        if let Some(line) = notification {
            output_queue.enqueue_line(line.into_bytes(), false);
        }
        match action.kind() {
            ControlPaneOutputActionKind::On => ControlPaneOutputRefresh::Now,
            ControlPaneOutputActionKind::Continue => ControlPaneOutputRefresh::Now,
            ControlPaneOutputActionKind::Off | ControlPaneOutputActionKind::Pause => {
                ControlPaneOutputRefresh::None
            }
        }
    }

    fn allocate_epoch(&mut self) -> u64 {
        self.next_epoch = self.next_epoch.saturating_add(1).max(1);
        self.next_epoch
    }

    fn store_state(&mut self, pane_id: u32, state: PaneOutputState) {
        self.states.insert(pane_id, state);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{
        ControlPaneOutputAction, ControlPaneOutputActionKind, ControlPaneOutputRefresh,
        ControlPaneOutputStates,
    };
    use crate::control::{flush_output_queue, ControlClientFlags, ControlOutputQueue};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn action(pane_id: u32, kind: ControlPaneOutputActionKind) -> ControlPaneOutputAction {
        ControlPaneOutputAction::new(pane_id, kind)
    }

    async fn drain_queue(queue: &mut ControlOutputQueue) -> Vec<u8> {
        let (mut writer, mut reader) = tokio::io::duplex(1024);
        flush_output_queue(
            queue,
            &mut writer,
            ControlClientFlags::default(),
            &mut std::collections::HashSet::new(),
            None,
        )
        .await
        .expect("control queue flushes");
        writer
            .shutdown()
            .await
            .expect("control queue writer closes");
        let mut bytes = Vec::new();
        reader
            .read_to_end(&mut bytes)
            .await
            .expect("control queue output reads");
        bytes
    }

    #[tokio::test]
    async fn state_actions_match_tmux_crossed_and_duplicate_transitions() {
        let pane_id = 7;
        let mut states = ControlPaneOutputStates::default();
        let mut queue = ControlOutputQueue::default();

        queue.enqueue_pane_output(pane_id, b"queued-output".to_vec(), None);
        assert_eq!(
            states.apply_actions(
                &[action(pane_id, ControlPaneOutputActionKind::Pause)],
                &mut queue,
            ),
            vec![ControlPaneOutputRefresh::None]
        );
        assert!(states.is_paused(pane_id));
        assert!(!states.wants_subscription(pane_id));
        assert_eq!(drain_queue(&mut queue).await, b"%pause %7\n");

        let block_count = queue.blocks.len();
        assert_eq!(
            states.apply_actions(
                &[action(pane_id, ControlPaneOutputActionKind::Pause)],
                &mut queue,
            ),
            vec![ControlPaneOutputRefresh::None]
        );
        assert_eq!(queue.blocks.len(), block_count, "duplicate pause is silent");

        assert_eq!(
            states.apply_actions(
                &[action(pane_id, ControlPaneOutputActionKind::On)],
                &mut queue,
            ),
            vec![ControlPaneOutputRefresh::None]
        );
        assert!(states.is_paused(pane_id), "on does not clear pause");

        assert_eq!(
            states.apply_actions(
                &[action(pane_id, ControlPaneOutputActionKind::Continue)],
                &mut queue,
            ),
            vec![ControlPaneOutputRefresh::Now]
        );
        assert!(!states.is_paused(pane_id));
        assert_eq!(drain_queue(&mut queue).await, b"%continue %7\n");

        assert_eq!(
            states.apply_actions(
                &[
                    action(pane_id, ControlPaneOutputActionKind::Pause),
                    action(pane_id, ControlPaneOutputActionKind::Off),
                    action(pane_id, ControlPaneOutputActionKind::Continue),
                ],
                &mut queue,
            ),
            vec![
                ControlPaneOutputRefresh::None,
                ControlPaneOutputRefresh::None,
                ControlPaneOutputRefresh::Now,
            ]
        );
        assert!(!states.is_paused(pane_id));
        assert!(
            !states.wants_subscription(pane_id),
            "continue keeps off set"
        );

        assert_eq!(
            states.apply_actions(
                &[action(pane_id, ControlPaneOutputActionKind::Continue)],
                &mut queue,
            ),
            vec![ControlPaneOutputRefresh::None]
        );
        assert_eq!(
            states.apply_actions(
                &[action(pane_id, ControlPaneOutputActionKind::On)],
                &mut queue,
            ),
            vec![ControlPaneOutputRefresh::Now]
        );
        assert!(states.wants_subscription(pane_id));
    }

    #[test]
    fn pane_output_state_retirement_prunes_removed_panes() {
        let mut states = ControlPaneOutputStates::default();
        let mut queue = ControlOutputQueue::default();
        let actions = [
            action(7, ControlPaneOutputActionKind::Off),
            action(8, ControlPaneOutputActionKind::Pause),
        ];
        let _ = states.apply_actions(&actions, &mut queue);
        assert_eq!(states.state_count(), 2);

        states.prune_missing(&std::iter::once(8).collect());

        assert_eq!(states.state_count(), 1);
        assert_eq!(states.epoch_for(7), 0);
        assert!(!states.is_off(7));
        assert!(states.is_paused(8));
    }

    #[test]
    fn pane_output_state_retirement_rejects_late_subscription_events() {
        let mut states = ControlPaneOutputStates::default();
        let epoch = states.ensure_pane_epoch(7);
        assert!(states.accepts_event(7, epoch));

        states.prune_missing(&HashSet::new());

        assert!(!states.accepts_event(7, epoch));
    }

    #[test]
    fn mixed_actions_keep_each_pane_restart_position_independent() {
        let mut states = ControlPaneOutputStates::default();
        let mut queue = ControlOutputQueue::default();
        let _ = states.apply_actions(
            &[
                action(7, ControlPaneOutputActionKind::Off),
                action(8, ControlPaneOutputActionKind::Pause),
            ],
            &mut queue,
        );

        assert_eq!(
            states.apply_actions(
                &[
                    action(7, ControlPaneOutputActionKind::On),
                    action(8, ControlPaneOutputActionKind::Continue),
                ],
                &mut queue,
            ),
            vec![ControlPaneOutputRefresh::Now, ControlPaneOutputRefresh::Now]
        );
        assert!(states.wants_subscription(7));
        assert!(states.wants_subscription(8));
    }
}
