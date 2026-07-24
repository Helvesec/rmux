use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rmux_core::{
    events::{PaneOutputSubscriptionKey, SubscriptionLimits, SubscriptionRegistry},
    PaneId,
};
use rmux_proto::{
    PaneOutputSubscriptionId, PaneRawRebaseReason, PaneStreamEndReason, PaneStreamMode,
};
use tokio::sync::watch;

use crate::pane_io::PaneOutputReceiver;

use super::super::pane_stream_support::{
    CachedRawRebase, EndedPaneStream, PaneStreamSource, PaneStreamSubscription,
    PendingSurfaceRefresh, SurfaceDriver,
};

pub(in crate::handler) enum SurfaceDriverRoute {
    Ready,
    Initialize { token: u64 },
    Wait(watch::Receiver<bool>),
}

pub(in crate::handler) enum RawInitializationRoute {
    Ready(Arc<CachedRawRebase>),
    Initialize { token: u64 },
    Wait(watch::Receiver<bool>),
}

struct PaneStreamInitialization {
    token: u64,
    completion: watch::Sender<bool>,
}

pub(crate) struct OutputSubscriptionState {
    pub(in crate::handler) registry: SubscriptionRegistry,
    pub(in crate::handler) receivers: HashMap<PaneOutputSubscriptionId, PaneOutputReceiver>,
    pub(in crate::handler) streams: HashMap<PaneOutputSubscriptionId, PaneStreamSubscription>,
    pub(in crate::handler) ended_streams: HashMap<PaneOutputSubscriptionId, EndedPaneStream>,
    pub(in crate::handler) surface_drivers: HashMap<PaneOutputSubscriptionKey, SurfaceDriver>,
    pub(in crate::handler) raw_rebases: HashMap<PaneOutputSubscriptionKey, Arc<CachedRawRebase>>,
    surface_initializations: HashMap<PaneOutputSubscriptionKey, PaneStreamInitialization>,
    next_surface_initialization_token: u64,
    raw_initializations: HashMap<PaneOutputSubscriptionKey, PaneStreamInitialization>,
    next_raw_initialization_token: u64,
    draining_pane_progress: HashMap<PaneOutputSubscriptionKey, Instant>,
    draining_stream_sources: HashMap<PaneOutputSubscriptionKey, PaneStreamSource>,
}

impl std::fmt::Debug for OutputSubscriptionState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OutputSubscriptionState")
            .field("registry", &self.registry)
            .field("receiver_count", &self.receivers.len())
            .field("stream_count", &self.streams.len())
            .field("ended_stream_count", &self.ended_streams.len())
            .field("surface_driver_count", &self.surface_drivers.len())
            .field(
                "surface_initialization_count",
                &self.surface_initializations.len(),
            )
            .field("raw_rebase_count", &self.raw_rebases.len())
            .field("raw_initialization_count", &self.raw_initializations.len())
            .field("draining_pane_count", &self.draining_pane_progress.len())
            .field(
                "draining_stream_source_count",
                &self.draining_stream_sources.len(),
            )
            .finish()
    }
}

impl OutputSubscriptionState {
    pub(crate) fn new(limits: SubscriptionLimits) -> Self {
        Self {
            registry: SubscriptionRegistry::new(limits),
            receivers: HashMap::new(),
            streams: HashMap::new(),
            ended_streams: HashMap::new(),
            surface_drivers: HashMap::new(),
            raw_rebases: HashMap::new(),
            surface_initializations: HashMap::new(),
            next_surface_initialization_token: 0,
            raw_initializations: HashMap::new(),
            next_raw_initialization_token: 0,
            draining_pane_progress: HashMap::new(),
            draining_stream_sources: HashMap::new(),
        }
    }

    pub(in crate::handler) fn limits(&self) -> SubscriptionLimits {
        self.registry.limits()
    }

