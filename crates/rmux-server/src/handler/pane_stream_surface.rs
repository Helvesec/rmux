use std::sync::Arc;
use std::time::Instant;

use rmux_core::events::{OutputCursorItem, DEFAULT_SUBSCRIPTION_BATCH_EVENTS};
use rmux_proto::{
    ErrorResponse, PaneStreamCursorRequest, PaneStreamEndReason, PaneStreamEvent,
    PaneStreamLifecycleEvent, PaneStreamMode, Response, RmuxError,
};

use crate::pane_io::PaneObservationItem;

use super::super::subscription_support::{cursor_event_limit, OutputSubscriptionState};
use super::{
    capture_surface_source, materialize_surface_frame, owned_stream_record, slow_consumer_response,
    stream_cursor_response, validate_surface_frame_size, wrong_stream_mode, PaneStreamSubscription,
    PendingSurfaceRefresh, RequestHandler, SurfaceRefreshGuard,
};

impl RequestHandler {
    pub(super) async fn poll_surface_stream(
        &self,
        connection_id: u64,
        request: PaneStreamCursorRequest,
    ) -> Response {
        let now = Instant::now();
        let refresh = {
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
            let Some(driver) = subscriptions.surface_drivers.get_mut(&key) else {
                return Response::Error(ErrorResponse {
                    error: RmuxError::Server("pane surface driver not found".to_owned()),
                });
            };
            if driver.refreshing {
                return deliver_surface_latest(
                    &mut subscriptions,
                    request.subscription_id,
                    limit,
                    false,
                );
            }
            let pending_refresh = driver.pending_refresh();
            let mut dirty = pending_refresh.is_some();
            let mut reset = pending_refresh.is_some_and(|pending| pending.reset);
            let mut lifecycle_events = 0_u64;
            let mut frame_lifecycle_revision = pending_refresh
                .map_or(driver.lifecycle_revision, |pending| {
                    pending.frame_lifecycle_revision
                });
            let mut observed = 0_usize;
            for _ in 0..limit {
                let Some(item) = driver.receiver.try_recv_observed() else {
                    break;
                };
                observed = observed.saturating_add(1);
                let lifecycle_at_observation =
                    driver.lifecycle_revision.saturating_add(lifecycle_events);
                match item {
                    PaneObservationItem::Invalidated(_) => {
                        frame_lifecycle_revision = lifecycle_at_observation;
                        dirty = true;
                        reset = true;
                        break;
                    }
                    PaneObservationItem::ProcessExited { .. } => {
                        lifecycle_events = lifecycle_events.saturating_add(1);
                    }
                    PaneObservationItem::Output(OutputCursorItem::Gap(_)) => {
                        frame_lifecycle_revision = lifecycle_at_observation;
                        dirty = true;
                        reset = true;
                        break;
                    }
                    PaneObservationItem::Output(OutputCursorItem::Event(_)) => {
                        frame_lifecycle_revision = lifecycle_at_observation;
                        dirty = true;
                    }
                }
            }
            driver.lifecycle_revision = driver.lifecycle_revision.saturating_add(lifecycle_events);
            let source_batch_limited = observed == limit;
            if !dirty {
                return deliver_surface_latest(
                    &mut subscriptions,
                    request.subscription_id,
                    limit,
                    source_batch_limited,
                );
            }
            let Some(token) = driver.begin_refresh() else {
                return deliver_surface_latest(
                    &mut subscriptions,
                    request.subscription_id,
                    limit,
                    source_batch_limited,
                );
            };
            let epoch = if reset {
                driver.latest.epoch.saturating_add(1)
            } else {
                driver.latest.epoch
            };
            let pending = PendingSurfaceRefresh {
                reset,
                frame_lifecycle_revision,
            };
            let refresh = (
                key.clone(),
                token,
                pending,
                epoch,
                driver.revision.saturating_add(1),
                reset,
                driver.fingerprint.clone(),
                driver.receiver.observed_process_exit_revision(),
                frame_lifecycle_revision,
                limit,
                source_batch_limited,
            );
            if observed > 0 {
                subscriptions.note_pane_drain_progress(&key, now);
            }
            refresh
        };

        let (
            key,
            token,
            pending,
            epoch,
            revision,
            reset,
            previous_fingerprint,
            observed_process_exit_revision,
            frame_lifecycle_revision,
            limit,
            source_batch_limited,
        ) = refresh;
        let mut refresh_guard =
            SurfaceRefreshGuard::new(&self.subscriptions, key.pane_id(), token, pending);
        let mut captured = None;
        for _ in 0..super::MAX_SOURCE_CAPTURE_ATTEMPTS {
            let source = match self
                .resolve_stream_source_for_pane(key.pane_id(), key.runtime_session_name())
                .await
            {
                Ok(source) => source,
                Err(_) => {
                    self.finish_stream_after_end(request.subscription_id);
                    return stream_cursor_response(
                        request.subscription_id,
                        vec![PaneStreamEvent::End(PaneStreamEndReason::PaneRemoved)],
                        false,
                    );
                }
            };
            let candidate = match capture_surface_source(&source, &previous_fingerprint, reset) {
                Ok(candidate) => candidate,
                Err(_) => {
                    self.finish_stream_after_end(request.subscription_id);
                    return stream_cursor_response(
                        request.subscription_id,
                        vec![PaneStreamEvent::End(PaneStreamEndReason::ProjectionFailed)],
                        false,
                    );
                }
            };
            if candidate.boundary.generation == source.generation {
                captured = Some(candidate);
                break;
            }
        }
        let Some(mut captured) = captured else {
            self.finish_stream_after_end(request.subscription_id);
            return stream_cursor_response(
                request.subscription_id,
                vec![PaneStreamEvent::End(PaneStreamEndReason::ProjectionFailed)],
                false,
            );
        };
        captured
            .receiver
            .preserve_process_exits_since(observed_process_exit_revision);
        let Some(seed) = captured.seed.as_ref() else {
            let mut subscriptions = self
                .subscriptions
                .lock()
                .expect("subscription registry mutex must not be poisoned");
            if let Some(current_key) = subscriptions.surface_driver_key_for_pane_id(key.pane_id()) {
                if subscriptions
                    .surface_drivers
                    .get_mut(&current_key)
                    .is_some_and(|driver| {
                        driver.finish_unchanged_refresh(
                            token,
                            captured.receiver,
                            captured.fingerprint,
                        )
                    })
                {
                    refresh_guard.disarm();
                }
            }
            return deliver_surface_latest(
                &mut subscriptions,
                request.subscription_id,
                limit,
                source_batch_limited,
            );
        };
        let frame = match materialize_surface_frame(
            self,
            key.pane_id(),
            epoch,
            revision,
            captured.boundary.next_output_sequence,
            seed,
        ) {
            Ok(frame) => frame,
            Err(_error) => {
                let mut subscriptions = self
                    .subscriptions
                    .lock()
                    .expect("subscription registry mutex must not be poisoned");
                if let Some(current_key) =
                    subscriptions.surface_driver_key_for_pane_id(key.pane_id())
                {
                    subscriptions.end_pane_streams(
                        &current_key,
                        PaneStreamMode::Surface,
                        PaneStreamEndReason::ProjectionFailed,
                        Instant::now(),
                    );
                    subscriptions.ended_streams.remove(&request.subscription_id);
                }
                return stream_cursor_response(
                    request.subscription_id,
                    vec![PaneStreamEvent::End(PaneStreamEndReason::ProjectionFailed)],
                    false,
                );
            }
        };
        if let Err(error) = validate_surface_frame_size(&frame) {
            let mut subscriptions = self
                .subscriptions
                .lock()
                .expect("subscription registry mutex must not be poisoned");
            if let Some(current_key) = subscriptions.surface_driver_key_for_pane_id(key.pane_id()) {
                subscriptions.end_pane_streams(
                    &current_key,
                    PaneStreamMode::Surface,
                    PaneStreamEndReason::SlowConsumer,
                    Instant::now(),
                );
                subscriptions.ended_streams.remove(&request.subscription_id);
            }
            return match error {
                RmuxError::FrameTooLarge { .. } => slow_consumer_response(request.subscription_id),
                error => Response::Error(ErrorResponse { error }),
            };
        }

        let mut subscriptions = self
            .subscriptions
            .lock()
            .expect("subscription registry mutex must not be poisoned");
        let Some(current_key) = subscriptions.surface_driver_key_for_pane_id(key.pane_id()) else {
            return stream_cursor_response(request.subscription_id, Vec::new(), false);
        };
        let Some(driver) = subscriptions.surface_drivers.get_mut(&current_key) else {
            return stream_cursor_response(request.subscription_id, Vec::new(), false);
        };
        let finished = if !reset && surface_content_equal(&driver.latest, &frame) {
            driver.finish_unchanged_refresh(token, captured.receiver, captured.fingerprint)
        } else {
            driver.finish_refresh(
                token,
                captured.receiver,
                frame,
                captured.fingerprint,
                frame_lifecycle_revision,
            )
        };
        if finished {
            refresh_guard.disarm();
        }
        deliver_surface_latest(
            &mut subscriptions,
            request.subscription_id,
            limit,
            source_batch_limited,
        )
    }
}

