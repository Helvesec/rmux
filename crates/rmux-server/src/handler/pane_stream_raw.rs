use std::sync::{Arc, Mutex as StdMutex, Weak};
use std::time::Instant;

use rmux_core::events::{OutputCursorItem, DEFAULT_SUBSCRIPTION_BATCH_EVENTS};
use rmux_proto::{
    ErrorResponse, PaneRawBytes, PaneRawRebaseReason, PaneStreamCursorRequest, PaneStreamEndReason,
    PaneStreamEvent, PaneStreamLifecycleEvent, Response, RmuxError,
    DEFAULT_MAX_DETACHED_FRAME_LENGTH,
};

use crate::pane_io::{PaneBoundary, PaneObservationItem, PaneOutputReceiver};

use super::super::subscription_support::{
    cursor_event_limit, OutputSubscriptionState, RawInitializationRoute,
};
use super::protocol::detached_response_size;
use super::types::RawPaneStream;
use super::{
    capture_source, materialize_raw_rebase, owned_stream_record, raw_reason,
    reserved_stream_key_if_owned, slow_consumer_response, stream_cursor_response,
    subscribe_response, validate_detached_response, validate_raw_rebase_size, wrong_stream_mode,
    CachedRawRebase, CapturedPaneBoundary, PaneStreamSource, PaneStreamSubscription,
    RawRebaseGuard, RequestHandler,
};

fn encoded_stream_event_size(event: &PaneStreamEvent) -> Result<usize, RmuxError> {
    let encoded =
        bincode::serialized_size(event).map_err(|error| RmuxError::Encode(error.to_string()))?;
    Ok(usize::try_from(encoded).unwrap_or(usize::MAX))
}

fn encoded_raw_bytes_size(epoch: u64, sequence: u64, byte_len: usize) -> Result<usize, RmuxError> {
    let empty = PaneStreamEvent::RawBytes(PaneRawBytes {
        epoch,
        sequence,
        bytes: Vec::new(),
    });
    Ok(encoded_stream_event_size(&empty)?.saturating_add(byte_len))
}

pub(super) enum RawInitializationOutcome {
    Complete(Response),
    Capture {
        source: PaneStreamSource,
        guard: RawInitializationGuard,
    },
}

pub(super) struct RawInitializationGuard {
    subscriptions: Weak<StdMutex<OutputSubscriptionState>>,
    token: u64,
}

pub(super) struct RawSubscriptionStart {
    receiver: PaneOutputReceiver,
    boundary: PaneBoundary,
    rebase: rmux_proto::PaneRawRebase,
    include_snapshot: bool,
    cached_rebase: Option<Arc<CachedRawRebase>>,
}

impl RawSubscriptionStart {
    pub(super) fn captured(
        captured: CapturedPaneBoundary,
        rebase: rmux_proto::PaneRawRebase,
        include_snapshot: bool,
    ) -> Self {
        Self {
            receiver: captured.receiver,
            boundary: captured.boundary,
            rebase,
            include_snapshot,
            cached_rebase: None,
        }
    }

    fn cached(
        receiver: PaneOutputReceiver,
        cached_rebase: Arc<CachedRawRebase>,
        rebase: rmux_proto::PaneRawRebase,
        include_snapshot: bool,
    ) -> Self {
        Self {
            receiver,
            boundary: cached_rebase.boundary,
            rebase,
            include_snapshot,
            cached_rebase: Some(cached_rebase),
        }
    }
}

impl RawInitializationGuard {
    fn new(subscriptions: &Arc<StdMutex<OutputSubscriptionState>>, token: u64) -> Self {
        Self {
            subscriptions: Arc::downgrade(subscriptions),
            token,
        }
    }
}

impl Drop for RawInitializationGuard {
    fn drop(&mut self) {
        let Some(subscriptions) = self.subscriptions.upgrade() else {
            return;
        };
        subscriptions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .finish_raw_initialization(self.token);
    }
}

