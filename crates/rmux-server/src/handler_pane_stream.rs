use std::sync::{Arc, Mutex as StdMutex, Weak};
use std::time::Instant;

use rmux_proto::{
    ErrorResponse, PaneRawRebaseReason, PaneStreamCursorRequest, PaneStreamEndReason,
    PaneStreamEvent, PaneStreamMode, Response, RmuxError, SubscribePaneStreamRequest,
    UnsubscribePaneStreamRequest, UnsubscribePaneStreamResponse,
};

use crate::pane_io::PaneOutputReceiver;
use crate::pane_terminals::HandlerState;

use super::pane_support::resolve_pane_target_ref;
use super::subscription_support::{OutputSubscriptionState, SurfaceDriverRoute};
use super::RequestHandler;

#[path = "handler/pane_stream_capture.rs"]
mod capture;
#[path = "handler/pane_stream_protocol.rs"]
mod protocol;
#[path = "handler/pane_stream_raw.rs"]
mod raw;
#[path = "handler/pane_stream_surface.rs"]
mod surface;
#[path = "handler/pane_stream_types.rs"]
mod types;

use capture::{
    capture_source, capture_surface_source, materialize_raw_rebase, materialize_surface_frame,
    raw_reason, CapturedPaneBoundary,
};
use protocol::{
    not_owned_error, owned_stream, owned_stream_record, reserved_stream_key_if_owned,
    reserved_stream_lost_response, slow_consumer_response, stream_cursor_response,
    stream_subscription_limit_error, subscribe_response, validate_detached_response,
    validate_raw_rebase_size, validate_surface_frame_size, wrong_stream_mode,
};
use raw::{RawInitializationOutcome, RawSubscriptionStart};
pub(in crate::handler) use types::{
    CachedRawRebase, EndedPaneStream, PaneStreamSubscription, PendingSurfaceRefresh, SurfaceDriver,
};
use types::{PaneStreamSource, PaneSurfaceFingerprint, SurfacePaneStream};

pub(super) const MAX_SOURCE_CAPTURE_ATTEMPTS: usize = 4;

struct SurfaceInitializationGuard {
    subscriptions: Weak<StdMutex<OutputSubscriptionState>>,
    token: u64,
}

impl SurfaceInitializationGuard {
    fn new(subscriptions: &Arc<StdMutex<OutputSubscriptionState>>, token: u64) -> Self {
        Self {
            subscriptions: Arc::downgrade(subscriptions),
            token,
        }
    }
}

impl Drop for SurfaceInitializationGuard {
    fn drop(&mut self) {
        let Some(subscriptions) = self.subscriptions.upgrade() else {
            return;
        };
        subscriptions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .finish_surface_initialization(self.token);
    }
}

struct StreamReservationGuard {
    subscriptions: Weak<StdMutex<OutputSubscriptionState>>,
    subscription_id: rmux_proto::PaneOutputSubscriptionId,
    armed: bool,
}