    pub(in crate::handler) fn cleanup_stale(&mut self, now: Instant) {
        let tombstone_ttl = self.limits().stale_ttl();
        self.ended_streams
            .retain(|_, ended| now.saturating_duration_since(ended.ended_at) < tombstone_ttl);
        for record in self.registry.cleanup_stale(now) {
            if self.streams.remove(&record.id()).is_some() {
                self.ended_streams.insert(
                    record.id(),
                    EndedPaneStream::new(
                        record.connection_id(),
                        PaneStreamEndReason::SubscriptionExpired,
                        now,
                    ),
                );
            } else {
                self.receivers.remove(&record.id());
            }
            self.discard_stream_cache_if_unused(record.pane());
            self.discard_drain_if_unused(record.pane());
        }
    }

    pub(super) fn remove_connection(&mut self, connection_id: u64) {
        for record in self.registry.remove_connection(connection_id) {
            self.receivers.remove(&record.id());
            self.streams.remove(&record.id());
            self.discard_stream_cache_if_unused(record.pane());
            self.discard_drain_if_unused(record.pane());
        }
        self.ended_streams
            .retain(|_, ended| ended.connection_id != connection_id);
    }

    pub(super) fn remove_pane(&mut self, pane: &PaneOutputSubscriptionKey) -> bool {
        let removed = self.registry.remove_pane(pane);
        let removed_any = !removed.is_empty();
        for record in removed {
            if self.streams.remove(&record.id()).is_some() {
                self.ended_streams.insert(
                    record.id(),
                    EndedPaneStream::new(
                        record.connection_id(),
                        PaneStreamEndReason::PaneRemoved,
                        Instant::now(),
                    ),
                );
            } else {
                self.receivers.remove(&record.id());
            }
        }
        self.surface_drivers.remove(pane);
        self.cancel_surface_initialization(pane);
        self.raw_rebases.remove(pane);
        self.cancel_raw_initialization(pane);
        self.draining_pane_progress.remove(pane);
        self.draining_stream_sources.remove(pane);
        removed_any
    }

    pub(crate) fn rekey_pane(
        &mut self,
        previous: &PaneOutputSubscriptionKey,
        current: PaneOutputSubscriptionKey,
    ) {
        let _ = self.registry.rekey_pane(previous, current.clone());
        if let Some(driver) = self.surface_drivers.remove(previous) {
            self.surface_drivers.insert(current.clone(), driver);
        }
        if let Some(rebase) = self.raw_rebases.remove(previous) {
            self.raw_rebases.insert(current.clone(), rebase);
        }
        if let Some(initialization) = self.surface_initializations.remove(previous) {
            self.surface_initializations
                .insert(current.clone(), initialization);
        }
        if let Some(initialization) = self.raw_initializations.remove(previous) {
            self.raw_initializations
                .insert(current.clone(), initialization);
        }
        if let Some(progress) = self.draining_pane_progress.remove(previous) {
            self.draining_pane_progress
                .insert(current.clone(), progress);
        }
        if let Some(mut source) = self.draining_stream_sources.remove(previous) {
            source.key = current.clone();
            self.draining_stream_sources.insert(current.clone(), source);
        }
    }

    pub(super) fn begin_pane_drain(
        &mut self,
        pane: PaneOutputSubscriptionKey,
        source: Option<PaneStreamSource>,
        now: Instant,
    ) -> bool {
        if self.registry.ids_for_pane(&pane).is_empty() {
            return false;
        }
        if let Some(source) = source {
            self.draining_stream_sources.insert(pane.clone(), source);
        }
        self.draining_pane_progress.insert(pane.clone(), now);
        true
    }

    pub(in crate::handler) fn note_pane_drain_progress(
        &mut self,
        pane: &PaneOutputSubscriptionKey,
        now: Instant,
    ) {
        if let Some(last_progress) = self.draining_pane_progress.get_mut(pane) {
            *last_progress = (*last_progress).max(now);
        }
    }

    pub(in crate::handler) fn draining_stream_source(
        &self,
        pane: &PaneOutputSubscriptionKey,
    ) -> Option<PaneStreamSource> {
        self.draining_stream_sources.get(pane).cloned()
    }

