use super::*;
use rmux_core::events::SubscriptionLimits;
use rmux_proto::WebShareCreatedResponse;
use rmux_proto::{encode_attach_message, AttachMessage};
use rmux_proto::{
    CreateWebShareRequest, KillPaneRequest, KillSessionRequest, ListWebSharesRequest,
    NewSessionRequest, PaneTarget, RenameSessionRequest, Request, Response, SessionName,
    SplitDirection, SplitWindowRequest, SplitWindowTarget, StopWebShareRequest, TerminalSize,
    WebShareScope,
};
use tokio::io::AsyncWriteExt;
use tokio::time::{sleep, timeout, Duration, Instant};

#[tokio::test]
async fn web_share_create_starts_lazy_listener() {
    let handler = handler_with_web_port(unused_web_port());
    let session_name = new_session(&handler, "lazy-start").await;

    let response = handler
        .handle(Request::WebShare(Box::new(WebShareRequest::Create(
            share_request(WebShareScope::Session(session_name)),
        ))))
        .await;

    assert!(matches!(
        response,
        Response::WebShare(response)
            if matches!(response.as_ref(), rmux_proto::WebShareResponse::Created(_))
    ));
}

#[tokio::test]
async fn web_share_config_starts_lazy_listener() {
    let port = unused_web_port();
    let handler = handler_with_web_port(port);

    let response = handler
        .handle(Request::WebShare(Box::new(WebShareRequest::Config(
            rmux_proto::WebShareConfigRequest,
        ))))
        .await;

    let Response::WebShare(response) = response else {
        panic!("expected web-share config response");
    };
    let rmux_proto::WebShareResponse::Config(config) = *response else {
        panic!("expected web-share config response");
    };
    assert_eq!(config.listener.port, port);
}

#[tokio::test]
async fn implicit_web_share_port_falls_back_when_default_is_busy() {
    let blocker = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind blocker");
    let blocked_port = blocker.local_addr().expect("blocker addr").port();
    let handler = handler_with_web_settings(
        crate::web::WebShareSettings::from_options_with_port_explicit(blocked_port, None, false)
            .expect("web settings"),
    );

    let response = handler
        .handle(Request::WebShare(Box::new(WebShareRequest::Config(
            rmux_proto::WebShareConfigRequest,
        ))))
        .await;

    let Response::WebShare(response) = response else {
        panic!("expected web-share config response");
    };
    let rmux_proto::WebShareResponse::Config(config) = *response else {
        panic!("expected web-share config response");
    };
    assert_eq!(config.listener.host, "127.0.0.1");
    assert_ne!(config.listener.port, blocked_port);
}

#[tokio::test]
async fn concurrent_web_share_create_waits_for_lazy_listener_start() {
    let handler = handler_with_web_port(unused_web_port());
    let alpha = new_session(&handler, "lazy-alpha").await;
    let beta = new_session(&handler, "lazy-beta").await;

    let left_handler = handler.clone();
    let right_handler = handler.clone();
    let (left, right) = tokio::join!(
        left_handler.handle(Request::WebShare(Box::new(WebShareRequest::Create(
            share_request(WebShareScope::Session(alpha),)
        )))),
        right_handler.handle(Request::WebShare(Box::new(WebShareRequest::Create(
            share_request(WebShareScope::Session(beta),)
        )))),
    );

    assert!(matches!(
        left,
        Response::WebShare(response)
            if matches!(response.as_ref(), rmux_proto::WebShareResponse::Created(_))
    ));
    assert!(matches!(
        right,
        Response::WebShare(response)
            if matches!(response.as_ref(), rmux_proto::WebShareResponse::Created(_))
    ));
    assert_eq!(list_shares(&handler).await.len(), 2);
}

