//! Revisioned pane-state event journal for SDK streams.

use std::collections::{HashMap, HashSet, VecDeque};

use rmux_core::events::{SubscriptionLimitError, SubscriptionLimits};
use rmux_core::PaneId;
use rmux_proto::{
    ForegroundStateDto, PaneStateClosedReason, PaneStateEventDto, PaneStateSubscriptionId,
    DEFAULT_MAX_DETACHED_FRAME_LENGTH,
};

#[path = "pane_state_journal/retention.rs"]
mod retention;

use retention::retained_record_bytes;

pub(crate) const PANE_STATE_JOURNAL_CAPACITY: usize = 4096;
pub(crate) const PANE_STATE_CURSOR_BATCH: usize = 256;
pub(crate) const PANE_STATE_JOURNAL_BYTE_CAPACITY: usize = DEFAULT_MAX_DETACHED_FRAME_LENGTH / 2;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PaneStateSubscriptionError {
    Limit(SubscriptionLimitError),
    Capacity { limit: usize },
}

#[derive(Debug, Clone)]
struct PaneStateSubscription {
    connection_id: u64,
    pane_id: PaneId,
    include: PaneStateInclude,
    generation: Option<u64>,
    closed: bool,
    closed_revision: Option<u64>,
    closed_reason: Option<PaneStateClosedReason>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct EvictedPaneStateRevisions {
    title: u64,
    options: u64,
    foreground: u64,
    closed: u64,
}

impl EvictedPaneStateRevisions {
    fn record(&mut self, record: &PaneStateRecord) {
        let revision = record.revision;
        match record.change {
            PaneStateChange::TitleChanged { .. } => {
                self.title = self.title.max(revision);
            }
            PaneStateChange::OptionSet { .. } | PaneStateChange::OptionUnset { .. } => {
                self.options = self.options.max(revision);
            }
            PaneStateChange::ForegroundChanged { .. } => {
                self.foreground = self.foreground.max(revision);
            }
            PaneStateChange::Closed { .. } => {
                self.closed = self.closed.max(revision);
            }
        }
    }

    fn max_matching(self, include: PaneStateInclude) -> u64 {
        self.closed.max(self.max_matching_state_change(include))
    }

