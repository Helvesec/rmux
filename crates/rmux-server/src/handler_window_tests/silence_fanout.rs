use super::*;

async fn link_alias(handler: &RequestHandler, source: WindowTarget, destination: WindowTarget) {
    let response = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source,
            target: destination,
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(response, Response::LinkWindow(_)), "{response:?}");
}

fn timer_snapshot(handler: &RequestHandler, target: &WindowTarget) -> (u64, tokio::time::Instant) {
    handler
        .silence_timer_snapshot_for_test(target)
        .expect("silence timer is armed")
}

async fn replace_deadline(
    handler: &RequestHandler,
    target: &WindowTarget,
    offset_seconds: u64,
) -> (u64, tokio::time::Instant) {
    // Let any in-flight ConPTY startup-activity re-arm land before pinning a
    // deterministic deadline: the fixture panes are quiet, so once the timer
    // is stable nothing re-arms it again and the later exact-equality
    // assertions cannot race the asynchronous alert queue (the class fixed
    // for link_window_after_moves_silence_expiry_to_new_target_without_delay).
    let settle_deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut previous = timer_snapshot(handler, target);
    let mut stable_since = tokio::time::Instant::now();
    loop {
        tokio::time::sleep(Duration::from_millis(25)).await;
        let current = timer_snapshot(handler, target);
        let now = tokio::time::Instant::now();
        if current == previous {
            if now.duration_since(stable_since) >= Duration::from_millis(250) {
                break;
            }
        } else {
            previous = current;
            stable_since = now;
        }
        if now >= settle_deadline {
            break;
        }
    }

    handler.replace_silence_timer_deadline_for_test(
        target,
        tokio::time::Instant::now() + Duration::from_secs(offset_seconds),
    );
    timer_snapshot(handler, target)
}

async fn create_destination_group(
    handler: &RequestHandler,
    owner_name: &str,
    peer_name: &str,
) -> (SessionName, SessionName) {
    let owner = session_name(owner_name);
    let peer = session_name(peer_name);
    create_session(handler, owner.as_str()).await;
    create_grouped_session(handler, peer.as_str(), &owner).await;
    (owner, peer)
}

async fn set_session_monitor_silence(handler: &RequestHandler, session: &SessionName, value: &str) {
    let response = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Session(session.clone()),
            option: OptionName::MonitorSilence,
            value: value.to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(response, Response::SetOption(_)), "{response:?}");
}

async fn expire_session_window_zero(handler: &RequestHandler, session: &SessionName) {
    let target = WindowTarget::with_window(session.clone(), 0);
    let identity = handler
        .silence_timer_identity_for_test(&target)
        .expect("silence timer is armed before expiry");
    handler
        .expire_silence_timer_for_test(target, identity.0, identity.1, identity.2)
        .await;
}

#[tokio::test]
async fn new_group_peer_arms_fresh_when_matching_source_alias_is_unmonitored() {
    let handler = RequestHandler::new();
    let owner = session_name("new-peer-unmonitored-owner");
    create_session(&handler, owner.as_str()).await;
    handler.wait_for_initial_panes_for_test().await;
    enable_global_monitor_silence(&handler).await;

    let response = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Session(owner.clone()),
            option: OptionName::MonitorSilence,
            value: "0".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(response, Response::SetOption(_)), "{response:?}");
    assert_eq!(
        handler.silence_timer_snapshot_for_test(&WindowTarget::with_window(owner.clone(), 0)),
        None
    );

    let peer = session_name("new-peer-unmonitored-peer");
    create_grouped_session(&handler, peer.as_str(), &owner).await;
    assert!(
        handler
            .silence_timer_snapshot_for_test(&WindowTarget::with_window(peer, 0))
            .is_some(),
        "an unmonitored source is not an expired source; the monitored peer arms fresh"
    );
}

