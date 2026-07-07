//! Revisioned pane-state event journal for SDK streams.

use std::collections::{HashMap, VecDeque};

use rmux_core::PaneId;
use rmux_proto::{
    ForegroundStateDto, PaneStateClosedReason, PaneStateEventDto, PaneStateSubscriptionId,
};

pub(crate) const PANE_STATE_JOURNAL_CAPACITY: usize = 4096;
pub(crate) const PANE_STATE_CURSOR_BATCH: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PaneStateInclude {
    pub(crate) title: bool,
    pub(crate) options: bool,
    pub(crate) foreground: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PaneStateRecord {
    pub(crate) revision: u64,
    pub(crate) pane_id: PaneId,
    pub(crate) generation: Option<u64>,
    pub(crate) change: PaneStateChange,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PaneStateChange {
    TitleChanged {
        old: String,
        new: String,
    },
    OptionSet {
        name: String,
        old: Option<String>,
        new: String,
    },
    OptionUnset {
        name: String,
        old: Option<String>,
    },
    ForegroundChanged {
        old: ForegroundStateDto,
        new: ForegroundStateDto,
    },
    Closed {
        reason: PaneStateClosedReason,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PaneStateRead {
    Ready {
        next_revision: u64,
        limited: bool,
        event_count: usize,
    },
    Lag {
        missed_from_revision: u64,
        resume_revision: u64,
    },
}

#[derive(Debug, Clone)]
struct PaneStateSubscription {
    connection_id: u64,
    pane_id: PaneId,
    include: PaneStateInclude,
    closed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PaneStateSubscriptionInfo {
    pub(crate) pane_id: PaneId,
    pub(crate) include: PaneStateInclude,
}

#[derive(Debug)]
pub(crate) struct PaneStateJournal {
    capacity: usize,
    next_revision: u64,
    next_subscription: u64,
    records: VecDeque<PaneStateRecord>,
    subscriptions: HashMap<PaneStateSubscriptionId, PaneStateSubscription>,
}

impl Default for PaneStateJournal {
    fn default() -> Self {
        Self::new(PANE_STATE_JOURNAL_CAPACITY)
    }
}

impl PaneStateJournal {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            next_revision: 0,
            next_subscription: 1,
            records: VecDeque::new(),
            subscriptions: HashMap::new(),
        }
    }

    pub(crate) const fn current_revision(&self) -> u64 {
        self.next_revision
    }

    pub(crate) fn push(
        &mut self,
        pane_id: PaneId,
        generation: Option<u64>,
        change: PaneStateChange,
    ) -> u64 {
        self.next_revision = self.next_revision.saturating_add(1);
        let revision = self.next_revision;
        self.records.push_back(PaneStateRecord {
            revision,
            pane_id,
            generation,
            change,
        });
        while self.records.len() > self.capacity {
            self.records.pop_front();
        }
        revision
    }

    pub(crate) fn subscribe(
        &mut self,
        connection_id: u64,
        pane_id: PaneId,
        include: PaneStateInclude,
    ) -> PaneStateSubscriptionId {
        let id = PaneStateSubscriptionId::new(self.next_subscription);
        self.next_subscription = self.next_subscription.saturating_add(1).max(1);
        self.subscriptions.insert(
            id,
            PaneStateSubscription {
                connection_id,
                pane_id,
                include,
                closed: false,
            },
        );
        id
    }

    pub(crate) fn unsubscribe(
        &mut self,
        connection_id: u64,
        subscription_id: PaneStateSubscriptionId,
    ) -> Result<bool, &'static str> {
        let Some(subscription) = self.subscriptions.get(&subscription_id) else {
            return Ok(false);
        };
        if subscription.connection_id != connection_id {
            return Err("subscription is not owned by this connection");
        }
        Ok(self.subscriptions.remove(&subscription_id).is_some())
    }

    pub(crate) fn remove_connection(&mut self, connection_id: u64) {
        self.subscriptions
            .retain(|_, subscription| subscription.connection_id != connection_id);
    }

    pub(crate) fn remove_closed_subscription(
        &mut self,
        connection_id: u64,
        subscription_id: PaneStateSubscriptionId,
    ) -> bool {
        let Some(subscription) = self.subscriptions.get(&subscription_id) else {
            return false;
        };
        if subscription.connection_id != connection_id || !subscription.closed {
            return false;
        }
        self.subscriptions.remove(&subscription_id).is_some()
    }

    pub(crate) fn mark_pane_closed(&mut self, pane_id: PaneId) {
        for subscription in self.subscriptions.values_mut() {
            if subscription.pane_id == pane_id {
                subscription.closed = true;
            }
        }
    }

    pub(crate) fn foreground_subscription_count(&self) -> usize {
        self.subscriptions
            .values()
            .filter(|subscription| subscription.include.foreground && !subscription.closed)
            .count()
    }

    pub(crate) fn subscription_info(
        &self,
        connection_id: u64,
        subscription_id: PaneStateSubscriptionId,
    ) -> Result<Option<PaneStateSubscriptionInfo>, &'static str> {
        let Some(subscription) = self.subscriptions.get(&subscription_id) else {
            return Ok(None);
        };
        if subscription.connection_id != connection_id {
            return Err("subscription is not owned by this connection");
        }
        Ok(Some(PaneStateSubscriptionInfo {
            pane_id: subscription.pane_id,
            include: subscription.include,
        }))
    }

    pub(crate) fn pane_ids_with_foreground_subscriptions(&self) -> Vec<PaneId> {
        let mut pane_ids = self
            .subscriptions
            .values()
            .filter(|subscription| subscription.include.foreground && !subscription.closed)
            .map(|subscription| subscription.pane_id)
            .collect::<Vec<_>>();
        pane_ids.sort_by_key(|pane_id| pane_id.as_u32());
        pane_ids.dedup();
        pane_ids
    }

    pub(crate) fn read_after(
        &self,
        connection_id: u64,
        subscription_id: PaneStateSubscriptionId,
        after_revision: u64,
        max_events: usize,
        output: &mut Vec<PaneStateEventDto>,
    ) -> Result<PaneStateRead, &'static str> {
        let Some(subscription) = self.subscriptions.get(&subscription_id) else {
            return Err("subscription not found");
        };
        if subscription.connection_id != connection_id {
            return Err("subscription is not owned by this connection");
        }
        if let Some(oldest) = self.records.front().map(|record| record.revision) {
            if after_revision.saturating_add(1) < oldest && self.next_revision > after_revision {
                return Ok(PaneStateRead::Lag {
                    missed_from_revision: after_revision,
                    resume_revision: oldest,
                });
            }
        }

        let limit = max_events.max(1);
        for record in self.records.iter().filter(|record| {
            record.revision > after_revision
                && record.pane_id == subscription.pane_id
                && record_matches_include(record, subscription.include)
        }) {
            output.push(record_to_dto(record));
            if output.len() >= limit {
                let next_revision = output_revision(output).unwrap_or(after_revision);
                return Ok(PaneStateRead::Ready {
                    next_revision,
                    limited: true,
                    event_count: output.len(),
                });
            }
        }

        Ok(PaneStateRead::Ready {
            next_revision: self.next_revision.max(after_revision),
            limited: false,
            event_count: output.len(),
        })
    }
}

