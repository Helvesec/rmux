use super::*;

async fn set_session_monitor_silence(
    handler: &RequestHandler,
    session_name: SessionName,
    seconds: &str,
) {
    let response = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Session(session_name),
            option: OptionName::MonitorSilence,
            value: seconds.to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(response, Response::SetOption(_)), "{response:?}");
}

async fn expect_attach_exited(
    control_rx: &mut mpsc::UnboundedReceiver<AttachControl>,
    context: &str,
) {
    timeout(Duration::from_secs(2), async {
        while let Some(control) = control_rx.recv().await {
            if matches!(control, AttachControl::Exited) {
                return;
            }
        }
        panic!("attach control channel closed before Exited: {context}");
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for attached client exit: {context}"));
}

#[tokio::test]
async fn move_window_last_source_session_exits_attached_client() {
    let handler = RequestHandler::new();
    let source = session_name("move-attached-source");
    let destination = session_name("move-attached-destination");
    create_session(&handler, source.as_str()).await;
    create_session(&handler, destination.as_str()).await;
    let attach_pid = 81_001;
    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(attach_pid, source.clone(), control_tx)
        .await;
    drain_attach_controls(&mut control_rx).await;

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(source.clone(), 0)),
            target: MoveWindowTarget::Window(WindowTarget::with_window(destination, 1)),
            renumber: false,
            kill_destination: false,
            detached: true,
            after: false,
            before: false,
        }))
        .await;
    assert!(matches!(response, Response::MoveWindow(_)), "{response:?}");
    assert!(handler
        .state
        .lock()
        .await
        .sessions
        .session(&source)
        .is_none());

    expect_attach_exited(&mut control_rx, "single removed source session").await;
    assert!(
        !handler
            .active_attach
            .lock()
            .await
            .by_pid
            .contains_key(&attach_pid),
        "removed source session must not leave a stale attached client"
    );
}

#[tokio::test]
async fn move_window_session_target_exits_source_and_refreshes_destination_attaches() {
    let handler = RequestHandler::new();
    let source = session_name("move-session-target-attached-source");
    let destination = session_name("move-session-target-attached-destination");
    create_session(&handler, source.as_str()).await;
    create_session(&handler, destination.as_str()).await;
    let source_pid = 81_031;
    let destination_pid = 81_032;
    let (source_tx, mut source_rx) = mpsc::unbounded_channel();
    let (destination_tx, mut destination_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(source_pid, source.clone(), source_tx)
        .await;
    handler
        .register_attach(destination_pid, destination.clone(), destination_tx)
        .await;
    drain_attach_controls(&mut source_rx).await;
    drain_attach_controls(&mut destination_rx).await;

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(source.clone(), 0)),
            target: MoveWindowTarget::Session(destination.clone()),
            renumber: false,
            kill_destination: false,
            detached: true,
            after: false,
            before: false,
        }))
        .await;
    assert!(matches!(response, Response::MoveWindow(_)), "{response:?}");

    expect_attach_exited(&mut source_rx, "removed source with session-only target").await;
    let destination_refresh = timeout(Duration::from_secs(2), destination_rx.recv())
        .await
        .expect("destination attached client is refreshed")
        .expect("destination attach channel stays open");
    let AttachControl::Switch(target) = destination_refresh else {
        panic!("expected destination Switch, got {destination_refresh:?}");
    };
    let target = target.into_target();
    assert_eq!(target.session_name, destination);
    let active_attach = handler.active_attach.lock().await;
    assert!(!active_attach.by_pid.contains_key(&source_pid));
    assert!(active_attach.by_pid.contains_key(&destination_pid));
    drop(active_attach);
    assert!(handler
        .state
        .lock()
        .await
        .sessions
        .session(&source)
        .is_none());
}

#[tokio::test]
async fn move_window_last_source_group_exits_all_attached_clients() {
    let handler = RequestHandler::new();
    let owner = session_name("move-attached-group-owner");
    let peer = session_name("move-attached-group-peer");
    let destination = session_name("move-attached-group-destination");
    create_session(&handler, owner.as_str()).await;
    create_grouped_session(&handler, peer.as_str(), &owner).await;
    create_session(&handler, destination.as_str()).await;
    let owner_pid = 81_011;
    let peer_pid = 81_012;
    let (owner_tx, mut owner_rx) = mpsc::unbounded_channel();
    let (peer_tx, mut peer_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(owner_pid, owner.clone(), owner_tx)
        .await;
    handler
        .register_attach(peer_pid, peer.clone(), peer_tx)
        .await;
    drain_attach_controls(&mut owner_rx).await;
    drain_attach_controls(&mut peer_rx).await;

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(peer.clone(), 0)),
            target: MoveWindowTarget::Window(WindowTarget::with_window(destination, 1)),
            renumber: false,
            kill_destination: false,
            detached: true,
            after: false,
            before: false,
        }))
        .await;
    assert!(matches!(response, Response::MoveWindow(_)), "{response:?}");
    {
        let state = handler.state.lock().await;
        assert!(state.sessions.session(&owner).is_none());
        assert!(state.sessions.session(&peer).is_none());
    }

    expect_attach_exited(&mut owner_rx, "removed source group owner").await;
    expect_attach_exited(&mut peer_rx, "removed source group peer").await;
    let active_attach = handler.active_attach.lock().await;
    assert!(!active_attach.by_pid.contains_key(&owner_pid));
    assert!(!active_attach.by_pid.contains_key(&peer_pid));
}

#[tokio::test]
async fn move_window_source_session_with_remaining_window_keeps_attached_client() {
    let handler = RequestHandler::new();
    let source = session_name("move-attached-surviving-source");
    let destination = session_name("move-attached-surviving-destination");
    create_session(&handler, source.as_str()).await;
    insert_window(&handler, &source, 1).await;
    create_session(&handler, destination.as_str()).await;
    let attach_pid = 81_021;
    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(attach_pid, source.clone(), control_tx)
        .await;
    drain_attach_controls(&mut control_rx).await;

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(source.clone(), 1)),
            target: MoveWindowTarget::Window(WindowTarget::with_window(destination, 1)),
            renumber: false,
            kill_destination: false,
            detached: true,
            after: false,
            before: false,
        }))
        .await;
    assert!(matches!(response, Response::MoveWindow(_)), "{response:?}");

    let refresh = timeout(Duration::from_secs(2), control_rx.recv())
        .await
        .expect("surviving source attached client is refreshed")
        .expect("surviving source attach channel stays open");
    assert!(
        matches!(refresh, AttachControl::Switch(_)),
        "surviving source should refresh instead of exit, got {refresh:?}"
    );
    assert!(handler
        .active_attach
        .lock()
        .await
        .by_pid
        .contains_key(&attach_pid));
    let state = handler.state.lock().await;
    let source_session = state
        .sessions
        .session(&source)
        .expect("source session survives with its remaining window");
    assert_eq!(
        source_session.windows().keys().copied().collect::<Vec<_>>(),
        vec![0]
    );
}

