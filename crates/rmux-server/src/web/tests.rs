use rmux_core::events::OutputCursorItem;
use rmux_proto::{
    CreateWebShareRequest, ListWebSharesRequest, PaneId, PaneTargetRef, SessionName,
    StopAllWebSharesRequest, WebShareScope, WebShareUrlOptions, WebTerminalTheme,
};
use std::time::{Duration, Instant};

use crate::pane_io::pane_output_channel_with_limits;
use crate::web::origin::validate_public_base_url;
use crate::web::secrets::random_token;
use crate::web::WebShareRegistry;

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
            scope: WebShareScope::Pane(target()),
            public_base_url: Some("https://share.example".to_owned()),
            frontend_url: None,
            ttl_seconds: Some(60),
            max_readers: Some(2),
            url_options: Default::default(),
            require_pin: false,
            terminal_palette: None,
            writable: true,
            controls: false,
        })
        .expect("share creates");

    assert!(created.read_url.contains("#e=wss://share.example/share&t="));
    assert!(created
        .operator_url
        .as_deref()
        .is_some_and(|url| url.contains("#e=wss://share.example/share&t=")));
    let stdout = String::from_utf8_lossy(created.output.stdout());
    assert!(stdout.contains("read "));
    assert!(!stdout.contains("operator "));

    let listed = registry.list(ListWebSharesRequest);
    assert_eq!(listed.shares.len(), 1);
    let redacted = listed.shares[0].read_url.as_deref().expect("url");
    assert_eq!(
        redacted,
        format!("https://share.rmux.io/#e=wss://share.example/share&t=[REDACTED]")
    );
}

#[tokio::test]
async fn default_local_share_uses_hosted_frontend_and_local_websocket_endpoint() {
    let registry = WebShareRegistry::default();
    let created = registry
        .create(CreateWebShareRequest {
            scope: WebShareScope::Pane(target()),
            public_base_url: None,
            frontend_url: None,
            ttl_seconds: Some(60),
            max_readers: Some(2),
            url_options: Default::default(),
            require_pin: false,
            terminal_palette: None,
            writable: false,
            controls: false,
        })
        .expect("share creates");

    assert!(created.read_url.starts_with("https://share.rmux.io/#t="));
    assert!(!created.read_url.contains("role="));

    let read_token = token_from_url(&created.read_url);
    let access = registry
        .connect(&read_token, None)
        .await
        .expect("read connects");
    assert!(access.origin_allowed("https://share.rmux.io"));
    assert!(access.origin_allowed("http://localhost:4321"));
    assert!(access.origin_allowed("http://127.0.0.1:5173"));
    assert!(!access.origin_allowed("https://evil.example"));
}

#[tokio::test]
async fn frontend_override_changes_browser_origin_without_changing_local_endpoint() {
    let registry = WebShareRegistry::new(
        crate::web::WebShareSettings::from_options(
            9778,
            Some("https://share.fork.example".to_owned()),
        )
        .expect("settings"),
    );
    let created = registry
        .create(CreateWebShareRequest {
            scope: WebShareScope::Pane(target()),
            public_base_url: None,
            frontend_url: None,
            ttl_seconds: Some(60),
            max_readers: Some(2),
            url_options: Default::default(),
            require_pin: false,
            terminal_palette: None,
            writable: false,
            controls: false,
        })
        .expect("share creates");

    assert!(created
        .read_url
        .starts_with("https://share.fork.example/#t="));
    let read_token = token_from_url(&created.read_url);
    let access = registry
        .connect(&read_token, None)
        .await
        .expect("read connects");
    assert!(access.origin_allowed("https://share.fork.example"));
    assert!(!access.origin_allowed("https://share.rmux.io"));
}

