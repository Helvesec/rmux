use std::collections::HashMap;

use rmux_proto::{PaneId, SessionId, WindowId};

use super::{
    control_escaped_len, render_subscription_value, ControlFormatLimitExceeded,
    ControlFormatNotificationBatch, ControlFormatSelector, ControlFormatSnapshot,
    ControlFormatSubscriptions, HandlerState, Pane, SubscriptionContext, Window,
};
use crate::pane_terminals::PaneLifecycleProcessState;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Target {
    // An explicit ID already identifies the object across session switches.
    IdAtIndex(u32),
    Wildcard(Occurrence),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Occurrence {
    session: SessionId,
    window: WindowId,
    index: u32,
    pane: Option<PaneId>,
}

#[derive(Debug, Clone, Default)]
pub(super) struct TargetValues(HashMap<Target, String>);

impl TargetValues {
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

    // As in tmux, wildcard baselines follow live occurrences. Explicit IDs remember each index.
    pub(super) fn prune(&mut self, state: &HandlerState) {
        self.0.retain(|target, _| {
            let Target::Wildcard(target) = target else {
                return true;
            };
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

struct TargetView<'a> {
    index: u32,
    window: &'a Window,
    pane: Option<&'a Pane>,
}

impl ControlFormatSubscriptions {
    pub(super) fn changed_target_notifications(
        &mut self,
        entry_index: usize,
        selector: ControlFormatSelector,
        state: &HandlerState,
        context: SubscriptionContext<'_>,
        notifications: &mut ControlFormatNotificationBatch,
    ) -> Result<(), ControlFormatLimitExceeded> {
        if let ControlFormatSelector::PaneId(id) = selector {
            if !state.pane_lifecycle(PaneId::new(id)).is_some_and(|pane| {
                matches!(pane.process, PaneLifecycleProcessState::Running { .. })
            }) {
                return Ok(());
            }
        }
        for (index, window) in context.session.windows() {
            let mut visit = |pane| {
                self.target_change(
                    entry_index,
                    state,
                    TargetView {
                        index,
                        window,
                        pane,
                    },
                    context,
                    notifications,
                )
            };
            match selector {
                ControlFormatSelector::AllPanes => {
                    for pane in window.panes() {
                        visit(Some(pane))?;
                    }
                }
                ControlFormatSelector::PaneId(id) => {
                    if let Some(pane) = window.panes().iter().find(|pane| pane.id().as_u32() == id)
                    {
                        visit(Some(pane))?;
                    }
                }
                ControlFormatSelector::AllWindows => visit(None)?,
                ControlFormatSelector::WindowId(id) if window.id().as_u32() == id => visit(None)?,
                _ => {}
            }
        }
        Ok(())
    }

    fn target_change(
        &mut self,
        entry_index: usize,
        state: &HandlerState,
        view: TargetView<'_>,
        context: SubscriptionContext<'_>,
        notifications: &mut ControlFormatNotificationBatch,
    ) -> Result<(), ControlFormatLimitExceeded> {
        let TargetView {
            index,
            window,
            pane,
        } = view;
        let session = context.session;
        let target = match self.entries[entry_index].selector {
            ControlFormatSelector::WindowId(_) | ControlFormatSelector::PaneId(_) => {
                Target::IdAtIndex(index)
            }
            _ => Target::Wildcard(Occurrence {
                session: session.id(),
                window: window.id(),
                index,
                pane: pane.map(Pane::id),
            }),
        };
        let value = render_subscription_value(
            state,
            context,
            index,
            window,
            pane.or_else(|| window.active_pane()),
            (
                &self.entries[entry_index].template,
                &self.entries[entry_index].jobs,
            ),
        );
        if self.entries[entry_index].target_values.get(&target) == Some(&value) {
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
        self.ensure_target_cache_budget(entry_index, &target, &snapshot.value)?;
        self.entries[entry_index]
            .target_values
            .insert(target, snapshot.value);
        notifications.push_unchecked(notification);
        Ok(())
    }

    fn ensure_target_cache_budget(
        &self,
        entry_index: usize,
        target: &Target,
        new_value: &str,
    ) -> Result<(), ControlFormatLimitExceeded> {
        let existing = self.entries[entry_index].target_values.get(target);
        let next_targets = self
            .target_count()
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