#[tokio::test]
async fn move_window_preserves_unrelated_and_grouped_peer_silence_deadlines() {
    let handler = RequestHandler::new();
    let alpha = session_name("move-silence-alpha");
    let beta = session_name("move-silence-beta");
    create_session(&handler, alpha.as_str()).await;
    insert_window(&handler, &alpha, 1).await;
    create_grouped_session(&handler, beta.as_str(), &alpha).await;
    enable_global_monitor_silence(&handler).await;

    let unrelated = WindowTarget::with_window(alpha.clone(), 0);
    let peer_source = WindowTarget::with_window(beta.clone(), 1);
    let unrelated_before = handler
        .silence_timer_snapshot_for_test(&unrelated)
        .expect("unrelated timer is armed");
    let peer_before = handler
        .silence_timer_snapshot_for_test(&peer_source)
        .expect("grouped peer timer is armed");
    let peer_identity_before = handler
        .silence_timer_identity_for_test(&peer_source)
        .expect("grouped peer timer has stable identity");

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(alpha.clone(), 1)),
            target: MoveWindowTarget::Window(WindowTarget::with_window(alpha.clone(), 3)),
            renumber: false,
            kill_destination: false,
            detached: true,
            after: false,
            before: false,
        }))
        .await;
    assert!(matches!(response, Response::MoveWindow(_)), "{response:?}");

    assert_eq!(
        handler.silence_timer_snapshot_for_test(&unrelated),
        Some(unrelated_before),
        "move-window must not rearm an unrelated window"
    );
    let peer_destination = WindowTarget::with_window(beta, 3);
    let peer_after = handler
        .silence_timer_snapshot_for_test(&peer_destination)
        .expect("grouped peer timer follows the move");
    assert_eq!(
        peer_after.1, peer_before.1,
        "the grouped peer deadline follows the moved WindowId"
    );
    assert!(peer_after.0 > peer_before.0);
    let peer_identity_after = handler
        .silence_timer_identity_for_test(&peer_destination)
        .expect("moved grouped peer keeps an identity");
    assert_eq!(
        (peer_identity_after.0, peer_identity_after.1),
        (peer_identity_before.0, peer_identity_before.1)
    );
    assert!(peer_identity_after.2 > peer_identity_before.2);
    assert_eq!(handler.silence_timer_snapshot_for_test(&peer_source), None);
}

#[tokio::test]
async fn move_window_preserves_distinct_duplicate_alias_silence_deadlines() {
    let handler = RequestHandler::new();
    let alpha = session_name("move-duplicate-silence");
    create_session(&handler, alpha.as_str()).await;
    let linked = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(alpha.clone(), 0),
            target: WindowTarget::with_window(alpha.clone(), 2),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(linked, Response::LinkWindow(_)), "{linked:?}");
    enable_global_monitor_silence(&handler).await;

    let source = WindowTarget::with_window(alpha.clone(), 0);
    let sibling = WindowTarget::with_window(alpha.clone(), 2);
    let destination = WindowTarget::with_window(alpha.clone(), 3);
    let base_deadline = tokio::time::Instant::now() + Duration::from_secs(120);
    handler.replace_silence_timer_deadline_for_test(&source, base_deadline);
    handler
        .replace_silence_timer_deadline_for_test(&sibling, base_deadline + Duration::from_secs(7));
    let source_before = handler
        .silence_timer_snapshot_for_test(&source)
        .expect("source alias timer is armed");
    let sibling_before = handler
        .silence_timer_snapshot_for_test(&sibling)
        .expect("sibling alias timer is armed");

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(source.clone()),
            target: MoveWindowTarget::Window(destination.clone()),
            renumber: false,
            kill_destination: false,
            detached: true,
            after: false,
            before: false,
        }))
        .await;
    assert!(matches!(response, Response::MoveWindow(_)), "{response:?}");

    assert_eq!(handler.silence_timer_snapshot_for_test(&source), None);
    assert_eq!(
        handler.silence_timer_snapshot_for_test(&sibling),
        Some(sibling_before),
        "the untouched duplicate alias must keep its exact timer"
    );
    let destination_after = handler
        .silence_timer_snapshot_for_test(&destination)
        .expect("moved duplicate alias timer follows its slot");
    assert_eq!(destination_after.1, source_before.1);
    assert!(destination_after.0 > source_before.0);
}

#[tokio::test]
async fn move_window_kill_duplicate_alias_preserves_source_silence_deadline() {
    let handler = RequestHandler::new();
    let alpha = session_name("move-kill-duplicate-silence");
    create_session(&handler, alpha.as_str()).await;
    let linked = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(alpha.clone(), 0),
            target: WindowTarget::with_window(alpha.clone(), 2),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(linked, Response::LinkWindow(_)), "{linked:?}");
    enable_global_monitor_silence(&handler).await;

    let source = WindowTarget::with_window(alpha.clone(), 0);
    let destination = WindowTarget::with_window(alpha.clone(), 2);
    let base_deadline = tokio::time::Instant::now() + Duration::from_secs(120);
    handler.replace_silence_timer_deadline_for_test(&source, base_deadline);
    handler.replace_silence_timer_deadline_for_test(
        &destination,
        base_deadline + Duration::from_secs(11),
    );
    let source_before = handler
        .silence_timer_snapshot_for_test(&source)
        .expect("source alias timer is armed");
    let destination_before = handler
        .silence_timer_snapshot_for_test(&destination)
        .expect("destination alias timer is armed");

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(source.clone()),
            target: MoveWindowTarget::Window(destination.clone()),
            renumber: false,
            kill_destination: true,
            detached: true,
            after: false,
            before: false,
        }))
        .await;
    assert!(matches!(response, Response::MoveWindow(_)), "{response:?}");

    assert_eq!(handler.silence_timer_snapshot_for_test(&source), None);
    let destination_after = handler
        .silence_timer_snapshot_for_test(&destination)
        .expect("moved source timer replaces the killed alias timer");
    assert_eq!(destination_after.1, source_before.1);
    assert_ne!(destination_after.1, destination_before.1);
    assert!(destination_after.0 > source_before.0.max(destination_before.0));
}

#[tokio::test]
async fn move_window_kill_duplicate_alias_moves_group_peer_alerts_by_occurrence() {
    let handler = RequestHandler::new();
    let owner = session_name("move-alert-duplicate-owner");
    let peer = session_name("move-alert-duplicate-peer");
    create_session(&handler, owner.as_str()).await;
    link_duplicate_window(&handler, &owner, 0, 2).await;
    create_grouped_session(&handler, peer.as_str(), &owner).await;
    {
        let mut state = handler.state.lock().await;
        let peer_session = state
            .sessions
            .session_mut(&peer)
            .expect("group peer exists");
        assert!(peer_session.add_winlink_alert_flags(0, rmux_core::WINLINK_BELL));
        assert!(peer_session.add_winlink_alert_flags(2, rmux_core::WINLINK_ACTIVITY));
    }

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(owner.clone(), 0)),
            target: MoveWindowTarget::Window(WindowTarget::with_window(owner, 2)),
            renumber: false,
            kill_destination: true,
            detached: true,
            after: false,
            before: false,
        }))
        .await;
    assert!(matches!(response, Response::MoveWindow(_)), "{response:?}");

    let state = handler.state.lock().await;
    let peer_session = state.sessions.session(&peer).expect("group peer survives");
    assert!(peer_session.window_at(0).is_none());
    assert_eq!(
        peer_session.winlink_alert_flags(2),
        rmux_core::WINLINK_BELL,
        "the moved source occurrence must replace the killed destination occurrence"
    );
}