fn deliver_surface_latest(
    subscriptions: &mut OutputSubscriptionState,
    subscription_id: rmux_proto::PaneOutputSubscriptionId,
    limit: usize,
    source_batch_limited: bool,
) -> Response {
    let Some(record) = subscriptions.registry.get(subscription_id) else {
        return Response::Error(ErrorResponse {
            error: RmuxError::Server("subscription not found".to_owned()),
        });
    };
    let key = record.pane().clone();
    let Some(driver) = subscriptions.surface_drivers.get(&key) else {
        return Response::Error(ErrorResponse {
            error: RmuxError::Server("pane surface driver not found".to_owned()),
        });
    };
    let frame = Arc::clone(&driver.latest);
    let lifecycle_revision = if driver.refreshing {
        driver.frame_lifecycle_revision
    } else {
        driver.lifecycle_revision
    };
    let frame_lifecycle_revision = driver.frame_lifecycle_revision;
    let driver_refreshing = driver.refreshing;
    let (events, limited, finish_after_end) = {
        let Some(PaneStreamSubscription::Surface(stream)) =
            subscriptions.streams.get_mut(&subscription_id)
        else {
            return wrong_stream_mode();
        };
        let mut events = Vec::new();
        let has_pending_frame = stream.delivered_revision < frame.revision;
        let lifecycle_before_frame = if has_pending_frame {
            frame_lifecycle_revision
        } else {
            stream.delivered_lifecycle_revision
        };
        append_surface_lifecycle_events(&mut events, stream, lifecycle_before_frame, limit);
        if events.len() < limit && has_pending_frame {
            if stream.delivered_epoch != frame.epoch {
                events.push(PaneStreamEvent::SurfaceReset(Box::new((*frame).clone())));
            } else {
                events.push(PaneStreamEvent::SurfacePatch(Box::new((*frame).clone())));
            }
            stream.delivered_revision = frame.revision;
            stream.delivered_epoch = frame.epoch;
        }
        append_surface_lifecycle_events(&mut events, stream, lifecycle_revision, limit);
        let projection_drained = !source_batch_limited
            && !driver_refreshing
            && stream.delivered_revision >= frame.revision
            && stream.delivered_lifecycle_revision >= lifecycle_revision;
        let finish_after_end =
            projection_drained && events.len() < limit && stream.end_reason.is_some();
        if finish_after_end {
            events.push(PaneStreamEvent::End(
                stream
                    .end_reason
                    .expect("ending surface stream has a reason"),
            ));
        }
        let limited = source_batch_limited
            || stream.delivered_revision < frame.revision
            || stream.delivered_lifecycle_revision < lifecycle_revision
            || (stream.end_reason.is_some() && !finish_after_end);
        (events, limited, finish_after_end)
    };
    if events
        .iter()
        .any(|event| !matches!(event, PaneStreamEvent::End(_)))
    {
        subscriptions.note_pane_drain_progress(&key, Instant::now());
    }
    if finish_after_end {
        subscriptions.remove_subscription(subscription_id);
    }
    stream_cursor_response(subscription_id, events, limited)
}

