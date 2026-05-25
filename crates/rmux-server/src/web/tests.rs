use rmux_core::events::OutputCursorItem;
use rmux_proto::{
    CreateWebShareRequest, ListWebSharesRequest, PaneId, PaneTargetRef, SessionName,
    StopAllWebSharesRequest,
};

use crate::pane_io::pane_output_channel_with_limits;
use crate::web::origin::validate_public_base_url;
use crate::web::{WebShareConnectRole, WebShareRegistry};

#[test]
fn subscribe_from_future_sequence_skips_snapshot_covered_event() {
    let sender = pane_output_channel_with_limits(8, 1024);
    let mut receiver = sender.subscribe_from_sequence(1);

    assert_eq!(sender.send(b"covered-by-snapshot".to_vec()), 0);
    assert!(
        receiver.try_recv().is_none(),
        "event 0 is covered by the snapshot watermark and must be skipped"
    );

    assert_eq!(sender.send(b"post-snapshot".to_vec()), 1);
    let Some(OutputCursorItem::Event(event)) = receiver.try_recv() else {
        panic!("receiver should replay the first post-snapshot event");
    };
    assert_eq!(event.sequence(), 1);
    assert_eq!(event.bytes(), b"post-snapshot");
}

#[test]
fn subscribe_from_retained_sequence_replays_available_events() {
    let sender = pane_output_channel_with_limits(8, 1024);
    assert_eq!(sender.send(b"zero".to_vec()), 0);
    assert_eq!(sender.send(b"one".to_vec()), 1);

    let mut receiver = sender.subscribe_from_sequence(1);
    let Some(OutputCursorItem::Event(event)) = receiver.try_recv() else {
        panic!("receiver should replay retained event 1");
    };
    assert_eq!(event.sequence(), 1);
    assert_eq!(event.bytes(), b"one");
}

#[test]
fn create_returns_secret_urls_but_list_is_redacted() {
    let registry = WebShareRegistry::default();
    let created = registry
        .create(CreateWebShareRequest {
            target: target(),
            public_base_url: Some("https://share.example".to_owned()),
            frontend_url: None,
            ttl_seconds: Some(60),
            max_viewers: Some(2),
            writable: true,
        })
        .expect("share creates");

    assert!(created.viewer_url.contains("&key="));
    assert!(created
        .operator_url
        .as_deref()
        .is_some_and(|url| url.contains("&key=")));
    let stdout = String::from_utf8_lossy(created.output.stdout());
    assert!(stdout.contains("viewer "));
    assert!(!stdout.contains("operator "));

    let listed = registry.list(ListWebSharesRequest);
    assert_eq!(listed.shares.len(), 1);
    let redacted = listed.shares[0].viewer_url.as_deref().expect("url");
    assert_eq!(
        redacted,
        format!(
            "https://share.rmux.io/#endpoint=wss://share.example/share&id={}&key=[REDACTED]",
            created.share_id
        )
    );
}

#[test]
fn default_local_share_uses_hosted_frontend_and_local_websocket_endpoint() {
    let registry = WebShareRegistry::default();
    let created = registry
        .create(CreateWebShareRequest {
            target: target(),
            public_base_url: None,
            frontend_url: None,
            ttl_seconds: Some(60),
            max_viewers: Some(2),
            writable: false,
        })
        .expect("share creates");

    assert!(created
        .viewer_url
        .starts_with("https://share.rmux.io/#endpoint=ws://127.0.0.1:9777/share&id="));
    assert!(!created.viewer_url.contains("&role=viewer"));

    let viewer_key = key_from_url(&created.viewer_url);
    let access = registry
        .connect(&created.share_id, &viewer_key, WebShareConnectRole::Viewer)
        .expect("viewer connects");
    assert!(access.origin_allowed("https://share.rmux.io"));
    assert!(access.origin_allowed("http://localhost:4321"));
    assert!(access.origin_allowed("http://127.0.0.1:5173"));
    assert!(!access.origin_allowed("https://evil.example"));
}