impl RequestHandler {
    pub(super) async fn prepare_initial_raw_subscription(
        &self,
        connection_id: u64,
        subscription_id: rmux_proto::PaneOutputSubscriptionId,
        mut source: PaneStreamSource,
        mut route: RawInitializationRoute,
        include_snapshot: bool,
    ) -> RawInitializationOutcome {
        loop {
            match route {
                RawInitializationRoute::Ready(cached) => {
                    if let Some(receiver) = source.output.subscribe_at_boundary(cached.boundary) {
                        let rebase = Self::initial_rebase_from_cache(&cached, include_snapshot);
                        return RawInitializationOutcome::Complete(self.finish_raw_subscription(
                            connection_id,
                            subscription_id,
                            source,
                            RawSubscriptionStart::cached(
                                receiver,
                                cached,
                                rebase,
                                include_snapshot,
                            ),
                        ));
                    }
                    let mut subscriptions = self
                        .subscriptions
                        .lock()
                        .expect("subscription registry mutex must not be poisoned");
                    let Some(current_key) = reserved_stream_key_if_owned(
                        &subscriptions,
                        connection_id,
                        subscription_id,
                        source.key.pane_id(),
                    ) else {
                        return RawInitializationOutcome::Complete(
                            super::reserved_stream_lost_response(),
                        );
                    };
                    source.key = current_key;
                    subscriptions.discard_raw_rebase_if_current(&source.key, &cached);
                    route = subscriptions.raw_initialization_route(&source.key, include_snapshot);
                }
                RawInitializationRoute::Initialize { token } => {
                    return RawInitializationOutcome::Capture {
                        source,
                        guard: RawInitializationGuard::new(&self.subscriptions, token),
                    };
                }
                RawInitializationRoute::Wait(mut completion) => {
                    let _ = completion.changed().await;
                    let mut subscriptions = self
                        .subscriptions
                        .lock()
                        .expect("subscription registry mutex must not be poisoned");
                    let Some(current_key) = reserved_stream_key_if_owned(
                        &subscriptions,
                        connection_id,
                        subscription_id,
                        source.key.pane_id(),
                    ) else {
                        return RawInitializationOutcome::Complete(
                            super::reserved_stream_lost_response(),
                        );
                    };
                    source.key = current_key;
                    route = subscriptions.raw_initialization_route(&source.key, include_snapshot);
                }
            }
        }
    }

    fn initial_rebase_from_cache(
        cached: &CachedRawRebase,
        include_snapshot: bool,
    ) -> rmux_proto::PaneRawRebase {
        let rebase = &cached.rebase;
        rmux_proto::PaneRawRebase {
            epoch: 1,
            generation: rebase.generation,
            invalidation_revision: rebase.invalidation_revision,
            next_sequence: rebase.next_sequence,
            cols: rebase.cols,
            rows: rebase.rows,
            keyframe: rebase.keyframe.clone(),
            alternate: rebase.alternate,
            coverage: rebase.coverage,
            snapshot: if include_snapshot {
                rebase.snapshot.clone()
            } else {
                None
            },
            reason: PaneRawRebaseReason::Initial,
        }
    }

    pub(super) fn finish_raw_subscription(
        &self,
        connection_id: u64,
        subscription_id: rmux_proto::PaneOutputSubscriptionId,
        source: PaneStreamSource,
        start: RawSubscriptionStart,
    ) -> Response {
        let cached_rebase = start.cached_rebase.unwrap_or_else(|| {
            Arc::new(CachedRawRebase {
                boundary: start.boundary,
                rebase: start.rebase.clone(),
            })
        });
        let response = subscribe_response(
            subscription_id,
            &source,
            PaneStreamEvent::RawRebase(Box::new(start.rebase)),
        );
        if let Err(error) = validate_detached_response(&response) {
            self.remove_reserved_stream(subscription_id);
            return Response::Error(ErrorResponse { error });
        }
        let mut subscriptions = self
            .subscriptions
            .lock()
            .expect("subscription registry mutex must not be poisoned");
        let Some(current_key) = reserved_stream_key_if_owned(
            &subscriptions,
            connection_id,
            subscription_id,
            source.key.pane_id(),
        ) else {
            return super::reserved_stream_lost_response();
        };
        let end_reason = subscriptions
            .streams
            .get(&subscription_id)
            .and_then(PaneStreamSubscription::end_reason);
        subscriptions.streams.insert(
            subscription_id,
            PaneStreamSubscription::Raw(RawPaneStream::new(
                start.receiver,
                1,
                start.include_snapshot,
                end_reason,
            )),
        );
        subscriptions.raw_rebases.insert(current_key, cached_rebase);
        response
    }

