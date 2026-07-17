use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use rmux_core::events::{
    OutputCursor, OutputCursorItem, OutputGap, PaneOutputSubscriptionKey, SubscriptionLimitError,
    SubscriptionLimits, SubscriptionRegistry,
};
use rmux_proto::{
    ErrorResponse, PaneOutputCursor, PaneOutputCursorRequest, PaneOutputCursorResponse,
    PaneOutputEvent, PaneOutputLagNotice, PaneOutputLagResponse, PaneOutputSubscriptionId,
    PaneOutputSubscriptionStart, PaneRecentOutput, PaneTarget, PaneTargetRef, Response, RmuxError,
    SubscribePaneOutputRefRequest, SubscribePaneOutputRequest, SubscribePaneOutputResponse,
    UnsubscribePaneOutputRequest, UnsubscribePaneOutputResponse,
};

use crate::pane_io::{PaneOutputReceiver, PaneOutputSender};
use crate::pane_terminals::{session_not_found, HandlerState};

use super::{PaneOutputSubscriptionReconciliation, RequestHandler};

// Keep lag diagnostics well below the detached RPC frame cap after bincode
// overhead and the rest of the response envelope are added.
const MAX_LAG_RECENT_BYTES: usize = 64 * 1024;
const EXITED_PANE_DRAIN_POLL_INTERVAL: Duration = Duration::from_millis(25);
const EXITED_PANE_DRAIN_IDLE_TIMEOUT: Duration = Duration::from_secs(2);

pub(crate) struct OutputSubscriptionState {
    registry: SubscriptionRegistry,
    receivers: HashMap<PaneOutputSubscriptionId, PaneOutputReceiver>,
    draining_panes: HashSet<PaneOutputSubscriptionKey>,
}

impl std::fmt::Debug for OutputSubscriptionState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OutputSubscriptionState")
            .field("registry", &self.registry)
            .field("receiver_count", &self.receivers.len())
            .field("draining_pane_count", &self.draining_panes.len())
            .finish()
    }
}

impl OutputSubscriptionState {
    pub(crate) fn new(limits: SubscriptionLimits) -> Self {
        Self {
            registry: SubscriptionRegistry::new(limits),
            receivers: HashMap::new(),
            draining_panes: HashSet::new(),
        }
    }

    fn limits(&self) -> SubscriptionLimits {
        self.registry.limits()
    }

    fn cleanup_stale(&mut self, now: Instant) {
        for record in self.registry.cleanup_stale(now) {
            self.receivers.remove(&record.id());
            self.discard_drain_if_unused(record.pane());
        }
    }

    fn remove_connection(&mut self, connection_id: u64) {
        for record in self.registry.remove_connection(connection_id) {
            self.receivers.remove(&record.id());
            self.discard_drain_if_unused(record.pane());
        }
    }

    fn remove_pane(&mut self, pane: &PaneOutputSubscriptionKey) -> bool {
        let removed = self.registry.remove_pane(pane);
        let removed_any = !removed.is_empty();
        for record in removed {
            self.receivers.remove(&record.id());
        }
        self.draining_panes.remove(pane);
        removed_any
    }

    fn rekey_pane(
        &mut self,
        previous: &PaneOutputSubscriptionKey,
        current: PaneOutputSubscriptionKey,
    ) {
        let was_draining = self.draining_panes.remove(previous);
        let _ = self.registry.rekey_pane(previous, current.clone());
        if was_draining {
            self.draining_panes.insert(current);
        }
    }

    fn begin_pane_drain(&mut self, pane: PaneOutputSubscriptionKey) -> bool {
        if !self.registry.contains_pane(&pane) {
            return false;
        }
        self.draining_panes.insert(pane);
        true
    }

    fn pane_is_draining(&self, pane: &PaneOutputSubscriptionKey) -> bool {
        self.draining_panes.contains(pane)
    }

    fn pane_drain_idle_for(
        &self,
        pane: &PaneOutputSubscriptionKey,
        now: Instant,
    ) -> Option<Duration> {
        let last_seen = self
            .registry
            .ids_for_pane(pane)
            .into_iter()
            .filter_map(|id| self.registry.get(id).map(|record| record.last_seen()))
            .max()?;
        Some(now.saturating_duration_since(last_seen))
    }