#[tokio::test]
async fn move_window_reindex_remaps_group_peer_duplicate_alias_alerts_by_occurrence() {
    let handler = RequestHandler::new();
    let owner = session_name("reindex-alert-duplicate-owner");
    let peer = session_name("reindex-alert-duplicate-peer");
    create_session(&handler, owner.as_str()).await;
    let moved = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(owner.clone(), 0)),
            target: MoveWindowTarget::Window(WindowTarget::with_window(owner.clone(), 1)),
            renumber: false,
            kill_destination: false,
            detached: true,
            after: false,
            before: false,
        }))
        .await;
    assert!(matches!(moved, Response::MoveWindow(_)), "{moved:?}");
    link_duplicate_window(&handler, &owner, 1, 2).await;
    create_grouped_session(&handler, peer.as_str(), &owner).await;
    {
        let mut state = handler.state.lock().await;
        let peer_session = state
            .sessions
            .session_mut(&peer)
            .expect("group peer exists");
        assert!(peer_session.add_winlink_alert_flags(1, rmux_core::WINLINK_ACTIVITY));
        assert!(peer_session.add_winlink_alert_flags(2, rmux_core::WINLINK_BELL));
    }

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: None,
            target: MoveWindowTarget::Session(owner),
            renumber: true,
            kill_destination: false,
            detached: true,
            after: false,
            before: false,
        }))
        .await;
    assert!(matches!(response, Response::MoveWindow(_)), "{response:?}");

    let state = handler.state.lock().await;
    let peer_session = state.sessions.session(&peer).expect("group peer survives");
    assert_eq!(
        peer_session.winlink_alert_flags(0),
        rmux_core::WINLINK_ACTIVITY
    );
    assert_eq!(peer_session.winlink_alert_flags(1), rmux_core::WINLINK_BELL);
    assert!(peer_session.window_at(2).is_none());
}

#[tokio::test]
async fn move_window_relative_remaps_group_peer_duplicate_alias_alerts_by_occurrence() {
    let handler = RequestHandler::new();
    let owner = session_name("relative-alert-duplicate-owner");
    let peer = session_name("relative-alert-duplicate-peer");
    create_session(&handler, owner.as_str()).await;
    link_duplicate_window(&handler, &owner, 0, 1).await;
    create_grouped_session(&handler, peer.as_str(), &owner).await;
    {
        let mut state = handler.state.lock().await;
        let peer_session = state
            .sessions
            .session_mut(&peer)
            .expect("group peer exists");
        peer_session
            .select_window(1)
            .expect("peer selects the second winlink");
        peer_session
            .select_window(0)
            .expect("peer returns to the first winlink");
        assert!(peer_session.add_winlink_alert_flags(0, rmux_core::WINLINK_ACTIVITY));
        assert!(peer_session.add_winlink_alert_flags(1, rmux_core::WINLINK_BELL));
    }

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(owner.clone(), 1)),
            target: MoveWindowTarget::Window(WindowTarget::with_window(owner, 0)),
            renumber: false,
            kill_destination: false,
            detached: true,
            after: false,
            before: true,
        }))
        .await;
    assert!(matches!(response, Response::MoveWindow(_)), "{response:?}");

    let state = handler.state.lock().await;
    let peer_session = state.sessions.session(&peer).expect("group peer survives");
    assert_eq!(peer_session.active_window_index(), 0);
    assert_eq!(peer_session.last_window_index(), Some(1));
    assert_eq!(peer_session.winlink_alert_flags(0), rmux_core::WINLINK_BELL);
    assert_eq!(
        peer_session.winlink_alert_flags(1),
        rmux_core::WINLINK_ACTIVITY
    );
}

#[tokio::test]
async fn move_window_kill_duplicate_alias_emits_only_source_unlinked() {
    let handler = RequestHandler::new();
    let alpha = session_name("move-kill-duplicate-lifecycle");
    create_session(&handler, alpha.as_str()).await;
    let source = WindowTarget::with_window(alpha.clone(), 0);
    let destination = WindowTarget::with_window(alpha.clone(), 2);
    let linked = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: source.clone(),
            target: destination.clone(),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(linked, Response::LinkWindow(_)), "{linked:?}");
    let original_window_id = {
        let state = handler.state.lock().await;
        let source_window_id = state
            .sessions
            .session(&alpha)
            .and_then(|session| session.window_at(0))
            .map(rmux_core::Window::id)
            .expect("source alias exists");
        assert_eq!(
            state
                .sessions
                .session(&alpha)
                .and_then(|session| session.window_at(2))
                .map(rmux_core::Window::id),
            Some(source_window_id)
        );
        assert_eq!(state.window_link_count(&alpha, 0), 2);
        source_window_id
    };
    let mut events = handler.subscribe_lifecycle_events();

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(source.clone()),
            target: MoveWindowTarget::Window(destination.clone()),
            renumber: false,
            kill_destination: true,
            detached: true,
            after: false,
            before: false,
        }))
        .await;
    assert!(matches!(response, Response::MoveWindow(_)), "{response:?}");

    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("session survives");
    assert!(session.window_at(0).is_none());
    assert_eq!(
        session.window_at(2).map(rmux_core::Window::id),
        Some(original_window_id)
    );
    assert_eq!(state.window_link_count(&alpha, 2), 1);
    drop(state);

    let mut linked_count = 0;
    let mut unlinked_count = 0;
    while let Ok(event) = events.try_recv() {
        match event.event {
            rmux_core::LifecycleEvent::WindowLinked { .. } => linked_count += 1,
            rmux_core::LifecycleEvent::WindowUnlinked { window_id, .. } => {
                assert_eq!(window_id, Some(original_window_id.as_u32()));
                unlinked_count += 1;
            }
            _ => {}
        }
    }
    assert_eq!(unlinked_count, 1);
    assert_eq!(
        linked_count, 0,
        "the destination already linked this WindowId before move-window -k"
    );
}

#[tokio::test]
async fn move_window_across_sessions_preserves_silence_deadline_and_identity() {
    let handler = RequestHandler::new();
    let alpha = session_name("move-cross-silence-alpha");
    let beta = session_name("move-cross-silence-beta");
    create_session(&handler, alpha.as_str()).await;
    create_session(&handler, beta.as_str()).await;
    insert_window(&handler, &alpha, 1).await;
    enable_global_monitor_silence(&handler).await;

    let source = WindowTarget::with_window(alpha.clone(), 1);
    let destination = WindowTarget::with_window(beta.clone(), 1);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
    handler.replace_silence_timer_deadline_for_test(&source, deadline);
    let source_before = handler
        .silence_timer_snapshot_for_test(&source)
        .expect("cross-session source timer is armed");
    let source_identity = handler
        .silence_timer_identity_for_test(&source)
        .expect("cross-session source identity exists");
    let destination_session_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&beta)
            .expect("destination session exists")
            .id()
    };

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(source.clone()),
            target: MoveWindowTarget::Window(destination.clone()),
            renumber: false,
            kill_destination: false,
            detached: true,
            after: false,
            before: false,
        }))
        .await;
    assert!(matches!(response, Response::MoveWindow(_)), "{response:?}");

    assert_eq!(handler.silence_timer_snapshot_for_test(&source), None);
    let destination_after = handler
        .silence_timer_snapshot_for_test(&destination)
        .expect("cross-session destination timer exists");
    assert_eq!(destination_after.1, source_before.1);
    assert!(destination_after.0 > source_before.0);
    let destination_identity = handler
        .silence_timer_identity_for_test(&destination)
        .expect("cross-session destination identity exists");
    assert_eq!(destination_identity.0, destination_session_id);
    assert_eq!(destination_identity.1, source_identity.1);
}