    pub(super) async fn poll_raw_stream(
        &self,
        connection_id: u64,
        request: PaneStreamCursorRequest,
    ) -> Response {
        let now = Instant::now();
        let mut events = Vec::new();
        let mut response_size = match detached_response_size(&stream_cursor_response(
            request.subscription_id,
            Vec::new(),
            false,
        )) {
            Ok(size) => size,
            Err(error) => return Response::Error(ErrorResponse { error }),
        };
        let (rebase, reached_limit, finish_after_end) = {
            let mut subscriptions = self
                .subscriptions
                .lock()
                .expect("subscription registry mutex must not be poisoned");
            subscriptions.cleanup_stale(now);
            let limit = match cursor_event_limit(
                request.max_events,
                subscriptions
                    .limits()
                    .batch_events()
                    .min(DEFAULT_SUBSCRIPTION_BATCH_EVENTS),
            ) {
                Ok(limit) => limit,
                Err(error) => return Response::Error(ErrorResponse { error }),
            };
            let key =
                match owned_stream_record(&subscriptions, connection_id, request.subscription_id) {
                    Ok(record) => record.pane().clone(),
                    Err(error) => return Response::Error(ErrorResponse { error }),
                };
            let _ = subscriptions.registry.touch(request.subscription_id, now);
            let Some(PaneStreamSubscription::Raw(stream)) =
                subscriptions.streams.get_mut(&request.subscription_id)
            else {
                return wrong_stream_mode();
            };
            if stream.is_rebasing() {
                return stream_cursor_response(request.subscription_id, events, false);
            }
            let mut reason = stream.pending_rebase();
            let mut observed = 0_usize;
            let mut reached_frame_limit = false;
            let mut source_drained = false;
            if reason.is_none() {
                for _ in 0..limit {
                    let item = stream
                        .take_pending_observation()
                        .or_else(|| stream.receiver.try_recv_observed());
                    let Some(item) = item else {
                        source_drained = true;
                        break;
                    };
                    observed = observed.saturating_add(1);
                    match item {
                        PaneObservationItem::Invalidated(invalidation) => {
                            reason = Some(raw_reason(invalidation.reason));
                            break;
                        }
                        PaneObservationItem::ProcessExited { output_sequence } => {
                            let event = PaneStreamEvent::Lifecycle(
                                PaneStreamLifecycleEvent::ProcessExited { output_sequence },
                            );
                            let event_size = match encoded_stream_event_size(&event) {
                                Ok(size) => size,
                                Err(error) => return Response::Error(ErrorResponse { error }),
                            };
                            let next_response_size = response_size.saturating_add(event_size);
                            if !events.is_empty()
                                && next_response_size > DEFAULT_MAX_DETACHED_FRAME_LENGTH
                            {
                                stream.defer_observation(PaneObservationItem::ProcessExited {
                                    output_sequence,
                                });
                                reached_frame_limit = true;
                                break;
                            }
                            response_size = next_response_size;
                            events.push(event);
                        }
                        PaneObservationItem::Output(OutputCursorItem::Gap(_)) => {
                            reason = Some(PaneRawRebaseReason::Lag);
                            break;
                        }
                        PaneObservationItem::Output(OutputCursorItem::Event(event)) => {
                            let event_size = match encoded_raw_bytes_size(
                                stream.epoch,
                                event.sequence(),
                                event.byte_len(),
                            ) {
                                Ok(size) => size,
                                Err(error) => return Response::Error(ErrorResponse { error }),
                            };
                            let next_response_size = response_size.saturating_add(event_size);
                            if !events.is_empty()
                                && next_response_size > DEFAULT_MAX_DETACHED_FRAME_LENGTH
                            {
                                stream.defer_observation(PaneObservationItem::Output(
                                    OutputCursorItem::Event(event),
                                ));
                                reached_frame_limit = true;
                                break;
                            }
                            response_size = next_response_size;
                            events.push(PaneStreamEvent::RawBytes(PaneRawBytes {
                                epoch: stream.epoch,
                                sequence: event.sequence(),
                                bytes: event.into_bytes(),
                            }));
                        }
                    }
                }
            }
            let mut finish_after_end = false;
            if reason.is_none() && source_drained {
                if let Some(end_reason) = stream.end_reason() {
                    let end = PaneStreamEvent::End(end_reason);
                    let event_size = match encoded_stream_event_size(&end) {
                        Ok(size) => size,
                        Err(error) => return Response::Error(ErrorResponse { error }),
                    };
                    let next_response_size = response_size.saturating_add(event_size);
                    if events.is_empty() || next_response_size <= DEFAULT_MAX_DETACHED_FRAME_LENGTH
                    {
                        events.push(end);
                        finish_after_end = true;
                    } else {
                        reached_frame_limit = true;
                    }
                }
            }
            (
                reason.map(|reason| {
                    let token = stream
                        .begin_rebase()
                        .expect("checked pane stream is not already rebasing");
                    (
                        key,
                        token,
                        reason,
                        stream.epoch.saturating_add(1),
                        stream.include_snapshot,
                        stream.receiver.observed_process_exit_revision(),
                    )
                }),
                reason.is_none() && (observed == limit || reached_frame_limit),
                finish_after_end,
            )
        };

        let Some((key, token, reason, epoch, include_snapshot, observed_process_exit_revision)) =
            rebase
        else {
            let response = stream_cursor_response(request.subscription_id, events, reached_limit);
            if let Err(error) = validate_detached_response(&response) {
                self.finish_stream_after_end(request.subscription_id);
                return match error {
                    RmuxError::FrameTooLarge { .. } => {
                        slow_consumer_response(request.subscription_id)
                    }
                    error => Response::Error(ErrorResponse { error }),
                };
            }
            if finish_after_end {
                self.finish_stream_after_end(request.subscription_id);
            }
            return response;
        };

        let mut rebase_guard =
            RawRebaseGuard::new(&self.subscriptions, request.subscription_id, token, reason);
        let mut captured = None;
        for _ in 0..super::MAX_SOURCE_CAPTURE_ATTEMPTS {
            let source = match self
                .resolve_stream_source_for_pane(key.pane_id(), key.runtime_session_name())
                .await
            {
                Ok(source) => source,
                Err(_) => {
                    events.push(PaneStreamEvent::End(PaneStreamEndReason::PaneRemoved));
                    self.finish_stream_after_end(request.subscription_id);
                    return stream_cursor_response(request.subscription_id, events, false);
                }
            };
            match self.cached_or_captured_raw_rebase(&source, epoch, reason, include_snapshot) {
                Ok(result) if result.2.generation == source.generation => {
                    captured = Some(result);
                    break;
                }
                Ok(_) => {}
                Err(_error) => {
                    self.finish_stream_after_end(request.subscription_id);
                    return stream_cursor_response(
                        request.subscription_id,
                        vec![PaneStreamEvent::End(PaneStreamEndReason::ProjectionFailed)],
                        false,
                    );
                }
            }
        }
        let Some((rebase, mut receiver, boundary)) = captured else {
            self.finish_stream_after_end(request.subscription_id);
            return stream_cursor_response(
                request.subscription_id,
                vec![PaneStreamEvent::End(PaneStreamEndReason::ProjectionFailed)],
                false,
            );
        };
        receiver.preserve_process_exits_since(observed_process_exit_revision);
        if let Err(error) = validate_raw_rebase_size(&rebase) {
            self.finish_stream_after_end(request.subscription_id);
            return match error {
                RmuxError::FrameTooLarge { .. } => slow_consumer_response(request.subscription_id),
                error => Response::Error(ErrorResponse { error }),
            };
        }
        let rebase_event = PaneStreamEvent::RawRebase(Box::new(rebase.clone()));
        events.push(rebase_event);
        let response = stream_cursor_response(request.subscription_id, events, false);
        if let Err(error) = validate_detached_response(&response) {
            self.finish_stream_after_end(request.subscription_id);
            return match error {
                RmuxError::FrameTooLarge { .. } => slow_consumer_response(request.subscription_id),
                error => Response::Error(ErrorResponse { error }),
            };
        }

        let mut subscriptions = self
            .subscriptions
            .lock()
            .expect("subscription registry mutex must not be poisoned");
        let Some(current_key) = reserved_stream_key_if_owned(
            &subscriptions,
            connection_id,
            request.subscription_id,
            key.pane_id(),
        ) else {
            return stream_cursor_response(request.subscription_id, Vec::new(), false);
        };
        let finished = match subscriptions.streams.get_mut(&request.subscription_id) {
            Some(PaneStreamSubscription::Raw(stream)) => {
                stream.finish_rebase(token, receiver, epoch)
            }
            Some(PaneStreamSubscription::Reserved { .. } | PaneStreamSubscription::Surface(_)) => {
                return wrong_stream_mode()
            }
            None => {
                return stream_cursor_response(request.subscription_id, Vec::new(), false);
            }
        };
        if !finished {
            return stream_cursor_response(request.subscription_id, Vec::new(), false);
        }
        subscriptions.raw_rebases.insert(
            current_key,
            Arc::new(CachedRawRebase {
                boundary,
                rebase: rebase.clone(),
            }),
        );
        rebase_guard.disarm();
        response
    }