    pub(in crate::handler) fn is_empty(&self) -> bool {
        self.registry.is_empty()
    }

    fn remove_subscription(&mut self, subscription_id: PaneOutputSubscriptionId) {
        if let Some(record) = self.registry.unsubscribe(subscription_id) {
            self.receivers.remove(&subscription_id);
            self.discard_drain_if_unused(record.pane());
        }
    }

    fn discard_drain_if_unused(&mut self, pane: &PaneOutputSubscriptionKey) {
        if !self.registry.contains_pane(pane) {
            self.draining_panes.remove(pane);
        }
    }
}

impl RequestHandler {
    #[cfg(test)]
    pub(in crate::handler) fn pane_output_subscription_key_for_test(
        &self,
        subscription_id: PaneOutputSubscriptionId,
    ) -> Option<PaneOutputSubscriptionKey> {
        self.subscriptions
            .lock()
            .expect("subscription registry mutex must not be poisoned")
            .registry
            .get(subscription_id)
            .map(|record| record.pane().clone())
    }

    pub(in crate::handler) async fn handle_subscribe_pane_output(
        &self,
        connection_id: u64,
        request: SubscribePaneOutputRequest,
    ) -> Response {
        self.subscribe_pane_output(
            connection_id,
            PaneTargetRef::slot(request.target),
            request.start,
        )
        .await
    }

    pub(in crate::handler) async fn handle_subscribe_pane_output_ref(
        &self,
        connection_id: u64,
        request: SubscribePaneOutputRefRequest,
    ) -> Response {
        self.subscribe_pane_output(connection_id, request.target, request.start)
            .await
    }

    async fn subscribe_pane_output(
        &self,
        connection_id: u64,
        target_ref: PaneTargetRef,
        start: PaneOutputSubscriptionStart,
    ) -> Response {
        let now = Instant::now();
        let live_error = {
            // Keep the state guard until the registry insert commits. Every
            // runtime-owner mutation takes the same state -> subscriptions
            // lock order, so a subscription is registered wholly before or
            // wholly after its key is rekeyed.
            let state = self.state.lock().await;
            let source = resolve_pane_target_ref(&state, &target_ref).and_then(|target| {
                let pane_key = state.pane_output_subscription_key_for_target(&target)?;
                let output = state.pane_output_for_target(
                    target.session_name(),
                    target.window_index(),
                    target.pane_index(),
                )?;
                Ok((target, pane_key, output))
            });
            match source {
                Ok(source) => {
                    #[cfg(test)]
                    tests::pause_before_live_subscription_commit(&source.1).await;
                    return self.register_pane_output_subscription(
                        connection_id,
                        start,
                        now,
                        source,
                    );
                }
                Err(error) => error,
            }
        };

        if start == PaneOutputSubscriptionStart::Oldest {
            // Serialize a retained lookup and its registry insertion with
            // rename-session. The retained cache and pane-output registry are
            // both rekeyed while rename holds this state guard, preventing a
            // subscriber from cloning an old-name record and registering it
            // after the rename transaction.
            let _state = self.state.lock().await;
            match retained_lookup(self, &target_ref, now) {
                Ok(Some((target, retained))) if retained.output().is_some() => {
                    let output = retained
                        .output()
                        .expect("retained output presence was checked")
                        .clone();
                    return self.register_pane_output_subscription(
                        connection_id,
                        start,
                        now,
                        (target, retained.pane().clone(), output),
                    );
                }
                Ok(Some(_)) | Ok(None) => {}
                Err(error) => return Response::Error(ErrorResponse { error }),
            }
        }
        Response::Error(ErrorResponse { error: live_error })
    }

