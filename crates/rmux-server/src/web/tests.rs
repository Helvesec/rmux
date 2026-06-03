use rmux_core::events::OutputCursorItem;
use rmux_proto::{
    CreateWebShareRequest, ListWebSharesRequest, PaneId, PaneTargetRef, SessionName,
    StopAllWebSharesRequest, WebShareScope, WebShareUrlOptions, WebTerminalTheme,
};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::pane_io::pane_output_channel_with_limits;
use crate::web::origin::validate_public_base_url;
use crate::web::secrets::{derive_spectator_token, random_token};
use crate::web::{WebShareRegistry, WebShareSettings};

fn available_registry() -> WebShareRegistry {
    let registry = WebShareRegistry::default();
    registry.mark_listener_available();
    registry
}

fn available_registry_with_settings(settings: WebShareSettings) -> WebShareRegistry {
    let registry = WebShareRegistry::new(settings);
    registry.mark_listener_available();
    registry
}

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
    let registry = available_registry();
    let created = registry
        .create(CreateWebShareRequest {
            scope: WebShareScope::Pane(target()),
            public_base_url: Some("https://share.example".to_owned()),
            tunnel_provider: None,
            frontend_url: None,
            ttl_seconds: Some(60),
            expires_at_unix: None,
            max_spectators: Some(2),
            max_operators: None,
            url_options: Default::default(),
            require_pin: false,
            operator_pin: None,
            spectator_pin: None,
            terminal_palette: None,
            operator: true,
            spectator: true,
            controls: false,
            kill_session_on_expire: false,
        })
        .expect("share creates");

    assert!(created
        .spectator_url
        .as_deref()
        .expect("spectator URL")
        .contains("#e=wss://share.example/share&t="));
    assert!(created
        .operator_url
        .as_deref()
        .is_some_and(|url| url.contains("#e=wss://share.example/share&t=")));
    let stdout = String::from_utf8_lossy(created.output.stdout());
    assert!(stdout.contains("spectator "));
    assert!(stdout.contains("operator URL emitted on stderr"));

    let listed = registry.list(ListWebSharesRequest);
    assert_eq!(listed.shares.len(), 1);
    let redacted = listed.shares[0].spectator_url.as_deref().expect("url");
    assert_eq!(
        redacted,
        format!("https://share.rmux.io/#e=wss://share.example/share&t=[REDACTED]")
    );
}

#[tokio::test]
async fn default_local_share_uses_hosted_frontend_and_local_websocket_endpoint() {
    let registry = available_registry();
    let created = registry
        .create(CreateWebShareRequest {
            scope: WebShareScope::Pane(target()),
            public_base_url: None,
            tunnel_provider: None,
            frontend_url: None,
            ttl_seconds: Some(60),
            expires_at_unix: None,
            max_spectators: Some(2),
            max_operators: None,
            url_options: Default::default(),
            require_pin: false,
            operator_pin: None,
            spectator_pin: None,
            terminal_palette: None,
            operator: false,
            spectator: true,
            controls: false,
            kill_session_on_expire: false,
        })
        .expect("share creates");

    assert!(created
        .spectator_url
        .as_deref()
        .expect("spectator URL")
        .starts_with("https://share.rmux.io/#t="));
    assert!(!created
        .spectator_url
        .as_deref()
        .expect("spectator URL")
        .contains("role="));

    let spectator_token = token_from_url(created.spectator_url.as_deref().expect("spectator URL"));
    let access = registry
        .connect(&spectator_token, None)
        .await
        .expect("spectator connects");
    assert!(access.origin_allowed("https://share.rmux.io"));
    assert!(access.origin_allowed("http://localhost:4321"));
    assert!(access.origin_allowed("http://127.0.0.1:5173"));
    assert!(!access.origin_allowed("https://evil.example"));
}