    pub(in crate::handler) fn mark_pane_streams_ending(
        &mut self,
        pane: &PaneOutputSubscriptionKey,
        reason: PaneStreamEndReason,
    ) {
        for id in self.registry.ids_for_pane(pane) {
            if let Some(stream) = self.streams.get_mut(&id) {
                stream.mark_ending(reason);
            }
        }
    }

    pub(super) fn pane_is_draining(&self, pane: &PaneOutputSubscriptionKey) -> bool {
        self.draining_pane_progress.contains_key(pane)
    }

    #[cfg(test)]
    pub(super) fn pane_drain_idle_for(
        &self,
        pane: &PaneOutputSubscriptionKey,
        now: Instant,
    ) -> Option<Duration> {
        self.draining_pane_progress
            .get(pane)
            .map(|last_progress| now.saturating_duration_since(*last_progress))
    }

    pub(super) fn expire_pane_drain_if_idle(
        &mut self,
        pane: &PaneOutputSubscriptionKey,
        now: Instant,
        idle_timeout: Duration,
    ) -> bool {
        let Some(last_progress) = self.draining_pane_progress.get(pane).copied() else {
            return false;
        };
        if now.saturating_duration_since(last_progress) < idle_timeout {
            return false;
        }
        self.expire_pane_drain(pane, now);
        true
    }

    pub(in crate::handler) fn is_empty(&self) -> bool {
        self.registry.is_empty()
    }

    pub(in crate::handler) fn remove_subscription(
        &mut self,
        subscription_id: PaneOutputSubscriptionId,
    ) {
        if let Some(record) = self.registry.unsubscribe(subscription_id) {
            self.receivers.remove(&subscription_id);
            self.streams.remove(&subscription_id);
            self.discard_stream_cache_if_unused(record.pane());
            self.discard_drain_if_unused(record.pane());
        }
        self.ended_streams.remove(&subscription_id);
    }

    pub(in crate::handler) fn end_pane_streams(
        &mut self,
        pane: &PaneOutputSubscriptionKey,
        mode: PaneStreamMode,
        reason: PaneStreamEndReason,
        now: Instant,
    ) {
        let records = self
            .registry
            .ids_for_pane(pane)
            .into_iter()
            .filter(|id| {
                self.streams
                    .get(id)
                    .is_some_and(|stream| stream.mode() == mode)
            })
            .filter_map(|id| self.registry.get(id).cloned())
            .collect::<Vec<_>>();
        for record in records {
            let _ = self.registry.unsubscribe(record.id());
            self.streams.remove(&record.id());
            self.ended_streams.insert(
                record.id(),
                EndedPaneStream::new(record.connection_id(), reason, now),
            );
        }
        match mode {
            PaneStreamMode::Raw => {
                self.raw_rebases.remove(pane);
                self.cancel_raw_initialization(pane);
            }
            PaneStreamMode::Surface => {
                self.surface_drivers.remove(pane);
                self.cancel_surface_initialization(pane);
            }
        }
    }

    pub(in crate::handler) fn surface_driver_route(
        &mut self,
        pane: &PaneOutputSubscriptionKey,
    ) -> SurfaceDriverRoute {
        if self.surface_drivers.contains_key(pane) {
            return SurfaceDriverRoute::Ready;
        }
        if let Some(initialization) = self.surface_initializations.get(pane) {
            return SurfaceDriverRoute::Wait(initialization.completion.subscribe());
        }
        self.next_surface_initialization_token =
            self.next_surface_initialization_token.saturating_add(1);
        let token = self.next_surface_initialization_token;
        let (completion, _) = watch::channel(false);
        self.surface_initializations
            .insert(pane.clone(), PaneStreamInitialization { token, completion });
        SurfaceDriverRoute::Initialize { token }
    }

    pub(in crate::handler) fn raw_initialization_route(
        &mut self,
        pane: &PaneOutputSubscriptionKey,
        include_snapshot: bool,
    ) -> RawInitializationRoute {
        if let Some(rebase) = self
            .raw_rebases
            .get(pane)
            .filter(|rebase| !include_snapshot || rebase.rebase.snapshot.is_some())
        {
            return RawInitializationRoute::Ready(Arc::clone(rebase));
        }
        if let Some(initialization) = self.raw_initializations.get(pane) {
            return RawInitializationRoute::Wait(initialization.completion.subscribe());
        }
        self.next_raw_initialization_token = self.next_raw_initialization_token.saturating_add(1);
        let token = self.next_raw_initialization_token;
        let (completion, _) = watch::channel(false);
        self.raw_initializations
            .insert(pane.clone(), PaneStreamInitialization { token, completion });
        RawInitializationRoute::Initialize { token }
    }