    fn register_pane_output_subscription(
        &self,
        connection_id: u64,
        start: PaneOutputSubscriptionStart,
        now: Instant,
        source: (PaneTarget, PaneOutputSubscriptionKey, PaneOutputSender),
    ) -> Response {
        let (target, pane_key, output) = source;
        let receiver = match start {
            PaneOutputSubscriptionStart::Now => output.subscribe(),
            PaneOutputSubscriptionStart::Oldest => output.subscribe_from_oldest(),
        };
        let mut subscriptions = self
            .subscriptions
            .lock()
            .expect("subscription registry mutex must not be poisoned");
        subscriptions.cleanup_stale(now);
        let record = match subscriptions
            .registry
            .subscribe(connection_id, pane_key.clone(), now)
        {
            Ok(record) => record,
            Err(error) => {
                return Response::Error(ErrorResponse {
                    error: subscription_limit_error(error),
                });
            }
        };
        let cursor = cursor_dto(receiver.cursor());
        let subscription_id = record.id();
        subscriptions.receivers.insert(record.id(), receiver);
        Response::SubscribePaneOutput(SubscribePaneOutputResponse {
            subscription_id,
            target,
            pane_id: pane_key.pane_id(),
            cursor,
        })
    }

    pub(in crate::handler) async fn handle_unsubscribe_pane_output(
        &self,
        connection_id: u64,
        request: UnsubscribePaneOutputRequest,
    ) -> Response {
        let now = Instant::now();
        let mut subscriptions = self
            .subscriptions
            .lock()
            .expect("subscription registry mutex must not be poisoned");
        subscriptions.cleanup_stale(now);

        let Some(record) = subscriptions.registry.get(request.subscription_id).cloned() else {
            return Response::UnsubscribePaneOutput(UnsubscribePaneOutputResponse {
                subscription_id: request.subscription_id,
                removed: false,
            });
        };
        if record.connection_id() != connection_id {
            return Response::Error(ErrorResponse {
                error: RmuxError::Server("subscription is not owned by this connection".to_owned()),
            });
        }

        let removed = subscriptions
            .registry
            .get(request.subscription_id)
            .is_some();
        subscriptions.remove_subscription(request.subscription_id);
        Response::UnsubscribePaneOutput(UnsubscribePaneOutputResponse {
            subscription_id: request.subscription_id,
            removed,
        })
    }

    pub(in crate::handler) async fn handle_pane_output_cursor(
        &self,
        connection_id: u64,
        request: PaneOutputCursorRequest,
    ) -> Response {
        let now = Instant::now();
        let (items, cursor, limit) = {
            let mut subscriptions = self
                .subscriptions
                .lock()
                .expect("subscription registry mutex must not be poisoned");
            subscriptions.cleanup_stale(now);
            let limit =
                match cursor_event_limit(request.max_events, subscriptions.limits().batch_events())
                {
                    Ok(limit) => limit,
                    Err(error) => return Response::Error(ErrorResponse { error }),
                };

            let Some(record) = subscriptions.registry.get(request.subscription_id).cloned() else {
                return Response::Error(ErrorResponse {
                    error: RmuxError::Server("subscription not found".to_owned()),
                });
            };
            if record.connection_id() != connection_id {
                return Response::Error(ErrorResponse {
                    error: RmuxError::Server(
                        "subscription is not owned by this connection".to_owned(),
                    ),
                });
            }
            let _ = subscriptions.registry.touch(request.subscription_id, now);

            let Some(receiver) = subscriptions.receivers.get_mut(&request.subscription_id) else {
                subscriptions.remove_subscription(request.subscription_id);
                return Response::Error(ErrorResponse {
                    error: RmuxError::Server("subscription receiver not found".to_owned()),
                });
            };

            let items = receiver.try_recv_batch(limit);
            let cursor = cursor_dto(receiver.cursor());
            (items, cursor, limit)
        };

        let mut events = Vec::new();
        for item in items {
            match item {
                OutputCursorItem::Event(event) => {
                    events.push(PaneOutputEvent {
                        sequence: event.sequence(),
                        bytes: event.into_bytes(),
                    });
                }
                OutputCursorItem::Gap(gap) => {
                    return Response::PaneOutputLag(Box::new(PaneOutputLagResponse {
                        subscription_id: request.subscription_id,
                        cursor,
                        lag: lag_dto(&gap),
                    }));
                }
            }
        }
        Response::PaneOutputCursor(PaneOutputCursorResponse {
            subscription_id: request.subscription_id,
            cursor,
            limited: events.len() == limit,
            events,
        })
    }