async fn assert_new_group_peer_uses_requested_template_silence_state(
    label: &str,
    template_is_expired: bool,
) {
    let handler = RequestHandler::new();
    let first = session_name(&format!("a-{label}"));
    let template = session_name(&format!("b-{label}"));
    let created = session_name(&format!("c-{label}"));
    create_session(&handler, first.as_str()).await;
    create_grouped_session(&handler, template.as_str(), &first).await;
    handler.wait_for_initial_panes_for_test().await;
    enable_global_monitor_silence(&handler).await;

    if template_is_expired {
        set_session_monitor_silence(&handler, &first, "0").await;
        expire_session_window_zero(&handler, &template).await;
    } else {
        expire_session_window_zero(&handler, &first).await;
        set_session_monitor_silence(&handler, &template, "0").await;
    }

    create_grouped_session(&handler, created.as_str(), &template).await;
    let created_target = WindowTarget::with_window(created.clone(), 0);
    assert_eq!(
        handler
            .silence_timer_snapshot_for_test(&created_target)
            .is_some(),
        !template_is_expired,
        "the new peer must inherit the requested template's monitored state"
    );
    let state = handler.state.lock().await;
    assert_eq!(
        state
            .sessions
            .session(&created)
            .expect("new grouped peer exists")
            .winlink_alert_flags(0)
            .contains(rmux_core::WINLINK_SILENCE),
        template_is_expired,
        "the cloned flag and inherited timer state must describe the same template"
    );
}

#[tokio::test]
async fn new_group_peer_prefers_requested_template_over_alphabetical_group_member() {
    assert_new_group_peer_uses_requested_template_silence_state("template-expired", true).await;
    assert_new_group_peer_uses_requested_template_silence_state("template-unmonitored", false)
        .await;
}

#[tokio::test]
async fn link_window_fans_out_source_silence_deadline_only_to_new_group_aliases() {
    let handler = RequestHandler::new();
    let source_session = session_name("link-fanout-source");
    let external_session = session_name("link-fanout-external");
    create_session(&handler, source_session.as_str()).await;
    create_session(&handler, external_session.as_str()).await;
    let source = WindowTarget::with_window(source_session, 0);
    let external = WindowTarget::with_window(external_session, 1);
    link_alias(&handler, source.clone(), external.clone()).await;
    let (owner, peer) = create_destination_group(
        &handler,
        "link-fanout-destination-owner",
        "link-fanout-destination-peer",
    )
    .await;
    handler.wait_for_initial_panes_for_test().await;
    enable_global_monitor_silence(&handler).await;

    let source_before = replace_deadline(&handler, &source, 121).await;
    let external_before = replace_deadline(&handler, &external, 137).await;
    let owner_unrelated = WindowTarget::with_window(owner.clone(), 0);
    let peer_unrelated = WindowTarget::with_window(peer.clone(), 0);
    let owner_unrelated_before = replace_deadline(&handler, &owner_unrelated, 149).await;
    let peer_unrelated_before = replace_deadline(&handler, &peer_unrelated, 163).await;

    let owner_destination = WindowTarget::with_window(owner, 1);
    let peer_destination = WindowTarget::with_window(peer, 1);
    link_alias(&handler, source.clone(), owner_destination.clone()).await;

    assert_eq!(timer_snapshot(&handler, &source), source_before);
    assert_eq!(timer_snapshot(&handler, &external), external_before);
    assert_eq!(
        timer_snapshot(&handler, &owner_unrelated),
        owner_unrelated_before
    );
    assert_eq!(
        timer_snapshot(&handler, &peer_unrelated),
        peer_unrelated_before
    );
    for destination in [owner_destination, peer_destination] {
        assert_eq!(
            timer_snapshot(&handler, &destination).1,
            source_before.1,
            "new destination alias {destination} inherits the addressed source deadline"
        );
    }
}

#[tokio::test]
async fn link_window_kill_clears_replaced_group_alerts_before_deadline_fanout() {
    let handler = RequestHandler::new();
    let source_session = session_name("link-kill-alert-source");
    create_session(&handler, source_session.as_str()).await;
    let source = WindowTarget::with_window(source_session, 0);
    let (owner, peer) =
        create_destination_group(&handler, "link-kill-alert-owner", "link-kill-alert-peer").await;
    handler.wait_for_initial_panes_for_test().await;
    enable_global_monitor_silence(&handler).await;
    let source_before = replace_deadline(&handler, &source, 127).await;
    let stale_flags = rmux_core::WINLINK_ALERTFLAGS;
    {
        let mut state = handler.state.lock().await;
        for session_name in [&owner, &peer] {
            assert!(state
                .sessions
                .session_mut(session_name)
                .expect("destination group member exists")
                .add_winlink_alert_flags(0, stale_flags));
        }
    }

    let response = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source,
            target: WindowTarget::with_window(owner.clone(), 0),
            after: false,
            before: false,
            kill_destination: true,
            detached: true,
        }))
        .await;
    assert!(matches!(response, Response::LinkWindow(_)), "{response:?}");

    let state = handler.state.lock().await;
    for session_name in [owner, peer] {
        let target = WindowTarget::with_window(session_name.clone(), 0);
        assert!(
            state
                .sessions
                .session(&session_name)
                .expect("destination group member survives")
                .winlink_alert_flags(0)
                .is_empty(),
            "alerts from the replaced WindowId must not survive at {target}"
        );
        assert_eq!(
            timer_snapshot(&handler, &target).1,
            source_before.1,
            "the active source deadline still fans out to {target}"
        );
    }
}