#[tokio::test]
async fn per_share_frontend_url_overrides_daemon_default() {
    let registry = WebShareRegistry::default();
    let created = registry
        .create(CreateWebShareRequest {
            scope: WebShareScope::Pane(target()),
            public_base_url: Some("https://terminal.example".to_owned()),
            frontend_url: Some("https://share.fork.example/share".to_owned()),
            ttl_seconds: Some(60),
            max_readers: Some(2),
            url_options: Default::default(),
            require_pin: false,
            terminal_palette: None,
            writable: false,
            controls: false,
        })
        .expect("share creates");

    assert!(created
        .read_url
        .starts_with("https://share.fork.example/share/#e=wss://terminal.example/share&t="));
    let read_token = token_from_url(&created.read_url);
    let access = registry
        .connect(&read_token, None)
        .await
        .expect("read connects");
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
fn local_web_share_requires_bound_listener_and_valid_port() {
    assert!(crate::web::WebShareSettings::from_options(0, None).is_err());

    let registry = WebShareRegistry::default();
    registry.mark_listener_unavailable("address already in use");
    let error = registry
        .create(CreateWebShareRequest {
            scope: WebShareScope::Pane(target()),
            public_base_url: None,
            frontend_url: None,
            ttl_seconds: Some(60),
            max_readers: Some(2),
            url_options: Default::default(),
            require_pin: false,
            terminal_palette: None,
            writable: false,
            controls: false,
        })
        .expect_err("dead listener must reject local share URLs");
    assert!(error.to_string().contains("listener unavailable"));
    assert!(registry
        .config(rmux_proto::WebShareConfigRequest)
        .expect_err("dead listener must reject config")
        .to_string()
        .contains("listener unavailable"));

    registry.mark_listener_available();
    assert!(registry
        .create(CreateWebShareRequest {
            scope: WebShareScope::Pane(target()),
            public_base_url: None,
            frontend_url: None,
            ttl_seconds: Some(60),
            max_readers: Some(2),
            url_options: Default::default(),
            require_pin: false,
            terminal_palette: None,
            writable: false,
            controls: false,
        })
        .is_ok());
}

#[test]
fn public_url_scheme_is_case_insensitive_for_websocket_endpoint() {
    let registry = WebShareRegistry::default();
    let created = registry
        .create(CreateWebShareRequest {
            scope: WebShareScope::Pane(target()),
            public_base_url: Some("HTTPS://terminal.example".to_owned()),
            frontend_url: None,
            ttl_seconds: Some(60),
            max_readers: Some(2),
            url_options: Default::default(),
            require_pin: false,
            terminal_palette: None,
            writable: false,
            controls: false,
        })
        .expect("uppercase HTTPS is valid");

    assert!(created
        .read_url
        .starts_with("https://share.rmux.io/#e=wss://terminal.example/share&t="));
}

#[test]
fn url_options_are_encoded_in_read_urls() {
    let registry = WebShareRegistry::default();
    let created = registry
        .create(CreateWebShareRequest {
            scope: WebShareScope::Pane(target()),
            public_base_url: None,
            frontend_url: None,
            ttl_seconds: Some(60),
            max_readers: Some(2),
            url_options: WebShareUrlOptions {
                no_navbar: true,
                no_disclaimer: true,
                terminal_theme: Some(WebTerminalTheme::Light),
            },
            require_pin: false,
            terminal_palette: None,
            writable: true,
            controls: false,
        })
        .expect("share creates");

    assert!(created.read_url.contains("&navbar=off"));
    assert!(created.read_url.contains("&disclaimer=off"));
    assert!(created.read_url.contains("&theme=light"));
    assert!(created
        .operator_url
        .as_deref()
        .is_some_and(|url| url.contains("&navbar=off")
            && url.contains("&disclaimer=off")
            && url.contains("&theme=light")));
}

#[tokio::test]
async fn pairing_code_is_required_out_of_band_when_pin_enabled() {
    let registry = WebShareRegistry::default();
    let created = registry
        .create(CreateWebShareRequest {
            scope: WebShareScope::Pane(target()),
            public_base_url: None,
            frontend_url: None,
            ttl_seconds: Some(60),
            max_readers: Some(2),
            url_options: Default::default(),
            require_pin: true,
            terminal_palette: None,
            writable: false,
            controls: false,
        })
        .expect("share creates");

    let pairing_code = created
        .pairing_code
        .as_deref()
        .expect("pin-enabled share returns pairing code");
    assert_eq!(pairing_code.len(), 6);
    assert!(pairing_code.bytes().all(|byte| byte.is_ascii_digit()));
    assert!(created.read_url.contains("&pin=required"));
    assert!(!created.read_url.contains(pairing_code));
    let stdout = String::from_utf8_lossy(created.output.stdout());
    assert!(stdout.contains(&format!("pin {pairing_code}\n")));

    let read_token = token_from_url(&created.read_url);
    assert!(registry.connect(&read_token, None).await.is_err());
    assert!(registry.connect(&read_token, Some("000000")).await.is_err());
    assert!(registry
        .connect(&read_token, Some(pairing_code))
        .await
        .is_ok());
}

#[test]
fn controls_require_writable_session_share() {
    let registry = WebShareRegistry::default();
    let session = SessionName::new("alpha").expect("valid session");

    let read_only_error = registry
        .create(CreateWebShareRequest {
            scope: WebShareScope::Session(session.clone()),
            public_base_url: None,
            frontend_url: None,
            ttl_seconds: None,
            max_readers: None,
            url_options: Default::default(),
            require_pin: false,
            terminal_palette: None,
            writable: false,
            controls: true,
        })
        .expect_err("controls require writable");
    assert!(read_only_error
        .to_string()
        .contains("controls require --writable"));

    let pane_error = registry
        .create(CreateWebShareRequest {
            scope: WebShareScope::Pane(target()),
            public_base_url: None,
            frontend_url: None,
            ttl_seconds: None,
            max_readers: None,
            url_options: Default::default(),
            require_pin: false,
            terminal_palette: None,
            writable: true,
            controls: true,
        })
        .expect_err("controls require a session scope");
    assert!(pane_error
        .to_string()
        .contains("controls require a session target"));

    let created = registry
        .create(CreateWebShareRequest {
            scope: WebShareScope::Session(session.clone()),
            public_base_url: None,
            frontend_url: None,
            ttl_seconds: None,
            max_readers: None,
            url_options: Default::default(),
            require_pin: false,
            terminal_palette: None,
            writable: true,
            controls: true,
        })
        .expect("session controls share creates");
    assert!(matches!(created.scope, WebShareScope::Session(ref actual) if actual == &session));
    assert!(created.controls);

    let listed = registry.list(ListWebSharesRequest);
    assert!(matches!(
        listed.shares[0].scope,
        WebShareScope::Session(ref actual) if actual == &session
    ));
    assert!(listed.shares[0].controls);
}

#[test]
fn stop_all_reports_removed_share_count() {
    let registry = WebShareRegistry::default();
    for _ in 0..2 {
        registry
            .create(CreateWebShareRequest {
                scope: WebShareScope::Pane(target()),
                public_base_url: None,
                frontend_url: None,
                ttl_seconds: None,
                max_readers: None,
                url_options: Default::default(),
                require_pin: false,
                terminal_palette: None,
                writable: false,
                controls: false,
            })
            .expect("share creates");
    }
    assert_eq!(registry.stop_all(StopAllWebSharesRequest).stopped, 2);
    assert!(registry.list(ListWebSharesRequest).shares.is_empty());
}

#[tokio::test]
async fn connect_enforces_read_cap_and_single_operator() {
    let registry = WebShareRegistry::default();
    let created = registry
        .create(CreateWebShareRequest {
            scope: WebShareScope::Pane(target()),
            public_base_url: None,
            frontend_url: None,
            ttl_seconds: None,
            max_readers: Some(1),
            url_options: Default::default(),
            require_pin: false,
            terminal_palette: None,
            writable: true,
            controls: false,
        })
        .expect("share creates");
    let read_token = token_from_url(&created.read_url);
    let operator_token = token_from_url(created.operator_url.as_deref().expect("operator url"));

    let read = registry
        .connect(&read_token, None)
        .await
        .expect("read connects");
    assert!(!read.is_operator());
    assert!(registry.connect(&read_token, None).await.is_err());

    let operator = registry
        .connect(&operator_token, None)
        .await
        .expect("operator connects");
    assert!(operator.is_operator());
    assert!(registry.connect(&operator_token, None).await.is_err());

    drop(read);
    assert!(registry.connect(&read_token, None).await.is_ok());
}

#[tokio::test]
async fn capability_tokens_grant_only_their_daemon_owned_roles() {
    let registry = WebShareRegistry::default();
    let created = registry
        .create(CreateWebShareRequest {
            scope: WebShareScope::Pane(target()),
            public_base_url: None,
            frontend_url: None,
            ttl_seconds: None,
            max_readers: Some(2),
            url_options: Default::default(),
            require_pin: false,
            terminal_palette: None,
            writable: true,
            controls: false,
        })
        .expect("share creates");

    assert!(!created.read_url.contains("id="));
    assert!(!created.read_url.contains("key="));
    assert!(!created.read_url.contains("role="));
    let operator_url = created.operator_url.as_deref().expect("operator URL");
    assert!(!operator_url.contains("id="));
    assert!(!operator_url.contains("key="));
    assert!(!operator_url.contains("role="));

    let read_token = token_from_url(&created.read_url);
    let read_access = registry
        .connect(&read_token, None)
        .await
        .expect("read token connects");
    assert!(!read_access.is_operator());
    assert!(!read_access.controls());
    drop(read_access);

    let operator_token = token_from_url(operator_url);
    let operator_access = registry
        .connect(&operator_token, None)
        .await
        .expect("operator token connects");
    assert!(operator_access.is_operator());
    assert!(!operator_access.controls());
}

#[tokio::test]
async fn stopped_or_expired_share_rejects_previous_tokens() {
    let registry = WebShareRegistry::default();
    let created = registry
        .create(CreateWebShareRequest {
            scope: WebShareScope::Pane(target()),
            public_base_url: None,
            frontend_url: None,
            ttl_seconds: None,
            max_readers: Some(2),
            url_options: Default::default(),
            require_pin: false,
            terminal_palette: None,
            writable: true,
            controls: false,
        })
        .expect("share creates");
    let read_token = token_from_url(&created.read_url);
    let operator_token = token_from_url(created.operator_url.as_deref().expect("operator URL"));

    assert!(
        registry
            .stop(rmux_proto::StopWebShareRequest {
                share_id: created.share_id,
            })
            .stopped
    );
    assert!(registry.connect(&read_token, None).await.is_err());
    assert!(registry.connect(&operator_token, None).await.is_err());
}

#[tokio::test]
async fn auth_failures_backoff_per_share_id() {
    let registry = WebShareRegistry::default();
    let _created = registry
        .create(CreateWebShareRequest {
            scope: WebShareScope::Pane(target()),
            public_base_url: None,
            frontend_url: None,
            ttl_seconds: None,
            max_readers: Some(2),
            url_options: Default::default(),
            require_pin: false,
            terminal_palette: None,
            writable: false,
            controls: false,
        })
        .expect("share creates");
    let wrong_token = random_token().expect("test token");

    let start = Instant::now();
    for _ in 0..4 {
        assert!(registry.connect(&wrong_token, None).await.is_err());
    }

    assert!(
        start.elapsed() >= Duration::from_millis(650),
        "expected exponential backoff to delay repeated failures"
    );
}

fn target() -> PaneTargetRef {
    PaneTargetRef::by_id(
        SessionName::new("alpha").expect("valid session"),
        PaneId::new(7),
    )
}

fn token_from_url(url: &str) -> String {
    url.split_once("t=")
        .map(|(_, token)| token.split('&').next().unwrap_or(token).to_owned())
        .expect("token query")
}
