use super::RequestHandler;
use rmux_core::PaneId;
use rmux_proto::{
    KillSessionRequest, LinkWindowRequest, NewSessionExtRequest, NewSessionRequest,
    NewWindowRequest, PaneKillRequest, PaneOptionGetRequest, PaneOptionSetRequest,
    PaneRespawnRequest, PaneStateClosedReason, PaneStateCursorRequest, PaneStateEventDto,
    PaneStateSnapshot, PaneStateSubscriptionId, PaneTarget, PaneTargetRef, Request,
    RespawnPaneRequest, RespawnWindowRequest, Response, SessionName, SetOptionMode,
    SubscribePaneStateRequest, TerminalSize, UnlinkWindowRequest, WindowTarget,
};

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

async fn create_session(handler: &RequestHandler, value: &str) -> SessionName {
    let session = session_name(value);
    let response = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session.clone(),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))
        .await;
    assert!(matches!(response, Response::NewSession(_)), "{response:?}");
    session
}

async fn create_grouped_session(
    handler: &RequestHandler,
    value: &str,
    group_target: &SessionName,
) -> SessionName {
    let session = session_name(value);
    let response = handler
        .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
            session_name: Some(session.clone()),
            working_directory: None,
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
            group_target: Some(group_target.clone()),
            attach_if_exists: false,
            detach_other_clients: false,
            kill_other_clients: false,
            flags: None,
            window_name: None,
            print_session_info: false,
            print_format: None,
            command: None,
            process_command: None,
            client_environment: None,
            skip_environment_update: false,
        })))
        .await;
    assert!(matches!(response, Response::NewSession(_)), "{response:?}");
    session
}

async fn create_window(handler: &RequestHandler, session: &SessionName, index: u32) {
    let response = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: session.clone(),
            name: None,
            detached: true,
            environment: None,
            command: None,
            start_directory: None,
            target_window_index: Some(index),
            insert_at_target: false,
            process_command: None,
        })))
        .await;
    assert!(matches!(response, Response::NewWindow(_)), "{response:?}");
}

async fn subscribe(
    handler: &RequestHandler,
    connection_id: u64,
    target: PaneTarget,
) -> (PaneStateSubscriptionId, PaneId, PaneStateSnapshot) {
    match handler
        .handle_subscribe_pane_state(
            connection_id,
            SubscribePaneStateRequest {
                target: PaneTargetRef::slot(target),
                include_title: false,
                include_options: true,
                include_foreground: false,
            },
        )
        .await
    {
        Response::SubscribePaneState(response) => (
            response.subscription_id,
            response.pane_id,
            response.snapshot,
        ),
        response => panic!("subscribe-pane-state failed: {response:?}"),
    }
}

async fn read_cursor(
    handler: &RequestHandler,
    connection_id: u64,
    subscription_id: PaneStateSubscriptionId,
    after_revision: u64,
) -> Response {
    handler
        .handle_pane_state_cursor(
            connection_id,
            PaneStateCursorRequest {
                subscription_id,
                after_revision,
                wait: false,
                max_events: Some(16),
            },
        )
        .await
}

async fn set_option(
    handler: &RequestHandler,
    target: PaneTargetRef,
    name: &str,
    value: &str,
) -> Response {
    handler
        .handle(Request::PaneOptionSet(PaneOptionSetRequest {
            target,
            name: name.to_owned(),
            value: Some(value.to_owned()),
            mode: SetOptionMode::Replace,
            unset: false,
        }))
        .await
}

async fn get_option(handler: &RequestHandler, target: PaneTargetRef, name: &str) -> Option<String> {
    match handler
        .handle(Request::PaneOptionGet(PaneOptionGetRequest {
            target,
            name: name.to_owned(),
        }))
        .await
    {
        Response::PaneOptionGet(response) => response.value,
        response => panic!("pane-option-get failed: {response:?}"),
    }
}

fn snapshot_has(snapshot: &PaneStateSnapshot, name: &str, value: &str) -> bool {
    snapshot
        .options
        .iter()
        .any(|entry| entry.name == name && entry.value == value)
}

