use rmux_core::{input::InputParser, PaneId, Screen};
use rmux_proto::{
    PaneOutputSubscriptionId, PaneStreamEndReason, PaneStreamEvent, PaneStreamMode, PaneTarget,
    PaneTargetRef, ResizeWindowRequest, Response, RmuxError, SubscribePaneStreamRequest,
    TerminalSize, UnsubscribePaneStreamRequest, WindowTarget, DEFAULT_MAX_DETACHED_FRAME_LENGTH,
};

use crate::pane_recovery::{PaneProjectionSeed, MAX_RECOVERY_STRING_BYTES};
use crate::pane_transcript::SharedPaneTranscript;

use super::CONNECTION_ID;

pub(super) const SECOND_CONNECTION_ID: u64 = CONNECTION_ID + 1;
const SURFACE_RESPONSE_RESERVE: usize = 64 * 1024;
const SURFACE_EVENT_DISCRIMINANT: usize = std::mem::size_of::<u32>();
pub(super) const SURFACE_POLL_FRAME_LIMIT: usize =
    DEFAULT_MAX_DETACHED_FRAME_LENGTH - SURFACE_RESPONSE_RESERVE - SURFACE_EVENT_DISCRIMINANT;
pub(super) const EXPECTED_MIN_SURFACE_CELL_ENCODED_BYTES: u64 = 28;
pub(super) const EXPECTED_MAX_SURFACE_CELL_ENCODED_BYTES: u64 = 49;
pub(super) const MAX_CELL_TEXT: &str =
    "a\u{0301}\u{0301}\u{0301}\u{0301}\u{0301}\u{0301}\u{0301}\u{0301}\u{0301}\u{0301}";

pub(super) fn install_blank_screen(transcript: &SharedPaneTranscript, size: TerminalSize) {
    let history_limit = transcript.lock().history_limit();
    transcript
        .lock()
        .set_screen_for_test(Screen::new(size, history_limit));
}

pub(super) fn install_max_text_screen(transcript: &SharedPaneTranscript, size: TerminalSize) {
    assert_eq!(MAX_CELL_TEXT.len(), 21);
    let cells = usize::from(size.cols) * usize::from(size.rows);
    let history_limit = transcript.lock().history_limit();
    let mut screen = Screen::new(size, history_limit);
    let mut parser = InputParser::new();
    let content = MAX_CELL_TEXT.repeat(cells);
    parser.parse(content.as_bytes(), &mut screen);
    transcript.lock().set_screen_for_test(screen);
}

pub(super) fn install_frame_at_size(
    handler: &super::RequestHandler,
    transcript: &SharedPaneTranscript,
    target_size: usize,
) -> (std::sync::Arc<rmux_proto::PaneSurfaceFrame>, usize) {
    let size = TerminalSize {
        cols: 512,
        rows: 330,
    };
    install_max_text_screen(transcript, size);

    let base = materialize_frame(handler, transcript);
    let base_size =
        usize::try_from(bincode::serialized_size(base.as_ref()).expect("base frame size"))
            .expect("base frame size fits usize");
    let title_len = target_size
        .checked_sub(base_size)
        .expect("chosen cell grid must leave room for title calibration");
    assert!(
        title_len <= MAX_RECOVERY_STRING_BYTES,
        "title calibration {title_len} exceeds the production metadata bound"
    );
    transcript.lock().set_title("t".repeat(title_len));

    (materialize_frame(handler, transcript), title_len)
}

pub(super) fn publish_title(
    transcript: &SharedPaneTranscript,
    output: &crate::pane_io::PaneOutputSender,
    title_length: usize,
    byte: u8,
) {
    let mut payload = Vec::with_capacity(title_length + 5);
    payload.extend_from_slice(b"\x1b]0;");
    payload.resize(payload.len() + title_length, byte);
    payload.push(b'\x07');
    crate::pane_io::publish_pane_bytes_for_test(transcript, output, payload);
}

pub(super) async fn resize_window(
    handler: &super::RequestHandler,
    target: &PaneTarget,
    cols: u16,
    rows: u16,
) {
    let response = handler
        .handle(rmux_proto::Request::ResizeWindow(ResizeWindowRequest {
            target: WindowTarget::with_window(target.session_name().clone(), target.window_index()),
            width: Some(cols),
            height: Some(rows),
            adjustment: None,
        }))
        .await;
    assert!(
        matches!(response, Response::ResizeWindow(_)),
        "window resize failed: {response:?}"
    );
}

pub(super) fn materialize_frame(
    handler: &super::RequestHandler,
    transcript: &SharedPaneTranscript,
) -> std::sync::Arc<rmux_proto::PaneSurfaceFrame> {
    let seed = {
        let transcript = transcript.lock();
        PaneProjectionSeed::capture(&transcript).expect("test Surface projection")
    };
    super::super::materialize_surface_frame(handler, PaneId::new(1), 1, 1, 1, 0, &seed)
        .expect("materialize test Surface frame")
}

pub(super) async fn subscribe_response(
    handler: &super::RequestHandler,
    connection_id: u64,
    target: &PaneTarget,
) -> Response {
    subscribe_mode_response(handler, connection_id, target, PaneStreamMode::Surface).await
}

