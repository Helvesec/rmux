use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use rmux_proto::{PaneId, PaneOutputSubscriptionId, SessionName};

/// Default maximum live pane-output subscriptions owned by one connection.
pub const DEFAULT_MAX_SUBSCRIPTIONS_PER_CONNECTION: usize = 16;
/// Default maximum live pane-output subscriptions attached to one pane.
pub const DEFAULT_MAX_SUBSCRIPTIONS_PER_PANE: usize = 64;
/// Default maximum output events returned by one cursor poll.
pub const DEFAULT_SUBSCRIPTION_BATCH_EVENTS: usize = 64;
/// Default idle TTL before a subscription can be removed as stale.
pub const DEFAULT_SUBSCRIPTION_STALE_TTL: Duration = Duration::from_secs(300);

/// Runtime identity of a pane-output stream used for subscription accounting.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PaneOutputSubscriptionKey {
    runtime_session_name: SessionName,
    pane_id: PaneId,
}

impl PaneOutputSubscriptionKey {
    /// Builds a subscription key from the runtime session owner and pane id.
    #[must_use]
    pub fn new(runtime_session_name: SessionName, pane_id: PaneId) -> Self {
        Self {
            runtime_session_name,
            pane_id,
        }
    }

    /// Returns the runtime session name that owns the pane output stream.
    #[must_use]
    pub fn runtime_session_name(&self) -> &SessionName {
        &self.runtime_session_name
    }

    /// Returns the stable pane id.
    #[must_use]
    pub const fn pane_id(&self) -> PaneId {
        self.pane_id
    }
}

/// Numeric limits enforced by the pane-output subscription registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubscriptionLimits {
    max_per_connection: usize,
    max_per_pane: usize,
    batch_events: usize,
    stale_ttl: Duration,
}

impl SubscriptionLimits {
    /// Builds explicit subscription limits.
    #[must_use]
    pub const fn new(
        max_per_connection: usize,
        max_per_pane: usize,
        batch_events: usize,
        stale_ttl: Duration,
    ) -> Self {
        Self {
            max_per_connection,
            max_per_pane,
            batch_events,
            stale_ttl,
        }
    }

    /// Returns the maximum subscriptions one connection may own.
    #[must_use]
    pub const fn max_per_connection(self) -> usize {
        self.max_per_connection
    }

    /// Returns the maximum subscriptions one pane may have.
    #[must_use]
    pub const fn max_per_pane(self) -> usize {
        self.max_per_pane
    }

    /// Returns the maximum output events returned by one cursor response.
    #[must_use]
    pub const fn batch_events(self) -> usize {
        self.batch_events
    }

    /// Returns the idle TTL used by stale cleanup.
    #[must_use]
    pub const fn stale_ttl(self) -> Duration {
        self.stale_ttl
    }
}

impl Default for SubscriptionLimits {
    fn default() -> Self {
        Self::new(
            DEFAULT_MAX_SUBSCRIPTIONS_PER_CONNECTION,
            DEFAULT_MAX_SUBSCRIPTIONS_PER_PANE,
            DEFAULT_SUBSCRIPTION_BATCH_EVENTS,
            DEFAULT_SUBSCRIPTION_STALE_TTL,
        )
    }
}

/// Rejection raised when a subscription cap would be exceeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubscriptionLimitError {
    /// The connection already owns the configured maximum subscription count.
    PerConnection {
        /// Configured per-connection limit.
        limit: usize,
    },
    /// The pane already has the configured maximum subscription count.
    PerPane {
        /// Configured per-pane limit.
        limit: usize,
    },
}

/// Registered subscription metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputSubscriptionRecord {
    id: PaneOutputSubscriptionId,
    connection_id: u64,
    pane: PaneOutputSubscriptionKey,
    created_at: Instant,
    last_seen: Instant,
}

impl OutputSubscriptionRecord {
    /// Returns the subscription id.
    #[must_use]
    pub const fn id(&self) -> PaneOutputSubscriptionId {
        self.id
    }

    /// Returns the owning connection id.
    #[must_use]
    pub const fn connection_id(&self) -> u64 {
        self.connection_id
    }

    /// Returns the subscribed pane key.
    #[must_use]
    pub fn pane(&self) -> &PaneOutputSubscriptionKey {
        &self.pane
    }