#[tokio::test]
async fn failed_lazy_listener_start_does_not_create_share() {
    let blocker = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind blocker");
    let port = blocker.local_addr().expect("blocker addr").port();
    let handler = handler_with_web_port(port);
    let session_name = new_session(&handler, "lazy-bind-failure").await;

    let response = handler
        .handle(Request::WebShare(Box::new(WebShareRequest::Create(
            share_request(WebShareScope::Session(session_name)),
        ))))
        .await;

    let Response::Error(error) = response else {
        panic!("expected listener startup failure");
    };
    assert!(error.error.to_string().contains("listener unavailable"));
    assert!(list_shares(&handler).await.is_empty());

    drop(blocker);
}

#[tokio::test]
async fn web_share_create_resolves_slot_target_to_stable_pane_id() {
    let handler = RequestHandler::new();
    let session_name = new_session(&handler, "alpha").await;
    let created = create_share(
        &handler,
        share_request(WebShareScope::Pane(
            rmux_proto::PaneTarget::new(session_name.clone(), 0).into(),
        )),
    )
    .await;
    assert!(matches!(
        created.scope,
        WebShareScope::Pane(PaneTargetRef::Id {
            session_name: ref actual,
            ..
        }) if actual == &session_name
    ));
    assert!(created
        .spectator_url
        .as_deref()
        .expect("spectator URL")
        .contains("#e=wss://share.example/share&t="));
}

#[tokio::test]
async fn stopped_ttl_share_wakes_its_expiry_waiter() {
    let handler = RequestHandler::new();
    let session_name = new_session(&handler, "ttl-stop-wakeup").await;
    let created = create_share(
        &handler,
        CreateWebShareRequest {
            ttl_seconds: Some(7 * 24 * 60 * 60),
            ..share_request(WebShareScope::Session(session_name))
        },
    )
    .await;
    let mut revoke_rx = handler
        .web_shares
        .expiry_revoke_receiver(&created.share_id)
        .expect("TTL share has an expiry cancellation receiver");

    let stopped = handler
        .handle(Request::WebShare(Box::new(WebShareRequest::Stop(
            StopWebShareRequest {
                share_id: created.share_id,
            },
        ))))
        .await;
    assert!(matches!(
        stopped,
        Response::WebShare(response)
            if matches!(response.as_ref(), rmux_proto::WebShareResponse::Stopped(stopped) if stopped.stopped)
    ));
    timeout(Duration::from_millis(100), revoke_rx.changed())
        .await
        .expect("stopping the share must wake the seven-day expiry waiter")
        .expect("revoke sender remains valid through notification");
    assert_eq!(
        *revoke_rx.borrow(),
        Some(crate::web::WebShareRevokeReason::StoppedByOwner)
    );
}

#[tokio::test]
async fn tunnel_completion_revalidation_rejects_a_removed_target() {
    let handler = RequestHandler::new();
    let session_name = new_session(&handler, "web-stale-target").await;
    let resolved = handler
        .resolve_create_web_share(share_request(WebShareScope::Pane(
            PaneTarget::new(session_name.clone(), 0).into(),
        )))
        .await
        .expect("initial target resolves");

    let killed = handler
        .handle(Request::KillSession(KillSessionRequest {
            target: session_name,
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        }))
        .await;
    assert!(matches!(killed, Response::KillSession(_)));

    let state = handler.state.lock().await;
    assert!(validate_resolved_web_target(&state, resolved.target()).is_err());
    drop(state);
    assert!(list_shares(&handler).await.is_empty());
}

