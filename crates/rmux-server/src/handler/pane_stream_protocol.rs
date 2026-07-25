use rmux_core::events::{
    OutputSubscriptionRecord, PaneOutputSubscriptionKey, DEFAULT_SUBSCRIPTION_BATCH_EVENTS,
};
use rmux_proto::{
    ErrorResponse, PaneOutputSubscriptionId, PaneStreamCursorResponse, PaneStreamEndReason,
    PaneStreamEvent, Response, RmuxError, SubscribePaneStreamResponse,
    DEFAULT_MAX_DETACHED_FRAME_LENGTH,
};

use super::super::subscription_support::{
    OutputSubscriptionAdmissionError, OutputSubscriptionState,
};
use super::{PaneStreamSource, PaneStreamSubscription};

const PANE_STREAM_RESPONSE_RESERVE: usize = 64 * 1024;
const _: () = assert!(
    DEFAULT_SUBSCRIPTION_BATCH_EVENTS * crate::pane_io::READ_BUFFER_SIZE
        + PANE_STREAM_RESPONSE_RESERVE
        < DEFAULT_MAX_DETACHED_FRAME_LENGTH
);

pub(super) fn subscribe_response(
    subscription_id: PaneOutputSubscriptionId,
    source: &PaneStreamSource,
    event: PaneStreamEvent,
) -> Response {
    Response::SubscribePaneStream(Box::new(SubscribePaneStreamResponse {
        subscription_id,
        target: source.target.clone(),
        pane_id: source.key.pane_id(),
        event,
    }))
}

pub(super) fn stream_cursor_response(
    subscription_id: PaneOutputSubscriptionId,
    events: Vec<PaneStreamEvent>,
    limited: bool,
) -> Response {
    Response::PaneStreamCursor(Box::new(PaneStreamCursorResponse {
        subscription_id,
        events,
        limited,
    }))
}

pub(super) fn validate_detached_response(response: &Response) -> Result<(), RmuxError> {
    let length = detached_response_size(response)?;
    if length > DEFAULT_MAX_DETACHED_FRAME_LENGTH {
        return Err(RmuxError::FrameTooLarge {
            length,
            maximum: DEFAULT_MAX_DETACHED_FRAME_LENGTH,
        });
    }
    Ok(())
}

pub(super) fn detached_response_size(response: &Response) -> Result<usize, RmuxError> {
    let encoded =
        bincode::serialized_size(response).map_err(|error| RmuxError::Encode(error.to_string()))?;
    Ok(usize::try_from(encoded).unwrap_or(usize::MAX))
}

pub(super) fn validate_raw_rebase_size(
    rebase: &rmux_proto::PaneRawRebase,
) -> Result<(), RmuxError> {
    let encoded =
        bincode::serialized_size(rebase).map_err(|error| RmuxError::Encode(error.to_string()))?;
    validate_stream_payload_size(encoded)
}

pub(super) fn validate_surface_frame_size(
    frame: &rmux_proto::PaneSurfaceFrame,
) -> Result<(), RmuxError> {
    let encoded =
        bincode::serialized_size(frame).map_err(|error| RmuxError::Encode(error.to_string()))?;
    validate_stream_payload_size(encoded)
}

fn validate_stream_payload_size(encoded: u64) -> Result<(), RmuxError> {
    // Four bytes cover bincode's enum discriminant. The larger reserve keeps
    // the enclosing response and any lifecycle companion below the hard cap.
    let event_length = usize::try_from(encoded)
        .unwrap_or(usize::MAX)
        .saturating_add(std::mem::size_of::<u32>());
    let length = event_length.saturating_add(PANE_STREAM_RESPONSE_RESERVE);
    if length > DEFAULT_MAX_DETACHED_FRAME_LENGTH {
        return Err(RmuxError::FrameTooLarge {
            length,
            maximum: DEFAULT_MAX_DETACHED_FRAME_LENGTH,
        });
    }
    Ok(())
}

pub(super) fn slow_consumer_response(subscription_id: PaneOutputSubscriptionId) -> Response {
    stream_cursor_response(
        subscription_id,
        vec![PaneStreamEvent::End(PaneStreamEndReason::SlowConsumer)],
        false,
    )
}

pub(super) fn owned_stream(
    subscriptions: &OutputSubscriptionState,
    connection_id: u64,
    subscription_id: PaneOutputSubscriptionId,
) -> Result<&PaneStreamSubscription, RmuxError> {
    let record = owned_stream_record(subscriptions, connection_id, subscription_id)?;
    subscriptions
        .streams
        .get(&record.id())
        .ok_or_else(|| RmuxError::Server("pane stream state not found".to_owned()))
}

pub(super) fn owned_stream_record(
    subscriptions: &OutputSubscriptionState,
    connection_id: u64,
    subscription_id: PaneOutputSubscriptionId,
) -> Result<&OutputSubscriptionRecord, RmuxError> {
    let record = subscriptions
        .registry
        .get(subscription_id)
        .ok_or_else(|| RmuxError::Server("subscription not found".to_owned()))?;
    if record.connection_id() != connection_id {
        return Err(RmuxError::Server(
            "subscription is not owned by this connection".to_owned(),
        ));
    }
    if !subscriptions.streams.contains_key(&subscription_id) {
        return Err(RmuxError::Server(
            "subscription is not a pane stream".to_owned(),
        ));
    }
    Ok(record)
}

pub(super) fn reserved_stream_key_if_owned(
    subscriptions: &OutputSubscriptionState,
    connection_id: u64,
    subscription_id: PaneOutputSubscriptionId,
    pane_id: rmux_core::PaneId,
) -> Option<PaneOutputSubscriptionKey> {
    subscriptions
        .registry
        .get(subscription_id)
        .filter(|record| {
            record.connection_id() == connection_id && record.pane().pane_id() == pane_id
        })
        .map(|record| record.pane().clone())
}

pub(super) fn reserved_stream_lost_response() -> Response {
    Response::Error(ErrorResponse {
        error: RmuxError::Server("pane stream reservation was removed".to_owned()),
    })
}

pub(super) fn stream_subscription_limit_error(
    error: OutputSubscriptionAdmissionError,
) -> RmuxError {
    match error {
        OutputSubscriptionAdmissionError::Configured(
            rmux_core::events::SubscriptionLimitError::PerConnection { limit },
        ) => RmuxError::Server(format!(
            "pane stream subscription limit exceeded for connection (limit {limit})"
        )),
        OutputSubscriptionAdmissionError::Configured(
            rmux_core::events::SubscriptionLimitError::PerPane { limit },
        ) => RmuxError::Server(format!(
            "pane stream subscription limit exceeded for pane (limit {limit})"
        )),
        OutputSubscriptionAdmissionError::Global { limit } => RmuxError::Server(format!(
            "pane stream global subscription limit exceeded (limit {limit})"
        )),
    }
}

pub(super) fn not_owned_error() -> Response {
    Response::Error(ErrorResponse {
        error: RmuxError::Server("subscription is not owned by this connection".to_owned()),
    })
}

pub(super) fn wrong_stream_mode() -> Response {
    Response::Error(ErrorResponse {
        error: RmuxError::Server("subscription uses a different pane stream projection".to_owned()),
    })
}
