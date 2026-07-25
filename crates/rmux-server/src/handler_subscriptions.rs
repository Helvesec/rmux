use std::time::{Duration, Instant};

use rmux_core::events::{
    OutputCursor, OutputCursorItem, OutputGap, PaneOutputSubscriptionKey, SubscriptionLimitError,
};
#[cfg(test)]
use rmux_proto::PaneOutputSubscriptionId;
use rmux_proto::{
    ErrorResponse, PaneOutputCursor, PaneOutputCursorRequest, PaneOutputCursorResponse,
    PaneOutputEvent, PaneOutputLagNotice, PaneOutputLagResponse, PaneOutputSubscriptionStart,
    PaneRecentOutput, PaneStreamEndReason, PaneTarget, PaneTargetRef, Response, RmuxError,
    SubscribePaneOutputRefRequest, SubscribePaneOutputRequest, SubscribePaneOutputResponse,
    UnsubscribePaneOutputRequest, UnsubscribePaneOutputResponse,
};

use crate::pane_io::PaneOutputSender;
use crate::pane_terminals::{session_not_found, HandlerState};

use super::{PaneOutputSubscriptionReconciliation, RequestHandler};

#[path = "handler/output_subscription_state.rs"]
mod output_subscription_state;
pub(crate) use output_subscription_state::OutputSubscriptionState;
pub(in crate::handler) use output_subscription_state::{
    OutputSubscriptionAdmissionError, RawInitializationRoute, SurfaceDriverRoute,
};

