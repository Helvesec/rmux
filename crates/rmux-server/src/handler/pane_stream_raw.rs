use std::sync::Arc;
use std::time::Instant;

use rmux_core::events::{OutputCursorItem, DEFAULT_SUBSCRIPTION_BATCH_EVENTS};
use rmux_proto::{
    ErrorResponse, PaneRawBytes, PaneRawRebaseReason, PaneStreamCursorRequest, PaneStreamEndReason,
    PaneStreamEvent, PaneStreamLifecycleEvent, Response, RmuxError,
};

use crate::pane_io::{PaneObservationItem, PaneOutputReceiver};

use super::super::subscription_support::cursor_event_limit;
use super::{
    capture_source, materialize_raw_rebase, owned_stream_record, raw_reason,
    reserved_stream_key_if_owned, slow_consumer_response, stream_cursor_response,
    validate_detached_response, validate_raw_rebase_size, wrong_stream_mode, CachedRawRebase,
    PaneStreamSource, PaneStreamSubscription, RawRebaseGuard, RequestHandler,
};

impl RequestHandler {
    pub(super) async fn poll_raw_stream(
        &self,
        connection_id: u64,
        request: PaneStreamCursorRequest,
    ) -> Response {
        let now = Instant::now();
        let mut events = Vec::new();
        let (rebase, reached_limit) = {
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
            if reason.is_none() {
                for _ in 0..limit {
                    let Some(item) = stream.receiver.try_recv_observed() else {
                        break;
                    };
                    observed = observed.saturating_add(1);
                    match item {
                        PaneObservationItem::Invalidated(invalidation) => {
                            reason = Some(raw_reason(invalidation.reason));
                            break;
                        }
                        PaneObservationItem::ProcessExited { output_sequence } => {
                            events.push(PaneStreamEvent::Lifecycle(
                                PaneStreamLifecycleEvent::ProcessExited { output_sequence },
                            ));
                        }
                        PaneObservationItem::Output(OutputCursorItem::Gap(_)) => {
                            reason = Some(PaneRawRebaseReason::Lag);
                            break;
                        }
                        PaneObservationItem::Output(OutputCursorItem::Event(event)) => {
                            events.push(PaneStreamEvent::RawBytes(PaneRawBytes {
                                epoch: stream.epoch,
                                sequence: event.sequence(),
                                bytes: event.into_bytes(),
                            }));
                        }
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
                reason.is_none() && observed == limit,
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
            return response;
        };

        let mut rebase_guard =
            RawRebaseGuard::new(&self.subscriptions, request.subscription_id, token, reason);
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
        let (rebase, mut receiver, boundary) =
            match self.cached_or_captured_raw_rebase(&source, epoch, reason, include_snapshot) {
                Ok(result) => result,
                Err(_error) => {
                    self.finish_stream_after_end(request.subscription_id);
                    return stream_cursor_response(
                        request.subscription_id,
                        vec![PaneStreamEvent::End(PaneStreamEndReason::ProjectionFailed)],
                        false,
                    );
                }
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
            Some(PaneStreamSubscription::Reserved(_) | PaneStreamSubscription::Surface(_)) => {
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

        let captured = capture_source(source);
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