fn record_matches_include(record: &PaneStateRecord, include: PaneStateInclude) -> bool {
    match record.change {
        PaneStateChange::TitleChanged { .. } => include.title,
        PaneStateChange::OptionSet { .. } | PaneStateChange::OptionUnset { .. } => include.options,
        PaneStateChange::ForegroundChanged { .. } => include.foreground,
        PaneStateChange::Closed { .. } => true,
    }
}

fn record_to_dto(record: &PaneStateRecord) -> PaneStateEventDto {
    match &record.change {
        PaneStateChange::TitleChanged { old, new } => PaneStateEventDto::TitleChanged {
            revision: record.revision,
            pane_id: record.pane_id,
            old_title: old.clone(),
            new_title: new.clone(),
        },
        PaneStateChange::OptionSet { name, old, new } => PaneStateEventDto::OptionSet {
            revision: record.revision,
            pane_id: record.pane_id,
            name: name.clone(),
            old_value: old.clone(),
            new_value: new.clone(),
        },
        PaneStateChange::OptionUnset { name, old } => PaneStateEventDto::OptionUnset {
            revision: record.revision,
            pane_id: record.pane_id,
            name: name.clone(),
            old_value: old.clone(),
        },
        PaneStateChange::ForegroundChanged { old, new } => PaneStateEventDto::ForegroundChanged {
            revision: record.revision,
            pane_id: record.pane_id,
            old_state: old.clone(),
            new_state: new.clone(),
        },
        PaneStateChange::Closed { reason } => PaneStateEventDto::Closed {
            revision: record.revision,
            pane_id: record.pane_id,
            reason: *reason,
        },
    }
}

