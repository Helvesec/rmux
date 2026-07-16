//! Subscription lifecycle and cursor reads for the pane-state journal.

use super::*;

impl PaneStateJournal {
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
                evicted_state_revision_before_close: 0,
            },
        );
        increment_count(&mut self.subscription_counts, pane_id);
        increment_generation_count(
            &mut self.subscription_generation_counts,
            (pane_id, generation),
        );
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
        let evicted_revisions = &self.evicted_revisions;
        for subscription in self.subscriptions.values_mut() {
            if subscription.pane_id == pane_id
                && subscription.closed
                && subscription.closed_revision.is_none()
            {
                subscription.evicted_state_revision_before_close =
                    evicted_revisions_for_subscription(evicted_revisions, subscription)
                        .max_matching_state_change(subscription.include);
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
                    && record_matches_subscription(record, subscription)
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
            record.revision > after_revision && record_matches_subscription(record, subscription)
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
            .evicted_revisions_for(subscription)
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
        // The terminal Closed record is synthesized by read_after even after
        // eviction. Track matching state evictions that actually happened
        // before that terminal record: clamping a later eviction to
        // `closed_revision - 1` fabricates history that never existed.
        let evicted_revision = subscription.evicted_state_revision_before_close;
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
                    && record_matches_subscription(record, subscription)
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
        decrement_generation_count(
            &mut self.subscription_generation_counts,
            (subscription.pane_id, subscription.generation),
        );
        self.prune_evicted_revisions_for_pane(subscription.pane_id);
        Some(subscription)
    }

    fn evicted_revisions_for(
        &self,
        subscription: &PaneStateSubscription,
    ) -> EvictedPaneStateRevisions {
        evicted_revisions_for_subscription(&self.evicted_revisions, subscription)
    }
}

fn evicted_revisions_for_subscription(
    evicted_revisions: &HashMap<PaneGeneration, EvictedPaneStateRevisions>,
    subscription: &PaneStateSubscription,
) -> EvictedPaneStateRevisions {
    match subscription.generation {
        Some(generation) => evicted_revisions
            .get(&(subscription.pane_id, Some(generation)))
            .copied()
            .unwrap_or_default(),
        None => evicted_revisions
            .iter()
            .filter(|((pane_id, _), _)| *pane_id == subscription.pane_id)
            .fold(
                EvictedPaneStateRevisions::default(),
                |combined, (_, next)| combined.merge(*next),
            ),
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