#[tokio::test]
async fn both_role_share_has_no_expiry_and_default_role_caps() {
    let registry = available_registry();
    let created = registry
        .create(CreateWebShareRequest {
            scope: WebShareScope::Pane(target()),
            public_base_url: None,
            tunnel_provider: None,
            frontend_url: None,
            ttl_seconds: None,
            expires_at_unix: None,
            max_spectators: None,
            max_operators: None,
            url_options: Default::default(),
            require_pin: false,
            operator_pin: None,
            spectator_pin: None,
            terminal_palette: None,
            operator: true,
            spectator: true,
            controls: false,
            kill_session_on_expire: false,
        })
        .expect("share creates");

    assert!(created.operator_url.is_some());
    assert!(created.spectator_url.is_some());
    assert!(created.operator);
    assert!(created.spectator);
    assert_eq!(created.expires_at_unix, None);
    assert_eq!(created.max_operators, Some(1));
    assert_eq!(created.max_spectators, Some(12));
    let stdout = String::from_utf8_lossy(created.output.stdout());
    assert!(stdout.contains("spectator "));
    assert!(stdout.contains("operator URL emitted on stderr"));
    assert!(stdout.contains("share does not expire"));

    let spectator_token = token_from_url(created.spectator_url.as_deref().expect("spectator URL"));
    let first = registry
        .connect(&spectator_token, None)
        .await
        .expect("first spectator connects");
    let second = registry
        .connect(&spectator_token, None)
        .await
        .expect("second spectator fits default spectator cap");
    assert_eq!(second.connection_counts().spectators_active, 2);
    drop((first, second));
}

#[test]
fn operator_only_share_does_not_mint_spectator_url() {
    let registry = available_registry();
    let created = registry
        .create(CreateWebShareRequest {
            scope: WebShareScope::Pane(target()),
            public_base_url: None,
            tunnel_provider: None,
            frontend_url: None,
            ttl_seconds: None,
            expires_at_unix: None,
            max_spectators: None,
            max_operators: None,
            url_options: Default::default(),
            require_pin: false,
            operator_pin: None,
            spectator_pin: None,
            terminal_palette: None,
            operator: true,
            spectator: false,
            controls: false,
            kill_session_on_expire: false,
        })
        .expect("operator-only share creates");

    assert!(created.spectator_url.is_none());
    assert!(created.operator_url.is_some());
    let stdout = String::from_utf8_lossy(created.output.stdout());
    assert!(!stdout.contains("spectator "));
    assert!(stdout.contains("operator URL emitted on stderr"));

    let listed = registry.list(ListWebSharesRequest);
    assert_eq!(listed.shares[0].spectator_url, None);
    assert_eq!(
        String::from_utf8_lossy(listed.output.stdout()).trim_end(),
        format!("{} {} -", created.share_id, target())
    );
}

#[test]
fn max_spectators_requires_spectator_url() {
    let registry = available_registry();
    let error = registry
        .create(CreateWebShareRequest {
            scope: WebShareScope::Pane(target()),
            public_base_url: None,
            tunnel_provider: None,
            frontend_url: None,
            ttl_seconds: None,
            expires_at_unix: None,
            max_spectators: Some(1),
            max_operators: None,
            url_options: Default::default(),
            require_pin: false,
            operator_pin: None,
            spectator_pin: None,
            terminal_palette: None,
            operator: true,
            spectator: false,
            controls: false,
            kill_session_on_expire: false,
        })
        .expect_err("spectator cap without spectator is invalid");

    assert!(error
        .to_string()
        .contains("web-share --max-spectators cannot be used without a spectator URL"));
}

#[test]
fn max_operators_requires_operator_url() {
    let registry = available_registry();
    let error = registry
        .create(CreateWebShareRequest {
            scope: WebShareScope::Pane(target()),
            public_base_url: None,
            tunnel_provider: None,
            frontend_url: None,
            ttl_seconds: None,
            expires_at_unix: None,
            max_spectators: None,
            max_operators: Some(1),
            url_options: Default::default(),
            require_pin: false,
            operator_pin: None,
            spectator_pin: None,
            terminal_palette: None,
            operator: false,
            spectator: true,
            controls: false,
            kill_session_on_expire: false,
        })
        .expect_err("operator cap without operator is invalid");

    assert!(error
        .to_string()
        .contains("web-share --max-operators cannot be used without an operator URL"));
}

#[tokio::test]
async fn known_token_origin_precheck_does_not_consume_a_read_slot() {
    let registry = available_registry();
    let created = registry
        .create(CreateWebShareRequest {
            scope: WebShareScope::Pane(target()),
            public_base_url: None,
            tunnel_provider: None,
            frontend_url: None,
            ttl_seconds: Some(60),
            expires_at_unix: None,
            max_spectators: Some(1),
            max_operators: None,
            url_options: Default::default(),
            require_pin: false,
            operator_pin: None,
            spectator_pin: None,
            terminal_palette: None,
            operator: false,
            spectator: true,
            controls: false,
            kill_session_on_expire: false,
        })
        .expect("share creates");
    let token = token_from_url(created.spectator_url.as_deref().expect("spectator URL"));

    assert_eq!(
        registry.known_token_origin_allowed(&token, "https://evil.example"),
        Some(false)
    );
    assert!(registry
        .connect(&token, None)
        .await
        .expect("spectator connects after rejected origin precheck")
        .origin_allowed("https://share.rmux.io"));
}