pub(super) async fn subscribe_mode_response(
    handler: &super::RequestHandler,
    connection_id: u64,
    target: &PaneTarget,
    mode: PaneStreamMode,
) -> Response {
    handler
        .handle_subscribe_pane_stream(
            connection_id,
            SubscribePaneStreamRequest {
                target: PaneTargetRef::slot(target.clone()),
                mode,
                include_snapshot: false,
            },
        )
        .await
}

pub(super) fn expect_surface_subscription(response: Response) -> PaneOutputSubscriptionId {
    expect_subscription(response, PaneStreamMode::Surface)
}

pub(super) fn expect_subscription(
    response: Response,
    expected_mode: PaneStreamMode,
) -> PaneOutputSubscriptionId {
    let Response::SubscribePaneStream(response) = response else {
        panic!("{expected_mode:?} subscription failed: {response:?}");
    };
    match (&response.event, expected_mode) {
        (PaneStreamEvent::SurfaceReset(_), PaneStreamMode::Surface)
        | (PaneStreamEvent::RawRebase(_), PaneStreamMode::Raw) => response.subscription_id,
        _ => panic!(
            "{expected_mode:?} subscription returned the wrong initial event: {:?}",
            response.event
        ),
    }
}

pub(super) async fn unsubscribe(
    handler: &super::RequestHandler,
    connection_id: u64,
    subscription_id: PaneOutputSubscriptionId,
) {
    let response = handler
        .handle_unsubscribe_pane_stream(
            connection_id,
            UnsubscribePaneStreamRequest { subscription_id },
        )
        .await;
    assert!(
        matches!(
            response,
            Response::UnsubscribePaneStream(ref response) if response.removed
        ),
        "unsubscribe failed: {response:?}"
    );
}

pub(super) async fn cursor_response_for_connection(
    handler: &super::RequestHandler,
    connection_id: u64,
    subscription_id: PaneOutputSubscriptionId,
) -> Response {
    handler
        .handle_pane_stream_cursor(
            connection_id,
            rmux_proto::PaneStreamCursorRequest {
                subscription_id,
                max_events: Some(32),
            },
        )
        .await
}

pub(super) async fn cursor_for_connection(
    handler: &super::RequestHandler,
    connection_id: u64,
    subscription_id: PaneOutputSubscriptionId,
) -> Vec<PaneStreamEvent> {
    let response = cursor_response_for_connection(handler, connection_id, subscription_id).await;
    let Response::PaneStreamCursor(response) = response else {
        panic!("cursor failed: {response:?}");
    };
    response.events
}

pub(super) async fn cursor_until_end(
    handler: &super::RequestHandler,
    connection_id: u64,
    subscription_id: PaneOutputSubscriptionId,
) -> Vec<PaneStreamEvent> {
    let mut events = Vec::new();
    for _ in 0..8 {
        let next = cursor_for_connection(handler, connection_id, subscription_id).await;
        let ended = next
            .iter()
            .any(|event| matches!(event, PaneStreamEvent::End(_)));
        events.extend(next);
        if ended {
            return events;
        }
    }
    panic!("stream did not end after short destruction: {events:?}");
}

pub(super) fn end_reason(events: Vec<PaneStreamEvent>, mode: &str) -> PaneStreamEndReason {
    events
        .into_iter()
        .find_map(|event| match event {
            PaneStreamEvent::End(reason) => Some(reason),
            _ => None,
        })
        .unwrap_or_else(|| panic!("{mode} stream did not deliver a typed end"))
}

pub(super) fn assert_surface_budget_error(response: &Response) {
    match response {
        Response::Error(error) => assert_eq!(
            error.error,
            RmuxError::FrameTooLarge {
                length: DEFAULT_MAX_DETACHED_FRAME_LENGTH + 1,
                maximum: DEFAULT_MAX_DETACHED_FRAME_LENGTH,
            }
        ),
        Response::SubscribePaneStream(_) => {
            panic!("Surface subscribe accepted a frame outside the cursor envelope")
        }
        _ => panic!("Surface subscribe returned an unexpected response kind"),
    }
}

pub(super) fn assert_frame_too_large(response: &Response) {
    assert!(
        matches!(
            response,
            Response::Error(rmux_proto::ErrorResponse {
                error: RmuxError::FrameTooLarge { length, maximum },
            }) if *length > *maximum && *maximum == DEFAULT_MAX_DETACHED_FRAME_LENGTH
        ),
        "expected exact FrameTooLarge response, got {response:?}"
    );
}

pub(super) fn assert_surface_state(
    handler: &super::RequestHandler,
    subscriptions_expected: usize,
    drivers_expected: usize,
) {
    let subscriptions = handler
        .subscriptions
        .lock()
        .expect("subscription registry mutex");
    assert_eq!(subscriptions.registry.len(), subscriptions_expected);
    assert_eq!(subscriptions.streams.len(), subscriptions_expected);
    assert_eq!(subscriptions.surface_drivers.len(), drivers_expected);
}