    pub(crate) fn cleanup_connection_subscriptions_sync(&self, connection_id: u64) {
        {
            let mut subscriptions = self
                .subscriptions
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            subscriptions.remove_connection(connection_id);
        }
        let _ = self.request_shutdown_if_pending();
    }

    pub(crate) async fn cleanup_pane_output_subscriptions(
        &self,
        panes: &[PaneOutputSubscriptionKey],
    ) {
        {
            let mut subscriptions = self
                .subscriptions
                .lock()
                .expect("subscription registry mutex must not be poisoned");
            for pane in panes {
                let _ = subscriptions.remove_pane(pane);
            }
        }
        let _ = self.request_shutdown_if_pending();
    }

    /// Applies the complete pane-output registry delta for one committed state
    /// mutation while the caller still owns the state lock.
    ///
    /// Returning whether a live record was removed lets callers retry pending
    /// idle shutdown only after both state and registry locks have been
    /// released.
    pub(in crate::handler) fn apply_pane_output_subscription_reconciliation(
        &self,
        reconciliation: PaneOutputSubscriptionReconciliation,
    ) -> bool {
        let (rekeys, removals) = reconciliation.into_parts();
        let mut subscriptions = self
            .subscriptions
            .lock()
            .expect("subscription registry mutex must not be poisoned");
        for (previous, current) in rekeys {
            subscriptions.rekey_pane(&previous, current);
        }
        let mut removed_any = false;
        for pane in removals {
            removed_any = subscriptions.remove_pane(&pane) || removed_any;
        }
        removed_any
    }

    pub(crate) fn rekey_pane_output_subscriptions(
        &self,
        rekeys: &[(PaneOutputSubscriptionKey, PaneOutputSubscriptionKey)],
    ) {
        let mut subscriptions = self
            .subscriptions
            .lock()
            .expect("subscription registry mutex must not be poisoned");
        for (previous, current) in rekeys {
            subscriptions.rekey_pane(previous, current.clone());
        }
    }

    pub(crate) async fn drain_exited_pane_output_subscriptions(
        &self,
        pane: PaneOutputSubscriptionKey,
    ) {
        let should_watch = {
            let mut subscriptions = self
                .subscriptions
                .lock()
                .expect("subscription registry mutex must not be poisoned");
            subscriptions.begin_pane_drain(pane.clone())
        };
        if should_watch {
            self.watch_exited_pane_drain(pane);
        }
    }

    fn watch_exited_pane_drain(&self, pane: PaneOutputSubscriptionKey) {
        let handler = self.downgrade();
        tokio::spawn(async move {
            loop {
                let Some(handler) = handler.upgrade() else {
                    return;
                };
                if handler.pane_drain_finished(&pane).await {
                    return;
                }
                if handler
                    .pane_drain_idle_for(&pane)
                    .await
                    .is_some_and(|idle_for| idle_for >= EXITED_PANE_DRAIN_IDLE_TIMEOUT)
                {
                    handler
                        .cleanup_pane_output_subscriptions(std::slice::from_ref(&pane))
                        .await;
                    return;
                }
                tokio::time::sleep(EXITED_PANE_DRAIN_POLL_INTERVAL).await;
            }
        });
    }

    async fn pane_drain_finished(&self, pane: &PaneOutputSubscriptionKey) -> bool {
        let subscriptions = self
            .subscriptions
            .lock()
            .expect("subscription registry mutex must not be poisoned");
        !subscriptions.pane_is_draining(pane)
    }

    async fn pane_drain_idle_for(&self, pane: &PaneOutputSubscriptionKey) -> Option<Duration> {
        let subscriptions = self
            .subscriptions
            .lock()
            .expect("subscription registry mutex must not be poisoned");
        subscriptions.pane_drain_idle_for(pane, Instant::now())
    }
}