#[tokio::test]
async fn move_window_does_not_rearm_expired_unrelated_silence_timer() {
    let handler = RequestHandler::new();
    let alpha = session_name("move-expired-silence");
    create_session(&handler, alpha.as_str()).await;
    insert_window(&handler, &alpha, 1).await;
    enable_global_monitor_silence(&handler).await;

    let expired = WindowTarget::with_window(alpha.clone(), 0);
    let source = WindowTarget::with_window(alpha.clone(), 1);
    let destination = WindowTarget::with_window(alpha.clone(), 2);
    let source_deadline = tokio::time::Instant::now() + Duration::from_secs(120);
    handler.replace_silence_timer_deadline_for_test(&source, source_deadline);
    let source_before = handler
        .silence_timer_snapshot_for_test(&source)
        .expect("move source timer is armed");
    let expired_identity = handler
        .silence_timer_identity_for_test(&expired)
        .expect("timer to expire is armed");
    handler
        .expire_silence_timer_for_test(
            expired.clone(),
            expired_identity.0,
            expired_identity.1,
            expired_identity.2,
        )
        .await;
    assert_eq!(handler.silence_timer_snapshot_for_test(&expired), None);

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(source.clone()),
            target: MoveWindowTarget::Window(destination.clone()),
            renumber: false,
            kill_destination: false,
            detached: true,
            after: false,
            before: false,
        }))
        .await;
    assert!(matches!(response, Response::MoveWindow(_)), "{response:?}");

    assert_eq!(
        handler.silence_timer_snapshot_for_test(&expired),
        None,
        "a structural mutation must not restart an already-fired timer"
    );
    assert_eq!(handler.silence_timer_snapshot_for_test(&source), None);
    let destination_after = handler
        .silence_timer_snapshot_for_test(&destination)
        .expect("moved timer follows the source window");
    assert_eq!(destination_after.1, source_before.1);
}

#[tokio::test]
async fn move_window_across_sessions_arms_timer_when_monitor_silence_becomes_nonzero() {
    let handler = RequestHandler::new();
    let alpha = session_name("move-cross-monitor-zero");
    let beta = session_name("move-cross-monitor-sixty");
    create_session(&handler, alpha.as_str()).await;
    create_session(&handler, beta.as_str()).await;
    insert_window(&handler, &alpha, 1).await;
    set_session_monitor_silence(&handler, beta.clone(), "60").await;

    let source = WindowTarget::with_window(alpha.clone(), 1);
    let destination = WindowTarget::with_window(beta.clone(), 1);
    let unrelated_destination = WindowTarget::with_window(beta.clone(), 0);
    assert_eq!(handler.silence_timer_snapshot_for_test(&source), None);
    let unrelated_before = handler
        .silence_timer_snapshot_for_test(&unrelated_destination)
        .expect("existing destination timer is armed");

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(source.clone()),
            target: MoveWindowTarget::Window(destination.clone()),
            renumber: false,
            kill_destination: false,
            detached: true,
            after: false,
            before: false,
        }))
        .await;
    assert!(matches!(response, Response::MoveWindow(_)), "{response:?}");

    assert_eq!(handler.silence_timer_snapshot_for_test(&source), None);
    assert!(
        handler
            .silence_timer_snapshot_for_test(&destination)
            .is_some(),
        "the destination session's nonzero option must arm the moved window"
    );
    assert_eq!(
        handler.silence_timer_snapshot_for_test(&unrelated_destination),
        Some(unrelated_before),
        "arming the moved window must not restart an existing destination timer"
    );
}

async fn assert_cross_session_move_preserves_expired_silence_alert(kill_destination: bool) {
    let label = if kill_destination { "kill" } else { "empty" };
    let handler = RequestHandler::new();
    let alpha = session_name(&format!("move-alert-{label}-alpha"));
    let beta = session_name(&format!("move-alert-{label}-beta"));
    create_session(&handler, alpha.as_str()).await;
    create_session(&handler, beta.as_str()).await;
    insert_window(&handler, &alpha, 1).await;
    enable_global_monitor_silence(&handler).await;

    let source = WindowTarget::with_window(alpha.clone(), 1);
    let destination_index = if kill_destination { 0 } else { 1 };
    let destination = WindowTarget::with_window(beta.clone(), destination_index);
    if kill_destination {
        assert!(
            handler
                .silence_timer_snapshot_for_test(&destination)
                .is_some(),
            "the occupied destination starts with its own timer"
        );
    } else {
        assert_eq!(handler.silence_timer_snapshot_for_test(&destination), None);
    }
    let source_identity = handler
        .silence_timer_identity_for_test(&source)
        .expect("source timer identity exists before expiry");
    handler
        .expire_silence_timer_for_test(
            source.clone(),
            source_identity.0,
            source_identity.1,
            source_identity.2,
        )
        .await;
    assert_eq!(handler.silence_timer_snapshot_for_test(&source), None);
    {
        let state = handler.state.lock().await;
        assert!(
            state
                .sessions
                .session(&alpha)
                .expect("source session exists before move")
                .winlink_alert_flags(source.window_index())
                .contains(rmux_core::WINLINK_SILENCE),
            "the real timer expiry marks the source winlink silent"
        );
    }

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(source.clone()),
            target: MoveWindowTarget::Window(destination.clone()),
            renumber: false,
            kill_destination,
            detached: true,
            after: false,
            before: false,
        }))
        .await;
    assert!(matches!(response, Response::MoveWindow(_)), "{response:?}");

    {
        let state = handler.state.lock().await;
        let source_session = state
            .sessions
            .session(&alpha)
            .expect("source session survives the move");
        assert!(source_session.window_at(source.window_index()).is_none());
        assert!(source_session
            .winlink_alert_flags(source.window_index())
            .is_empty());
        let destination_session = state
            .sessions
            .session(&beta)
            .expect("destination session survives the move");
        assert_eq!(
            destination_session
                .window_at(destination.window_index())
                .expect("moved window exists at destination")
                .id(),
            source_identity.1,
        );
        assert!(
            destination_session
                .winlink_alert_flags(destination.window_index())
                .contains(rmux_core::WINLINK_SILENCE),
            "the silence flag follows the moved WindowId"
        );
    }
    assert_eq!(handler.silence_timer_snapshot_for_test(&source), None);
    assert_eq!(
        handler.silence_timer_snapshot_for_test(&destination),
        None,
        "moving an already-expired window must not arm a second timer"
    );
}

#[tokio::test]
async fn move_window_across_sessions_preserves_expired_silence_alert_for_empty_and_killed_slots() {
    assert_cross_session_move_preserves_expired_silence_alert(false).await;
    assert_cross_session_move_preserves_expired_silence_alert(true).await;
}