fn assert_option_event(response: Response, pane_id: PaneId, name: &str, value: &str) {
    match response {
        Response::PaneStateCursor(response) => assert!(response.events.iter().any(|event| {
            matches!(event, PaneStateEventDto::OptionSet {
                pane_id: event_pane_id,
                name: event_name,
                new_value,
                ..
            } if *event_pane_id == pane_id && event_name == name && new_value == value)
        })),
        response => panic!("expected pane option event, got {response:?}"),
    }
}

fn closed_revision(response: Response, pane_id: PaneId) -> u64 {
    match response {
        Response::PaneStateCursor(response) => match response.events.as_slice() {
            [PaneStateEventDto::Closed {
                revision,
                pane_id: event_pane_id,
                reason: PaneStateClosedReason::Killed,
            }] if *event_pane_id == pane_id => *revision,
            events => panic!("expected one killed Closed event, got {events:?}"),
        },
        response => panic!("expected terminal Closed, got {response:?}"),
    }
}

fn assert_no_events(response: Response) {
    match response {
        Response::PaneStateCursor(response) => assert!(response.events.is_empty()),
        response => panic!("pane-state cursor failed: {response:?}"),
    }
}

#[tokio::test]
async fn linked_pane_aliases_share_option_get_snapshot_and_events() {
    let handler = RequestHandler::new();
    let alpha = create_session(&handler, "alias-link-alpha").await;
    let beta = create_session(&handler, "alias-link-beta").await;
    let linked = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(alpha.clone(), 0),
            target: WindowTarget::with_window(beta.clone(), 0),
            after: false,
            before: false,
            kill_destination: true,
            detached: true,
        }))
        .await;
    assert!(matches!(linked, Response::LinkWindow(_)), "{linked:?}");
    let alpha_target = PaneTarget::with_window(alpha.clone(), 0, 0);
    let beta_target = PaneTarget::with_window(beta.clone(), 0, 0);
    let (alpha_subscription, pane_id, alpha_snapshot) =
        subscribe(&handler, 1101, alpha_target.clone()).await;
    let (beta_subscription, beta_pane_id, beta_snapshot) =
        subscribe(&handler, 1102, beta_target.clone()).await;
    assert_eq!(beta_pane_id, pane_id);

    let set = set_option(
        &handler,
        PaneTargetRef::slot(beta_target.clone()),
        "@alias.link",
        "shared",
    )
    .await;
    assert!(matches!(set, Response::PaneOptionSet(_)), "{set:?}");
    assert_eq!(
        get_option(
            &handler,
            PaneTargetRef::slot(alpha_target.clone()),
            "@alias.link",
        )
        .await,
        Some("shared".to_owned())
    );
    assert_option_event(
        read_cursor(&handler, 1101, alpha_subscription, alpha_snapshot.revision).await,
        pane_id,
        "@alias.link",
        "shared",
    );
    assert_option_event(
        read_cursor(&handler, 1102, beta_subscription, beta_snapshot.revision).await,
        pane_id,
        "@alias.link",
        "shared",
    );

    let (_, _, alpha_after) = subscribe(&handler, 1103, alpha_target).await;
    let (_, _, beta_after) = subscribe(&handler, 1104, beta_target).await;
    assert!(snapshot_has(&alpha_after, "@alias.link", "shared"));
    assert!(snapshot_has(&beta_after, "@alias.link", "shared"));
}

#[tokio::test]
async fn grouped_pane_aliases_copy_existing_options_and_share_later_mutations() {
    let handler = RequestHandler::new();
    let alpha = create_session(&handler, "alias-group-alpha").await;
    let alpha_target = PaneTarget::with_window(alpha.clone(), 0, 0);
    let set = set_option(
        &handler,
        PaneTargetRef::slot(alpha_target.clone()),
        "@alias.before-group",
        "copied",
    )
    .await;
    assert!(matches!(set, Response::PaneOptionSet(_)), "{set:?}");
    let beta = create_grouped_session(&handler, "alias-group-beta", &alpha).await;
    let beta_target = PaneTarget::with_window(beta.clone(), 0, 0);
    assert_eq!(
        get_option(
            &handler,
            PaneTargetRef::slot(beta_target.clone()),
            "@alias.before-group",
        )
        .await,
        Some("copied".to_owned())
    );

    let (alpha_subscription, pane_id, alpha_snapshot) =
        subscribe(&handler, 1111, alpha_target.clone()).await;
    let (beta_subscription, beta_pane_id, beta_snapshot) =
        subscribe(&handler, 1112, beta_target.clone()).await;
    assert_eq!(beta_pane_id, pane_id);
    assert!(snapshot_has(
        &alpha_snapshot,
        "@alias.before-group",
        "copied"
    ));
    assert!(snapshot_has(
        &beta_snapshot,
        "@alias.before-group",
        "copied"
    ));

    let set = set_option(
        &handler,
        PaneTargetRef::slot(beta_target),
        "@alias.after-group",
        "shared",
    )
    .await;
    assert!(matches!(set, Response::PaneOptionSet(_)), "{set:?}");
    assert_eq!(
        get_option(
            &handler,
            PaneTargetRef::slot(alpha_target),
            "@alias.after-group",
        )
        .await,
        Some("shared".to_owned())
    );
    assert_option_event(
        read_cursor(&handler, 1111, alpha_subscription, alpha_snapshot.revision).await,
        pane_id,
        "@alias.after-group",
        "shared",
    );
    assert_option_event(
        read_cursor(&handler, 1112, beta_subscription, beta_snapshot.revision).await,
        pane_id,
        "@alias.after-group",
        "shared",
    );
}