#[tokio::test]
async fn web_session_share_drains_initial_attach_output() {
    let handler = RequestHandler::new();
    let session_name = new_session(&handler, "websession").await;
    let created = create_share(
        &handler,
        CreateWebShareRequest {
            operator: true,
            spectator: true,
            controls: true,
            ..share_request(WebShareScope::Session(session_name))
        },
    )
    .await;
    let operator_url = created.operator_url.as_deref().expect("operator URL");
    let operator_token = token_from_url(operator_url);
    let stream = handler
        .open_web_share(&operator_token, None)
        .await
        .expect("session web share opens");
    let WebShareStream::Session(mut session_stream) = stream else {
        panic!("expected session web share stream");
    };
    let mut reader = session_stream.take_attach_reader();
    let event = timeout(Duration::from_secs(2), reader.read_event())
        .await
        .expect("attach stream should produce initial output")
        .expect("attach read succeeds")
        .expect("initial attach output is present");
    assert!(matches!(event, WebSessionAttachEvent::Data(_)));
    assert_eq!(session_stream.snapshot.size.cols, 80);
    assert_eq!(session_stream.snapshot.size.rows, 24);
}

#[tokio::test]
async fn web_session_attach_reader_emits_resize_events() {
    let (mut writer, reader) = tokio::io::duplex(128);
    let (reader, _) = tokio::io::split(reader);
    let mut reader = WebSessionAttachReader::new(reader);
    let frame = encode_attach_message(&AttachMessage::Resize(TerminalSize {
        cols: 100,
        rows: 30,
    }))
    .expect("resize attach message encodes");

    writer.write_all(&frame).await.expect("write attach frame");

    let event = timeout(Duration::from_secs(2), reader.read_event())
        .await
        .expect("attach reader should observe resize")
        .expect("attach read succeeds")
        .expect("resize event is present");
    assert!(matches!(event, WebSessionAttachEvent::Resize));
}

#[tokio::test]
async fn web_session_operator_registers_writable_attach() {
    let handler = RequestHandler::new();
    let session_name = new_session(&handler, "websession-write").await;
    let created = create_share(
        &handler,
        CreateWebShareRequest {
            operator: true,
            ..share_request(WebShareScope::Session(session_name.clone()))
        },
    )
    .await;
    let operator_token = token_from_url(created.operator_url.as_deref().expect("operator URL"));
    let stream = handler
        .open_web_share(&operator_token, None)
        .await
        .expect("session web share opens");
    let WebShareStream::Session(session_stream) = stream else {
        panic!("expected session web share stream");
    };
    assert!(session_stream.is_operator());
    assert!(session_stream.controls());

    let active_attach = handler.active_attach.lock().await;
    let active = active_attach
        .by_pid
        .values()
        .find(|active| active.session_name == session_name)
        .expect("web session attach is registered");
    assert!(active.can_write);
    assert!(!active.flags.contains(ClientFlags::READONLY));
}

#[tokio::test]
async fn web_session_spectator_share_attach_ignores_browser_size() {
    let handler = RequestHandler::new();
    let session_name = new_session(&handler, "websession-read-size").await;
    let created = create_share(
        &handler,
        share_request(WebShareScope::Session(session_name.clone())),
    )
    .await;
    let stream = handler
        .open_web_share(
            &token_from_url(created.spectator_url.as_deref().expect("spectator URL")),
            None,
        )
        .await
        .expect("session web share opens");
    let WebShareStream::Session(session_stream) = stream else {
        panic!("expected session web share stream");
    };
    assert!(!session_stream.is_operator());

    let active_attach = handler.active_attach.lock().await;
    let active = active_attach
        .by_pid
        .values()
        .find(|active| active.session_name == session_name)
        .expect("web session attach is registered");
    assert!(!active.can_write);
    assert!(active.flags.contains(ClientFlags::READONLY));
    assert!(active.flags.contains(ClientFlags::IGNORESIZE));
}