impl StreamReservationGuard {
    fn new(
        subscriptions: &Arc<StdMutex<OutputSubscriptionState>>,
        subscription_id: rmux_proto::PaneOutputSubscriptionId,
    ) -> Self {
        Self {
            subscriptions: Arc::downgrade(subscriptions),
            subscription_id,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for StreamReservationGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let Some(subscriptions) = self.subscriptions.upgrade() else {
            return;
        };
        subscriptions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove_subscription(self.subscription_id);
    }
}

struct SurfaceRefreshGuard {
    subscriptions: Weak<StdMutex<OutputSubscriptionState>>,
    pane_id: rmux_core::PaneId,
    token: u64,
    pending: PendingSurfaceRefresh,
    armed: bool,
}

impl SurfaceRefreshGuard {
    fn new(
        subscriptions: &Arc<StdMutex<OutputSubscriptionState>>,
        pane_id: rmux_core::PaneId,
        token: u64,
        pending: PendingSurfaceRefresh,
    ) -> Self {
        Self {
            subscriptions: Arc::downgrade(subscriptions),
            pane_id,
            token,
            pending,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for SurfaceRefreshGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let Some(subscriptions) = self.subscriptions.upgrade() else {
            return;
        };
        subscriptions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .cancel_surface_refresh(self.pane_id, self.token, self.pending);
    }
}

struct RawRebaseGuard {
    subscriptions: Weak<StdMutex<OutputSubscriptionState>>,
    subscription_id: rmux_proto::PaneOutputSubscriptionId,
    token: u64,
    reason: PaneRawRebaseReason,
    armed: bool,
}

impl RawRebaseGuard {
    fn new(
        subscriptions: &Arc<StdMutex<OutputSubscriptionState>>,
        subscription_id: rmux_proto::PaneOutputSubscriptionId,
        token: u64,
        reason: PaneRawRebaseReason,
    ) -> Self {
        Self {
            subscriptions: Arc::downgrade(subscriptions),
            subscription_id,
            token,
            reason,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for RawRebaseGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let Some(subscriptions) = self.subscriptions.upgrade() else {
            return;
        };
        subscriptions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .cancel_raw_rebase(self.subscription_id, self.token, self.reason);
    }
}

impl RequestHandler {
    pub(in crate::handler) async fn handle_subscribe_pane_stream(
        &self,
        connection_id: u64,
        request: SubscribePaneStreamRequest,
    ) -> Response {
        let now = Instant::now();
        let (mut source, subscription_id, surface_route, raw_route) = {
            let state = self.state.lock().await;
            let source = match resolve_stream_source(&state, &request) {
                Ok(source) => source,
                Err(error) => return Response::Error(ErrorResponse { error }),
            };
            let mut subscriptions = self
                .subscriptions
                .lock()
                .expect("subscription registry mutex must not be poisoned");
            subscriptions.cleanup_stale(now);
            let subscription_id =
                match subscriptions
                    .registry
                    .subscribe(connection_id, source.key.clone(), now)
                {
                    Ok(record) => record.id(),
                    Err(error) => {
                        return Response::Error(ErrorResponse {
                            error: stream_subscription_limit_error(error),
                        });
                    }
                };
            subscriptions.streams.insert(
                subscription_id,
                PaneStreamSubscription::Reserved(request.mode),
            );
            let surface_route = (request.mode == PaneStreamMode::Surface)
                .then(|| subscriptions.surface_driver_route(&source.key));
            let raw_route = (request.mode == PaneStreamMode::Raw).then(|| {
                subscriptions.raw_initialization_route(&source.key, request.include_snapshot)
            });
            (source, subscription_id, surface_route, raw_route)
        };
        let mut reservation_guard =
            StreamReservationGuard::new(&self.subscriptions, subscription_id);

        let _raw_initialization = if let Some(route) = raw_route {
            match self
                .prepare_initial_raw_subscription(
                    connection_id,
                    subscription_id,
                    source,
                    route,
                    request.include_snapshot,
                )
                .await
            {
                RawInitializationOutcome::Complete(response) => {
                    if matches!(response, Response::SubscribePaneStream(_)) {
                        reservation_guard.disarm();
                    }
                    return response;
                }
                RawInitializationOutcome::Capture {
                    source: current_source,
                    guard,
                } => {
                    source = current_source;
                    Some(guard)
                }
            }
        } else {
            None
        };

        let _surface_initialization = if let Some(mut route) = surface_route {
            loop {
                match route {
                    SurfaceDriverRoute::Ready => {
                        let response = self.finish_existing_surface_subscription(
                            connection_id,
                            subscription_id,
                            source,
                        );
                        if matches!(response, Response::SubscribePaneStream(_)) {
                            reservation_guard.disarm();
                        }
                        return response;
                    }
                    SurfaceDriverRoute::Initialize { token } => {
                        break Some(SurfaceInitializationGuard::new(&self.subscriptions, token));
                    }
                    SurfaceDriverRoute::Wait(mut completion) => {
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
                            return reserved_stream_lost_response();
                        };
                        source.key = current_key;
                        route = subscriptions.surface_driver_route(&source.key);
                    }
                }
            }
        } else {
            None
        };