#[tokio::test]
async fn killing_group_session_alias_keeps_runtime_and_emits_no_false_closed() {
    let handler = RequestHandler::new();
    let alpha = create_session(&handler, "kill-alias-alpha").await;
    let beta = create_grouped_session(&handler, "kill-alias-beta", &alpha).await;
    let alpha_target = PaneTarget::with_window(alpha.clone(), 0, 0);
    let beta_target = PaneTarget::with_window(beta.clone(), 0, 0);
    let (alpha_subscription, pane_id, alpha_snapshot) =
        subscribe(&handler, 1121, alpha_target).await;
    let (beta_subscription, beta_pane_id, beta_snapshot) =
        subscribe(&handler, 1122, beta_target).await;
    assert_eq!(beta_pane_id, pane_id);

    let killed = handler
        .handle(Request::KillSession(KillSessionRequest {
            target: beta,
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        }))
        .await;
    assert!(matches!(killed, Response::KillSession(_)), "{killed:?}");
    {
        let state = handler.state.lock().await;
        assert!(state.sessions.contains_session(&alpha));
        state
            .ensure_panes_exist(&alpha, &[pane_id])
            .expect("surviving group alias retains runtime");
    }
    assert_no_events(
        read_cursor(&handler, 1121, alpha_subscription, alpha_snapshot.revision).await,
    );
    assert_no_events(read_cursor(&handler, 1122, beta_subscription, beta_snapshot.revision).await);
}

#[tokio::test]
async fn deleting_last_pane_or_link_alias_preserves_the_shared_runtime() {
    let handler = RequestHandler::new();
    let alpha = create_session(&handler, "delete-alias-alpha").await;
    let beta = create_grouped_session(&handler, "delete-alias-beta", &alpha).await;
    let target = PaneTarget::with_window(alpha.clone(), 0, 0);
    let (subscription, pane_id, snapshot) = subscribe(&handler, 1131, target).await;
    let killed = handler
        .handle(Request::PaneKill(PaneKillRequest {
            target: PaneTargetRef::by_id(beta, pane_id),
            kill_all_except: false,
        }))
        .await;
    assert!(matches!(killed, Response::KillPane(_)), "{killed:?}");
    {
        let state = handler.state.lock().await;
        state
            .ensure_panes_exist(&alpha, &[pane_id])
            .expect("last-pane deletion through peer keeps owner runtime");
    }
    assert_no_events(read_cursor(&handler, 1131, subscription, snapshot.revision).await);

    let link_owner = create_session(&handler, "delete-link-owner").await;
    let link_peer = create_session(&handler, "delete-link-peer").await;
    create_window(&handler, &link_peer, 1).await;
    let linked = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(link_owner.clone(), 0),
            target: WindowTarget::with_window(link_peer.clone(), 0),
            after: false,
            before: false,
            kill_destination: true,
            detached: true,
        }))
        .await;
    assert!(matches!(linked, Response::LinkWindow(_)), "{linked:?}");
    let link_target = PaneTarget::with_window(link_owner.clone(), 0, 0);
    let (link_subscription, link_pane_id, link_snapshot) =
        subscribe(&handler, 1132, link_target).await;
    let unlinked = handler
        .handle(Request::UnlinkWindow(UnlinkWindowRequest {
            target: WindowTarget::with_window(link_peer, 0),
            kill_if_last: false,
        }))
        .await;
    assert!(
        matches!(unlinked, Response::UnlinkWindow(_)),
        "{unlinked:?}"
    );
    {
        let state = handler.state.lock().await;
        state
            .ensure_panes_exist(&link_owner, &[link_pane_id])
            .expect("unlinking one alias keeps linked runtime");
    }
    assert_no_events(read_cursor(&handler, 1132, link_subscription, link_snapshot.revision).await);
}