#[tokio::test]
async fn web_session_snapshot_tracks_canonical_session_size() {
    let handler = RequestHandler::new();
    let session_name = new_session(&handler, "websession-snapshot-size").await;
    let created = create_share(
        &handler,
        share_request(WebShareScope::Session(session_name.clone())),
    )
    .await;
    let stream = handler
        .open_web_share(
            &token_from_url(created.spectator_url.as_deref().expect("spectator URL")),
            None,
        )
        .await
        .expect("session web share opens");
    let WebShareStream::Session(session_stream) = stream else {
        panic!("expected session web share stream");
    };
    assert_eq!(
        session_stream.snapshot.size,
        TerminalSize { cols: 80, rows: 24 }
    );

    {
        let mut state = handler.state.lock().await;
        state
            .sessions
            .session_mut(&session_name)
            .expect("session exists")
            .resize_terminal(TerminalSize { cols: 60, rows: 10 });
    }

    let snapshot = handler
        .web_session_snapshot(session_stream.target())
        .await
        .expect("session snapshot refreshes");
    assert_eq!(snapshot.size, TerminalSize { cols: 60, rows: 10 });
}

#[tokio::test]
async fn web_share_expiry_kills_session_after_unix_second_rounding_window() {
    let handler = RequestHandler::new();
    let session_name = new_session(&handler, "websession-expire").await;
    create_share(
        &handler,
        CreateWebShareRequest {
            ttl_seconds: Some(1),
            kill_session_on_expire: true,
            ..share_request(WebShareScope::Session(session_name.clone()))
        },
    )
    .await;

    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        let removed = {
            let state = handler.state.lock().await;
            state.sessions.session(&session_name).is_none()
        };
        if removed {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "expired web-share did not kill its session"
        );
        sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test]
async fn kill_session_prunes_web_session_share_before_name_reuse() {
    let handler = RequestHandler::new();
    let session_name = new_session(&handler, "websession").await;
    let created = create_share(
        &handler,
        CreateWebShareRequest {
            operator: true,
            spectator: true,
            controls: true,
            ..share_request(WebShareScope::Session(session_name.clone()))
        },
    )
    .await;
    let operator_token = token_from_url(created.operator_url.as_deref().expect("operator URL"));

    let killed = handler
        .handle(Request::KillSession(KillSessionRequest {
            target: session_name.clone(),
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        }))
        .await;
    assert!(matches!(killed, Response::KillSession(_)));

    assert!(
        list_shares(&handler).await.is_empty(),
        "shares for a removed session should be pruned"
    );

    new_session(&handler, session_name.as_str()).await;

    let error = handler
        .open_web_share(&operator_token, None)
        .await
        .err()
        .expect("old share must not attach to a recreated session");
    assert!(error.to_string().contains("does not exist"));
}

#[tokio::test]
async fn killing_last_pane_prunes_web_session_share() {
    let handler = RequestHandler::new();
    let session_name = new_session(&handler, "websession-kill-pane").await;
    let created = create_share(
        &handler,
        share_request(WebShareScope::Session(session_name.clone())),
    )
    .await;
    let spectator_token = token_from_url(created.spectator_url.as_deref().expect("spectator URL"));

    let killed = handler
        .handle(Request::KillPane(KillPaneRequest {
            target: PaneTarget::new(session_name.clone(), 0),
            kill_all_except: false,
        }))
        .await;
    assert!(matches!(killed, Response::KillPane(_)));

    assert!(
        list_shares(&handler).await.is_empty(),
        "session shares should be pruned when the last pane destroys the session"
    );

    let error = handler
        .open_web_share(&spectator_token, None)
        .await
        .err()
        .expect("old share must not attach after the session was destroyed");
    assert!(error.to_string().contains("does not exist"));
}

#[tokio::test]
async fn killing_one_shared_pane_revokes_only_its_stable_share() {
    let handler = RequestHandler::new();
    let session_name = new_session(&handler, "web-pane-kill").await;
    assert!(matches!(
        handler
            .handle(Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Session(session_name.clone()),
                direction: SplitDirection::Horizontal,
                before: false,
                environment: None,
            }))
            .await,
        Response::SplitWindow(_)
    ));
    let pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&session_name)
            .expect("session exists")
            .window()
            .pane(1)
            .expect("second pane exists")
            .id()
    };
    let created = create_share(
        &handler,
        share_request(WebShareScope::Pane(PaneTargetRef::by_id(
            session_name.clone(),
            pane_id,
        ))),
    )
    .await;
    let token = token_from_url(created.spectator_url.as_deref().expect("spectator URL"));

    let killed = handler
        .handle(Request::KillPane(KillPaneRequest {
            target: PaneTarget::with_window(session_name, 0, 1),
            kill_all_except: false,
        }))
        .await;
    assert!(matches!(killed, Response::KillPane(_)));
    assert!(list_shares(&handler).await.is_empty());
    assert!(handler.open_web_share(&token, None).await.is_err());
}