    /// Returns when the subscription was created.
    #[must_use]
    pub fn created_at(&self) -> Instant {
        self.created_at
    }

    /// Returns when the subscription was last used.
    #[must_use]
    pub fn last_seen(&self) -> Instant {
        self.last_seen
    }
}

/// Pane-output subscription registry with connection and pane cap accounting.
#[derive(Debug, Clone)]
pub struct SubscriptionRegistry {
    limits: SubscriptionLimits,
    next_id: u64,
    records: HashMap<PaneOutputSubscriptionId, OutputSubscriptionRecord>,
    by_connection: HashMap<u64, HashSet<PaneOutputSubscriptionId>>,
    by_pane: HashMap<PaneOutputSubscriptionKey, HashSet<PaneOutputSubscriptionId>>,
}

impl SubscriptionRegistry {
    /// Builds an empty registry using explicit limits.
    #[must_use]
    pub fn new(limits: SubscriptionLimits) -> Self {
        Self {
            limits,
            next_id: 1,
            records: HashMap::new(),
            by_connection: HashMap::new(),
            by_pane: HashMap::new(),
        }
    }

    /// Returns the configured limits.
    #[must_use]
    pub const fn limits(&self) -> SubscriptionLimits {
        self.limits
    }

    /// Registers a new subscription if both caps allow it.
    pub fn subscribe(
        &mut self,
        connection_id: u64,
        pane: PaneOutputSubscriptionKey,
        now: Instant,
    ) -> Result<OutputSubscriptionRecord, SubscriptionLimitError> {
        let _ = self.cleanup_stale(now);

        let connection_count = self
            .by_connection
            .get(&connection_id)
            .map_or(0, HashSet::len);
        if connection_count >= self.limits.max_per_connection {
            return Err(SubscriptionLimitError::PerConnection {
                limit: self.limits.max_per_connection,
            });
        }

        let pane_count = self.by_pane.get(&pane).map_or(0, HashSet::len);
        if pane_count >= self.limits.max_per_pane {
            return Err(SubscriptionLimitError::PerPane {
                limit: self.limits.max_per_pane,
            });
        }

        let id = self.allocate_id();
        let record = OutputSubscriptionRecord {
            id,
            connection_id,
            pane: pane.clone(),
            created_at: now,
            last_seen: now,
        };
        self.records.insert(id, record.clone());
        self.by_connection
            .entry(connection_id)
            .or_default()
            .insert(id);
        self.by_pane.entry(pane).or_default().insert(id);
        Ok(record)
    }

    /// Returns a registered subscription.
    #[must_use]
    pub fn get(&self, id: PaneOutputSubscriptionId) -> Option<&OutputSubscriptionRecord> {
        self.records.get(&id)
    }

    /// Marks a subscription as recently used and returns its updated record.
    pub fn touch(
        &mut self,
        id: PaneOutputSubscriptionId,
        now: Instant,
    ) -> Option<OutputSubscriptionRecord> {
        let _ = self.cleanup_stale(now);
        let record = self.records.get_mut(&id)?;
        record.last_seen = now;
        Some(record.clone())
    }

    /// Removes one subscription exactly once.
    pub fn unsubscribe(
        &mut self,
        id: PaneOutputSubscriptionId,
    ) -> Option<OutputSubscriptionRecord> {
        let record = self.records.remove(&id)?;
        self.remove_indexes(&record);
        Some(record)
    }

    /// Removes every subscription owned by a connection.
    pub fn remove_connection(&mut self, connection_id: u64) -> Vec<OutputSubscriptionRecord> {
        let ids = self
            .by_connection
            .remove(&connection_id)
            .unwrap_or_default()
            .into_iter()
            .collect::<Vec<_>>();
        self.remove_ids(ids)
    }

    /// Removes every subscription attached to a pane.
    pub fn remove_pane(
        &mut self,
        pane: &PaneOutputSubscriptionKey,
    ) -> Vec<OutputSubscriptionRecord> {
        let ids = self
            .by_pane
            .remove(pane)
            .unwrap_or_default()
            .into_iter()
            .collect::<Vec<_>>();
        self.remove_ids(ids)
    }