fn append_surface_lifecycle_events(
    events: &mut Vec<PaneStreamEvent>,
    stream: &mut super::SurfacePaneStream,
    target_revision: u64,
    limit: usize,
) {
    while events.len() < limit && stream.delivered_lifecycle_revision < target_revision {
        events.push(PaneStreamEvent::Lifecycle(
            PaneStreamLifecycleEvent::ProcessExited {
                output_sequence: None,
            },
        ));
        stream.delivered_lifecycle_revision = stream.delivered_lifecycle_revision.saturating_add(1);
    }
}

fn surface_content_equal(
    current: &rmux_proto::PaneSurfaceFrame,
    next: &rmux_proto::PaneSurfaceFrame,
) -> bool {
    let current = &current.snapshot;
    let next = &next.snapshot;
    current.cols == next.cols
        && current.rows == next.rows
        && current.cells == next.cells
        && current.hyperlinks == next.hyperlinks
        && current.cursor == next.cursor
        && current.title == next.title
        && current.path == next.path
        && current.dynamic_colors == next.dynamic_colors
        && current.metadata_complete == next.metadata_complete
        && current.mode_bits == next.mode_bits
        && current.alternate == next.alternate
        && current.scroll_top == next.scroll_top
        && current.scroll_bottom == next.scroll_bottom
        && current.history_size == next.history_size
        && current.history_bytes == next.history_bytes
}