    pub(in crate::handler) fn discard_raw_rebase_if_current(
        &mut self,
        pane: &PaneOutputSubscriptionKey,
        stale: &Arc<CachedRawRebase>,
    ) {
        if self
            .raw_rebases
            .get(pane)
            .is_some_and(|current| Arc::ptr_eq(current, stale))
        {
            self.raw_rebases.remove(pane);
        }
    }

    pub(in crate::handler) fn finish_raw_initialization(&mut self, token: u64) {
        let pane = self
            .raw_initializations
            .iter()
            .find_map(|(pane, initialization)| {
                (initialization.token == token).then(|| pane.clone())
            });
        let Some(pane) = pane else {
            return;
        };
        if let Some(initialization) = self.raw_initializations.remove(&pane) {
            let _ = initialization.completion.send(true);
        }
    }

    pub(in crate::handler) fn finish_surface_initialization(&mut self, token: u64) {
        let pane = self
            .surface_initializations
            .iter()
            .find_map(|(pane, initialization)| {
                (initialization.token == token).then(|| pane.clone())
            });
        let Some(pane) = pane else {
            return;
        };
        if let Some(initialization) = self.surface_initializations.remove(&pane) {
            let _ = initialization.completion.send(true);
        }
    }

    pub(in crate::handler) fn surface_driver_key_for_pane_id(
        &self,
        pane_id: PaneId,
    ) -> Option<PaneOutputSubscriptionKey> {
        self.surface_drivers
            .keys()
            .find(|key| key.pane_id() == pane_id)
            .cloned()
    }

    pub(in crate::handler) fn cancel_surface_refresh(
        &mut self,
        pane_id: PaneId,
        token: u64,
        pending: PendingSurfaceRefresh,
    ) {
        let Some(key) = self.surface_driver_key_for_pane_id(pane_id) else {
            return;
        };
        if let Some(driver) = self.surface_drivers.get_mut(&key) {
            driver.cancel_refresh(token, pending);
        }
    }

    pub(in crate::handler) fn cancel_raw_rebase(
        &mut self,
        subscription_id: PaneOutputSubscriptionId,
        token: u64,
        reason: PaneRawRebaseReason,
    ) {
        if let Some(PaneStreamSubscription::Raw(stream)) = self.streams.get_mut(&subscription_id) {
            stream.cancel_rebase(token, reason);
        }
    }

    fn cancel_surface_initialization(&mut self, pane: &PaneOutputSubscriptionKey) {
        if let Some(initialization) = self.surface_initializations.remove(pane) {
            let _ = initialization.completion.send(true);
        }
    }

    fn cancel_raw_initialization(&mut self, pane: &PaneOutputSubscriptionKey) {
        if let Some(initialization) = self.raw_initializations.remove(pane) {
            let _ = initialization.completion.send(true);
        }
    }

    fn discard_drain_if_unused(&mut self, pane: &PaneOutputSubscriptionKey) {
        if self.registry.ids_for_pane(pane).is_empty() {
            self.draining_pane_progress.remove(pane);
            self.draining_stream_sources.remove(pane);
        }
    }

    fn discard_stream_cache_if_unused(&mut self, pane: &PaneOutputSubscriptionKey) {
        let mut has_raw = false;
        let mut has_surface = false;
        for id in self.registry.ids_for_pane(pane) {
            match self.streams.get(&id) {
                Some(PaneStreamSubscription::Reserved {
                    mode: PaneStreamMode::Raw,
                    ..
                }) => has_raw = true,
                Some(PaneStreamSubscription::Reserved {
                    mode: PaneStreamMode::Surface,
                    ..
                }) => has_surface = true,
                Some(PaneStreamSubscription::Raw(_)) => has_raw = true,
                Some(PaneStreamSubscription::Surface(_)) => has_surface = true,
                None => {}
            }
        }
        if !has_raw {
            self.raw_rebases.remove(pane);
        }
        if !has_surface {
            self.surface_drivers.remove(pane);
        }
    }