    fn max_matching_state_change(self, include: PaneStateInclude) -> u64 {
        let mut revision = 0;
        if include.title {
            revision = revision.max(self.title);
        }
        if include.options {
            revision = revision.max(self.options);
        }
        if include.foreground {
            revision = revision.max(self.foreground);
        }
        revision
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PaneStateSubscriptionInfo {
    pub(crate) pane_id: PaneId,
    pub(crate) include: PaneStateInclude,
    pub(crate) generation: Option<u64>,
    pub(crate) closed: bool,
    pub(crate) closed_revision: Option<u64>,
}

#[derive(Debug)]
pub(crate) struct PaneStateJournal {
    capacity: usize,
    byte_capacity: usize,
    retained_bytes: usize,
    next_revision: u64,
    next_subscription: u64,
    limits: SubscriptionLimits,
    records: VecDeque<PaneStateRecord>,
    retained_record_counts: HashMap<PaneId, usize>,
    evicted_revisions: HashMap<PaneId, EvictedPaneStateRevisions>,
    subscriptions: HashMap<PaneStateSubscriptionId, PaneStateSubscription>,
    subscription_counts: HashMap<PaneId, usize>,
    closed_panes: HashSet<PaneId>,
    closed_pane_order: VecDeque<PaneId>,
}

impl Default for PaneStateJournal {
    fn default() -> Self {
        Self::new(PANE_STATE_JOURNAL_CAPACITY)
    }
}

impl PaneStateJournal {
    pub(crate) fn new(capacity: usize) -> Self {
        Self::with_limits(capacity, SubscriptionLimits::default())
    }

    pub(crate) fn with_limits(capacity: usize, limits: SubscriptionLimits) -> Self {
        Self::with_limits_and_byte_capacity(capacity, PANE_STATE_JOURNAL_BYTE_CAPACITY, limits)
    }

    fn with_limits_and_byte_capacity(
        capacity: usize,
        byte_capacity: usize,
        limits: SubscriptionLimits,
    ) -> Self {
        Self {
            capacity: capacity.max(1),
            byte_capacity: byte_capacity.max(1),
            retained_bytes: 0,
            next_revision: 0,
            next_subscription: 1,
            limits,
            records: VecDeque::new(),
            retained_record_counts: HashMap::new(),
            evicted_revisions: HashMap::new(),
            subscriptions: HashMap::new(),
            subscription_counts: HashMap::new(),
            closed_panes: HashSet::new(),
            closed_pane_order: VecDeque::new(),
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
        let record = PaneStateRecord {
            revision,
            pane_id,
            generation,
            change,
        };
        self.retained_bytes = self
            .retained_bytes
            .saturating_add(retained_record_bytes(&record));
        self.records.push_back(record);
        increment_count(&mut self.retained_record_counts, pane_id);
        while self.records.len() > self.capacity || self.retained_bytes > self.byte_capacity {
            if let Some(record) = self.records.pop_front() {
                self.retained_bytes = self
                    .retained_bytes
                    .saturating_sub(retained_record_bytes(&record));
                decrement_count(&mut self.retained_record_counts, record.pane_id);
                self.record_eviction(&record);
                self.prune_evicted_revision_for(record.pane_id);
            }
        }
        debug_assert!(self.retained_bytes <= self.byte_capacity);
        revision
    }

    fn record_eviction(&mut self, record: &PaneStateRecord) {
        self.evicted_revisions
            .entry(record.pane_id)
            .or_default()
            .record(record);
    }

    fn prune_evicted_revision_for(&mut self, pane_id: PaneId) {
        if !self.retained_record_counts.contains_key(&pane_id)
            && !self.subscription_counts.contains_key(&pane_id)
        {
            self.evicted_revisions.remove(&pane_id);
        }
    }

    fn prune_closed_panes(&mut self) {
        while self.closed_panes.len() > self.capacity {
            let Some(pane_id) = self.closed_pane_order.pop_front() else {
                break;
            };
            self.closed_panes.remove(&pane_id);
        }
    }

    #[cfg(test)]
    pub(crate) fn subscribe(
        &mut self,
        connection_id: u64,
        pane_id: PaneId,
        include: PaneStateInclude,
    ) -> Result<PaneStateSubscriptionId, PaneStateSubscriptionError> {
        self.subscribe_at_generation(connection_id, pane_id, include, None)
    }

    pub(crate) fn subscribe_at_generation(
        &mut self,
        connection_id: u64,
        pane_id: PaneId,
        include: PaneStateInclude,
        generation: Option<u64>,
    ) -> Result<PaneStateSubscriptionId, PaneStateSubscriptionError> {
        let connection_count = self
            .subscriptions
            .values()
            .filter(|subscription| subscription.connection_id == connection_id)
            .count();
        if connection_count >= self.limits.max_per_connection() {
            return Err(PaneStateSubscriptionError::Limit(
                SubscriptionLimitError::PerConnection {
                    limit: self.limits.max_per_connection(),
                },
            ));
        }

        let pane_count = self
            .subscription_counts
            .get(&pane_id)
            .copied()
            .unwrap_or_default();
        if pane_count >= self.limits.max_per_pane() {
            return Err(PaneStateSubscriptionError::Limit(
                SubscriptionLimitError::PerPane {
                    limit: self.limits.max_per_pane(),
                },
            ));
        }

        if self.subscriptions.len() >= self.capacity {
            return Err(PaneStateSubscriptionError::Capacity {
                limit: self.capacity,
            });
        }

        let id = PaneStateSubscriptionId::new(self.next_subscription);
        self.next_subscription = self.next_subscription.saturating_add(1).max(1);
        self.subscriptions.insert(
            id,
            PaneStateSubscription {
                connection_id,
                pane_id,
                include,
                generation,
                closed: false,
                closed_revision: None,
                closed_reason: None,
            },
        );
        increment_count(&mut self.subscription_counts, pane_id);
        Ok(id)
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
        let removed = self.remove_subscription_entry(subscription_id).is_some();
        Ok(removed)
    }

    pub(crate) fn remove_connection(&mut self, connection_id: u64) {
        let subscription_ids = self
            .subscriptions
            .iter()
            .filter_map(|(id, subscription)| {
                (subscription.connection_id == connection_id).then_some(*id)
            })
            .collect::<Vec<_>>();
        for subscription_id in subscription_ids {
            self.remove_subscription_entry(subscription_id);
        }
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
        self.remove_subscription_entry(subscription_id).is_some()
    }

    pub(crate) fn mark_pane_closed(&mut self, pane_id: PaneId) -> bool {
        let newly_closed = self.closed_panes.insert(pane_id);
        if newly_closed {
            self.closed_pane_order.push_back(pane_id);
            self.prune_closed_panes();
        }
        let has_open_subscription = self
            .subscriptions
            .values()
            .any(|subscription| subscription.pane_id == pane_id && !subscription.closed);
        let should_record_close = newly_closed || has_open_subscription;
        if should_record_close {
            for subscription in self.subscriptions.values_mut() {
                if subscription.pane_id == pane_id && !subscription.closed {
                    subscription.closed = true;
                }
            }
        }
        should_record_close
    }

    pub(crate) fn remember_pane_closed_event(
        &mut self,
        pane_id: PaneId,
        reason: PaneStateClosedReason,
        revision: u64,
    ) {
        for subscription in self.subscriptions.values_mut() {
            if subscription.pane_id == pane_id
                && subscription.closed
                && subscription.closed_revision.is_none()
            {
                subscription.closed_revision = Some(revision);
                subscription.closed_reason = Some(reason);
            }
        }
    }

    pub(crate) fn reopen_pane(&mut self, pane_id: PaneId) {
        if self.closed_panes.remove(&pane_id) {
            self.closed_pane_order.retain(|closed| *closed != pane_id);
        }
    }

    pub(crate) fn foreground_subscription_count(&self) -> usize {
        self.subscriptions
            .values()
            .filter(|subscription| subscription.include.foreground && !subscription.closed)
            .count()
    }

    pub(crate) fn title_subscription_count(&self) -> usize {
        self.subscriptions
            .values()
            .filter(|subscription| subscription.include.title && !subscription.closed)
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
            generation: subscription.generation,
            closed: subscription.closed,
            closed_revision: subscription.closed_revision,
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
        let limit = max_events.max(1);
        if subscription.closed {
            if let Some(read) = self.closed_relevant_lag(subscription, after_revision) {
                return Ok(read);
            }
            let closed_revision = subscription.closed_revision;
            for record in self.records.iter().filter(|record| {
                record.revision > after_revision
                    && closed_revision.is_none_or(|revision| record.revision <= revision)
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
            if !output.is_empty() {
                let next_revision = output_revision(output).unwrap_or(after_revision);
                return Ok(PaneStateRead::Ready {
                    next_revision,
                    limited: false,
                    event_count: output.len(),
                });
            }
            if let (Some(revision), Some(reason)) =
                (subscription.closed_revision, subscription.closed_reason)
            {
                // Closed is terminal delivery state, not merely a cursor
                // delta. A lag rebase snapshots current state and may advance
                // the client's cursor past this revision, but the subscription
                // must still deliver its one terminal event before removal.
                output.push(PaneStateEventDto::Closed {
                    revision,
                    pane_id: subscription.pane_id,
                    reason,
                });
                return Ok(PaneStateRead::Ready {
                    next_revision: after_revision.max(revision),
                    limited: false,
                    event_count: output.len(),
                });
            }
            return Ok(PaneStateRead::Ready {
                next_revision: after_revision,
                limited: false,
                event_count: 0,
            });
        } else if let Some(read) = self.relevant_lag(subscription, after_revision) {
            return Ok(read);
        }

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

    fn relevant_lag(
        &self,
        subscription: &PaneStateSubscription,
        after_revision: u64,
    ) -> Option<PaneStateRead> {
        let evicted_revision = self
            .evicted_revisions
            .get(&subscription.pane_id)?
            .max_matching(subscription.include);
        if evicted_revision == 0 || after_revision >= evicted_revision {
            return None;
        }
        Some(PaneStateRead::Lag {
            missed_from_revision: after_revision,
            resume_revision: self.oldest_matching_revision_after(subscription, after_revision),
        })
    }

    fn closed_relevant_lag(
        &self,
        subscription: &PaneStateSubscription,
        after_revision: u64,
    ) -> Option<PaneStateRead> {
        let evicted_revision = self
            .evicted_revisions
            .get(&subscription.pane_id)?
            .max_matching_state_change(subscription.include);
        // The terminal Closed record is synthesized by read_after even after
        // eviction, so only evictions strictly before the close require a
        // snapshot rebase; gating on evicted post-close revisions would
        // re-trigger Lag forever once the cursor parks at closed_revision - 1.
        let evicted_revision = match subscription.closed_revision {
            Some(closed_revision) => evicted_revision.min(closed_revision.saturating_sub(1)),
            None => evicted_revision,
        };
        if evicted_revision == 0 || after_revision >= evicted_revision {
            return None;
        }
        if subscription
            .closed_revision
            .is_some_and(|revision| after_revision >= revision)
        {
            return None;
        }
        Some(PaneStateRead::Lag {
            missed_from_revision: after_revision,
            resume_revision: self.oldest_matching_revision_after_bounded(
                subscription,
                after_revision,
                subscription.closed_revision,
            ),
        })
    }

    fn oldest_matching_revision_after(
        &self,
        subscription: &PaneStateSubscription,
        after_revision: u64,
    ) -> u64 {
        self.oldest_matching_revision_after_bounded(subscription, after_revision, None)
    }

    fn oldest_matching_revision_after_bounded(
        &self,
        subscription: &PaneStateSubscription,
        after_revision: u64,
        upper_revision: Option<u64>,
    ) -> u64 {
        self.records
            .iter()
            .find(|record| {
                record.revision > after_revision
                    && upper_revision.is_none_or(|revision| record.revision <= revision)
                    && record.pane_id == subscription.pane_id
                    && record_matches_include(record, subscription.include)
            })
            .map_or_else(
                || upper_revision.unwrap_or_else(|| self.next_revision.max(after_revision)),
                |record| record.revision,
            )
    }

    fn remove_subscription_entry(
        &mut self,
        subscription_id: PaneStateSubscriptionId,
    ) -> Option<PaneStateSubscription> {
        let subscription = self.subscriptions.remove(&subscription_id)?;
        decrement_count(&mut self.subscription_counts, subscription.pane_id);
        self.prune_evicted_revision_for(subscription.pane_id);
        Some(subscription)
    }
}

fn increment_count(counts: &mut HashMap<PaneId, usize>, pane_id: PaneId) {
    *counts.entry(pane_id).or_insert(0) += 1;
}

fn decrement_count(counts: &mut HashMap<PaneId, usize>, pane_id: PaneId) {
    if let Some(count) = counts.get_mut(&pane_id) {
        *count = count.saturating_sub(1);
        if *count == 0 {
            counts.remove(&pane_id);
        }
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
        _ => unreachable!("unknown pane-state event variant from this rmux-proto version"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane_id(value: u32) -> PaneId {
        PaneId::new(value)
    }

    fn close_pane(journal: &mut PaneStateJournal, pane_id: PaneId) -> u64 {
        assert!(journal.mark_pane_closed(pane_id));
        let revision = journal.push(
            pane_id,
            Some(9),
            PaneStateChange::Closed {
                reason: PaneStateClosedReason::Killed,
            },
        );
        journal.remember_pane_closed_event(pane_id, PaneStateClosedReason::Killed, revision);
        revision
    }

    #[test]
    fn journal_byte_budget_evicts_oversized_records_and_preserves_cursor_progress() {
        let watched = pane_id(41);
        let mut journal =
            PaneStateJournal::with_limits_and_byte_capacity(8, 512, SubscriptionLimits::default());
        let subscription = journal
            .subscribe(
                7,
                watched,
                PaneStateInclude {
                    title: true,
                    options: false,
                    foreground: false,
                },
            )
            .expect("subscription fits");

        let oversized_revision = journal.push(
            watched,
            Some(1),
            PaneStateChange::TitleChanged {
                old: "a".repeat(512),
                new: "b".repeat(512),
            },
        );
        assert_eq!(oversized_revision, 1);
        assert!(journal.records.is_empty());
        assert_eq!(journal.retained_bytes, 0);

        let mut events = Vec::new();
        assert_eq!(
            journal
                .read_after(7, subscription, 0, 8, &mut events)
                .expect("lag is reported"),
            PaneStateRead::Lag {
                missed_from_revision: 0,
                resume_revision: 1,
            }
        );
        assert!(events.is_empty());

        journal.push(
            watched,
            Some(1),
            PaneStateChange::TitleChanged {
                old: "b".to_owned(),
                new: "c".to_owned(),
            },
        );
        assert!(journal.retained_bytes <= journal.byte_capacity);
        assert!(matches!(
            journal
                .read_after(7, subscription, 1, 8, &mut events)
                .expect("cursor progresses after rebase"),
            PaneStateRead::Ready { event_count: 1, .. }
        ));
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
        let subscription = journal
            .subscribe(
                7,
                pane_id(1),
                PaneStateInclude {
                    title: true,
                    options: false,
                    foreground: false,
                },
            )
            .expect("subscription within limits");
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
        let subscription = journal
            .subscribe(
                7,
                pane_id(1),
                PaneStateInclude {
                    title: true,
                    options: false,
                    foreground: false,
                },
            )
            .expect("subscription within limits");
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
        let subscription = journal
            .subscribe(
                7,
                pane_id(1),
                PaneStateInclude {
                    title: true,
                    options: true,
                    foreground: false,
                },
            )
            .expect("subscription within limits");
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
        let subscription = journal
            .subscribe(
                7,
                pane_id(1),
                PaneStateInclude {
                    title: false,
                    options: false,
                    foreground: true,
                },
            )
            .expect("subscription within limits");
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

    #[test]
    fn unread_closed_subscriptions_count_toward_connection_and_pane_limits() {
        let mut journal = PaneStateJournal::with_limits(
            8,
            SubscriptionLimits::new(1, 1, 16, std::time::Duration::from_secs(60)),
        );
        let include = PaneStateInclude {
            title: false,
            options: false,
            foreground: true,
        };
        let first = journal
            .subscribe(7, pane_id(1), include)
            .expect("first subscription fits the limit");
        close_pane(&mut journal, pane_id(1));

        assert_eq!(
            journal.subscribe(7, pane_id(2), include),
            Err(PaneStateSubscriptionError::Limit(
                SubscriptionLimitError::PerConnection { limit: 1 }
            ))
        );
        assert_eq!(
            journal.subscribe(8, pane_id(1), include),
            Err(PaneStateSubscriptionError::Limit(
                SubscriptionLimitError::PerPane { limit: 1 }
            ))
        );

        assert_eq!(journal.unsubscribe(7, first), Ok(true));
        let replacement = journal
            .subscribe(7, pane_id(1), include)
            .expect("unsubscribe must release connection and pane quota");
        close_pane(&mut journal, pane_id(1));
        journal.remove_connection(7);
        journal
            .subscribe(8, pane_id(1), include)
            .expect("disconnect must release connection and pane quota");
        assert_ne!(replacement, first);
    }

    #[test]
    fn capacity_rejects_n_plus_one_and_retains_oldest_closed_until_delivery() {
        let mut journal = PaneStateJournal::with_limits(
            2,
            SubscriptionLimits::new(8, 8, 16, std::time::Duration::from_secs(60)),
        );
        let include = PaneStateInclude {
            title: false,
            options: false,
            foreground: true,
        };

        let oldest = journal
            .subscribe(7, pane_id(1), include)
            .expect("first subscription fits capacity");
        let oldest_closed_revision = close_pane(&mut journal, pane_id(1));
        journal
            .subscribe(8, pane_id(2), include)
            .expect("Nth subscription fits capacity");
        close_pane(&mut journal, pane_id(2));

        for pane in 3..=4 {
            journal.push(
                pane_id(pane),
                Some(9),
                PaneStateChange::TitleChanged {
                    old: "old".to_owned(),
                    new: "new".to_owned(),
                },
            );
        }

        assert_eq!(
            journal.subscribe(9, pane_id(3), include),
            Err(PaneStateSubscriptionError::Capacity { limit: 2 })
        );
        assert_eq!(journal.subscriptions.len(), 2);

        let mut events = Vec::new();
        assert_eq!(
            journal.read_after(7, oldest, 0, 16, &mut events),
            Ok(PaneStateRead::Ready {
                next_revision: oldest_closed_revision,
                limited: false,
                event_count: 1,
            })
        );
        assert!(matches!(
            events.as_slice(),
            [PaneStateEventDto::Closed {
                revision,
                reason: PaneStateClosedReason::Killed,
                ..
            }] if *revision == oldest_closed_revision
        ));

        assert!(journal.remove_closed_subscription(7, oldest));
        journal
            .subscribe(9, pane_id(3), include)
            .expect("delivering Closed must release global capacity");
        assert_eq!(journal.subscriptions.len(), 2);
    }

    #[test]
    fn evicted_revisions_drop_panes_without_records_or_subscriptions() {
        let mut journal = PaneStateJournal::new(1);
        journal.push(
            pane_id(1),
            Some(1),
            PaneStateChange::TitleChanged {
                old: "a".to_owned(),
                new: "b".to_owned(),
            },
        );
        journal.push(
            pane_id(2),
            Some(1),
            PaneStateChange::TitleChanged {
                old: "c".to_owned(),
                new: "d".to_owned(),
            },
        );

        assert!(!journal.evicted_revisions.contains_key(&pane_id(1)));
        assert!(journal.evicted_revisions.is_empty());
    }

    #[test]
    fn evicted_revisions_keep_subscribed_panes_without_records() {
        let mut journal = PaneStateJournal::new(1);
        let subscription = journal
            .subscribe(
                7,
                pane_id(1),
                PaneStateInclude {
                    title: true,
                    options: false,
                    foreground: false,
                },
            )
            .expect("subscription within limits");
        journal.push(
            pane_id(1),
            Some(1),
            PaneStateChange::TitleChanged {
                old: "a".to_owned(),
                new: "b".to_owned(),
            },
        );
        journal.push(
            pane_id(2),
            Some(1),
            PaneStateChange::TitleChanged {
                old: "c".to_owned(),
                new: "d".to_owned(),
            },
        );

        assert!(journal.evicted_revisions.contains_key(&pane_id(1)));
        assert_eq!(
            journal.read_after(7, subscription, 0, 16, &mut Vec::new()),
            Ok(PaneStateRead::Lag {
                missed_from_revision: 0,
                resume_revision: 2,
            })
        );
    }

    #[test]
    fn closed_pane_tracking_is_bounded_by_journal_capacity() {
        let mut journal = PaneStateJournal::new(2);

        assert!(journal.mark_pane_closed(pane_id(1)));
        assert!(journal.mark_pane_closed(pane_id(2)));
        assert!(journal.mark_pane_closed(pane_id(3)));

        assert_eq!(journal.closed_panes.len(), 2);
        assert!(!journal.closed_panes.contains(&pane_id(1)));
        assert!(journal.closed_panes.contains(&pane_id(2)));
        assert!(journal.closed_panes.contains(&pane_id(3)));
    }

    #[test]
    fn closed_subscription_reads_retained_closed_event_before_reporting_global_lag() {
        let mut journal = PaneStateJournal::new(2);
        let subscription = journal
            .subscribe(
                7,
                pane_id(1),
                PaneStateInclude {
                    title: true,
                    options: true,
                    foreground: true,
                },
            )
            .expect("subscription within limits");

        for index in 0..4 {
            journal.push(
                pane_id(2),
                Some(1),
                PaneStateChange::TitleChanged {
                    old: index.to_string(),
                    new: (index + 1).to_string(),
                },
            );
        }
        journal.push(
            pane_id(1),
            Some(9),
            PaneStateChange::Closed {
                reason: PaneStateClosedReason::Killed,
            },
        );
        journal.mark_pane_closed(pane_id(1));

        let mut events = Vec::new();
        assert_eq!(
            journal.read_after(7, subscription, 0, 16, &mut events),
            Ok(PaneStateRead::Ready {
                next_revision: 5,
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
    }

    #[test]
    fn closed_subscription_reports_pane_scoped_lag_before_retained_closed_suffix() {
        let mut journal = PaneStateJournal::new(1);
        let subscription = journal
            .subscribe(
                7,
                pane_id(1),
                PaneStateInclude {
                    title: true,
                    options: false,
                    foreground: false,
                },
            )
            .expect("subscription within limits");

        for index in 0..5 {
            journal.push(
                pane_id(1),
                Some(1),
                PaneStateChange::TitleChanged {
                    old: index.to_string(),
                    new: (index + 1).to_string(),
                },
            );
        }
        let closed_revision = journal.push(
            pane_id(1),
            Some(1),
            PaneStateChange::Closed {
                reason: PaneStateClosedReason::Killed,
            },
        );
        journal.mark_pane_closed(pane_id(1));
        journal.remember_pane_closed_event(
            pane_id(1),
            PaneStateClosedReason::Killed,
            closed_revision,
        );

        let mut events = Vec::new();
        assert_eq!(
            journal.read_after(7, subscription, 0, 16, &mut events),
            Ok(PaneStateRead::Lag {
                missed_from_revision: 0,
                resume_revision: closed_revision,
            })
        );
        assert!(events.is_empty());

        let mut events = Vec::new();
        assert_eq!(
            journal.read_after(7, subscription, closed_revision - 1, 16, &mut events),
            Ok(PaneStateRead::Ready {
                next_revision: closed_revision,
                limited: false,
                event_count: 1,
            })
        );
        assert!(matches!(
            events.as_slice(),
            [PaneStateEventDto::Closed {
                revision,
                reason: PaneStateClosedReason::Killed,
                ..
            }] if *revision == closed_revision
        ));
    }

    #[test]
    fn closed_subscription_synthesizes_closed_after_record_eviction() {
        let mut journal = PaneStateJournal::new(2);
        let subscription = journal
            .subscribe(
                7,
                pane_id(1),
                PaneStateInclude {
                    title: true,
                    options: false,
                    foreground: false,
                },
            )
            .expect("subscription within limits");

        let closed_revision = journal.push(
            pane_id(1),
            Some(1),
            PaneStateChange::Closed {
                reason: PaneStateClosedReason::Killed,
            },
        );
        assert!(journal.mark_pane_closed(pane_id(1)));
        journal.remember_pane_closed_event(
            pane_id(1),
            PaneStateClosedReason::Killed,
            closed_revision,
        );
        for index in 0..3 {
            journal.push(
                pane_id(2),
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
            Ok(PaneStateRead::Ready {
                next_revision: closed_revision,
                limited: false,
                event_count: 1,
            })
        );
        assert!(matches!(
            events.as_slice(),
            [PaneStateEventDto::Closed {
                revision,
                reason: PaneStateClosedReason::Killed,
                ..
            }] if *revision == closed_revision
        ));
    }

    #[test]
    fn closed_subscription_synthesizes_closed_before_retained_post_close_state() {
        let mut journal = PaneStateJournal::new(2);
        let subscription = journal
            .subscribe(
                7,
                pane_id(1),
                PaneStateInclude {
                    title: false,
                    options: true,
                    foreground: false,
                },
            )
            .expect("subscription within limits");

        let closed_revision = journal.push(
            pane_id(1),
            Some(1),
            PaneStateChange::Closed {
                reason: PaneStateClosedReason::DiedKept,
            },
        );
        assert!(journal.mark_pane_closed(pane_id(1)));
        journal.remember_pane_closed_event(
            pane_id(1),
            PaneStateClosedReason::DiedKept,
            closed_revision,
        );
        journal.push(
            pane_id(1),
            Some(1),
            PaneStateChange::OptionSet {
                name: "@post-close".to_owned(),
                old: None,
                new: "ignored".to_owned(),
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
                next_revision: closed_revision,
                limited: false,
                event_count: 1,
            })
        );
        assert!(matches!(
            events.as_slice(),
            [PaneStateEventDto::Closed {
                revision,
                reason: PaneStateClosedReason::DiedKept,
                ..
            }] if *revision == closed_revision
        ));
    }

    #[test]
    fn closed_subscription_reports_lag_before_partial_retained_suffix() {
        let mut journal = PaneStateJournal::new(2);
        let subscription = journal
            .subscribe(
                7,
                pane_id(1),
                PaneStateInclude {
                    title: true,
                    options: false,
                    foreground: false,
                },
            )
            .expect("subscription within limits");

        journal.push(
            pane_id(1),
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
                old: "b".to_owned(),
                new: "c".to_owned(),
            },
        );
        let closed_revision = journal.push(
            pane_id(1),
            Some(1),
            PaneStateChange::Closed {
                reason: PaneStateClosedReason::Killed,
            },
        );
        assert!(journal.mark_pane_closed(pane_id(1)));
        journal.remember_pane_closed_event(
            pane_id(1),
            PaneStateClosedReason::Killed,
            closed_revision,
        );

        let mut events = Vec::new();
        assert_eq!(
            journal.read_after(7, subscription, 0, 16, &mut events),
            Ok(PaneStateRead::Lag {
                missed_from_revision: 0,
                resume_revision: 2,
            })
        );
        assert!(events.is_empty());

        let mut events = Vec::new();
        assert_eq!(
            journal.read_after(7, subscription, 1, 16, &mut events),
            Ok(PaneStateRead::Ready {
                next_revision: closed_revision,
                limited: false,
                event_count: 2,
            })
        );
        assert!(matches!(
            events.as_slice(),
            [
                PaneStateEventDto::TitleChanged { revision: 2, .. },
                PaneStateEventDto::Closed { revision, .. },
            ] if *revision == closed_revision
        ));
    }

    #[test]
    fn late_subscription_after_died_kept_receives_final_killed_close() {
        let mut journal = PaneStateJournal::new(8);
        let first_closed_revision = journal.push(
            pane_id(1),
            Some(1),
            PaneStateChange::Closed {
                reason: PaneStateClosedReason::DiedKept,
            },
        );
        assert!(journal.mark_pane_closed(pane_id(1)));
        journal.remember_pane_closed_event(
            pane_id(1),
            PaneStateClosedReason::DiedKept,
            first_closed_revision,
        );

        let subscription = journal
            .subscribe(
                7,
                pane_id(1),
                PaneStateInclude {
                    title: false,
                    options: false,
                    foreground: false,
                },
            )
            .expect("late subscription within limits");
        assert!(
            journal.mark_pane_closed(pane_id(1)),
            "an open late subscription requires a final close record even for an already closed pane"
        );
        let killed_revision = journal.push(
            pane_id(1),
            None,
            PaneStateChange::Closed {
                reason: PaneStateClosedReason::Killed,
            },
        );
        journal.remember_pane_closed_event(
            pane_id(1),
            PaneStateClosedReason::Killed,
            killed_revision,
        );

        let mut events = Vec::new();
        assert_eq!(
            journal.read_after(7, subscription, first_closed_revision, 16, &mut events),
            Ok(PaneStateRead::Ready {
                next_revision: killed_revision,
                limited: false,
                event_count: 1,
            })
        );
        assert!(matches!(
            events.as_slice(),
            [PaneStateEventDto::Closed {
                revision,
                reason: PaneStateClosedReason::Killed,
                ..
            }] if *revision == killed_revision
        ));
    }

    #[test]
    fn unrelated_evictions_do_not_lag_active_subscription() {
        let mut journal = PaneStateJournal::new(2);
        let subscription = journal
            .subscribe(
                7,
                pane_id(1),
                PaneStateInclude {
                    title: true,
                    options: false,
                    foreground: false,
                },
            )
            .expect("subscription within limits");

        for index in 0..4 {
            journal.push(
                pane_id(2),
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
            Ok(PaneStateRead::Ready {
                next_revision: 4,
                limited: false,
                event_count: 0,
            })
        );
        assert!(events.is_empty());
    }

    #[test]
    fn closed_subscription_is_bounded_at_close_revision_after_reopen() {
        let mut journal = PaneStateJournal::new(8);
        let subscription = journal
            .subscribe(
                7,
                pane_id(1),
                PaneStateInclude {
                    title: true,
                    options: false,
                    foreground: false,
                },
            )
            .expect("subscription within limits");

        journal.push(
            pane_id(1),
            Some(1),
            PaneStateChange::TitleChanged {
                old: "initial".to_owned(),
                new: "before-close".to_owned(),
            },
        );
        let closed_revision = journal.push(
            pane_id(1),
            Some(1),
            PaneStateChange::Closed {
                reason: PaneStateClosedReason::Killed,
            },
        );
        assert!(journal.mark_pane_closed(pane_id(1)));
        journal.remember_pane_closed_event(
            pane_id(1),
            PaneStateClosedReason::Killed,
            closed_revision,
        );
        journal.reopen_pane(pane_id(1));
        journal.push(
            pane_id(1),
            Some(2),
            PaneStateChange::TitleChanged {
                old: "after-reopen".to_owned(),
                new: "new-generation".to_owned(),
            },
        );

        let mut events = Vec::new();
        assert_eq!(
            journal.read_after(7, subscription, 0, 16, &mut events),
            Ok(PaneStateRead::Ready {
                next_revision: closed_revision,
                limited: false,
                event_count: 2,
            })
        );
        assert!(matches!(
            events.as_slice(),
            [
                PaneStateEventDto::TitleChanged { revision: 1, .. },
                PaneStateEventDto::Closed { revision, .. },
            ] if *revision == closed_revision
        ));

        journal.remove_closed_subscription(7, subscription);
        let mut events = Vec::new();
        assert!(journal
            .read_after(7, subscription, closed_revision, 16, &mut events)
            .is_err());
    }

    /// Model-based check of docs/design/pane-state-invariants.md I1/I2/I3/I5:
    /// random interleavings of watched-pane pushes (including post-close
    /// races), eviction-driving noise pushes, a close, and bounded consumer
    /// reads against a tiny journal. The consumer must observe its matching
    /// events in order with every gap preceded by a Lag rebase, must receive
    /// exactly one terminal Closed within a bounded number of reads (no stuck
    /// cursor, no error), and must observe nothing after it.
    #[test]
    fn invariants_hold_under_random_push_evict_close_read_interleavings() {
        for seed in 0..128u64 {
            run_invariant_scenario(seed);
        }
    }

    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            self.0 >> 33
        }

        fn below(&mut self, bound: u64) -> u64 {
            self.next() % bound
        }
    }

    fn watched_title_change(step: u64) -> PaneStateChange {
        PaneStateChange::TitleChanged {
            old: format!("t{step}"),
            new: format!("t{}", step + 1),
        }
    }

    fn event_revision(event: &PaneStateEventDto) -> u64 {
        match event {
            PaneStateEventDto::TitleChanged { revision, .. }
            | PaneStateEventDto::OptionSet { revision, .. }
            | PaneStateEventDto::OptionUnset { revision, .. }
            | PaneStateEventDto::ForegroundChanged { revision, .. }
            | PaneStateEventDto::Closed { revision, .. } => *revision,
            _ => unreachable!("unknown pane-state event variant from this rmux-proto version"),
        }
    }

    struct InvariantConsumer {
        seed: u64,
        subscription: PaneStateSubscriptionId,
        cursor: u64,
        expected: VecDeque<u64>,
        closed_delivered: usize,
    }

    impl InvariantConsumer {
        fn read(&mut self, journal: &PaneStateJournal, max_events: usize) {
            // The request handler removes terminal subscriptions immediately
            // after delivering Closed. Model that terminal stream contract
            // rather than calling the journal again after completion.
            if self.closed_delivered > 0 {
                return;
            }
            let seed = self.seed;
            let mut events = Vec::new();
            match journal
                .read_after(7, self.subscription, self.cursor, max_events, &mut events)
                .unwrap_or_else(|error| {
                    panic!("seed {seed}: read_after must never error (I2), got: {error}")
                }) {
                PaneStateRead::Ready { next_revision, .. } => {
                    for event in &events {
                        let revision = event_revision(event);
                        assert!(
                            revision > self.cursor,
                            "seed {seed}: delivered revision {revision} not past cursor (I1)"
                        );
                        self.cursor = revision;
                        if matches!(event, PaneStateEventDto::Closed { .. }) {
                            self.closed_delivered += 1;
                            assert!(
                                self.expected.is_empty(),
                                "seed {seed}: Closed delivered before pending events (I5)"
                            );
                        } else {
                            assert_eq!(
                                self.expected.pop_front(),
                                Some(revision),
                                "seed {seed}: out-of-order or silently skipped event (I5)"
                            );
                        }
                        assert!(
                            self.closed_delivered <= 1,
                            "seed {seed}: more than one terminal Closed delivered (I3)"
                        );
                    }
                    assert!(
                        next_revision >= self.cursor,
                        "seed {seed}: next_revision regressed below cursor (I1)"
                    );
                    if events.is_empty() {
                        self.cursor = self.cursor.max(next_revision);
                    }
                }
                PaneStateRead::Lag {
                    resume_revision, ..
                } => {
                    // Rebase this journal-only consumer to the first retained
                    // revision. The handler separately returns a current
                    // snapshot before draining retained events.
                    let rebased = resume_revision.saturating_sub(1);
                    assert!(
                        rebased >= self.cursor,
                        "seed {seed}: Lag rebase moved cursor backwards (I1)"
                    );
                    self.cursor = rebased;
                    while self
                        .expected
                        .front()
                        .is_some_and(|revision| *revision <= rebased)
                    {
                        self.expected.pop_front();
                    }
                }
            }
        }
    }

    fn run_invariant_scenario(seed: u64) {
        let mut rng = Lcg(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15).wrapping_add(1));
        let mut journal = PaneStateJournal::new(4);
        let include = PaneStateInclude {
            title: true,
            options: true,
            foreground: true,
        };
        let watched = pane_id(1);
        let subscription = journal
            .subscribe(7, watched, include)
            .expect("subscribe watched pane");
        let mut consumer = InvariantConsumer {
            seed,
            subscription,
            cursor: 0,
            expected: VecDeque::new(),
            closed_delivered: 0,
        };
        let mut closed_revision: Option<u64> = None;
        let close = |journal: &mut PaneStateJournal, closed_revision: &mut Option<u64>| {
            if closed_revision.is_none() && journal.mark_pane_closed(watched) {
                let revision = journal.push(
                    watched,
                    Some(1),
                    PaneStateChange::Closed {
                        reason: PaneStateClosedReason::Killed,
                    },
                );
                journal.remember_pane_closed_event(
                    watched,
                    PaneStateClosedReason::Killed,
                    revision,
                );
                *closed_revision = Some(revision);
            }
        };

        for step in 0..200u64 {
            match rng.below(14) {
                0..=4 => {
                    // Watched-pane state change; post-close pushes model the
                    // in-flight title/option tasks that race a kill and must
                    // never be delivered past the terminal Closed (I3) nor
                    // wedge the cursor once evicted.
                    let revision = journal.push(watched, Some(1), watched_title_change(step));
                    if closed_revision.is_none() {
                        consumer.expected.push_back(revision);
                    }
                }
                5..=9 => {
                    let noise = pane_id(2 + rng.below(3) as u32);
                    journal.push(noise, Some(1), watched_title_change(step));
                }
                10 => close(&mut journal, &mut closed_revision),
                _ => {
                    let max_events = 1 + rng.below(3) as usize;
                    consumer.read(&journal, max_events);
                }
            }
        }

        close(&mut journal, &mut closed_revision);
        assert!(closed_revision.is_some(), "seed {seed}: close recorded");

        // Drain: the consumer must reach the terminal Closed in bounded
        // reads — a stuck cursor (Lag that never progresses) fails here.
        let mut drain_reads = 0usize;
        while consumer.closed_delivered == 0 {
            drain_reads += 1;
            assert!(
                drain_reads < 1_000,
                "seed {seed}: consumer did not reach terminal Closed within 1000 reads \
                 (stuck cursor; I2/progress)"
            );
            consumer.read(&journal, 2);
        }
        assert_eq!(
            consumer.closed_delivered, 1,
            "seed {seed}: exactly one Closed (I3)"
        );

        // Post-terminal reads must deliver nothing (I3).
        for _ in 0..2 {
            let before_closed = consumer.closed_delivered;
            let before_expected = consumer.expected.len();
            let cursor_before = consumer.cursor;
            consumer.read(&journal, 4);
            assert_eq!(
                consumer.closed_delivered, before_closed,
                "seed {seed}: event after Closed (I3)"
            );
            assert_eq!(
                consumer.expected.len(),
                before_expected,
                "seed {seed}: matching event delivered after Closed (I3)"
            );
            assert!(
                consumer.cursor >= cursor_before,
                "seed {seed}: cursor regressed after terminal Closed (I1)"
            );
        }
    }
}