        let (source, captured) = match self.capture_current_stream_source(source).await {
            Ok(captured) => captured,
            Err(error) => {
                self.remove_reserved_stream(subscription_id);
                return Response::Error(ErrorResponse { error });
            }
        };
        let response = match request.mode {
            PaneStreamMode::Raw => {
                let rebase = match materialize_raw_rebase(
                    self,
                    source.key.pane_id(),
                    1,
                    PaneRawRebaseReason::Initial,
                    request.include_snapshot,
                    &captured,
                ) {
                    Ok(rebase) => rebase,
                    Err(error) => {
                        self.remove_reserved_stream(subscription_id);
                        return Response::Error(ErrorResponse { error });
                    }
                };
                self.finish_raw_subscription(
                    connection_id,
                    subscription_id,
                    source,
                    RawSubscriptionStart::captured(captured, rebase, request.include_snapshot),
                )
            }
            PaneStreamMode::Surface => {
                let fingerprint = PaneSurfaceFingerprint::capture(
                    captured.seed.screen(),
                    captured.seed.dynamic_colors(),
                );
                let frame = match materialize_surface_frame(
                    self,
                    source.key.pane_id(),
                    1,
                    1,
                    captured.boundary.next_output_sequence,
                    &captured.seed,
                ) {
                    Ok(frame) => frame,
                    Err(error) => {
                        self.remove_reserved_stream(subscription_id);
                        return Response::Error(ErrorResponse { error });
                    }
                };
                self.finish_new_surface_subscription(
                    connection_id,
                    subscription_id,
                    source,
                    captured.receiver,
                    frame,
                    fingerprint,
                )
            }
        };
        if matches!(response, Response::SubscribePaneStream(_)) {
            reservation_guard.disarm();
        }
        response
    }

    pub(in crate::handler) async fn handle_pane_stream_cursor(
        &self,
        connection_id: u64,
        request: PaneStreamCursorRequest,
    ) -> Response {
        if let Some(response) =
            self.take_ended_stream_response(connection_id, request.subscription_id, Instant::now())
        {
            return response;
        }

        let mode = {
            let subscriptions = self
                .subscriptions
                .lock()
                .expect("subscription registry mutex must not be poisoned");
            match owned_stream(&subscriptions, connection_id, request.subscription_id) {
                Ok(stream) => stream.mode(),
                Err(error) => return Response::Error(ErrorResponse { error }),
            }
        };
        match mode {
            PaneStreamMode::Raw => self.poll_raw_stream(connection_id, request).await,
            PaneStreamMode::Surface => self.poll_surface_stream(connection_id, request).await,
        }
    }

    pub(in crate::handler) async fn handle_unsubscribe_pane_stream(
        &self,
        connection_id: u64,
        request: UnsubscribePaneStreamRequest,
    ) -> Response {
        let now = Instant::now();
        let mut subscriptions = self
            .subscriptions
            .lock()
            .expect("subscription registry mutex must not be poisoned");
        subscriptions.cleanup_stale(now);
        if let Some(ended) = subscriptions.ended_streams.get(&request.subscription_id) {
            if ended.connection_id != connection_id {
                return not_owned_error();
            }
            subscriptions.ended_streams.remove(&request.subscription_id);
            return Response::UnsubscribePaneStream(UnsubscribePaneStreamResponse {
                subscription_id: request.subscription_id,
                removed: true,
            });
        }
        let Some(record) = subscriptions.registry.get(request.subscription_id) else {
            return Response::UnsubscribePaneStream(UnsubscribePaneStreamResponse {
                subscription_id: request.subscription_id,
                removed: false,
            });
        };
        if record.connection_id() != connection_id {
            return not_owned_error();
        }
        if !subscriptions.streams.contains_key(&request.subscription_id) {
            return Response::Error(ErrorResponse {
                error: RmuxError::Server("subscription is not a pane stream".to_owned()),
            });
        }
        subscriptions.remove_subscription(request.subscription_id);
        Response::UnsubscribePaneStream(UnsubscribePaneStreamResponse {
            subscription_id: request.subscription_id,
            removed: true,
        })
    }