#[tokio::test]
async fn frontend_override_changes_browser_origin_without_changing_local_endpoint() {
    let registry = available_registry_with_settings(
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
            tunnel_provider: None,
            frontend_url: None,
            ttl_seconds: Some(60),
            expires_at_unix: None,
            max_spectators: Some(2),
            max_operators: None,
            url_options: Default::default(),
            require_pin: false,
            operator_pin: None,
            spectator_pin: None,
            terminal_palette: None,
            operator: false,
            spectator: true,
            controls: false,
            kill_session_on_expire: false,
        })
        .expect("share creates");

    assert!(created
        .spectator_url
        .as_deref()
        .expect("spectator URL")
        .starts_with("https://share.fork.example/#e=ws://127.0.0.1:9778/share&t="));
    let spectator_token = token_from_url(created.spectator_url.as_deref().expect("spectator URL"));
    let access = registry
        .connect(&spectator_token, None)
        .await
        .expect("spectator connects");
    assert!(access.origin_allowed("https://share.fork.example"));
    assert!(!access.origin_allowed("https://share.rmux.io"));
}

#[tokio::test]
async fn per_share_frontend_url_overrides_daemon_default() {
    let registry = available_registry();
    let created = registry
        .create(CreateWebShareRequest {
            scope: WebShareScope::Pane(target()),
            public_base_url: Some("https://terminal.example".to_owned()),
            tunnel_provider: None,
            frontend_url: Some("https://share.fork.example/share".to_owned()),
            ttl_seconds: Some(60),
            expires_at_unix: None,
            max_spectators: Some(2),
            max_operators: None,
            url_options: Default::default(),
            require_pin: false,
            operator_pin: None,
            spectator_pin: None,
            terminal_palette: None,
            operator: false,
            spectator: true,
            controls: false,
            kill_session_on_expire: false,
        })
        .expect("share creates");

    assert!(created
        .spectator_url
        .as_deref()
        .expect("spectator URL")
        .starts_with("https://share.fork.example/share/#e=wss://terminal.example/share&t="));
    let spectator_token = token_from_url(created.spectator_url.as_deref().expect("spectator URL"));
    let access = registry
        .connect(&spectator_token, None)
        .await
        .expect("spectator connects");
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
fn tunnel_provider_and_tunnel_url_are_mutually_exclusive() {
    let registry = available_registry();
    let error = registry
        .create(CreateWebShareRequest {
            scope: WebShareScope::Pane(target()),
            public_base_url: Some("https://share.example".to_owned()),
            tunnel_provider: Some("srv-us".to_owned()),
            frontend_url: None,
            ttl_seconds: Some(60),
            expires_at_unix: None,
            max_spectators: Some(2),
            max_operators: None,
            url_options: Default::default(),
            require_pin: false,
            operator_pin: None,
            spectator_pin: None,
            terminal_palette: None,
            operator: false,
            spectator: true,
            controls: false,
            kill_session_on_expire: false,
        })
        .expect_err("mutually exclusive tunnel options are rejected");
    assert!(error.to_string().contains("mutually exclusive"));
}

#[test]
fn local_web_share_requires_bound_listener_and_valid_port() {
    assert!(crate::web::WebShareSettings::from_options(0, None).is_err());

    let cold_registry = WebShareRegistry::default();
    assert!(cold_registry
        .config(rmux_proto::WebShareConfigRequest)
        .expect_err("cold listener must reject config")
        .to_string()
        .contains("not started"));

    let registry = available_registry();
    registry.mark_listener_unavailable("address already in use");
    let error = registry
        .create(CreateWebShareRequest {
            scope: WebShareScope::Pane(target()),
            public_base_url: None,
            tunnel_provider: None,
            frontend_url: None,
            ttl_seconds: Some(60),
            expires_at_unix: None,
            max_spectators: Some(2),
            max_operators: None,
            url_options: Default::default(),
            require_pin: false,
            operator_pin: None,
            spectator_pin: None,
            terminal_palette: None,
            operator: false,
            spectator: true,
            controls: false,
            kill_session_on_expire: false,
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
            tunnel_provider: None,
            frontend_url: None,
            ttl_seconds: Some(60),
            expires_at_unix: None,
            max_spectators: Some(2),
            max_operators: None,
            url_options: Default::default(),
            require_pin: false,
            operator_pin: None,
            spectator_pin: None,
            terminal_palette: None,
            operator: false,
            spectator: true,
            controls: false,
            kill_session_on_expire: false,
        })
        .is_ok());
}