#[tokio::test]
async fn move_window_across_sessions_migrates_the_terminal_ownership_map() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    create_session(&handler, "alpha").await;
    create_session(&handler, "beta").await;
    insert_window(&handler, &alpha, 1).await;

    let moved_pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&alpha)
            .expect("alpha should exist")
            .window_at(1)
            .expect("window 1 should exist")
            .pane(0)
            .expect("pane 0 should exist")
            .id()
    };

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(alpha.clone(), 1)),
            target: MoveWindowTarget::Window(WindowTarget::with_window(beta.clone(), 4)),
            renumber: false,
            kill_destination: false,
            detached: true,
            after: false,
            before: false,
        }))
        .await;

    assert_eq!(
        response,
        Response::MoveWindow(rmux_proto::MoveWindowResponse {
            session_name: beta.clone(),
            target: Some(WindowTarget::with_window(beta.clone(), 4)),
        })
    );

    let state = handler.state.lock().await;
    let alpha_session = state.sessions.session(&alpha).expect("alpha should exist");
    let beta_session = state.sessions.session(&beta).expect("beta should exist");
    assert_eq!(
        alpha_session.windows().keys().copied().collect::<Vec<_>>(),
        vec![0]
    );
    assert_eq!(
        beta_session.windows().keys().copied().collect::<Vec<_>>(),
        vec![0, 4]
    );
    assert_eq!(
        beta_session
            .window_at(4)
            .and_then(|window| window.pane(0))
            .map(|pane| pane.id()),
        Some(moved_pane_id)
    );
    state
        .pane_profile_in_window(&beta, 4, 0)
        .expect("moved pane terminal should exist in the destination session");
    assert_eq!(
        state.pane_profile_in_window(&alpha, 1, 0).unwrap_err(),
        rmux_proto::RmuxError::invalid_target("alpha:1", "window index does not exist in session")
    );
}

#[tokio::test]
async fn move_window_within_session_moves_linked_slot_metadata() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    create_session(&handler, "alpha").await;
    create_session(&handler, "beta").await;

    let link = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(alpha.clone(), 0),
            target: WindowTarget::with_window(beta.clone(), 1),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(link, Response::LinkWindow(_)));

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(alpha.clone(), 0)),
            target: MoveWindowTarget::Window(WindowTarget::with_window(alpha.clone(), 2)),
            renumber: false,
            kill_destination: false,
            detached: true,
            after: false,
            before: false,
        }))
        .await;
    assert_eq!(
        response,
        Response::MoveWindow(rmux_proto::MoveWindowResponse {
            session_name: alpha.clone(),
            target: Some(WindowTarget::with_window(alpha.clone(), 2)),
        })
    );

    {
        let state = handler.state.lock().await;
        assert_eq!(state.window_link_count(&alpha, 2), 2);
        assert_eq!(state.window_link_count(&beta, 1), 2);
        assert_eq!(state.window_link_count(&alpha, 0), 1);
        assert_eq!(
            state.window_linked_sessions_list(&beta, 1),
            vec![alpha.clone(), beta.clone()]
        );
    }

    let rename = handler
        .handle(Request::RenameWindow(RenameWindowRequest {
            target: WindowTarget::with_window(beta.clone(), 1),
            name: "logs".to_owned(),
        }))
        .await;
    assert!(matches!(rename, Response::RenameWindow(_)));

    let state = handler.state.lock().await;
    assert_eq!(
        state
            .sessions
            .session(&alpha)
            .and_then(|session| session.window_at(2))
            .and_then(|window| window.name()),
        Some("logs")
    );
    assert_eq!(
        state
            .sessions
            .session(&beta)
            .and_then(|session| session.window_at(1))
            .and_then(|window| window.name()),
        Some("logs")
    );
}

#[tokio::test]
async fn move_window_from_group_peer_moves_runtime_state_and_removes_empty_group() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    let gamma = session_name("gamma");
    create_session(&handler, "alpha").await;
    create_grouped_session(&handler, "beta", &alpha).await;
    create_session(&handler, "gamma").await;

    let moved_pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&beta)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0))
            .map(|pane| pane.id())
            .expect("grouped pane should exist")
    };

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(beta.clone(), 0)),
            target: MoveWindowTarget::Window(WindowTarget::with_window(gamma.clone(), 1)),
            renumber: false,
            kill_destination: false,
            detached: true,
            after: false,
            before: false,
        }))
        .await;
    assert_eq!(
        response,
        Response::MoveWindow(rmux_proto::MoveWindowResponse {
            session_name: gamma.clone(),
            target: Some(WindowTarget::with_window(gamma.clone(), 1)),
        })
    );

    let state = handler.state.lock().await;
    assert!(state.sessions.session(&alpha).is_none());
    assert!(state.sessions.session(&beta).is_none());
    assert_eq!(
        state
            .sessions
            .session(&gamma)
            .and_then(|session| session.window_at(1))
            .and_then(|window| window.pane(0))
            .map(|pane| pane.id()),
        Some(moved_pane_id)
    );
    state
        .pane_profile_in_window(&gamma, 1, 0)
        .expect("moved group pane terminal should live in the destination session");
}

#[tokio::test]
async fn move_window_rejects_cross_session_move_within_same_session_group() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    create_session(&handler, "alpha").await;
    create_grouped_session(&handler, "beta", &alpha).await;

    let shared_pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&alpha)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0))
            .map(|pane| pane.id())
            .expect("grouped pane should exist before move-window")
    };

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(beta.clone(), 0)),
            target: MoveWindowTarget::Window(WindowTarget::with_window(alpha.clone(), 5)),
            renumber: false,
            kill_destination: false,
            detached: true,
            after: false,
            before: false,
        }))
        .await;

    assert!(
        matches!(&response, Response::Error(error) if error.error.to_string().contains("sessions are grouped")),
        "expected grouped-session rejection, got {response:?}"
    );

    let state = handler.state.lock().await;
    let alpha_session = state.sessions.session(&alpha).expect("alpha should remain");
    let beta_session = state.sessions.session(&beta).expect("beta should remain");
    assert_eq!(
        alpha_session.windows().keys().copied().collect::<Vec<_>>(),
        vec![0]
    );
    assert_eq!(
        beta_session.windows().keys().copied().collect::<Vec<_>>(),
        vec![0]
    );
    assert_eq!(alpha_session.pane_id_in_window(0, 0), Some(shared_pane_id));
    assert_eq!(beta_session.pane_id_in_window(0, 0), Some(shared_pane_id));
    state
        .pane_profile_in_window(&alpha, 0, 0)
        .expect("alpha pane terminal should remain");
    state
        .pane_profile_in_window(&beta, 0, 0)
        .expect("beta grouped pane terminal should remain");
}

