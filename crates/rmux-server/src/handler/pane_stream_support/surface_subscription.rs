//! Completion of surface-mode subscriptions once the initial frame is known.

use std::sync::Arc;
use std::time::Instant;

use rmux_proto::{ErrorResponse, PaneStreamEvent, Response, RmuxError};

use super::protocol::{
    reserved_stream_key_if_owned, reserved_stream_lost_response, subscribe_response,
    validate_detached_response, validate_surface_frame_size,
};
use super::types::SurfacePaneStream;
use super::{PaneStreamSource, PaneStreamSubscription, RequestHandler, SurfaceDriver};

impl RequestHandler {
    pub(super) fn finish_existing_surface_subscription(
        &self,
        connection_id: u64,
        subscription_id: rmux_proto::PaneOutputSubscriptionId,
        mut source: PaneStreamSource,
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
        source.rekey(current_key.clone());
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
        let end_reason = subscriptions
            .streams
            .get(&subscription_id)
            .and_then(PaneStreamSubscription::end_reason);
        subscriptions.streams.insert(
            subscription_id,
            PaneStreamSubscription::Surface(SurfacePaneStream {
                delivered_revision: frame.revision,
                delivered_epoch: frame.epoch,
                delivered_lifecycle_revision: lifecycle_revision,
                end_reason,
            }),
        );
        subscriptions.note_pane_drain_progress(&current_key, Instant::now());
        drop(subscriptions);
        if let Err(error) = validate_surface_frame_size(&frame) {
            self.remove_reserved_stream(subscription_id);
            return Response::Error(ErrorResponse { error });
        }
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

    pub(super) fn finish_new_surface_subscription(
        &self,
        connection_id: u64,
        subscription_id: rmux_proto::PaneOutputSubscriptionId,
        mut source: PaneStreamSource,
        driver: SurfaceDriver,
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
        source.rekey(current_key.clone());
        let frame = if let Some(existing) = subscriptions.surface_drivers.get(&current_key) {
            Arc::clone(&existing.latest)
        } else {
            let frame = Arc::clone(&driver.latest);
            subscriptions
                .surface_drivers
                .insert(current_key.clone(), driver);
            frame
        };
        let lifecycle_revision = subscriptions
            .surface_drivers
            .get(&current_key)
            .map_or(0, |driver| driver.lifecycle_revision);
        let end_reason = subscriptions
            .streams
            .get(&subscription_id)
            .and_then(PaneStreamSubscription::end_reason);
        subscriptions.streams.insert(
            subscription_id,
            PaneStreamSubscription::Surface(SurfacePaneStream {
                delivered_revision: frame.revision,
                delivered_epoch: frame.epoch,
                delivered_lifecycle_revision: lifecycle_revision,
                end_reason,
            }),
        );
        subscriptions.note_pane_drain_progress(&current_key, Instant::now());
        drop(subscriptions);
        if let Err(error) = validate_surface_frame_size(&frame) {
            self.remove_reserved_stream(subscription_id);
            return Response::Error(ErrorResponse { error });
        }
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
}