#[test]
fn public_url_scheme_is_case_insensitive_for_websocket_endpoint() {
    let registry = available_registry();
    let created = registry
        .create(CreateWebShareRequest {
            scope: WebShareScope::Pane(target()),
            public_base_url: Some("HTTPS://terminal.example".to_owned()),
            tunnel_provider: None,
            frontend_url: None,
            ttl_seconds: Some(60),
            expires_at_unix: None,
            max_spectators: Some(2),
            max_operators: None,
            url_options: Default::default(),
            require_pin: false,
            operator_pin: None,
            spectator_pin: None,
            terminal_palette: None,
            operator: false,
            spectator: true,
            controls: false,
            kill_session_on_expire: false,
        })
        .expect("uppercase HTTPS is valid");

    assert!(created
        .spectator_url
        .as_deref()
        .expect("spectator URL")
        .starts_with("https://share.rmux.io/#e=wss://terminal.example/share&t="));
}

#[tokio::test]
async fn url_options_are_encoded_in_spectator_urls() {
    let registry = available_registry();
    let created = registry
        .create(CreateWebShareRequest {
            scope: WebShareScope::Pane(target()),
            public_base_url: None,
            tunnel_provider: None,
            frontend_url: None,
            ttl_seconds: Some(60),
            expires_at_unix: None,
            max_spectators: Some(2),
            max_operators: None,
            url_options: WebShareUrlOptions {
                no_navbar: true,
                no_disclaimer: true,
                show_viewers: true,
                terminal_theme: Some(WebTerminalTheme::Light),
            },
            require_pin: false,
            operator_pin: None,
            spectator_pin: None,
            terminal_palette: None,
            operator: true,
            spectator: true,
            controls: false,
            kill_session_on_expire: false,
        })
        .expect("share creates");

    assert!(created
        .spectator_url
        .as_deref()
        .expect("spectator URL")
        .contains("&navbar=off"));
    assert!(created
        .spectator_url
        .as_deref()
        .expect("spectator URL")
        .contains("&disclaimer=off"));
    assert!(!created
        .spectator_url
        .as_deref()
        .expect("spectator URL")
        .contains("&viewers=on"));
    assert!(created
        .spectator_url
        .as_deref()
        .expect("spectator URL")
        .contains("&theme=light"));
    assert!(created
        .operator_url
        .as_deref()
        .is_some_and(|url| url.contains("&navbar=off")
            && url.contains("&disclaimer=off")
            && !url.contains("&viewers=on")
            && url.contains("&theme=light")));

    let spectator_token = token_from_url(created.spectator_url.as_deref().expect("spectator URL"));
    let access = registry
        .connect(&spectator_token, None)
        .await
        .expect("spectator token connects");
    assert!(access.show_viewers());
}

#[tokio::test]
async fn pairing_code_is_required_out_of_band_when_pin_enabled() {
    let registry = available_registry();
    let created = registry
        .create(CreateWebShareRequest {
            scope: WebShareScope::Pane(target()),
            public_base_url: None,
            tunnel_provider: None,
            frontend_url: None,
            ttl_seconds: Some(60),
            expires_at_unix: None,
            max_spectators: Some(2),
            max_operators: None,
            url_options: Default::default(),
            require_pin: true,
            operator_pin: None,
            spectator_pin: None,
            terminal_palette: None,
            operator: false,
            spectator: true,
            controls: false,
            kill_session_on_expire: false,
        })
        .expect("share creates");

    assert!(
        created.operator_pairing_code.is_none(),
        "spectator-only share should not mint an operator PIN"
    );
    let pairing_code = created
        .spectator_pairing_code
        .as_deref()
        .expect("pin-enabled spectator share returns pairing code");
    assert_eq!(pairing_code.len(), 6);
    assert!(pairing_code.bytes().all(|byte| byte.is_ascii_digit()));
    assert!(!created
        .spectator_url
        .as_deref()
        .expect("spectator URL")
        .contains("&pin=required"));
    assert!(!created
        .spectator_url
        .as_deref()
        .expect("spectator URL")
        .contains(pairing_code));
    let stdout = String::from_utf8_lossy(created.output.stdout());
    assert!(stdout.contains(&format!("spectator pin {pairing_code}\n")));

    let spectator_token = token_from_url(created.spectator_url.as_deref().expect("spectator URL"));
    assert!(registry
        .connect(&spectator_token, None)
        .await
        .expect_err("pin must be supplied")
        .to_string()
        .contains("missing web-share pairing code"));
    assert!(registry
        .connect(&spectator_token, Some("000000"))
        .await
        .is_err());
    assert!(registry
        .connect(&spectator_token, Some(pairing_code))
        .await
        .is_ok());
}