#[tokio::test]
async fn move_window_relative_rejects_cross_session_move_within_same_session_group() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    create_session(&handler, "alpha").await;
    create_grouped_session(&handler, "beta", &alpha).await;

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(beta.clone(), 0)),
            target: MoveWindowTarget::Window(WindowTarget::with_window(alpha.clone(), 0)),
            renumber: false,
            kill_destination: false,
            detached: true,
            after: true,
            before: false,
        }))
        .await;

    assert!(
        matches!(&response, Response::Error(error) if error.error.to_string().contains("sessions are grouped")),
        "expected grouped-session rejection, got {response:?}"
    );

    let state = handler.state.lock().await;
    let alpha_session = state.sessions.session(&alpha).expect("alpha should remain");
    let beta_session = state.sessions.session(&beta).expect("beta should remain");
    assert_eq!(
        alpha_session.windows().keys().copied().collect::<Vec<_>>(),
        vec![0]
    );
    assert_eq!(
        beta_session.windows().keys().copied().collect::<Vec<_>>(),
        vec![0]
    );
}

#[tokio::test]
async fn move_window_from_group_peer_linked_source_removes_empty_group() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    let gamma = session_name("gamma");
    let delta = session_name("delta");
    create_session(&handler, "alpha").await;
    create_grouped_session(&handler, "beta", &alpha).await;
    create_session(&handler, "gamma").await;
    create_session(&handler, "delta").await;

    let linked_pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&beta)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0))
            .map(|pane| pane.id())
            .expect("grouped linked pane should exist")
    };

    let link = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(alpha.clone(), 0),
            target: WindowTarget::with_window(gamma.clone(), 1),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(link, Response::LinkWindow(_)));

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(beta.clone(), 0)),
            target: MoveWindowTarget::Window(WindowTarget::with_window(delta.clone(), 1)),
            renumber: false,
            kill_destination: false,
            detached: true,
            after: false,
            before: false,
        }))
        .await;
    assert_eq!(
        response,
        Response::MoveWindow(rmux_proto::MoveWindowResponse {
            session_name: delta.clone(),
            target: Some(WindowTarget::with_window(delta.clone(), 1)),
        })
    );

    let state = handler.state.lock().await;
    assert!(state.sessions.session(&alpha).is_none());
    assert!(state.sessions.session(&beta).is_none());
    assert_eq!(
        state
            .sessions
            .session(&delta)
            .and_then(|session| session.window_at(1))
            .and_then(|window| window.pane(0))
            .map(|pane| pane.id()),
        Some(linked_pane_id)
    );
    assert_eq!(
        state
            .sessions
            .session(&gamma)
            .and_then(|session| session.window_at(1))
            .and_then(|window| window.pane(0))
            .map(|pane| pane.id()),
        Some(linked_pane_id)
    );
    state
        .pane_profile_in_window(&delta, 1, 0)
        .expect("moved linked pane should live in the target runtime");
    state
        .pane_profile_in_window(&gamma, 1, 0)
        .expect("surviving linked peer should keep runtime access");
    assert_eq!(state.window_link_count(&delta, 1), 2);
    assert_eq!(state.window_link_count(&gamma, 1), 2);
}

#[tokio::test]
async fn move_window_kill_destination_preserves_surviving_linked_window_runtime() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let gamma = session_name("gamma");
    let delta = session_name("delta");
    create_session(&handler, "alpha").await;
    create_session(&handler, "gamma").await;
    create_session(&handler, "delta").await;

    let (source_pane_id, linked_pane_id) = {
        let state = handler.state.lock().await;
        (
            state
                .sessions
                .session(&alpha)
                .and_then(|session| session.window_at(0))
                .and_then(|window| window.pane(0))
                .map(|pane| pane.id())
                .expect("alpha pane should exist"),
            state
                .sessions
                .session(&gamma)
                .and_then(|session| session.window_at(0))
                .and_then(|window| window.pane(0))
                .map(|pane| pane.id())
                .expect("gamma pane should exist"),
        )
    };

    let link = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(gamma.clone(), 0),
            target: WindowTarget::with_window(delta.clone(), 1),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(link, Response::LinkWindow(_)));

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(alpha.clone(), 0)),
            target: MoveWindowTarget::Window(WindowTarget::with_window(gamma.clone(), 0)),
            renumber: false,
            kill_destination: true,
            detached: true,
            after: false,
            before: false,
        }))
        .await;
    assert_eq!(
        response,
        Response::MoveWindow(rmux_proto::MoveWindowResponse {
            session_name: gamma.clone(),
            target: Some(WindowTarget::with_window(gamma.clone(), 0)),
        })
    );

    let state = handler.state.lock().await;
    assert!(state.sessions.session(&alpha).is_none());
    assert_eq!(
        state
            .sessions
            .session(&gamma)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0))
            .map(|pane| pane.id()),
        Some(source_pane_id)
    );
    assert_eq!(
        state
            .sessions
            .session(&delta)
            .and_then(|session| session.window_at(1))
            .and_then(|window| window.pane(0))
            .map(|pane| pane.id()),
        Some(linked_pane_id)
    );
    state
        .pane_profile_in_window(&gamma, 0, 0)
        .expect("moved source pane should live in gamma");
    state
        .pane_profile_in_window(&delta, 1, 0)
        .expect("surviving linked pane should keep a runtime after overwrite");
    assert_eq!(state.window_link_count(&gamma, 0), 1);
    assert_eq!(state.window_link_count(&delta, 1), 1);
}

#[tokio::test]
async fn move_window_within_session_kill_destination_preserves_surviving_linked_runtime() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    create_session(&handler, "alpha").await;
    create_session(&handler, "beta").await;
    insert_window(&handler, &alpha, 2).await;

    let (source_pane_id, linked_pane_id) = {
        let state = handler.state.lock().await;
        (
            state
                .sessions
                .session(&alpha)
                .and_then(|session| session.window_at(2))
                .and_then(|window| window.pane(0))
                .map(|pane| pane.id())
                .expect("alpha:2 pane should exist"),
            state
                .sessions
                .session(&alpha)
                .and_then(|session| session.window_at(0))
                .and_then(|window| window.pane(0))
                .map(|pane| pane.id())
                .expect("alpha:0 pane should exist"),
        )
    };

    let link = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(alpha.clone(), 0),
            target: WindowTarget::with_window(beta.clone(), 1),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(link, Response::LinkWindow(_)));

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(alpha.clone(), 2)),
            target: MoveWindowTarget::Window(WindowTarget::with_window(alpha.clone(), 0)),
            renumber: false,
            kill_destination: true,
            detached: true,
            after: false,
            before: false,
        }))
        .await;
    assert_eq!(
        response,
        Response::MoveWindow(rmux_proto::MoveWindowResponse {
            session_name: alpha.clone(),
            target: Some(WindowTarget::with_window(alpha.clone(), 0)),
        })
    );

    let state = handler.state.lock().await;
    assert_eq!(
        state
            .sessions
            .session(&alpha)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0))
            .map(|pane| pane.id()),
        Some(source_pane_id)
    );
    assert_eq!(
        state
            .sessions
            .session(&beta)
            .and_then(|session| session.window_at(1))
            .and_then(|window| window.pane(0))
            .map(|pane| pane.id()),
        Some(linked_pane_id)
    );
    state
        .pane_profile_in_window(&alpha, 0, 0)
        .expect("moved source pane should remain available");
    state
        .pane_profile_in_window(&beta, 1, 0)
        .expect("surviving linked peer should keep its runtime");
    assert_eq!(state.window_link_count(&alpha, 0), 1);
    assert_eq!(state.window_link_count(&beta, 1), 1);
}