    pub(super) fn remove_drained_legacy_subscriptions(
        &mut self,
        pane: &PaneOutputSubscriptionKey,
    ) -> bool {
        let ids = self.registry.ids_for_pane(pane);
        let mut removed_any = false;
        for id in ids {
            if !self.receivers.contains_key(&id) {
                continue;
            }
            self.receivers.remove(&id);
            let _ = self.registry.unsubscribe(id);
            removed_any = true;
        }
        self.discard_drain_if_unused(pane);
        removed_any
    }

    pub(in crate::handler) fn expire_pane_drain(
        &mut self,
        pane: &PaneOutputSubscriptionKey,
        now: Instant,
    ) {
        let _ = self.remove_drained_legacy_subscriptions(pane);
        for mode in [PaneStreamMode::Raw, PaneStreamMode::Surface] {
            self.end_pane_streams(pane, mode, PaneStreamEndReason::PaneRemoved, now);
        }
        self.draining_pane_progress.remove(pane);
        self.draining_stream_sources.remove(pane);
    }
}

#[cfg(test)]
#[path = "output_subscription_state_raw_tests.rs"]
mod raw_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use rmux_core::PaneId;
    use rmux_proto::SessionName;

    fn pane() -> PaneOutputSubscriptionKey {
        PaneOutputSubscriptionKey::new(
            SessionName::new("surface-init").expect("valid session name"),
            PaneId::new(7),
        )
    }

    #[tokio::test]
    async fn one_surface_initializer_wakes_all_waiters_before_re_election() {
        let mut state = OutputSubscriptionState::new(SubscriptionLimits::default());
        let pane = pane();
        let SurfaceDriverRoute::Initialize { token } = state.surface_driver_route(&pane) else {
            panic!("first caller must initialize");
        };
        let SurfaceDriverRoute::Wait(mut waiter) = state.surface_driver_route(&pane) else {
            panic!("second caller must wait");
        };

        state.finish_surface_initialization(token);
        waiter.changed().await.expect("initializer completion");
        assert!(*waiter.borrow());
        assert!(matches!(
            state.surface_driver_route(&pane),
            SurfaceDriverRoute::Initialize { .. }
        ));
    }

    #[tokio::test]
    async fn pane_removal_wakes_surface_initialization_waiters() {
        let mut state = OutputSubscriptionState::new(SubscriptionLimits::default());
        let pane = pane();
        assert!(matches!(
            state.surface_driver_route(&pane),
            SurfaceDriverRoute::Initialize { .. }
        ));
        let SurfaceDriverRoute::Wait(mut waiter) = state.surface_driver_route(&pane) else {
            panic!("second caller must wait");
        };

        state.remove_pane(&pane);
        waiter.changed().await.expect("pane-removal completion");
        assert!(*waiter.borrow());
    }

    #[tokio::test]
    async fn surface_initialization_token_survives_a_pane_rekey() {
        let mut state = OutputSubscriptionState::new(SubscriptionLimits::default());
        let previous = pane();
        let current = PaneOutputSubscriptionKey::new(
            SessionName::new("surface-moved").expect("valid session name"),
            previous.pane_id(),
        );
        let SurfaceDriverRoute::Initialize { token } = state.surface_driver_route(&previous) else {
            panic!("first caller must initialize");
        };
        let SurfaceDriverRoute::Wait(mut waiter) = state.surface_driver_route(&previous) else {
            panic!("second caller must wait");
        };

        state.rekey_pane(&previous, current.clone());
        state.finish_surface_initialization(token);

        waiter
            .changed()
            .await
            .expect("rekeyed initializer completion");
        assert!(*waiter.borrow());
        assert!(!state.surface_initializations.contains_key(&previous));
        assert!(!state.surface_initializations.contains_key(&current));
    }
}
