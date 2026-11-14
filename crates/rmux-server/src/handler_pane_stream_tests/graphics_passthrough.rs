//! Raw pane stream graphics passthrough delivery rules.
//!
//! These tests pin the `sdk.pane.graphics_passthrough` contract: only
//! connections whose handshake required the capability may receive
//! `PaneStreamEvent::GraphicsPassthrough`, and only while the pane's
//! `allow-passthrough` option was `on` (or `all`) when the stream opened.
//! Server-correlated query protocols stay off the stream, and the surface
//! projection never carries graphics events.

use rmux_proto::{
    OptionName, PanePassthroughKind, PaneStreamEvent, PaneStreamMode, Request, Response,
    ScopeSelector, SetOptionMode, SetOptionRequest,
};

use super::{cursor, subscribe, test_pane, RequestHandler, CONNECTION_ID};

const KITTY_ESCAPE: &[u8] = b"\x1b_Gf=100;AAAA\x1b\\";
const SIXEL_ESCAPE: &[u8] = b"\x1bP0;1;0;0q#0;2;0;0;0#0~~~~\x1b\\";

async fn allow_passthrough_on(handler: &RequestHandler, target: &rmux_proto::PaneTarget) {
    let set = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Pane(target.clone()),
            option: OptionName::AllowPassthrough,
            value: "on".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(set, Response::SetOption(_)), "{set:?}");
}