#[tokio::test]
async fn move_window_within_session_restores_the_killed_destination_when_resize_fails() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;
    insert_window(&handler, &alpha, 1).await;

    let (source_pane_id, destination_pane_id) = {
        let state = handler.state.lock().await;
        let session = state.sessions.session(&alpha).expect("alpha should exist");
        (
            session
                .window_at(0)
                .and_then(|window| window.pane(0))
                .map(|pane| pane.id())
                .expect("window 0 pane should exist"),
            session
                .window_at(1)
                .and_then(|window| window.pane(0))
                .map(|pane| pane.id())
                .expect("window 1 pane should exist"),
        )
    };

    {
        let mut state = handler.state.lock().await;
        state.fail_next_resize_for_test();
    }

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(alpha.clone(), 0)),
            target: MoveWindowTarget::Window(WindowTarget::with_window(alpha.clone(), 1)),
            renumber: false,
            kill_destination: true,
            detached: true,
            after: false,
            before: false,
        }))
        .await;

    assert_eq!(
        response,
        Response::Error(rmux_proto::ErrorResponse {
            error: rmux_proto::RmuxError::Server(
                "injected pane terminal resize failure".to_owned()
            ),
        })
    );

    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("alpha should exist");
    assert_eq!(
        session.windows().keys().copied().collect::<Vec<_>>(),
        vec![0, 1]
    );
    assert_eq!(session.pane_id_in_window(0, 0), Some(source_pane_id));
    assert_eq!(session.pane_id_in_window(1, 0), Some(destination_pane_id));
    state
        .pane_profile_in_window(&alpha, 0, 0)
        .expect("source pane terminal should be restored");
    state
        .pane_profile_in_window(&alpha, 1, 0)
        .expect("destination pane terminal should be restored");
}

#[tokio::test]
async fn move_window_reindex_compacts_sparse_window_indices() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;
    insert_window(&handler, &alpha, 3).await;
    insert_window(&handler, &alpha, 7).await;

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: None,
            target: MoveWindowTarget::Session(alpha.clone()),
            renumber: true,
            kill_destination: false,
            detached: true,
            after: false,
            before: false,
        }))
        .await;

    assert_eq!(
        response,
        Response::MoveWindow(rmux_proto::MoveWindowResponse {
            session_name: alpha.clone(),
            target: None,
        })
    );

    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("alpha should exist");
    assert_eq!(
        session.windows().keys().copied().collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
}

#[tokio::test]
async fn move_window_reindex_with_source_ignores_source_and_renumbers_target_session() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    create_session(&handler, "alpha").await;
    create_session(&handler, "beta").await;
    insert_window(&handler, &alpha, 3).await;
    insert_window(&handler, &beta, 4).await;

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(alpha.clone(), 3)),
            target: MoveWindowTarget::Session(beta.clone()),
            renumber: true,
            kill_destination: false,
            detached: true,
            after: false,
            before: false,
        }))
        .await;

    assert_eq!(
        response,
        Response::MoveWindow(rmux_proto::MoveWindowResponse {
            session_name: beta.clone(),
            target: None,
        })
    );

    let state = handler.state.lock().await;
    let alpha_session = state.sessions.session(&alpha).expect("alpha should exist");
    assert!(alpha_session.window_at(3).is_some());
    let beta_session = state.sessions.session(&beta).expect("beta should exist");
    assert_eq!(
        beta_session.windows().keys().copied().collect::<Vec<_>>(),
        vec![0, 1]
    );
}

#[tokio::test]
async fn move_window_reindex_ignores_source_in_target_without_window_lifecycle_events() {
    let handler = RequestHandler::new();
    let alpha = session_name("reindex-source-in-target");
    create_session(&handler, alpha.as_str()).await;
    insert_window(&handler, &alpha, 3).await;
    insert_window(&handler, &alpha, 7).await;
    let mut events = handler.subscribe_lifecycle_events();

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(alpha.clone(), 7)),
            target: MoveWindowTarget::Session(alpha.clone()),
            renumber: true,
            kill_destination: false,
            detached: true,
            after: false,
            before: false,
        }))
        .await;

    assert_eq!(
        response,
        Response::MoveWindow(rmux_proto::MoveWindowResponse {
            session_name: alpha.clone(),
            target: None,
        })
    );
    let state = handler.state.lock().await;
    assert_eq!(
        state
            .sessions
            .session(&alpha)
            .expect("target session survives reindex")
            .windows()
            .keys()
            .copied()
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    drop(state);

    while let Ok(event) = events.try_recv() {
        assert!(
            !matches!(
                event.event,
                rmux_core::LifecycleEvent::WindowLinked { .. }
                    | rmux_core::LifecycleEvent::WindowUnlinked { .. }
            ),
            "move-window -r must not synthesize window lifecycle events: {event:?}"
        );
    }
}

#[tokio::test]
async fn move_window_reindex_with_window_target_renumbers_target_session() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;
    insert_window(&handler, &alpha, 5).await;
    insert_window(&handler, &alpha, 9).await;

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: None,
            target: MoveWindowTarget::Window(WindowTarget::with_window(alpha.clone(), 9)),
            renumber: true,
            kill_destination: false,
            detached: true,
            after: false,
            before: false,
        }))
        .await;

    assert_eq!(
        response,
        Response::MoveWindow(rmux_proto::MoveWindowResponse {
            session_name: alpha.clone(),
            target: None,
        })
    );

    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("alpha should exist");
    assert_eq!(
        session.windows().keys().copied().collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
}

#[tokio::test]
async fn move_window_reindex_with_source_and_window_target_ignores_source() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    create_session(&handler, "alpha").await;
    create_session(&handler, "beta").await;
    insert_window(&handler, &alpha, 2).await;
    insert_window(&handler, &alpha, 5).await;
    insert_window(&handler, &beta, 4).await;

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(alpha.clone(), 5)),
            target: MoveWindowTarget::Window(WindowTarget::with_window(beta.clone(), 4)),
            renumber: true,
            kill_destination: false,
            detached: true,
            after: false,
            before: false,
        }))
        .await;

    assert_eq!(
        response,
        Response::MoveWindow(rmux_proto::MoveWindowResponse {
            session_name: beta.clone(),
            target: None,
        })
    );

    let state = handler.state.lock().await;
    let alpha_session = state.sessions.session(&alpha).expect("alpha should exist");
    assert_eq!(
        alpha_session.windows().keys().copied().collect::<Vec<_>>(),
        vec![0, 2, 5]
    );
    let beta_session = state.sessions.session(&beta).expect("beta should exist");
    assert_eq!(
        beta_session.windows().keys().copied().collect::<Vec<_>>(),
        vec![0, 1]
    );
}