    pub(crate) fn handle_revoked_pane_stream_cursor(
        &self,
        connection_id: u64,
        request: PaneStreamCursorRequest,
    ) -> Response {
        let mut subscriptions = self
            .subscriptions
            .lock()
            .expect("subscription registry mutex must not be poisoned");
        let owned = subscriptions
            .registry
            .get(request.subscription_id)
            .is_some_and(|record| record.connection_id() == connection_id)
            && subscriptions.streams.contains_key(&request.subscription_id);
        if !owned {
            return Response::Error(ErrorResponse {
                error: RmuxError::Server("access not allowed".to_owned()),
            });
        }
        subscriptions.remove_subscription(request.subscription_id);
        stream_cursor_response(
            request.subscription_id,
            vec![PaneStreamEvent::End(PaneStreamEndReason::AccessRevoked)],
            false,
        )
    }

    pub(crate) fn revoke_inflight_pane_stream_response(
        &self,
        connection_id: u64,
        response: Response,
    ) -> Response {
        let subscription_id = match &response {
            Response::SubscribePaneStream(response) => response.subscription_id,
            Response::PaneStreamCursor(response) => response.subscription_id,
            _ => return response,
        };
        let mut subscriptions = self
            .subscriptions
            .lock()
            .expect("subscription registry mutex must not be poisoned");
        if subscriptions
            .registry
            .get(subscription_id)
            .is_some_and(|record| record.connection_id() == connection_id)
        {
            subscriptions.remove_subscription(subscription_id);
        }
        drop(subscriptions);
        match response {
            Response::SubscribePaneStream(mut response) => {
                response.event = PaneStreamEvent::End(PaneStreamEndReason::AccessRevoked);
                Response::SubscribePaneStream(response)
            }
            Response::PaneStreamCursor(_) => stream_cursor_response(
                subscription_id,
                vec![PaneStreamEvent::End(PaneStreamEndReason::AccessRevoked)],
                false,
            ),
            _ => unreachable!("pane stream response was matched above"),
        }
    }

    fn finish_existing_surface_subscription(
        &self,
        connection_id: u64,
        subscription_id: rmux_proto::PaneOutputSubscriptionId,
        source: PaneStreamSource,
    ) -> Response {
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
            return reserved_stream_lost_response();
        };
        let Some(driver) = subscriptions.surface_drivers.get(&current_key) else {
            subscriptions.remove_subscription(subscription_id);
            return Response::Error(ErrorResponse {
                error: RmuxError::Server(
                    "pane surface driver disappeared during subscription".to_owned(),
                ),
            });
        };
        let frame = Arc::clone(&driver.latest);
        let lifecycle_revision = driver.lifecycle_revision;
        subscriptions.streams.insert(
            subscription_id,
            PaneStreamSubscription::Surface(SurfacePaneStream {
                delivered_revision: frame.revision,
                delivered_epoch: frame.epoch,
                delivered_lifecycle_revision: lifecycle_revision,
            }),
        );
        drop(subscriptions);
        let response = subscribe_response(
            subscription_id,
            &source,
            PaneStreamEvent::SurfaceReset(Box::new((*frame).clone())),
        );
        if let Err(error) = validate_detached_response(&response) {
            self.remove_reserved_stream(subscription_id);
            return Response::Error(ErrorResponse { error });
        }
        response
    }

    fn finish_new_surface_subscription(
        &self,
        connection_id: u64,
        subscription_id: rmux_proto::PaneOutputSubscriptionId,
        source: PaneStreamSource,
        receiver: PaneOutputReceiver,
        frame: Arc<rmux_proto::PaneSurfaceFrame>,
        fingerprint: PaneSurfaceFingerprint,
    ) -> Response {
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
            return reserved_stream_lost_response();
        };
        let frame = if let Some(existing) = subscriptions.surface_drivers.get(&current_key) {
            Arc::clone(&existing.latest)
        } else {
            subscriptions.surface_drivers.insert(
                current_key.clone(),
                SurfaceDriver::new(receiver, Arc::clone(&frame), fingerprint),
            );
            frame
        };
        let lifecycle_revision = subscriptions
            .surface_drivers
            .get(&current_key)
            .map_or(0, |driver| driver.lifecycle_revision);
        subscriptions.streams.insert(
            subscription_id,
            PaneStreamSubscription::Surface(SurfacePaneStream {
                delivered_revision: frame.revision,
                delivered_epoch: frame.epoch,
                delivered_lifecycle_revision: lifecycle_revision,
            }),
        );
        drop(subscriptions);
        let response = subscribe_response(
            subscription_id,
            &source,
            PaneStreamEvent::SurfaceReset(Box::new((*frame).clone())),
        );
        if let Err(error) = validate_detached_response(&response) {
            self.remove_reserved_stream(subscription_id);
            return Response::Error(ErrorResponse { error });
        }
        response
    }