#[tokio::test]
async fn role_specific_pairing_codes_are_bound_to_their_access_role() {
    let registry = available_registry();
    let created = registry
        .create(CreateWebShareRequest {
            scope: WebShareScope::Session("alpha".parse().expect("session name")),
            public_base_url: None,
            tunnel_provider: None,
            frontend_url: None,
            ttl_seconds: Some(60),
            expires_at_unix: None,
            max_spectators: Some(2),
            max_operators: Some(2),
            url_options: Default::default(),
            require_pin: true,
            operator_pin: Some("123456".to_owned()),
            spectator_pin: Some("654321".to_owned()),
            terminal_palette: None,
            operator: true,
            spectator: true,
            controls: true,
            kill_session_on_expire: false,
        })
        .expect("share creates");

    assert_eq!(created.operator_pairing_code.as_deref(), Some("123456"));
    assert_eq!(created.spectator_pairing_code.as_deref(), Some("654321"));

    let operator_token = token_from_url(created.operator_url.as_deref().expect("operator URL"));
    let spectator_token = token_from_url(created.spectator_url.as_deref().expect("spectator URL"));

    assert!(registry
        .connect(&operator_token, Some("654321"))
        .await
        .expect_err("spectator PIN must not unlock operator token")
        .to_string()
        .contains("invalid web-share pairing code"));
    assert!(registry
        .connect(&spectator_token, Some("123456"))
        .await
        .expect_err("operator PIN must not unlock spectator token")
        .to_string()
        .contains("invalid web-share pairing code"));

    let operator = registry
        .connect(&operator_token, Some("123456"))
        .await
        .expect("operator token accepts operator PIN");
    assert!(operator.is_operator());

    let spectator = registry
        .connect(&spectator_token, Some("654321"))
        .await
        .expect("spectator token accepts spectator PIN");
    assert!(!spectator.is_operator());
}

#[test]
fn controls_are_derived_for_operator_session_shares() {
    let registry = available_registry();
    let session = SessionName::new("alpha").expect("valid session");

    let spectator_share = registry
        .create(CreateWebShareRequest {
            scope: WebShareScope::Session(session.clone()),
            public_base_url: None,
            tunnel_provider: None,
            frontend_url: None,
            ttl_seconds: None,
            expires_at_unix: None,
            max_spectators: None,
            max_operators: None,
            url_options: Default::default(),
            require_pin: false,
            operator_pin: None,
            spectator_pin: None,
            terminal_palette: None,
            operator: false,
            spectator: true,
            controls: true,
            kill_session_on_expire: false,
        })
        .expect("spectator session share creates");
    assert!(!spectator_share.controls);

    let pane = registry
        .create(CreateWebShareRequest {
            scope: WebShareScope::Pane(target()),
            public_base_url: None,
            tunnel_provider: None,
            frontend_url: None,
            ttl_seconds: None,
            expires_at_unix: None,
            max_spectators: None,
            max_operators: None,
            url_options: Default::default(),
            require_pin: false,
            operator_pin: None,
            spectator_pin: None,
            terminal_palette: None,
            operator: true,
            spectator: true,
            controls: true,
            kill_session_on_expire: false,
        })
        .expect("operator pane share creates");
    assert!(!pane.controls);

    let created = registry
        .create(CreateWebShareRequest {
            scope: WebShareScope::Session(session.clone()),
            public_base_url: None,
            tunnel_provider: None,
            frontend_url: None,
            ttl_seconds: None,
            expires_at_unix: None,
            max_spectators: None,
            max_operators: None,
            url_options: Default::default(),
            require_pin: false,
            operator_pin: None,
            spectator_pin: None,
            terminal_palette: None,
            operator: true,
            spectator: true,
            controls: false,
            kill_session_on_expire: false,
        })
        .expect("operator session share creates");
    assert!(matches!(created.scope, WebShareScope::Session(ref actual) if actual == &session));
    assert!(created.controls);

    let listed = registry.list(ListWebSharesRequest);
    let summary = listed
        .shares
        .iter()
        .find(|share| share.share_id == created.share_id)
        .expect("created share should be listed");
    assert!(matches!(
        summary.scope,
        WebShareScope::Session(ref actual) if actual == &session
    ));
    assert!(summary.controls);
}