fn output_revision(events: &[PaneStateEventDto]) -> Option<u64> {
    events.last().map(|event| match event {
        PaneStateEventDto::TitleChanged { revision, .. }
        | PaneStateEventDto::OptionSet { revision, .. }
        | PaneStateEventDto::OptionUnset { revision, .. }
        | PaneStateEventDto::ForegroundChanged { revision, .. }
        | PaneStateEventDto::Closed { revision, .. } => *revision,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane_id(value: u32) -> PaneId {
        PaneId::new(value)
    }

    #[test]
    fn revisions_are_global_and_strictly_increasing() {
        let mut journal = PaneStateJournal::new(8);
        assert_eq!(
            journal.push(
                pane_id(1),
                Some(1),
                PaneStateChange::TitleChanged {
                    old: "a".to_owned(),
                    new: "b".to_owned(),
                },
            ),
            1
        );
        assert_eq!(
            journal.push(
                pane_id(2),
                Some(1),
                PaneStateChange::OptionUnset {
                    name: "@x".to_owned(),
                    old: Some("1".to_owned()),
                },
            ),
            2
        );
    }

    #[test]
    fn subscription_filters_by_pane_and_include_mask() {
        let mut journal = PaneStateJournal::new(8);
        let subscription = journal.subscribe(
            7,
            pane_id(1),
            PaneStateInclude {
                title: true,
                options: false,
                foreground: false,
            },
        );
        journal.push(
            pane_id(1),
            Some(1),
            PaneStateChange::OptionSet {
                name: "@x".to_owned(),
                old: None,
                new: "1".to_owned(),
            },
        );
        journal.push(
            pane_id(2),
            Some(1),
            PaneStateChange::TitleChanged {
                old: "a".to_owned(),
                new: "b".to_owned(),
            },
        );
        journal.push(
            pane_id(1),
            Some(1),
            PaneStateChange::TitleChanged {
                old: "a".to_owned(),
                new: "b".to_owned(),
            },
        );

        let mut events = Vec::new();
        let read = journal
            .read_after(7, subscription, 0, 16, &mut events)
            .expect("read should succeed");
        assert_eq!(
            read,
            PaneStateRead::Ready {
                next_revision: 3,
                limited: false,
                event_count: 1,
            }
        );
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn empty_filtered_read_advances_to_current_revision() {
        let mut journal = PaneStateJournal::new(4);
        let subscription = journal.subscribe(
            7,
            pane_id(1),
            PaneStateInclude {
                title: true,
                options: false,
                foreground: false,
            },
        );
        journal.push(
            pane_id(1),
            Some(1),
            PaneStateChange::OptionSet {
                name: "@x".to_owned(),
                old: None,
                new: "1".to_owned(),
            },
        );
        journal.push(
            pane_id(2),
            Some(1),
            PaneStateChange::TitleChanged {
                old: "a".to_owned(),
                new: "b".to_owned(),
            },
        );

        let mut events = Vec::new();
        assert_eq!(
            journal.read_after(7, subscription, 0, 16, &mut events),
            Ok(PaneStateRead::Ready {
                next_revision: 2,
                limited: false,
                event_count: 0,
            })
        );
        assert!(events.is_empty());
    }

    #[test]
    fn stale_cursor_reports_lag() {
        let mut journal = PaneStateJournal::new(2);
        let subscription = journal.subscribe(
            7,
            pane_id(1),
            PaneStateInclude {
                title: true,
                options: true,
                foreground: false,
            },
        );
        for index in 0..4 {
            journal.push(
                pane_id(1),
                Some(1),
                PaneStateChange::TitleChanged {
                    old: index.to_string(),
                    new: (index + 1).to_string(),
                },
            );
        }
        let mut events = Vec::new();
        assert_eq!(
            journal.read_after(7, subscription, 0, 16, &mut events),
            Ok(PaneStateRead::Lag {
                missed_from_revision: 0,
                resume_revision: 3,
            })
        );
    }

    #[test]
    fn closed_pane_stops_foreground_watch_without_hiding_closed_event() {
        let mut journal = PaneStateJournal::new(8);
        let subscription = journal.subscribe(
            7,
            pane_id(1),
            PaneStateInclude {
                title: false,
                options: false,
                foreground: true,
            },
        );
        assert_eq!(journal.foreground_subscription_count(), 1);

        journal.push(
            pane_id(1),
            Some(9),
            PaneStateChange::Closed {
                reason: PaneStateClosedReason::Killed,
            },
        );
        journal.mark_pane_closed(pane_id(1));

        assert_eq!(journal.foreground_subscription_count(), 0);
        assert!(journal.pane_ids_with_foreground_subscriptions().is_empty());

        let mut events = Vec::new();
        assert_eq!(
            journal.read_after(7, subscription, 0, 16, &mut events),
            Ok(PaneStateRead::Ready {
                next_revision: 1,
                limited: false,
                event_count: 1,
            })
        );
        assert!(matches!(
            events.as_slice(),
            [PaneStateEventDto::Closed {
                reason: PaneStateClosedReason::Killed,
                ..
            }]
        ));
        assert!(journal.remove_closed_subscription(7, subscription));
        assert_eq!(
            journal.read_after(7, subscription, 1, 16, &mut Vec::new()),
            Err("subscription not found")
        );
    }
}
