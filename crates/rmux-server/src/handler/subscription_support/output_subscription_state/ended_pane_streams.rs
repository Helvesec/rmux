use std::collections::{BTreeSet, HashMap};
use std::time::{Duration, Instant};

use rmux_proto::PaneOutputSubscriptionId;

use super::super::super::pane_stream_support::EndedPaneStream;
use super::MAX_OUTPUT_SUBSCRIPTIONS_GLOBAL;

// Keep four generations of the default 16 live streams so a client can observe recent End
// reasons without letting a durable connection retain an unbounded history.
const MAX_ENDED_STREAMS_PER_CONNECTION: usize = 64;
const _: () = assert!(MAX_ENDED_STREAMS_PER_CONNECTION <= MAX_OUTPUT_SUBSCRIPTIONS_GLOBAL);

#[derive(Debug)]
pub(in crate::handler) struct EndedPaneStreams {
    entries: HashMap<PaneOutputSubscriptionId, EndedPaneStream>,
    order: BTreeSet<(Instant, PaneOutputSubscriptionId)>,
    by_connection: HashMap<u64, BTreeSet<(Instant, PaneOutputSubscriptionId)>>,
    max_per_connection: usize,
}

impl EndedPaneStreams {
    pub(in crate::handler) fn new(configured_max_per_connection: usize) -> Self {
        Self {
            entries: HashMap::new(),
            order: BTreeSet::new(),
            by_connection: HashMap::new(),
            max_per_connection: configured_max_per_connection.clamp(
                MAX_ENDED_STREAMS_PER_CONNECTION,
                MAX_OUTPUT_SUBSCRIPTIONS_GLOBAL,
            ),
        }
    }

    pub(in crate::handler) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(in crate::handler) fn get(
        &self,
        subscription_id: &PaneOutputSubscriptionId,
    ) -> Option<&EndedPaneStream> {
        self.entries.get(subscription_id)
    }

    pub(in crate::handler) fn insert(
        &mut self,
        subscription_id: PaneOutputSubscriptionId,
        ended: EndedPaneStream,
    ) {
        self.remove(&subscription_id);
        let key = (ended.ended_at, subscription_id);
        self.entries.insert(subscription_id, ended);
        self.order.insert(key);
        self.by_connection
            .entry(ended.connection_id)
            .or_default()
            .insert(key);

        while self
            .by_connection
            .get(&ended.connection_id)
            .is_some_and(|entries| entries.len() > self.max_per_connection)
        {
            let oldest = self.by_connection[&ended.connection_id]
                .first()
                .copied()
                .expect("over-limit connection has an ended stream");
            self.remove(&oldest.1);
        }
        while self.entries.len() > MAX_OUTPUT_SUBSCRIPTIONS_GLOBAL {
            let oldest = self
                .order
                .first()
                .copied()
                .expect("over-limit registry has an ended stream");
            self.remove(&oldest.1);
        }
    }

    pub(in crate::handler) fn remove(
        &mut self,
        subscription_id: &PaneOutputSubscriptionId,
    ) -> Option<EndedPaneStream> {
        let ended = self.entries.remove(subscription_id)?;
        let key = (ended.ended_at, *subscription_id);
        self.order.remove(&key);
        let remove_connection = self
            .by_connection
            .get_mut(&ended.connection_id)
            .is_some_and(|entries| {
                entries.remove(&key);
                entries.is_empty()
            });
        if remove_connection {
            self.by_connection.remove(&ended.connection_id);
        }
        Some(ended)
    }

    pub(in crate::handler) fn remove_connection(&mut self, connection_id: u64) {
        let Some(entries) = self.by_connection.remove(&connection_id) else {
            return;
        };
        for key in entries {
            self.order.remove(&key);
            self.entries.remove(&key.1);
        }
    }