#[test]
fn frontend_override_changes_browser_origin_without_changing_local_endpoint() {
    let registry = WebShareRegistry::new(
        crate::web::WebShareSettings::from_options(
            9778,
            Some("https://share.fork.example".to_owned()),
        )
        .expect("settings"),
    );
    let created = registry
        .create(CreateWebShareRequest {
            target: target(),
            public_base_url: None,
            frontend_url: None,
            ttl_seconds: Some(60),
            max_viewers: Some(2),
            writable: false,
        })
        .expect("share creates");

    assert!(created
        .viewer_url
        .starts_with("https://share.fork.example/#endpoint=ws://127.0.0.1:9778/share&id="));
    let viewer_key = key_from_url(&created.viewer_url);
    let access = registry
        .connect(&created.share_id, &viewer_key, WebShareConnectRole::Viewer)
        .expect("viewer connects");
    assert!(access.origin_allowed("https://share.fork.example"));
    assert!(!access.origin_allowed("https://share.rmux.io"));
}

#[test]
fn per_share_frontend_url_overrides_daemon_default() {
    let registry = WebShareRegistry::default();
    let created = registry
        .create(CreateWebShareRequest {
            target: target(),
            public_base_url: Some("https://terminal.example".to_owned()),
            frontend_url: Some("https://share.fork.example/share".to_owned()),
            ttl_seconds: Some(60),
            max_viewers: Some(2),
            writable: false,
        })
        .expect("share creates");

    assert!(created.viewer_url.starts_with(
        "https://share.fork.example/share/#endpoint=wss://terminal.example/share&id="
    ));
    let viewer_key = key_from_url(&created.viewer_url);
    let access = registry
        .connect(&created.share_id, &viewer_key, WebShareConnectRole::Viewer)
        .expect("viewer connects");
    assert!(access.origin_allowed("https://share.fork.example"));
    assert!(!access.origin_allowed("https://share.rmux.io"));
}

#[test]
fn public_base_url_rejects_query_and_fragment() {
    assert!(validate_public_base_url("https://x.test?a=1").is_err());
    assert!(validate_public_base_url("https://x.test#frag").is_err());
    assert!(validate_public_base_url("ssh://x.test").is_err());
}

#[test]
fn stop_all_reports_removed_share_count() {
    let registry = WebShareRegistry::default();
    for _ in 0..2 {
        registry
            .create(CreateWebShareRequest {
                target: target(),
                public_base_url: None,
                frontend_url: None,
                ttl_seconds: None,
                max_viewers: None,
                writable: false,
            })
            .expect("share creates");
    }
    assert_eq!(registry.stop_all(StopAllWebSharesRequest).stopped, 2);
    assert!(registry.list(ListWebSharesRequest).shares.is_empty());
}

#[test]
fn connect_enforces_viewer_cap_and_single_operator() {
    let registry = WebShareRegistry::default();
    let created = registry
        .create(CreateWebShareRequest {
            target: target(),
            public_base_url: None,
            frontend_url: None,
            ttl_seconds: None,
            max_viewers: Some(1),
            writable: true,
        })
        .expect("share creates");
    let viewer_key = key_from_url(&created.viewer_url);
    let operator_key = key_from_url(created.operator_url.as_deref().expect("operator url"));

    let viewer = registry
        .connect(&created.share_id, &viewer_key, WebShareConnectRole::Viewer)
        .expect("viewer connects");
    assert!(!viewer.is_operator());
    assert!(registry
        .connect(&created.share_id, &viewer_key, WebShareConnectRole::Viewer)
        .is_err());

    let operator = registry
        .connect(
            &created.share_id,
            &operator_key,
            WebShareConnectRole::Operator,
        )
        .expect("operator connects");
    assert!(operator.is_operator());
    assert!(registry
        .connect(
            &created.share_id,
            &operator_key,
            WebShareConnectRole::Operator,
        )
        .is_err());

    drop(viewer);
    assert!(registry
        .connect(&created.share_id, &viewer_key, WebShareConnectRole::Viewer)
        .is_ok());
}

fn target() -> PaneTargetRef {
    PaneTargetRef::by_id(
        SessionName::new("alpha").expect("valid session"),
        PaneId::new(7),
    )
}

fn key_from_url(url: &str) -> String {
    url.split_once("key=")
        .map(|(_, key)| key.split('&').next().unwrap_or(key).to_owned())
        .expect("key query")
}