#[tokio::test]
async fn move_window_after_source_already_after_target_matches_tmux_gap_shape() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;
    insert_window(&handler, &alpha, 1).await;
    insert_window(&handler, &alpha, 2).await;

    let (source_pane_id, trailing_pane_id) = {
        let state = handler.state.lock().await;
        let session = state.sessions.session(&alpha).expect("alpha should exist");
        (
            session
                .pane_id_in_window(1, 0)
                .expect("source pane should exist"),
            session
                .pane_id_in_window(2, 0)
                .expect("trailing pane should exist"),
        )
    };

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(alpha.clone(), 1)),
            target: MoveWindowTarget::Window(WindowTarget::with_window(alpha.clone(), 0)),
            renumber: false,
            kill_destination: false,
            detached: false,
            after: true,
            before: false,
        }))
        .await;

    assert_eq!(
        response,
        Response::MoveWindow(rmux_proto::MoveWindowResponse {
            session_name: alpha.clone(),
            target: Some(WindowTarget::with_window(alpha.clone(), 1)),
        })
    );

    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("alpha should exist");
    assert_eq!(
        session.windows().keys().copied().collect::<Vec<_>>(),
        vec![0, 1, 3]
    );
    assert_eq!(session.pane_id_in_window(1, 0), Some(source_pane_id));
    assert_eq!(session.pane_id_in_window(3, 0), Some(trailing_pane_id));
    assert_eq!(session.active_window_index(), 1);
}

#[tokio::test]
async fn move_window_before_source_is_target_matches_tmux_gap_shape() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;
    insert_window(&handler, &alpha, 1).await;
    insert_window(&handler, &alpha, 2).await;

    let (source_pane_id, next_pane_id, trailing_pane_id) = {
        let state = handler.state.lock().await;
        let session = state.sessions.session(&alpha).expect("alpha should exist");
        (
            session
                .pane_id_in_window(0, 0)
                .expect("source pane should exist"),
            session
                .pane_id_in_window(1, 0)
                .expect("next pane should exist"),
            session
                .pane_id_in_window(2, 0)
                .expect("trailing pane should exist"),
        )
    };

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(alpha.clone(), 0)),
            target: MoveWindowTarget::Window(WindowTarget::with_window(alpha.clone(), 0)),
            renumber: false,
            kill_destination: false,
            detached: false,
            after: false,
            before: true,
        }))
        .await;

    assert_eq!(
        response,
        Response::MoveWindow(rmux_proto::MoveWindowResponse {
            session_name: alpha.clone(),
            target: Some(WindowTarget::with_window(alpha.clone(), 0)),
        })
    );

    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("alpha should exist");
    assert_eq!(
        session.windows().keys().copied().collect::<Vec<_>>(),
        vec![0, 2, 3]
    );
    assert_eq!(session.pane_id_in_window(0, 0), Some(source_pane_id));
    assert_eq!(session.pane_id_in_window(2, 0), Some(next_pane_id));
    assert_eq!(session.pane_id_in_window(3, 0), Some(trailing_pane_id));
    assert_eq!(session.active_window_index(), 0);
}

#[tokio::test]
async fn move_window_reindex_starts_at_base_index() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;
    insert_window(&handler, &alpha, 3).await;
    insert_window(&handler, &alpha, 7).await;

    let set_base_index = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Session(alpha.clone()),
            option: OptionName::BaseIndex,
            value: "2".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(set_base_index, Response::SetOption(_)));

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: None,
            target: MoveWindowTarget::Session(alpha.clone()),
            renumber: true,
            kill_destination: false,
            detached: true,
            after: false,
            before: false,
        }))
        .await;

    assert_eq!(
        response,
        Response::MoveWindow(rmux_proto::MoveWindowResponse {
            session_name: alpha.clone(),
            target: None,
        })
    );

    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("alpha should exist");
    assert_eq!(
        session.windows().keys().copied().collect::<Vec<_>>(),
        vec![2, 3, 4]
    );
}

#[tokio::test]
async fn move_window_reindex_remaps_window_metadata() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;
    insert_window(&handler, &alpha, 2).await;
    insert_window(&handler, &alpha, 3).await;

    assert!(matches!(
        handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Window(WindowTarget::with_window(alpha.clone(), 3)),
                option: OptionName::WindowStyle,
                value: "fg=colour3".to_owned(),
                mode: SetOptionMode::Replace,
            }))
            .await,
        Response::SetOption(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::SetHook(rmux_proto::SetHookRequest {
                scope: ScopeSelector::Window(WindowTarget::with_window(alpha.clone(), 3)),
                hook: HookName::WindowLayoutChanged,
                command: "display-message remapped".to_owned(),
                lifecycle: HookLifecycle::Persistent,
            }))
            .await,
        Response::SetHook(_)
    ));

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: None,
            target: MoveWindowTarget::Session(alpha.clone()),
            renumber: true,
            kill_destination: false,
            detached: true,
            after: false,
            before: false,
        }))
        .await;
    assert!(matches!(response, Response::MoveWindow(_)));

    let state = handler.state.lock().await;
    assert_eq!(
        state
            .options
            .resolve_for_window(&alpha, 2, OptionName::WindowStyle),
        Some("fg=colour3")
    );
    assert_eq!(
        state.hooks.window_command(
            &WindowTarget::with_window(alpha, 2),
            HookName::WindowLayoutChanged
        ),
        Some("display-message remapped")
    );
}

#[tokio::test]
async fn move_window_across_sessions_restores_terminal_ownership_when_resize_fails() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    create_session(&handler, "alpha").await;
    create_session(&handler, "beta").await;
    insert_window(&handler, &alpha, 1).await;
    insert_window(&handler, &beta, 4).await;

    let (moved_pane_id, replaced_pane_id) = {
        let state = handler.state.lock().await;
        let alpha_session = state.sessions.session(&alpha).expect("alpha should exist");
        let beta_session = state.sessions.session(&beta).expect("beta should exist");
        (
            alpha_session
                .window_at(1)
                .and_then(|window| window.pane(0))
                .map(|pane| pane.id())
                .expect("alpha window 1 pane should exist"),
            beta_session
                .window_at(4)
                .and_then(|window| window.pane(0))
                .map(|pane| pane.id())
                .expect("beta window 4 pane should exist"),
        )
    };

    {
        let mut state = handler.state.lock().await;
        state.fail_next_resize_for_test();
    }

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(alpha.clone(), 1)),
            target: MoveWindowTarget::Window(WindowTarget::with_window(beta.clone(), 4)),
            renumber: false,
            kill_destination: true,
            detached: true,
            after: false,
            before: false,
        }))
        .await;

    assert_eq!(
        response,
        Response::Error(rmux_proto::ErrorResponse {
            error: rmux_proto::RmuxError::Server(
                "injected pane terminal resize failure".to_owned()
            ),
        })
    );

    let state = handler.state.lock().await;
    let alpha_session = state.sessions.session(&alpha).expect("alpha should exist");
    let beta_session = state.sessions.session(&beta).expect("beta should exist");
    assert_eq!(
        alpha_session.windows().keys().copied().collect::<Vec<_>>(),
        vec![0, 1]
    );
    assert_eq!(
        beta_session.windows().keys().copied().collect::<Vec<_>>(),
        vec![0, 4]
    );
    assert_eq!(alpha_session.pane_id_in_window(1, 0), Some(moved_pane_id));
    assert_eq!(beta_session.pane_id_in_window(4, 0), Some(replaced_pane_id));
    state
        .pane_profile_in_window(&alpha, 1, 0)
        .expect("moved pane terminal should return to the source session");
    state
        .pane_profile_in_window(&beta, 4, 0)
        .expect("replaced pane terminal should return to the destination session");
}