#[tokio::test]
async fn successful_respawn_pane_and_window_close_the_old_lifetime() {
    let handler = RequestHandler::new();
    let session = create_session(&handler, "respawn-old-lifetime").await;
    let target = PaneTarget::with_window(session.clone(), 0, 0);
    let (pane_subscription, pane_id, pane_snapshot) =
        subscribe(&handler, 1141, target.clone()).await;
    let respawned = handler
        .handle(Request::RespawnPane(Box::new(RespawnPaneRequest {
            target: target.clone(),
            kill: true,
            start_directory: None,
            environment: None,
            command: None,
            process_command: None,
        })))
        .await;
    assert!(
        matches!(respawned, Response::RespawnPane(_)),
        "{respawned:?}"
    );
    let _ = closed_revision(
        read_cursor(&handler, 1141, pane_subscription, pane_snapshot.revision).await,
        pane_id,
    );

    let (window_subscription, window_pane_id, window_snapshot) =
        subscribe(&handler, 1142, target).await;
    assert_eq!(window_pane_id, pane_id);
    let respawned = handler
        .handle(Request::RespawnWindow(Box::new(RespawnWindowRequest {
            target: WindowTarget::with_window(session, 0),
            kill: true,
            environment: None,
            command: None,
            start_directory: None,
        })))
        .await;
    assert!(
        matches!(respawned, Response::RespawnWindow(_)),
        "{respawned:?}"
    );
    let _ = closed_revision(
        read_cursor(
            &handler,
            1142,
            window_subscription,
            window_snapshot.revision,
        )
        .await,
        pane_id,
    );
}

#[tokio::test]
async fn pane_respawn_keep_alive_rolls_back_on_error_and_journals_on_success() {
    let handler = RequestHandler::new();
    let session = create_session(&handler, "pane-respawn-keep-alive").await;
    let target = PaneTarget::with_window(session.clone(), 0, 0);
    let (old_subscription, pane_id, old_snapshot) = subscribe(&handler, 1151, target.clone()).await;

    let failed = handler
        .handle(Request::PaneRespawn(Box::new(PaneRespawnRequest {
            target: PaneTargetRef::by_id(session.clone(), pane_id),
            kill: false,
            start_directory: None,
            environment: None,
            command: None,
            process_command: None,
            keep_alive_on_exit: Some(true),
        })))
        .await;
    assert!(matches!(failed, Response::Error(_)), "{failed:?}");
    assert_eq!(
        get_option(
            &handler,
            PaneTargetRef::slot(target.clone()),
            "remain-on-exit",
        )
        .await,
        None
    );
    assert_no_events(read_cursor(&handler, 1151, old_subscription, old_snapshot.revision).await);

    let succeeded = handler
        .handle(Request::PaneRespawn(Box::new(PaneRespawnRequest {
            target: PaneTargetRef::slot(target.clone()),
            kill: true,
            start_directory: None,
            environment: None,
            command: None,
            process_command: None,
            keep_alive_on_exit: Some(true),
        })))
        .await;
    assert!(
        matches!(succeeded, Response::RespawnPane(_)),
        "{succeeded:?}"
    );
    let close_revision = closed_revision(
        read_cursor(&handler, 1151, old_subscription, old_snapshot.revision).await,
        pane_id,
    );

    let (new_subscription, new_pane_id, new_snapshot) = subscribe(&handler, 1152, target).await;
    assert_eq!(new_pane_id, pane_id);
    assert!(snapshot_has(&new_snapshot, "remain-on-exit", "on"));
    assert_option_event(
        read_cursor(&handler, 1152, new_subscription, close_revision).await,
        pane_id,
        "remain-on-exit",
        "on",
    );
}