#[tokio::test]
async fn renaming_session_rekeys_stable_pane_share() {
    let handler = RequestHandler::new();
    let session_name = new_session(&handler, "web-pane-old").await;
    let renamed = SessionName::new("web-pane-new").expect("valid session name");
    let created = create_share(
        &handler,
        share_request(WebShareScope::Pane(
            PaneTarget::new(session_name.clone(), 0).into(),
        )),
    )
    .await;
    let token = token_from_url(created.spectator_url.as_deref().expect("spectator URL"));

    let response = handler
        .handle(Request::RenameSession(RenameSessionRequest {
            target: session_name,
            new_name: renamed.clone(),
        }))
        .await;
    assert!(matches!(response, Response::RenameSession(_)));
    let shares = list_shares(&handler).await;
    assert!(matches!(
        shares.as_slice(),
        [share]
            if matches!(&share.scope, WebShareScope::Pane(target) if target.session_name() == &renamed)
    ));
    assert!(handler.open_web_share(&token, None).await.is_ok());
}

#[tokio::test]
async fn kill_session_on_expire_follows_renamed_session_id() {
    let handler = RequestHandler::new();
    let session_name = new_session(&handler, "websession-expiry").await;
    let renamed_session = SessionName::new("websession-expiry-renamed").expect("valid session");
    create_share(
        &handler,
        CreateWebShareRequest {
            ttl_seconds: Some(6),
            operator: true,
            kill_session_on_expire: true,
            ..share_request(WebShareScope::Session(session_name.clone()))
        },
    )
    .await;

    let renamed = handler
        .handle(Request::RenameSession(RenameSessionRequest {
            target: session_name.clone(),
            new_name: renamed_session.clone(),
        }))
        .await;
    assert!(matches!(renamed, Response::RenameSession(_)));

    timeout(Duration::from_secs(10), async {
        loop {
            let session_gone = {
                let state = handler.state.lock().await;
                state.sessions.session(&renamed_session).is_none()
            };
            if session_gone {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("expiry task should kill the renamed session by id");

    let state = handler.state.lock().await;
    assert!(state.sessions.session(&session_name).is_none());
    assert!(state.sessions.session(&renamed_session).is_none());
}

#[tokio::test]
async fn web_session_select_pane_uses_explicit_pane_id() {
    let handler = RequestHandler::new();
    let session_name = new_session(&handler, "websession-select-pane").await;
    assert!(matches!(
        handler
            .handle(Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Session(session_name.clone()),
                direction: SplitDirection::Horizontal,
                before: false,
                environment: None,
            }))
            .await,
        Response::SplitWindow(_)
    ));
    let right_pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&session_name)
            .expect("session exists")
            .window()
            .pane(1)
            .expect("right pane exists")
            .id()
    };
    let created = create_share(
        &handler,
        CreateWebShareRequest {
            operator: true,
            spectator: false,
            max_spectators: None,
            max_operators: None,
            ..share_request(WebShareScope::Session(session_name.clone()))
        },
    )
    .await;
    let operator_token = token_from_url(created.operator_url.as_deref().expect("operator URL"));
    let stream = handler
        .open_web_share(&operator_token, None)
        .await
        .expect("session web share opens");
    let WebShareStream::Session(session_stream) = stream else {
        panic!("expected session web share stream");
    };

    handler
        .web_session_select_pane(session_stream.target(), right_pane_id)
        .await
        .expect("pane selection succeeds");

    let state = handler.state.lock().await;
    let active = state
        .sessions
        .session(&session_name)
        .expect("session exists")
        .window()
        .active_pane()
        .expect("active pane exists")
        .id();
    assert_eq!(active, right_pane_id);
}