    async fn resolve_stream_source_for_pane(
        &self,
        pane_id: rmux_core::PaneId,
        preferred_runtime_session: &rmux_proto::SessionName,
    ) -> Result<PaneStreamSource, RmuxError> {
        let state = self.state.lock().await;
        let target = state
            .pane_target_for_runtime_pane(preferred_runtime_session, pane_id)
            .or_else(|| state.pane_alias_targets(pane_id).into_iter().next())
            .ok_or_else(|| RmuxError::pane_not_found(preferred_runtime_session.clone(), pane_id))?;
        stream_source_for_target(&state, target)
    }

    async fn capture_current_stream_source(
        &self,
        mut source: PaneStreamSource,
    ) -> Result<(PaneStreamSource, CapturedPaneBoundary), RmuxError> {
        for _ in 0..MAX_SOURCE_CAPTURE_ATTEMPTS {
            let captured = capture_source(&source)?;
            if captured.boundary.generation == source.generation {
                return Ok((source, captured));
            }
            source = self
                .resolve_stream_source_for_pane(
                    source.key.pane_id(),
                    source.key.runtime_session_name(),
                )
                .await?;
        }
        Err(RmuxError::Server(
            "pane process changed repeatedly while capturing stream state".to_owned(),
        ))
    }

    fn remove_reserved_stream(&self, subscription_id: rmux_proto::PaneOutputSubscriptionId) {
        self.subscriptions
            .lock()
            .expect("subscription registry mutex must not be poisoned")
            .remove_subscription(subscription_id);
    }

    fn finish_stream_after_end(&self, subscription_id: rmux_proto::PaneOutputSubscriptionId) {
        self.remove_reserved_stream(subscription_id);
    }

    fn take_ended_stream_response(
        &self,
        connection_id: u64,
        subscription_id: rmux_proto::PaneOutputSubscriptionId,
        now: Instant,
    ) -> Option<Response> {
        let mut subscriptions = self
            .subscriptions
            .lock()
            .expect("subscription registry mutex must not be poisoned");
        subscriptions.cleanup_stale(now);
        let ended = subscriptions.ended_streams.get(&subscription_id).copied()?;
        if ended.connection_id != connection_id {
            return Some(not_owned_error());
        }
        subscriptions.ended_streams.remove(&subscription_id);
        Some(stream_cursor_response(
            subscription_id,
            vec![PaneStreamEvent::End(ended.reason)],
            false,
        ))
    }
}

fn resolve_stream_source(
    state: &HandlerState,
    request: &SubscribePaneStreamRequest,
) -> Result<PaneStreamSource, RmuxError> {
    let target = resolve_pane_target_ref(state, &request.target)?;
    stream_source_for_target(state, target)
}

fn stream_source_for_target(
    state: &HandlerState,
    target: rmux_proto::PaneTarget,
) -> Result<PaneStreamSource, RmuxError> {
    let key = state.pane_output_subscription_key_for_target(&target)?;
    let output = state.pane_output_for_target(
        target.session_name(),
        target.window_index(),
        target.pane_index(),
    )?;
    let transcript = state.transcript_handle(&target)?;
    let generation = output.current_generation();
    Ok(PaneStreamSource {
        target,
        key,
        output,
        transcript,
        generation,
    })
}

#[cfg(test)]
#[path = "handler_pane_stream_tests.rs"]
mod tests;