// Keep lag diagnostics well below the detached RPC frame cap after bincode
// overhead and the rest of the response envelope are added.
const MAX_LAG_RECENT_BYTES: usize = 64 * 1024;
const EXITED_PANE_DRAIN_POLL_INTERVAL: Duration = Duration::from_millis(25);
const EXITED_PANE_DRAIN_IDLE_TIMEOUT: Duration = Duration::from_secs(2);

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
        let record = match subscriptions.subscribe(connection_id, pane_key.clone(), now) {
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
            let pane = record.pane().clone();

            let Some(receiver) = subscriptions.receivers.get_mut(&request.subscription_id) else {
                subscriptions.remove_subscription(request.subscription_id);
                return Response::Error(ErrorResponse {
                    error: RmuxError::Server("subscription receiver not found".to_owned()),
                });
            };

            let items = receiver.try_recv_batch(limit);
            let cursor = cursor_dto(receiver.cursor());
            if !items.is_empty() {
                subscriptions.note_pane_drain_progress(&pane, now);
            }
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

    pub(crate) async fn drain_removed_pane_output_subscriptions(
        &self,
        panes: &[PaneOutputSubscriptionKey],
    ) {
        for pane in panes {
            self.drain_exited_pane_output_subscriptions(pane.clone(), None)
                .await;
        }
        let _ = self.request_shutdown_if_pending();
    }

    /// Publishes the capture handles of panes an explicit kill just removed,
    /// keeping only the ones the kill actually removed. Callers capture the
    /// sources with [`capture_pane_stream_sources`] before committing the kill
    /// and stage them under that same state lock, mirroring the natural
    /// pane-exit path, so cursor paths never observe a gap between live state
    /// and the staged source.
    pub(in crate::handler) fn stage_removed_pane_stream_sources(
        &self,
        removed: &[PaneOutputSubscriptionKey],
        sources: Vec<super::pane_stream_support::PaneStreamSource>,
    ) {
        if removed.is_empty() || sources.is_empty() {
            return;
        }
        let mut subscriptions = self
            .subscriptions
            .lock()
            .expect("subscription registry mutex must not be poisoned");
        for source in sources {
            if removed.contains(&source.key) {
                subscriptions.stage_pane_drain_source(source.key.clone(), source);
            }
        }
    }

    /// Applies the complete pane-output registry delta for one committed state
    /// mutation while the caller still owns the state lock.
    ///
    /// Destroyed panes start the same buffered-output drain as `kill-pane` and
    /// natural pane exit instead of being torn down synchronously, so output a
    /// pane already published is delivered before its subscription ends.
    ///
    /// Returning whether a drain started lets callers retry pending idle
    /// shutdown only after both state and registry locks have been released.
    pub(in crate::handler) fn apply_pane_output_subscription_reconciliation(
        &self,
        reconciliation: PaneOutputSubscriptionReconciliation,
    ) -> bool {
        let now = Instant::now();
        let (rekeys, removals) = reconciliation.into_parts();
        let draining = {
            let mut subscriptions = self
                .subscriptions
                .lock()
                .expect("subscription registry mutex must not be poisoned");
            for (previous, current) in rekeys {
                subscriptions.rekey_pane(&previous, current);
            }
            let mut draining = Vec::new();
            for pane in removals {
                subscriptions.mark_pane_streams_ending(&pane, PaneStreamEndReason::PaneRemoved);
                if subscriptions.begin_pane_drain(pane.clone(), None, now) {
                    draining.push(pane);
                } else {
                    // Nothing left to drain for this pane: release its cached
                    // stream state and any stranded drain bookkeeping now.
                    subscriptions.remove_pane(&pane);
                }
            }
            draining
        };
        let drained_any = !draining.is_empty();
        for pane in draining {
            self.watch_exited_pane_drain(pane);
        }
        drained_any
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

    /// Makes an exited pane's immutable capture handles visible while the
    /// caller still holds the state lock that removed the pane. Cursor paths
    /// resolve live state first and this staged source second, so they cannot
    /// observe a gap between those two ownership locations.
    pub(in crate::handler) fn stage_exited_pane_stream_source(
        &self,
        pane: PaneOutputSubscriptionKey,
        source: super::pane_stream_support::PaneStreamSource,
    ) {
        self.subscriptions
            .lock()
            .expect("subscription registry mutex must not be poisoned")
            .stage_pane_drain_source(pane, source);
    }

    pub(in crate::handler) async fn drain_exited_pane_output_subscriptions(
        &self,
        pane: PaneOutputSubscriptionKey,
        source: Option<super::pane_stream_support::PaneStreamSource>,
    ) {
        let should_watch = {
            let mut subscriptions = self
                .subscriptions
                .lock()
                .expect("subscription registry mutex must not be poisoned");
            subscriptions.mark_pane_streams_ending(&pane, PaneStreamEndReason::PaneRemoved);
            subscriptions.begin_pane_drain(pane.clone(), source, Instant::now())
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
                    .cleanup_drained_pane_output_subscriptions_if_idle(&pane)
                    .await
                {
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

    async fn cleanup_drained_pane_output_subscriptions_if_idle(
        &self,
        pane: &PaneOutputSubscriptionKey,
    ) -> bool {
        let expired = {
            let mut subscriptions = self
                .subscriptions
                .lock()
                .expect("subscription registry mutex must not be poisoned");
            subscriptions.expire_pane_drain_if_idle(
                pane,
                Instant::now(),
                EXITED_PANE_DRAIN_IDLE_TIMEOUT,
            )
        };
        if expired {
            let _ = self.request_shutdown_if_pending();
        }
        expired
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
        PaneTargetRef::Id { pane_id, .. } => {
            // PaneId is daemon-global. A stable SDK handle may still carry
            // the session name from before a rename or inter-session move,
            // while retained output has already been rekeyed to the new name.
            Ok(handler.retained_exited_pane_output_entry_by_id(*pane_id, now))
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

/// Captures the immutable stream handles of panes an explicit kill is about to
/// remove, while the caller still owns the state lock that will commit the
/// kill. Once the panes leave `state.sessions` the handles are unrecoverable,
/// so a surface stream could no longer project the output that was already
/// buffered when the kill ran.
pub(in crate::handler) fn capture_pane_stream_sources(
    state: &HandlerState,
    panes: &[PaneOutputSubscriptionKey],
) -> Vec<super::pane_stream_support::PaneStreamSource> {
    panes
        .iter()
        .filter_map(|pane| {
            let target =
                state.pane_target_for_runtime_pane(pane.runtime_session_name(), pane.pane_id())?;
            super::pane_stream_support::stream_source_for_target(state, target).ok()
        })
        .collect()
}

pub(in crate::handler) fn cursor_event_limit(
    requested: Option<u16>,
    default: usize,
) -> Result<usize, RmuxError> {
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

pub(in crate::handler) fn subscription_limit_error(
    error: OutputSubscriptionAdmissionError,
) -> RmuxError {
    match error {
        OutputSubscriptionAdmissionError::Configured(error) => {
            subscription_limit_error_for("pane output", error)
        }
        OutputSubscriptionAdmissionError::Global { limit } => RmuxError::Server(format!(
            "pane output global subscription limit exceeded (limit {limit})"
        )),
    }
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