    pub(in crate::handler) fn cleanup_stale(&mut self, now: Instant, ttl: Duration) {
        while let Some((ended_at, subscription_id)) = self.order.first().copied() {
            if now.saturating_duration_since(ended_at) < ttl {
                break;
            }
            self.remove(&subscription_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmux_proto::PaneStreamEndReason;

    fn ended(connection_id: u64, reason: PaneStreamEndReason, at: Instant) -> EndedPaneStream {
        EndedPaneStream::new(connection_id, reason, at)
    }

    fn streams() -> EndedPaneStreams {
        EndedPaneStreams::new(MAX_ENDED_STREAMS_PER_CONNECTION)
    }

    #[test]
    fn per_connection_churn_keeps_only_the_newest_tombstones() {
        let mut streams = streams();
        let start = Instant::now();
        for value in 1..=(MAX_ENDED_STREAMS_PER_CONNECTION as u64 + 8) {
            streams.insert(
                PaneOutputSubscriptionId::new(value),
                ended(
                    7,
                    PaneStreamEndReason::PaneRemoved,
                    start + Duration::from_nanos(value),
                ),
            );
        }

        assert_eq!(streams.len(), MAX_ENDED_STREAMS_PER_CONNECTION);
        assert!(streams.get(&PaneOutputSubscriptionId::new(8)).is_none());
        assert!(streams.get(&PaneOutputSubscriptionId::new(9)).is_some());
    }

    #[test]
    fn one_connection_churn_does_not_evict_another_connections_tombstone() {
        let mut streams = streams();
        let start = Instant::now();
        let other = PaneOutputSubscriptionId::new(1);
        streams.insert(other, ended(11, PaneStreamEndReason::PaneRemoved, start));
        for value in 2..=(MAX_ENDED_STREAMS_PER_CONNECTION as u64 + 2) {
            streams.insert(
                PaneOutputSubscriptionId::new(value),
                ended(
                    12,
                    PaneStreamEndReason::SlowConsumer,
                    start + Duration::from_nanos(value),
                ),
            );
        }

        assert!(streams.get(&other).is_some());
        assert_eq!(
            streams
                .by_connection
                .get(&12)
                .expect("churning connection remains indexed")
                .len(),
            MAX_ENDED_STREAMS_PER_CONNECTION
        );
    }

    #[test]
    fn connection_removal_clears_only_its_tombstones_and_indexes() {
        let mut streams = streams();
        let ended_at = Instant::now();
        let removed = PaneOutputSubscriptionId::new(1);
        let retained = PaneOutputSubscriptionId::new(2);
        streams.insert(
            removed,
            ended(21, PaneStreamEndReason::PaneRemoved, ended_at),
        );
        streams.insert(
            retained,
            ended(22, PaneStreamEndReason::PaneRemoved, ended_at),
        );

        streams.remove_connection(21);

        assert!(streams.get(&removed).is_none());
        assert!(streams.get(&retained).is_some());
        assert!(!streams.by_connection.contains_key(&21));
        assert!(!streams.order.contains(&(ended_at, removed)));
    }

    #[test]
    fn global_churn_evicts_the_oldest_tombstone_deterministically() {
        let mut streams = streams();
        let ended_at = Instant::now();
        for value in 1..=(MAX_OUTPUT_SUBSCRIPTIONS_GLOBAL as u64 + 1) {
            streams.insert(
                PaneOutputSubscriptionId::new(value),
                ended(value, PaneStreamEndReason::PaneRemoved, ended_at),
            );
        }

        assert_eq!(streams.len(), MAX_OUTPUT_SUBSCRIPTIONS_GLOBAL);
        assert!(streams.get(&PaneOutputSubscriptionId::new(1)).is_none());
        assert!(streams.get(&PaneOutputSubscriptionId::new(2)).is_some());
    }

    #[test]
    fn reinsertion_preserves_the_latest_end_reason() {
        let mut streams = streams();
        let subscription_id = PaneOutputSubscriptionId::new(3);
        let start = Instant::now();
        streams.insert(
            subscription_id,
            ended(4, PaneStreamEndReason::PaneRemoved, start),
        );
        streams.insert(
            subscription_id,
            ended(
                4,
                PaneStreamEndReason::ProjectionFailed,
                start + Duration::from_millis(1),
            ),
        );

        let latest = streams.get(&subscription_id).expect("latest tombstone");
        assert_eq!(latest.reason, PaneStreamEndReason::ProjectionFailed);
        assert_eq!(latest.ended_at, start + Duration::from_millis(1));
    }

    #[test]
    fn cleanup_retains_the_existing_ttl_boundary() {
        let mut streams = streams();
        let start = Instant::now();
        let ttl = Duration::from_secs(300);
        let subscription_id = PaneOutputSubscriptionId::new(5);
        streams.insert(
            subscription_id,
            ended(6, PaneStreamEndReason::PaneRemoved, start),
        );

        streams.cleanup_stale(start + ttl - Duration::from_nanos(1), ttl);
        assert!(streams.get(&subscription_id).is_some());
        streams.cleanup_stale(start + ttl, ttl);
        assert!(streams.get(&subscription_id).is_none());
    }
}