fn graphics_events(events: &[PaneStreamEvent]) -> Vec<&rmux_proto::PaneGraphicsPassthrough> {
    events
        .iter()
        .filter_map(|event| match event {
            PaneStreamEvent::GraphicsPassthrough(graphics) => Some(graphics),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn graphics_passthrough_delivers_typed_events_alongside_their_raw_chunks() {
    let handler = RequestHandler::new();
    let (target, output, transcript) = test_pane(&handler).await;
    allow_passthrough_on(&handler, &target).await;
    handler.note_pane_graphics_passthrough_connection(CONNECTION_ID);
    let subscribed = subscribe(&handler, &target, PaneStreamMode::Raw).await;

    crate::pane_io::publish_pane_bytes_for_test(&transcript, &output, b"OK".to_vec());
    crate::pane_io::publish_pane_bytes_for_test(&transcript, &output, KITTY_ESCAPE.to_vec());
    crate::pane_io::publish_pane_bytes_for_test(&transcript, &output, SIXEL_ESCAPE.to_vec());

    let events = cursor(&handler, subscribed.subscription_id).await;
    let graphics = graphics_events(&events);
    assert_eq!(
        graphics.len(),
        2,
        "kitty and sixel must both surface: {events:?}"
    );
    assert_eq!(graphics[0].kind, PanePassthroughKind::KittyGraphics);
    assert_eq!(graphics[0].bytes, KITTY_ESCAPE);
    assert_eq!((graphics[0].cursor_x, graphics[0].cursor_y), (2, 0));
    assert_eq!(graphics[1].kind, PanePassthroughKind::Sixel);
    assert_eq!(graphics[1].bytes, SIXEL_ESCAPE);
    assert_eq!(graphics[0].epoch, 1);
    assert_eq!(graphics[0].epoch, graphics[1].epoch);

    for graphics in &graphics {
        let raw = events.iter().find_map(|event| match event {
            PaneStreamEvent::RawBytes(bytes) if bytes.sequence == graphics.sequence => Some(bytes),
            _ => None,
        });
        let raw = raw.expect("each graphics event must name a raw chunk");
        assert_eq!(raw.epoch, graphics.epoch);
        assert_eq!(
            raw.bytes, graphics.bytes,
            "the raw chunk embeds the escape verbatim"
        );
    }
}

#[tokio::test]
async fn graphics_passthrough_requires_the_capability_handshake() {
    let handler = RequestHandler::new();
    let (target, output, transcript) = test_pane(&handler).await;
    allow_passthrough_on(&handler, &target).await;
    let subscribed = subscribe(&handler, &target, PaneStreamMode::Raw).await;

    crate::pane_io::publish_pane_bytes_for_test(&transcript, &output, KITTY_ESCAPE.to_vec());

    let events = cursor(&handler, subscribed.subscription_id).await;
    assert!(
        graphics_events(&events).is_empty(),
        "a connection that never required the capability must not receive graphics events: {events:?}"
    );
    assert!(events
        .iter()
        .any(|event| matches!(event, PaneStreamEvent::RawBytes(_))));
}

#[tokio::test]
async fn graphics_passthrough_requires_the_pane_option_at_subscribe_time() {
    let handler = RequestHandler::new();
    let (target, output, transcript) = test_pane(&handler).await;
    handler.note_pane_graphics_passthrough_connection(CONNECTION_ID);
    let subscribed = subscribe(&handler, &target, PaneStreamMode::Raw).await;

    crate::pane_io::publish_pane_bytes_for_test(&transcript, &output, KITTY_ESCAPE.to_vec());

    let events = cursor(&handler, subscribed.subscription_id).await;
    assert!(
        graphics_events(&events).is_empty(),
        "allow-passthrough off must suppress graphics events: {events:?}"
    );

    allow_passthrough_on(&handler, &target).await;
    let later = cursor(&handler, subscribed.subscription_id).await;
    assert!(
        graphics_events(&later).is_empty(),
        "the option snapshot is taken at subscribe time; a later toggle must not affect an open stream: {later:?}"
    );

    let resubscribed = subscribe(&handler, &target, PaneStreamMode::Raw).await;
    crate::pane_io::publish_pane_bytes_for_test(&transcript, &output, SIXEL_ESCAPE.to_vec());
    let fresh = cursor(&handler, resubscribed.subscription_id).await;
    let graphics = graphics_events(&fresh);
    assert_eq!(
        graphics.len(),
        1,
        "a fresh stream sees the enabled option: {fresh:?}"
    );
    assert_eq!(graphics[0].kind, PanePassthroughKind::Sixel);
}

#[tokio::test]
async fn graphics_passthrough_excludes_server_correlated_queries() {
    let handler = RequestHandler::new();
    let (target, output, transcript) = test_pane(&handler).await;
    allow_passthrough_on(&handler, &target).await;
    handler.note_pane_graphics_passthrough_connection(CONNECTION_ID);
    let subscribed = subscribe(&handler, &target, PaneStreamMode::Raw).await;

    crate::pane_io::publish_pane_bytes_for_test(
        &transcript,
        &output,
        b"\x1b]52;c;QQ==\x07".to_vec(),
    );
    crate::pane_io::publish_pane_bytes_for_test(&transcript, &output, b"\x1b]4;0;?\x1b\\".to_vec());

    let events = cursor(&handler, subscribed.subscription_id).await;
    assert!(
        graphics_events(&events).is_empty(),
        "clipboard and palette queries are server-correlated and must not be reflected: {events:?}"
    );
    assert!(events
        .iter()
        .any(|event| matches!(event, PaneStreamEvent::RawBytes(_))));
}

#[tokio::test]
async fn surface_streams_never_receive_graphics_events() {
    let handler = RequestHandler::new();
    let (target, output, transcript) = test_pane(&handler).await;
    allow_passthrough_on(&handler, &target).await;
    handler.note_pane_graphics_passthrough_connection(CONNECTION_ID);
    let subscribed = subscribe(&handler, &target, PaneStreamMode::Surface).await;
    assert!(
        matches!(subscribed.event, PaneStreamEvent::SurfaceReset(_)),
        "the surface projection opens on its own reset frame: {:?}",
        subscribed.event
    );

    crate::pane_io::publish_pane_bytes_for_test(&transcript, &output, KITTY_ESCAPE.to_vec());

    let events = cursor(&handler, subscribed.subscription_id).await;
    assert!(
        graphics_events(&events).is_empty(),
        "the surface projection never carries graphics events: {events:?}"
    );
}