#[test]
fn expiration_accepts_absolute_deadline_and_rejects_invalid_combinations() {
    let registry = available_registry();
    let future = unix_seconds(SystemTime::now() + Duration::from_secs(60));
    let created = registry
        .create(CreateWebShareRequest {
            scope: WebShareScope::Pane(target()),
            public_base_url: None,
            tunnel_provider: None,
            frontend_url: None,
            ttl_seconds: None,
            expires_at_unix: Some(future),
            max_spectators: None,
            max_operators: None,
            url_options: Default::default(),
            require_pin: false,
            operator_pin: None,
            spectator_pin: None,
            terminal_palette: None,
            operator: false,
            spectator: true,
            controls: false,
            kill_session_on_expire: false,
        })
        .expect("absolute expiry creates");
    assert_eq!(created.expires_at_unix, Some(future));
    assert!(String::from_utf8_lossy(created.output.stdout()).contains("share expires at "));

    let both_error = registry
        .create(CreateWebShareRequest {
            scope: WebShareScope::Pane(target()),
            public_base_url: None,
            tunnel_provider: None,
            frontend_url: None,
            ttl_seconds: Some(10),
            expires_at_unix: Some(future),
            max_spectators: None,
            max_operators: None,
            url_options: Default::default(),
            require_pin: false,
            operator_pin: None,
            spectator_pin: None,
            terminal_palette: None,
            operator: false,
            spectator: true,
            controls: false,
            kill_session_on_expire: false,
        })
        .expect_err("ttl and absolute expiry conflict");
    assert!(both_error.to_string().contains("mutually exclusive"));

    let past_error = registry
        .create(CreateWebShareRequest {
            scope: WebShareScope::Pane(target()),
            public_base_url: None,
            tunnel_provider: None,
            frontend_url: None,
            ttl_seconds: None,
            expires_at_unix: Some(1),
            max_spectators: None,
            max_operators: None,
            url_options: Default::default(),
            require_pin: false,
            operator_pin: None,
            spectator_pin: None,
            terminal_palette: None,
            operator: false,
            spectator: true,
            controls: false,
            kill_session_on_expire: false,
        })
        .expect_err("past expiry is rejected");
    assert!(past_error.to_string().contains("must be in the future"));

    let range_error = registry
        .create(CreateWebShareRequest {
            scope: WebShareScope::Pane(target()),
            public_base_url: None,
            tunnel_provider: None,
            frontend_url: None,
            ttl_seconds: None,
            expires_at_unix: Some(u64::MAX),
            max_spectators: None,
            max_operators: None,
            url_options: Default::default(),
            require_pin: false,
            operator_pin: None,
            spectator_pin: None,
            terminal_palette: None,
            operator: false,
            spectator: true,
            controls: false,
            kill_session_on_expire: false,
        })
        .expect_err("overflowing expiry is rejected");
    assert!(range_error.to_string().contains("out of range"));
}

#[test]
fn kill_session_on_expire_requires_session_scope() {
    let registry = available_registry();
    let pane_error = registry
        .create(CreateWebShareRequest {
            scope: WebShareScope::Pane(target()),
            public_base_url: None,
            tunnel_provider: None,
            frontend_url: None,
            ttl_seconds: Some(60),
            expires_at_unix: None,
            max_spectators: None,
            max_operators: None,
            url_options: Default::default(),
            require_pin: false,
            operator_pin: None,
            spectator_pin: None,
            terminal_palette: None,
            operator: false,
            spectator: true,
            controls: false,
            kill_session_on_expire: true,
        })
        .expect_err("pane expiry cannot kill a session");
    assert!(pane_error.to_string().contains("requires a session target"));

    let session = SessionName::new("expiry").expect("valid session");
    let created = registry
        .create(CreateWebShareRequest {
            scope: WebShareScope::Session(session),
            public_base_url: None,
            tunnel_provider: None,
            frontend_url: None,
            ttl_seconds: Some(60),
            expires_at_unix: None,
            max_spectators: None,
            max_operators: None,
            url_options: Default::default(),
            require_pin: false,
            operator_pin: None,
            spectator_pin: None,
            terminal_palette: None,
            operator: true,
            spectator: true,
            controls: false,
            kill_session_on_expire: true,
        })
        .expect("session kill-on-expiry share creates");
    assert!(created.kill_session_on_expire);
    assert!(String::from_utf8_lossy(created.output.stdout())
        .contains("session will be killed on expiry"));
}