    fn cached_or_captured_raw_rebase(
        &self,
        source: &PaneStreamSource,
        epoch: u64,
        reason: PaneRawRebaseReason,
        include_snapshot: bool,
    ) -> Result<
        (
            rmux_proto::PaneRawRebase,
            PaneOutputReceiver,
            crate::pane_io::PaneBoundary,
        ),
        RmuxError,
    > {
        let cached = self
            .subscriptions
            .lock()
            .expect("subscription registry mutex must not be poisoned")
            .raw_rebases
            .get(&source.key)
            .cloned();
        if let Some(cached) = cached {
            if !include_snapshot || cached.rebase.snapshot.is_some() {
                if let Some(receiver) = source.output.subscribe_at_boundary(cached.boundary) {
                    let mut rebase = cached.rebase.clone();
                    rebase.epoch = epoch;
                    rebase.reason = reason;
                    if !include_snapshot {
                        rebase.snapshot = None;
                    }
                    return Ok((rebase, receiver, cached.boundary));
                }
            }
        }

        let captured = capture_source(source)?;
        let rebase = materialize_raw_rebase(
            self,
            source.key.pane_id(),
            epoch,
            reason,
            include_snapshot,
            &captured,
        )?;
        Ok((rebase, captured.receiver, captured.boundary))
    }
}