#[tokio::test]
async fn link_window_fans_out_expired_silence_state_without_rearming_group_aliases() {
    let handler = RequestHandler::new();
    let source_session = session_name("link-expired-fanout-source");
    create_session(&handler, source_session.as_str()).await;
    let source = WindowTarget::with_window(source_session, 0);
    let (owner, peer) = create_destination_group(
        &handler,
        "link-expired-fanout-owner",
        "link-expired-fanout-peer",
    )
    .await;
    handler.wait_for_initial_panes_for_test().await;
    enable_global_monitor_silence(&handler).await;

    let identity = handler
        .silence_timer_identity_for_test(&source)
        .expect("source silence timer is armed before expiry");
    handler
        .expire_silence_timer_for_test(source.clone(), identity.0, identity.1, identity.2)
        .await;
    assert_eq!(handler.silence_timer_snapshot_for_test(&source), None);

    let owner_destination = WindowTarget::with_window(owner, 1);
    let peer_destination = WindowTarget::with_window(peer, 1);
    link_alias(&handler, source, owner_destination.clone()).await;

    let state = handler.state.lock().await;
    for destination in [owner_destination, peer_destination] {
        assert_eq!(
            handler.silence_timer_snapshot_for_test(&destination),
            None,
            "expired alias {destination} must not be rearmed"
        );
        assert!(
            state
                .sessions
                .session(destination.session_name())
                .expect("destination group member survives")
                .winlink_alert_flags(destination.window_index())
                .contains(rmux_core::WINLINK_SILENCE),
            "expired silence flag must fan out to {destination}"
        );
    }
}

#[tokio::test]
async fn move_window_fans_out_source_silence_deadline_without_touching_external_alias() {
    let handler = RequestHandler::new();
    let source_session = session_name("move-fanout-source");
    let external_session = session_name("move-fanout-external");
    create_session(&handler, source_session.as_str()).await;
    create_session(&handler, external_session.as_str()).await;
    let source = WindowTarget::with_window(source_session, 0);
    let external = WindowTarget::with_window(external_session, 1);
    link_alias(&handler, source.clone(), external.clone()).await;
    let (owner, peer) = create_destination_group(
        &handler,
        "move-fanout-destination-owner",
        "move-fanout-destination-peer",
    )
    .await;
    handler.wait_for_initial_panes_for_test().await;
    enable_global_monitor_silence(&handler).await;

    let source_before = replace_deadline(&handler, &source, 121).await;
    let external_before = replace_deadline(&handler, &external, 137).await;
    let owner_unrelated = WindowTarget::with_window(owner.clone(), 0);
    let peer_unrelated = WindowTarget::with_window(peer.clone(), 0);
    let owner_unrelated_before = replace_deadline(&handler, &owner_unrelated, 149).await;
    let peer_unrelated_before = replace_deadline(&handler, &peer_unrelated, 163).await;
    let owner_destination = WindowTarget::with_window(owner.clone(), 1);
    let peer_destination = WindowTarget::with_window(peer, 1);

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(source.clone()),
            target: MoveWindowTarget::Window(owner_destination.clone()),
            renumber: false,
            kill_destination: false,
            detached: true,
            after: false,
            before: false,
        }))
        .await;
    assert!(matches!(response, Response::MoveWindow(_)), "{response:?}");

    assert_eq!(handler.silence_timer_snapshot_for_test(&source), None);
    assert_eq!(timer_snapshot(&handler, &external), external_before);
    assert_eq!(
        timer_snapshot(&handler, &owner_unrelated),
        owner_unrelated_before
    );
    assert_eq!(
        timer_snapshot(&handler, &peer_unrelated),
        peer_unrelated_before
    );
    for destination in [owner_destination, peer_destination] {
        assert_eq!(
            timer_snapshot(&handler, &destination).1,
            source_before.1,
            "new destination alias {destination} inherits the moved source deadline"
        );
    }
}