#[test]
fn stop_all_reports_removed_share_count() {
    let registry = available_registry();
    for _ in 0..2 {
        registry
            .create(CreateWebShareRequest {
                scope: WebShareScope::Pane(target()),
                public_base_url: None,
                tunnel_provider: None,
                frontend_url: None,
                ttl_seconds: None,
                expires_at_unix: None,
                max_spectators: None,
                max_operators: None,
                url_options: Default::default(),
                require_pin: false,
                operator_pin: None,
                spectator_pin: None,
                terminal_palette: None,
                operator: false,
                spectator: true,
                controls: false,
                kill_session_on_expire: false,
            })
            .expect("share creates");
    }
    assert_eq!(registry.stop_all(StopAllWebSharesRequest).stopped, 2);
    assert!(registry.list(ListWebSharesRequest).shares.is_empty());
}

#[tokio::test]
async fn connect_enforces_role_caps() {
    let registry = available_registry();
    let created = registry
        .create(CreateWebShareRequest {
            scope: WebShareScope::Pane(target()),
            public_base_url: None,
            tunnel_provider: None,
            frontend_url: None,
            ttl_seconds: None,
            expires_at_unix: None,
            max_spectators: Some(1),
            max_operators: Some(2),
            url_options: Default::default(),
            require_pin: false,
            operator_pin: None,
            spectator_pin: None,
            terminal_palette: None,
            operator: true,
            spectator: true,
            controls: false,
            kill_session_on_expire: false,
        })
        .expect("share creates");
    let spectator_token = token_from_url(created.spectator_url.as_deref().expect("spectator URL"));
    let operator_token = token_from_url(created.operator_url.as_deref().expect("operator url"));

    let spectator = registry
        .connect(&spectator_token, None)
        .await
        .expect("spectator connects");
    assert!(!spectator.is_operator());
    assert_eq!(spectator.connection_counts().spectators_active, 1);
    assert_eq!(spectator.connection_counts().spectators_max, Some(1));
    assert_eq!(spectator.connection_counts().operators_active, 0);
    assert_eq!(spectator.connection_counts().operators_max, Some(2));
    assert_eq!(spectator.connection_counts().viewers_connected, 1);
    assert!(registry.connect(&spectator_token, None).await.is_err());

    let operator = registry
        .connect(&operator_token, None)
        .await
        .expect("operator connects");
    assert!(operator.is_operator());
    assert_eq!(operator.connection_counts().spectators_active, 1);
    assert_eq!(operator.connection_counts().operators_active, 1);
    assert_eq!(operator.connection_counts().viewers_connected, 2);

    let second_operator = registry
        .connect(&operator_token, None)
        .await
        .expect("second operator connects");
    assert_eq!(second_operator.connection_counts().operators_active, 2);
    assert_eq!(second_operator.connection_counts().viewers_connected, 3);
    assert!(registry.connect(&operator_token, None).await.is_err());

    drop(spectator);
    assert!(registry.connect(&spectator_token, None).await.is_ok());
}

#[tokio::test]
async fn connect_enforces_authenticated_process_capacity() {
    let registry = WebShareRegistry::new_with_authenticated_connection_limit(1);
    registry.mark_listener_available();
    let first = registry
        .create(CreateWebShareRequest {
            scope: WebShareScope::Pane(target()),
            public_base_url: None,
            tunnel_provider: None,
            frontend_url: None,
            ttl_seconds: None,
            expires_at_unix: None,
            max_spectators: None,
            max_operators: None,
            url_options: Default::default(),
            require_pin: false,
            operator_pin: None,
            spectator_pin: None,
            terminal_palette: None,
            operator: false,
            spectator: true,
            controls: false,
            kill_session_on_expire: false,
        })
        .expect("first share creates");
    let second = registry
        .create(CreateWebShareRequest {
            scope: WebShareScope::Pane(target()),
            public_base_url: None,
            tunnel_provider: None,
            frontend_url: None,
            ttl_seconds: None,
            expires_at_unix: None,
            max_spectators: None,
            max_operators: None,
            url_options: Default::default(),
            require_pin: false,
            operator_pin: None,
            spectator_pin: None,
            terminal_palette: None,
            operator: false,
            spectator: true,
            controls: false,
            kill_session_on_expire: false,
        })
        .expect("second share creates");
    let first_token = token_from_url(first.spectator_url.as_deref().expect("first spectator URL"));
    let second_token = token_from_url(
        second
            .spectator_url
            .as_deref()
            .expect("second spectator URL"),
    );

    let first_access = registry
        .connect(&first_token, None)
        .await
        .expect("first connection fits global capacity");
    assert!(
        registry.connect(&second_token, None).await.is_err(),
        "global authenticated capacity rejects a second connection even across shares"
    );

    drop(first_access);
    assert!(
        registry.connect(&second_token, None).await.is_ok(),
        "dropping a connection releases global capacity"
    );
}