fn retained_lookup(
    handler: &RequestHandler,
    target: &PaneTargetRef,
    now: Instant,
) -> Result<
    Option<(
        PaneTarget,
        super::exited_output_support::RetainedExitedPaneOutput,
    )>,
    RmuxError,
> {
    match target {
        PaneTargetRef::Slot(target) => Ok(handler
            .retained_exited_pane_output(target, now)
            .map(|retained| (target.clone(), retained))),
        PaneTargetRef::Id {
            session_name,
            pane_id,
        } => {
            let pane_key = PaneOutputSubscriptionKey::new(session_name.clone(), *pane_id);
            Ok(handler.retained_exited_pane_output_by_pane(&pane_key, now))
        }
    }
}

fn resolve_pane_target_ref(
    state: &HandlerState,
    target: &PaneTargetRef,
) -> Result<PaneTarget, RmuxError> {
    match target {
        PaneTargetRef::Slot(target) => Ok(target.clone()),
        PaneTargetRef::Id {
            session_name,
            pane_id,
        } => {
            let session = state
                .sessions
                .session(session_name)
                .ok_or_else(|| session_not_found(session_name))?;
            let window_index = session
                .window_index_for_pane_id(*pane_id)
                .ok_or_else(|| RmuxError::pane_not_found(session_name.clone(), *pane_id))?;
            let pane_index = session
                .window_at(window_index)
                .and_then(|window| {
                    window
                        .panes()
                        .iter()
                        .find(|pane| pane.id() == *pane_id)
                        .map(|pane| pane.index())
                })
                .ok_or_else(|| RmuxError::pane_not_found(session_name.clone(), *pane_id))?;
            Ok(PaneTarget::with_window(
                session_name.clone(),
                window_index,
                pane_index,
            ))
        }
    }
}

fn cursor_event_limit(requested: Option<u16>, default: usize) -> Result<usize, RmuxError> {
    match requested {
        Some(0) => Err(RmuxError::Server(
            "pane output cursor max_events must be greater than zero".to_owned(),
        )),
        Some(value) => Ok(usize::from(value).min(default)),
        None => Ok(default),
    }
}

fn cursor_dto(cursor: &OutputCursor) -> PaneOutputCursor {
    PaneOutputCursor {
        next_sequence: cursor.next_sequence(),
        missed_events: cursor.missed_events(),
    }
}

fn lag_dto(gap: &OutputGap) -> PaneOutputLagNotice {
    let recent = gap.recent_snapshot();
    let mut recent_bytes = recent.bytes().to_vec();
    let truncated = recent_bytes.len() > MAX_LAG_RECENT_BYTES;
    if truncated {
        recent_bytes = recent_bytes[recent_bytes.len() - MAX_LAG_RECENT_BYTES..].to_vec();
    }
    PaneOutputLagNotice {
        expected_sequence: gap.expected_sequence(),
        resume_sequence: gap.resume_sequence(),
        missed_events: gap.missed_events(),
        newest_sequence: gap.newest_sequence(),
        recent: PaneRecentOutput {
            bytes: recent_bytes,
            oldest_sequence: if truncated {
                None
            } else {
                recent.oldest_sequence()
            },
            newest_sequence: recent.newest_sequence(),
        },
    }
}

pub(in crate::handler) fn subscription_limit_error(error: SubscriptionLimitError) -> RmuxError {
    subscription_limit_error_for("pane output", error)
}

pub(in crate::handler) fn pane_state_subscription_limit_error(
    error: SubscriptionLimitError,
) -> RmuxError {
    subscription_limit_error_for("pane state", error)
}

fn subscription_limit_error_for(kind: &str, error: SubscriptionLimitError) -> RmuxError {
    match error {
        SubscriptionLimitError::PerConnection { limit } => RmuxError::Server(format!(
            "{kind} subscription limit exceeded for connection (limit {limit})"
        )),
        SubscriptionLimitError::PerPane { limit } => RmuxError::Server(format!(
            "{kind} subscription limit exceeded for pane (limit {limit})"
        )),
    }
}

#[cfg(test)]
#[path = "handler_subscriptions_tests.rs"]
mod tests;