#[tokio::test]
async fn web_session_operator_resize_reaches_attached_session() {
    let handler = RequestHandler::new();
    let session_name = new_session(&handler, "websession-browser-resize").await;
    let created = create_share(
        &handler,
        CreateWebShareRequest {
            operator: true,
            spectator: false,
            max_spectators: None,
            max_operators: None,
            ..share_request(WebShareScope::Session(session_name.clone()))
        },
    )
    .await;
    let operator_token = token_from_url(created.operator_url.as_deref().expect("operator URL"));
    let stream = handler
        .open_web_share(&operator_token, None)
        .await
        .expect("session web share opens");
    let WebShareStream::Session(mut session_stream) = stream else {
        panic!("expected session web share stream");
    };

    session_stream
        .send_attach_resize(TerminalSize {
            cols: 100,
            rows: 40,
        })
        .await
        .expect("resize is written to attach stream");

    timeout(Duration::from_secs(2), async {
        loop {
            let size = {
                let state = handler.state.lock().await;
                state
                    .sessions
                    .session(&session_name)
                    .expect("session exists")
                    .window()
                    .size()
            };
            if size
                == (TerminalSize {
                    cols: 100,
                    rows: 40,
                })
            {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("browser resize reaches the attached session");
}

fn token_from_url(url: &str) -> String {
    url.split_once('#')
        .and_then(|(_, fragment)| {
            fragment.split('&').find_map(|param| {
                let (key, value) = param.split_once('=')?;
                (key == "t").then_some(value.to_owned())
            })
        })
        .expect("URL contains access token")
}

async fn new_session(handler: &RequestHandler, name: &str) -> SessionName {
    let session_name = SessionName::new(name).expect("valid session");
    assert!(matches!(
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session_name.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
        Response::NewSession(_)
    ));
    session_name
}

async fn create_share(
    handler: &RequestHandler,
    request: CreateWebShareRequest,
) -> WebShareCreatedResponse {
    handler.mark_web_listener_available();
    let response = handler
        .handle(Request::WebShare(Box::new(WebShareRequest::Create(
            request,
        ))))
        .await;
    let Response::WebShare(response) = response else {
        panic!("expected created web-share response");
    };
    let rmux_proto::WebShareResponse::Created(created) = *response else {
        panic!("expected created web-share response");
    };
    created
}

fn share_request(scope: WebShareScope) -> CreateWebShareRequest {
    CreateWebShareRequest {
        scope,
        public_base_url: Some("https://share.example".to_owned()),
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
        operator: false,
        spectator: true,
        controls: false,
        kill_session_on_expire: false,
    }
}

fn handler_with_web_port(port: u16) -> RequestHandler {
    handler_with_web_settings(
        crate::web::WebShareSettings::from_options(port, None).expect("web settings"),
    )
}

fn handler_with_web_settings(settings: crate::web::WebShareSettings) -> RequestHandler {
    RequestHandler::with_owner_uid_subscription_limits_and_web_settings(
        current_owner_uid(),
        SubscriptionLimits::default(),
        settings,
    )
}

fn unused_web_port() -> u16 {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind free port probe");
    listener.local_addr().expect("free port addr").port()
}

async fn list_shares(handler: &RequestHandler) -> Vec<rmux_proto::WebShareSummary> {
    let response = handler
        .handle(Request::WebShare(Box::new(WebShareRequest::List(
            ListWebSharesRequest,
        ))))
        .await;
    let Response::WebShare(response) = response else {
        panic!("expected listed web-share response");
    };
    let rmux_proto::WebShareResponse::List(listed) = *response else {
        panic!("expected listed web-share response");
    };
    listed.shares
}