#[tokio::test]
async fn capability_tokens_grant_only_their_daemon_owned_roles() {
    let registry = available_registry();
    let created = registry
        .create(CreateWebShareRequest {
            scope: WebShareScope::Pane(target()),
            public_base_url: None,
            tunnel_provider: None,
            frontend_url: None,
            ttl_seconds: None,
            expires_at_unix: None,
            max_spectators: Some(2),
            max_operators: None,
            url_options: Default::default(),
            require_pin: false,
            operator_pin: None,
            spectator_pin: None,
            terminal_palette: None,
            operator: true,
            spectator: true,
            controls: false,
            kill_session_on_expire: false,
        })
        .expect("share creates");

    assert!(!created
        .spectator_url
        .as_deref()
        .expect("spectator URL")
        .contains("id="));
    assert!(!created
        .spectator_url
        .as_deref()
        .expect("spectator URL")
        .contains("key="));
    assert!(!created
        .spectator_url
        .as_deref()
        .expect("spectator URL")
        .contains("role="));
    let operator_url = created.operator_url.as_deref().expect("operator URL");
    assert!(!operator_url.contains("id="));
    assert!(!operator_url.contains("key="));
    assert!(!operator_url.contains("role="));

    let spectator_token = token_from_url(created.spectator_url.as_deref().expect("spectator URL"));
    let operator_token = token_from_url(operator_url);
    assert_eq!(
        spectator_token,
        derive_spectator_token(&operator_token).expect("derived spectator token")
    );

    let spectator_access = registry
        .connect(&spectator_token, None)
        .await
        .expect("spectator token connects");
    assert!(!spectator_access.is_operator());
    assert!(!spectator_access.controls());
    drop(spectator_access);

    let operator_access = registry
        .connect(&operator_token, None)
        .await
        .expect("operator token connects");
    assert!(operator_access.is_operator());
    assert!(!operator_access.controls());
}

#[tokio::test]
async fn stopped_or_expired_share_rejects_previous_tokens() {
    let registry = available_registry();
    let created = registry
        .create(CreateWebShareRequest {
            scope: WebShareScope::Pane(target()),
            public_base_url: None,
            tunnel_provider: None,
            frontend_url: None,
            ttl_seconds: None,
            expires_at_unix: None,
            max_spectators: Some(2),
            max_operators: None,
            url_options: Default::default(),
            require_pin: false,
            operator_pin: None,
            spectator_pin: None,
            terminal_palette: None,
            operator: true,
            spectator: true,
            controls: false,
            kill_session_on_expire: false,
        })
        .expect("share creates");
    let spectator_token = token_from_url(created.spectator_url.as_deref().expect("spectator URL"));
    let operator_token = token_from_url(created.operator_url.as_deref().expect("operator URL"));

    assert!(
        registry
            .stop(rmux_proto::StopWebShareRequest {
                share_id: created.share_id,
            })
            .stopped
    );
    assert!(registry.connect(&spectator_token, None).await.is_err());
    assert!(registry.connect(&operator_token, None).await.is_err());
}

#[tokio::test]
async fn auth_failures_backoff_per_share_id() {
    let registry = available_registry();
    let _created = registry
        .create(CreateWebShareRequest {
            scope: WebShareScope::Pane(target()),
            public_base_url: None,
            tunnel_provider: None,
            frontend_url: None,
            ttl_seconds: None,
            expires_at_unix: None,
            max_spectators: Some(2),
            max_operators: None,
            url_options: Default::default(),
            require_pin: false,
            operator_pin: None,
            spectator_pin: None,
            terminal_palette: None,
            operator: false,
            spectator: true,
            controls: false,
            kill_session_on_expire: false,
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
    url.split_once('#')
        .and_then(|(_, fragment)| {
            fragment.split('&').find_map(|param| {
                let (key, value) = param.split_once('=')?;
                (key == "t").then_some(value.to_owned())
            })
        })
        .expect("token fragment")
}

fn unix_seconds(value: SystemTime) -> u64 {
    value
        .duration_since(UNIX_EPOCH)
        .expect("test deadline after epoch")
        .as_secs()
}
