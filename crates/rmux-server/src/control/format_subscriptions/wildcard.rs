use std::collections::HashMap;

use rmux_proto::{PaneId, SessionId, WindowId};

use super::{
    control_escaped_len, render_subscription_value, ControlFormatLimitExceeded,
    ControlFormatNotificationBatch, ControlFormatSelector, ControlFormatSnapshot,
    ControlFormatSubscriptions, HandlerState, ManagedSessionView, Pane, Window,
};
use crate::client_format::ClientLoopSnapshot;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Target {
    session: SessionId,
    window: WindowId,
    index: u32,
    pane: Option<PaneId>,
}

#[derive(Debug, Clone, Default)]
pub(super) struct WildcardValues(HashMap<Target, String>);

impl WildcardValues {
    pub(super) fn len(&self) -> usize {
        self.0.len()
    }

    pub(super) fn encoded_value_bytes(&self) -> usize {
        self.0.values().fold(0usize, |sum, value| {
            sum.saturating_add(control_escaped_len(value))
        })
    }

    fn get(&self, target: &Target) -> Option<&String> {
        self.0.get(target)
    }

    fn insert(&mut self, target: Target, value: String) {
        self.0.insert(target, value);
    }

    // Keep live targets across session switches, as tmux does, but never retain
    // a destroyed session, window occurrence or pane indefinitely.
    pub(super) fn prune(&mut self, state: &HandlerState) {
        self.0.retain(|target, _| {
            state
                .sessions
                .session_by_id(target.session)
                .is_some_and(|session| {
                    session.window_at(target.index).is_some_and(|window| {
                        window.id() == target.window
                            && target
                                .pane
                                .is_none_or(|id| window.panes().iter().any(|pane| pane.id() == id))
                    })
                })
        });
    }
}

struct WildcardTargetView<'a> {
    session: ManagedSessionView<'a>,
    index: u32,
    window: &'a Window,
    pane: Option<&'a Pane>,
}

impl ControlFormatSubscriptions {
    pub(super) fn changed_wildcard_notifications(
        &mut self,
        entry_index: usize,
        state: &HandlerState,
        session: ManagedSessionView<'_>,
        client_loop_snapshots: &[ClientLoopSnapshot],
        notifications: &mut ControlFormatNotificationBatch,
    ) -> Result<(), ControlFormatLimitExceeded> {
        for (index, window) in session.windows() {
            if self.entries[entry_index].selector == ControlFormatSelector::AllPanes {
                for pane in window.panes() {
                    self.wildcard_change(
                        entry_index,
                        state,
                        WildcardTargetView {
                            session,
                            index,
                            window,
                            pane: Some(pane),
                        },
                        client_loop_snapshots,
                        notifications,
                    )?;
                }
            } else {
                self.wildcard_change(
                    entry_index,
                    state,
                    WildcardTargetView {
                        session,
                        index,
                        window,
                        pane: None,
                    },
                    client_loop_snapshots,
                    notifications,
                )?;
            }
        }
        Ok(())
    }

    fn wildcard_change(
        &mut self,
        entry_index: usize,
        state: &HandlerState,
        view: WildcardTargetView<'_>,
        client_loop_snapshots: &[ClientLoopSnapshot],
        notifications: &mut ControlFormatNotificationBatch,
    ) -> Result<(), ControlFormatLimitExceeded> {
        let WildcardTargetView {
            session,
            index,
            window,
            pane,
        } = view;
        let target = Target {
            session: session.id(),
            window: window.id(),
            index,
            pane: pane.map(Pane::id),
        };
        let value = render_subscription_value(
            state,
            session,
            index,
            window,
            pane.or_else(|| window.active_pane()),
            (
                &self.entries[entry_index].template,
                &self.entries[entry_index].jobs,
            ),
            client_loop_snapshots,
        );
        if self.entries[entry_index].wildcard_values.get(&target) == Some(&value) {
            return Ok(());
        }
        let snapshot = ControlFormatSnapshot {
            session_id: session.id().to_string(),
            window_id: Some(window.id().to_string()),
            window_index: Some(index),
            pane_id: pane.map(|pane| pane.id().to_string()),
            value,
        };
        let notification = self.entries[entry_index].notification(&snapshot);
        notifications.ensure_can_push(&notification)?;
        self.ensure_wildcard_cache_budget(entry_index, &target, &snapshot.value)?;
        self.entries[entry_index]
            .wildcard_values
            .insert(target, snapshot.value);
        notifications.push_unchecked(notification);
        Ok(())
    }

    fn ensure_wildcard_cache_budget(
        &self,
        entry_index: usize,
        target: &Target,
        new_value: &str,
    ) -> Result<(), ControlFormatLimitExceeded> {
        let existing = self.entries[entry_index].wildcard_values.get(target);
        let next_targets = self
            .wildcard_target_count()
            .saturating_add(usize::from(existing.is_none()));
        if next_targets > self.limits.max_target_cache_entries {
            return Err(ControlFormatLimitExceeded::new(
                "target cache",
                self.limits.max_target_cache_entries,
                "entries",
            ));
        }

        let old_bytes = existing
            .map(|value| control_escaped_len(value))
            .unwrap_or(0);
        let next_value_bytes = self
            .retained_value_bytes()
            .saturating_sub(old_bytes)
            .saturating_add(control_escaped_len(new_value));
        if next_value_bytes > self.limits.max_retained_value_bytes {
            return Err(ControlFormatLimitExceeded::new(
                "retained values",
                self.limits.max_retained_value_bytes,
                "encoded bytes",
            ));
        }
        Ok(())
    }
}