    /// Removes subscriptions that have not been touched within the stale TTL.
    pub fn cleanup_stale(&mut self, now: Instant) -> Vec<OutputSubscriptionRecord> {
        let ttl = self.limits.stale_ttl;
        let ids = self
            .records
            .iter()
            .filter_map(|(id, record)| (now.duration_since(record.last_seen) >= ttl).then_some(*id))
            .collect::<Vec<_>>();
        self.remove_ids(ids)
    }

    /// Returns the total number of live subscriptions.
    #[must_use]
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Returns whether the registry is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    fn allocate_id(&mut self) -> PaneOutputSubscriptionId {
        let id = PaneOutputSubscriptionId::new(self.next_id);
        self.next_id = self
            .next_id
            .checked_add(1)
            .expect("pane output subscription id space exhausted");
        id
    }

    fn remove_ids(
        &mut self,
        ids: impl IntoIterator<Item = PaneOutputSubscriptionId>,
    ) -> Vec<OutputSubscriptionRecord> {
        let mut removed = Vec::new();
        for id in ids {
            if let Some(record) = self.records.remove(&id) {
                self.remove_indexes(&record);
                removed.push(record);
            }
        }
        removed
    }

    fn remove_indexes(&mut self, record: &OutputSubscriptionRecord) {
        if let Some(ids) = self.by_connection.get_mut(&record.connection_id) {
            ids.remove(&record.id);
            if ids.is_empty() {
                self.by_connection.remove(&record.connection_id);
            }
        }
        if let Some(ids) = self.by_pane.get_mut(&record.pane) {
            ids.remove(&record.id);
            if ids.is_empty() {
                self.by_pane.remove(&record.pane);
            }
        }
    }
}

impl Default for SubscriptionRegistry {
    fn default() -> Self {
        Self::new(SubscriptionLimits::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> SessionName {
        SessionName::new("alpha").expect("valid session")
    }

    fn pane(id: u32) -> PaneOutputSubscriptionKey {
        PaneOutputSubscriptionKey::new(session(), PaneId::new(id))
    }

    #[test]
    fn caps_are_released_exactly_once_across_overlapping_removals() {
        let limits = SubscriptionLimits::new(1, 1, 64, Duration::from_secs(300));
        let mut registry = SubscriptionRegistry::new(limits);
        let now = Instant::now();
        let first = registry
            .subscribe(7, pane(1), now)
            .expect("first subscription");

        assert!(matches!(
            registry.subscribe(7, pane(2), now),
            Err(SubscriptionLimitError::PerConnection { limit: 1 })
        ));
        assert!(matches!(
            registry.subscribe(8, pane(1), now),
            Err(SubscriptionLimitError::PerPane { limit: 1 })
        ));

        assert_eq!(
            registry.unsubscribe(first.id()).map(|record| record.id()),
            Some(first.id())
        );
        assert!(registry.unsubscribe(first.id()).is_none());

        let second = registry
            .subscribe(8, pane(1), now)
            .expect("cap released after unsubscribe");
        let removed_by_pane = registry.remove_pane(second.pane());
        assert_eq!(removed_by_pane.len(), 1);
        assert_eq!(removed_by_pane[0].id(), second.id());
        assert!(registry
            .remove_connection(second.connection_id())
            .is_empty());
        assert!(registry.unsubscribe(second.id()).is_none());

        let third = registry
            .subscribe(9, pane(1), now)
            .expect("cap released after pane removal");
        let removed_by_connection = registry.remove_connection(third.connection_id());
        assert_eq!(removed_by_connection.len(), 1);
        assert_eq!(removed_by_connection[0].id(), third.id());
        assert!(registry.remove_pane(third.pane()).is_empty());
        assert!(registry.subscribe(10, pane(1), now).is_ok());
    }

    #[test]
    fn stale_cleanup_releases_connection_and_pane_caps() {
        let limits = SubscriptionLimits::new(1, 1, 64, Duration::from_millis(10));
        let mut registry = SubscriptionRegistry::new(limits);
        let now = Instant::now();
        let first = registry
            .subscribe(1, pane(1), now)
            .expect("first subscription");

        let removed = registry.cleanup_stale(now + Duration::from_millis(10));
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].id(), first.id());
        assert!(registry
            .cleanup_stale(now + Duration::from_millis(20))
            .is_empty());
        assert!(registry.subscribe(1, pane(1), now).is_ok());
    }
}