#[tokio::test]
async fn move_window_fanout_never_overwrites_a_represented_group_peer_deadline() {
    let handler = RequestHandler::new();
    let owner = session_name("move-represented-deadline-owner");
    let peer = session_name("move-represented-deadline-peer");
    create_session(&handler, owner.as_str()).await;
    insert_window(&handler, &owner, 1).await;
    create_grouped_session(&handler, peer.as_str(), &owner).await;
    handler.wait_for_initial_panes_for_test().await;
    enable_global_monitor_silence(&handler).await;

    let owner_source = WindowTarget::with_window(owner.clone(), 0);
    let peer_source = WindowTarget::with_window(peer.clone(), 0);
    let owner_unrelated = WindowTarget::with_window(owner.clone(), 1);
    let peer_unrelated = WindowTarget::with_window(peer.clone(), 1);
    let owner_source_before = replace_deadline(&handler, &owner_source, 121).await;
    let peer_source_before = replace_deadline(&handler, &peer_source, 137).await;
    let owner_unrelated_before = replace_deadline(&handler, &owner_unrelated, 149).await;
    let peer_unrelated_before = replace_deadline(&handler, &peer_unrelated, 163).await;

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(owner_source),
            target: MoveWindowTarget::Window(WindowTarget::with_window(owner.clone(), 2)),
            renumber: false,
            kill_destination: false,
            detached: true,
            after: false,
            before: false,
        }))
        .await;
    assert!(matches!(response, Response::MoveWindow(_)), "{response:?}");

    assert_eq!(
        timer_snapshot(&handler, &WindowTarget::with_window(owner, 2)).1,
        owner_source_before.1
    );
    assert_eq!(
        timer_snapshot(&handler, &WindowTarget::with_window(peer, 2)).1,
        peer_source_before.1,
        "the peer's represented timer wins over the addressed source fanout"
    );
    assert_eq!(
        timer_snapshot(&handler, &owner_unrelated),
        owner_unrelated_before
    );
    assert_eq!(
        timer_snapshot(&handler, &peer_unrelated),
        peer_unrelated_before
    );
}

#[tokio::test]
async fn swap_window_fans_out_each_addressed_deadline_without_touching_external_links() {
    let handler = RequestHandler::new();
    let source_session = session_name("swap-fanout-source");
    let source_external_session = session_name("swap-fanout-source-external");
    let target_external_session = session_name("swap-fanout-target-external");
    create_session(&handler, source_session.as_str()).await;
    create_session(&handler, source_external_session.as_str()).await;
    create_session(&handler, target_external_session.as_str()).await;
    let source = WindowTarget::with_window(source_session, 0);
    let source_external = WindowTarget::with_window(source_external_session, 1);
    link_alias(&handler, source.clone(), source_external.clone()).await;
    let (owner, peer) = create_destination_group(
        &handler,
        "swap-fanout-destination-owner",
        "swap-fanout-destination-peer",
    )
    .await;
    let owner_target = WindowTarget::with_window(owner.clone(), 0);
    let target_external = WindowTarget::with_window(target_external_session, 1);
    link_alias(&handler, owner_target.clone(), target_external.clone()).await;
    handler.wait_for_initial_panes_for_test().await;
    enable_global_monitor_silence(&handler).await;

    let source_before = replace_deadline(&handler, &source, 121).await;
    let source_external_before = replace_deadline(&handler, &source_external, 137).await;
    let owner_before = replace_deadline(&handler, &owner_target, 149).await;
    let peer_target = WindowTarget::with_window(peer, 0);
    let _peer_before = replace_deadline(&handler, &peer_target, 163).await;
    let target_external_before = replace_deadline(&handler, &target_external, 179).await;

    let response = handler
        .handle(Request::SwapWindow(SwapWindowRequest {
            source: source.clone(),
            target: owner_target.clone(),
            detached: true,
        }))
        .await;
    assert!(matches!(response, Response::SwapWindow(_)), "{response:?}");

    assert_eq!(timer_snapshot(&handler, &source).1, owner_before.1);
    assert_eq!(timer_snapshot(&handler, &owner_target).1, source_before.1);
    assert_eq!(timer_snapshot(&handler, &peer_target).1, source_before.1);
    assert_eq!(
        timer_snapshot(&handler, &source_external),
        source_external_before
    );
    assert_eq!(
        timer_snapshot(&handler, &target_external),
        target_external_before
    );
}
