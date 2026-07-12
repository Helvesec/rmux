use super::*;

#[tokio::test]
async fn swap_window_preserves_group_peer_selection_by_winlink_index() {
    for detached in [false, true] {
        let handler = RequestHandler::new();
        let owner = session_name(if detached {
            "swap-selection-owner-detached"
        } else {
            "swap-selection-owner"
        });
        let peer = session_name(if detached {
            "swap-selection-peer-detached"
        } else {
            "swap-selection-peer"
        });
        create_session(&handler, owner.as_str()).await;
        insert_window(&handler, &owner, 1).await;
        create_grouped_session(&handler, peer.as_str(), &owner).await;
        {
            let mut state = handler.state.lock().await;
            state
                .sessions
                .session_mut(&peer)
                .expect("group peer exists")
                .select_window(1)
                .expect("peer window selection succeeds");
        }

        let response = handler
            .handle(Request::SwapWindow(SwapWindowRequest {
                source: WindowTarget::with_window(owner.clone(), 0),
                target: WindowTarget::with_window(owner, 1),
                detached,
            }))
            .await;
        assert!(matches!(response, Response::SwapWindow(_)), "{response:?}");

        let state = handler.state.lock().await;
        let peer_session = state.sessions.session(&peer).expect("group peer survives");
        assert_eq!(peer_session.active_window_index(), 1);
        assert_eq!(peer_session.last_window_index(), Some(0));
    }
}

#[tokio::test]
async fn swap_window_permutes_group_peer_duplicate_alias_alerts_without_selection() {
    let handler = RequestHandler::new();
    let owner = session_name("swap-alert-duplicate-owner");
    let peer = session_name("swap-alert-duplicate-peer");
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
        assert!(peer_session.add_winlink_alert_flags(0, rmux_core::WINLINK_ACTIVITY));
        assert!(peer_session.add_winlink_alert_flags(1, rmux_core::WINLINK_BELL));
    }

    let response = handler
        .handle(Request::SwapWindow(SwapWindowRequest {
            source: WindowTarget::with_window(owner.clone(), 0),
            target: WindowTarget::with_window(owner, 1),
            detached: true,
        }))
        .await;
    assert!(matches!(response, Response::SwapWindow(_)), "{response:?}");

    let state = handler.state.lock().await;
    let peer_session = state.sessions.session(&peer).expect("group peer survives");
    assert_eq!(peer_session.active_window_index(), 1);
    assert_eq!(peer_session.last_window_index(), Some(0));
    assert_eq!(peer_session.winlink_alert_flags(0), rmux_core::WINLINK_BELL);
    assert_eq!(
        peer_session.winlink_alert_flags(1),
        rmux_core::WINLINK_ACTIVITY
    );
}

#[tokio::test]
async fn swap_window_preserves_unrelated_and_grouped_peer_silence_deadlines() {
    let handler = RequestHandler::new();
    let alpha = session_name("swap-silence-alpha");
    let beta = session_name("swap-silence-beta");
    create_session(&handler, alpha.as_str()).await;
    insert_window(&handler, &alpha, 1).await;
    insert_window(&handler, &alpha, 2).await;
    create_grouped_session(&handler, beta.as_str(), &alpha).await;
    enable_global_monitor_silence(&handler).await;

    let unrelated = WindowTarget::with_window(alpha.clone(), 0);
    let beta_one = WindowTarget::with_window(beta.clone(), 1);
    let beta_two = WindowTarget::with_window(beta.clone(), 2);
    let unrelated_before = handler
        .silence_timer_snapshot_for_test(&unrelated)
        .expect("unrelated timer is armed");
    let one_before = handler
        .silence_timer_snapshot_for_test(&beta_one)
        .expect("first grouped timer is armed");
    let two_before = handler
        .silence_timer_snapshot_for_test(&beta_two)
        .expect("second grouped timer is armed");
    let one_identity_before = handler
        .silence_timer_identity_for_test(&beta_one)
        .expect("first grouped timer has identity");
    let two_identity_before = handler
        .silence_timer_identity_for_test(&beta_two)
        .expect("second grouped timer has identity");

    let response = handler
        .handle(Request::SwapWindow(SwapWindowRequest {
            source: WindowTarget::with_window(alpha.clone(), 1),
            target: WindowTarget::with_window(alpha, 2),
            detached: false,
        }))
        .await;
    assert!(matches!(response, Response::SwapWindow(_)), "{response:?}");

    assert_eq!(
        handler.silence_timer_snapshot_for_test(&unrelated),
        Some(unrelated_before),
        "swap-window must not rearm an unrelated window"
    );
    let one_after = handler
        .silence_timer_snapshot_for_test(&beta_one)
        .expect("grouped timer remains armed at index one");
    let two_after = handler
        .silence_timer_snapshot_for_test(&beta_two)
        .expect("grouped timer remains armed at index two");
    let one_identity_after = handler
        .silence_timer_identity_for_test(&beta_one)
        .expect("grouped timer one keeps identity");
    let two_identity_after = handler
        .silence_timer_identity_for_test(&beta_two)
        .expect("grouped timer two keeps identity");

    assert_eq!(one_after.1, two_before.1);
    assert_eq!(two_after.1, one_before.1);
    assert_eq!(
        (one_identity_after.0, one_identity_after.1),
        (two_identity_before.0, two_identity_before.1)
    );
    assert_eq!(
        (two_identity_after.0, two_identity_after.1),
        (one_identity_before.0, one_identity_before.1)
    );
    assert!(one_after.0 > two_before.0);
    assert!(two_after.0 > one_before.0);
}

#[tokio::test]
async fn swap_window_swaps_distinct_duplicate_alias_silence_deadlines() {
    let handler = RequestHandler::new();
    let alpha = session_name("swap-duplicate-silence");
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

    let first = WindowTarget::with_window(alpha.clone(), 0);
    let second = WindowTarget::with_window(alpha.clone(), 2);
    let base_deadline = tokio::time::Instant::now() + Duration::from_secs(120);
    handler.replace_silence_timer_deadline_for_test(&first, base_deadline);
    handler
        .replace_silence_timer_deadline_for_test(&second, base_deadline + Duration::from_secs(9));
    let first_before = handler
        .silence_timer_snapshot_for_test(&first)
        .expect("first duplicate alias timer is armed");
    let second_before = handler
        .silence_timer_snapshot_for_test(&second)
        .expect("second duplicate alias timer is armed");

    let response = handler
        .handle(Request::SwapWindow(SwapWindowRequest {
            source: first.clone(),
            target: second.clone(),
            detached: true,
        }))
        .await;
    assert!(matches!(response, Response::SwapWindow(_)), "{response:?}");

    let first_after = handler
        .silence_timer_snapshot_for_test(&first)
        .expect("first duplicate alias timer remains armed");
    let second_after = handler
        .silence_timer_snapshot_for_test(&second)
        .expect("second duplicate alias timer remains armed");
    assert_eq!(first_after.1, second_before.1);
    assert_eq!(second_after.1, first_before.1);
    assert!(first_after.0 > second_before.0);
    assert!(second_after.0 > first_before.0);
}

#[tokio::test]
async fn swap_window_across_sessions_preserves_silence_deadlines_and_identities() {
    let handler = RequestHandler::new();
    let alpha = session_name("swap-cross-silence-alpha");
    let beta = session_name("swap-cross-silence-beta");
    create_session(&handler, alpha.as_str()).await;
    create_session(&handler, beta.as_str()).await;
    insert_window(&handler, &alpha, 1).await;
    enable_global_monitor_silence(&handler).await;

    let unrelated = WindowTarget::with_window(alpha.clone(), 0);
    let source = WindowTarget::with_window(alpha.clone(), 1);
    let target = WindowTarget::with_window(beta.clone(), 0);
    let base_deadline = tokio::time::Instant::now() + Duration::from_secs(120);
    handler.replace_silence_timer_deadline_for_test(&source, base_deadline);
    handler
        .replace_silence_timer_deadline_for_test(&target, base_deadline + Duration::from_secs(13));
    let unrelated_before = handler
        .silence_timer_snapshot_for_test(&unrelated)
        .expect("unrelated timer is armed");
    let source_before = handler
        .silence_timer_snapshot_for_test(&source)
        .expect("cross-session source timer is armed");
    let target_before = handler
        .silence_timer_snapshot_for_test(&target)
        .expect("cross-session target timer is armed");
    let source_identity = handler
        .silence_timer_identity_for_test(&source)
        .expect("cross-session source identity exists");
    let target_identity = handler
        .silence_timer_identity_for_test(&target)
        .expect("cross-session target identity exists");
    let (alpha_session_id, beta_session_id) = {
        let state = handler.state.lock().await;
        (
            state.sessions.session(&alpha).expect("alpha exists").id(),
            state.sessions.session(&beta).expect("beta exists").id(),
        )
    };

    let response = handler
        .handle(Request::SwapWindow(SwapWindowRequest {
            source: source.clone(),
            target: target.clone(),
            detached: true,
        }))
        .await;
    assert!(matches!(response, Response::SwapWindow(_)), "{response:?}");

    assert_eq!(
        handler.silence_timer_snapshot_for_test(&unrelated),
        Some(unrelated_before),
        "cross-session swap must not restart an unrelated timer"
    );
    let source_after = handler
        .silence_timer_snapshot_for_test(&source)
        .expect("timer from beta follows its window to alpha");
    let target_after = handler
        .silence_timer_snapshot_for_test(&target)
        .expect("timer from alpha follows its window to beta");
    assert_eq!(source_after.1, target_before.1);
    assert_eq!(target_after.1, source_before.1);
    assert!(source_after.0 > target_before.0);
    assert!(target_after.0 > source_before.0);
    let source_identity_after = handler
        .silence_timer_identity_for_test(&source)
        .expect("alpha destination identity exists");
    let target_identity_after = handler
        .silence_timer_identity_for_test(&target)
        .expect("beta destination identity exists");
    assert_eq!(
        (source_identity_after.0, source_identity_after.1),
        (alpha_session_id, target_identity.1)
    );
    assert_eq!(
        (target_identity_after.0, target_identity_after.1),
        (beta_session_id, source_identity.1)
    );
}

#[tokio::test]
async fn swap_window_across_sessions_moves_silence_alert_and_expired_timer_with_window_identity() {
    let handler = RequestHandler::new();
    let alpha = session_name("swap-alert-alpha");
    let beta = session_name("swap-alert-beta");
    create_session(&handler, alpha.as_str()).await;
    create_session(&handler, beta.as_str()).await;
    insert_window(&handler, &alpha, 1).await;
    enable_global_monitor_silence(&handler).await;

    let source = WindowTarget::with_window(alpha.clone(), 1);
    let target = WindowTarget::with_window(beta.clone(), 0);
    {
        let mut state = handler.state.lock().await;
        let _ = state
            .sessions
            .session_mut(&alpha)
            .expect("alpha exists")
            .clear_all_winlink_alert_flags(source.window_index());
        let _ = state
            .sessions
            .session_mut(&beta)
            .expect("beta exists")
            .clear_all_winlink_alert_flags(target.window_index());
    }
    let source_identity = handler
        .silence_timer_identity_for_test(&source)
        .expect("source timer identity exists before expiry");
    let target_identity = handler
        .silence_timer_identity_for_test(&target)
        .expect("target timer identity exists before swap");
    let target_timer_before = handler
        .silence_timer_snapshot_for_test(&target)
        .expect("target timer is armed before swap");
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
        assert!(state
            .sessions
            .session(&alpha)
            .expect("alpha exists before swap")
            .winlink_alert_flags(source.window_index())
            .contains(rmux_core::WINLINK_SILENCE));
        assert!(state
            .sessions
            .session(&beta)
            .expect("beta exists before swap")
            .winlink_alert_flags(target.window_index())
            .is_empty());
    }

    let response = handler
        .handle(Request::SwapWindow(SwapWindowRequest {
            source: source.clone(),
            target: target.clone(),
            detached: false,
        }))
        .await;
    assert!(matches!(response, Response::SwapWindow(_)), "{response:?}");

    let (alpha_session_id, beta_session_id) = {
        let state = handler.state.lock().await;
        let alpha_session = state.sessions.session(&alpha).expect("alpha survives swap");
        let beta_session = state.sessions.session(&beta).expect("beta survives swap");
        assert_eq!(
            alpha_session
                .window_at(source.window_index())
                .expect("target window moved into alpha")
                .id(),
            target_identity.1,
        );
        assert_eq!(
            beta_session
                .window_at(target.window_index())
                .expect("source window moved into beta")
                .id(),
            source_identity.1,
        );
        assert!(
            alpha_session
                .winlink_alert_flags(source.window_index())
                .is_empty(),
            "the target's empty flags follow its WindowId into alpha"
        );
        assert!(
            beta_session
                .winlink_alert_flags(target.window_index())
                .contains(rmux_core::WINLINK_SILENCE),
            "the source silence flag follows its WindowId into beta"
        );
        (alpha_session.id(), beta_session.id())
    };

    let source_timer_after = handler
        .silence_timer_snapshot_for_test(&source)
        .expect("the target's live timer follows its WindowId into alpha");
    assert_eq!(source_timer_after.1, target_timer_before.1);
    let source_timer_identity = handler
        .silence_timer_identity_for_test(&source)
        .expect("moved target timer identity exists");
    assert_eq!(
        (source_timer_identity.0, source_timer_identity.1),
        (alpha_session_id, target_identity.1)
    );
    assert_eq!(
        handler.silence_timer_snapshot_for_test(&target),
        None,
        "the already-expired source must remain without a timer after swap"
    );
    assert_eq!(
        handler
            .silence_timer_identity_for_test(&target)
            .map(|identity| identity.0),
        None,
        "no timer may remain under the destination SessionId"
    );
    assert_ne!(alpha_session_id, beta_session_id);
}

#[tokio::test]
async fn swap_window_with_d_selects_the_swapped_slots_across_sessions() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    create_session(&handler, "alpha").await;
    create_session(&handler, "beta").await;
    insert_window(&handler, &alpha, 2).await;
    insert_window(&handler, &beta, 4).await;

    // Both sessions have active_window at 0 by default.
    let response = handler
        .handle(Request::SwapWindow(SwapWindowRequest {
            source: WindowTarget::with_window(alpha.clone(), 2),
            target: WindowTarget::with_window(beta.clone(), 4),
            detached: true,
        }))
        .await;

    assert_eq!(
        response,
        Response::SwapWindow(rmux_proto::SwapWindowResponse {
            source: WindowTarget::with_window(alpha.clone(), 2),
            target: WindowTarget::with_window(beta.clone(), 4),
        })
    );

    // tmux cmd-swap-window.c selects the source/target winlinks when -d is
    // present.
    let state = handler.state.lock().await;
    let alpha_session = state.sessions.session(&alpha).expect("alpha should exist");
    let beta_session = state.sessions.session(&beta).expect("beta should exist");
    assert_eq!(alpha_session.active_window_index(), 2);
    assert_eq!(alpha_session.last_window_index(), Some(0));
    assert_eq!(beta_session.active_window_index(), 4);
    assert_eq!(beta_session.last_window_index(), Some(0));
}

#[tokio::test]
async fn swap_window_without_d_preserves_active_slots_across_sessions() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    create_session(&handler, "alpha").await;
    create_session(&handler, "beta").await;
    insert_window(&handler, &alpha, 2).await;
    insert_window(&handler, &beta, 4).await;

    let response = handler
        .handle(Request::SwapWindow(SwapWindowRequest {
            source: WindowTarget::with_window(alpha.clone(), 2),
            target: WindowTarget::with_window(beta.clone(), 4),
            detached: false,
        }))
        .await;

    assert_eq!(
        response,
        Response::SwapWindow(rmux_proto::SwapWindowResponse {
            source: WindowTarget::with_window(alpha.clone(), 2),
            target: WindowTarget::with_window(beta.clone(), 4),
        })
    );

    // Without -d, tmux preserves the current winlinks; only their contents are
    // swapped.
    let state = handler.state.lock().await;
    let alpha_session = state.sessions.session(&alpha).expect("alpha should exist");
    let beta_session = state.sessions.session(&beta).expect("beta should exist");
    assert_eq!(alpha_session.active_window_index(), 0);
    assert_eq!(alpha_session.last_window_index(), None);
    assert_eq!(beta_session.active_window_index(), 0);
    assert_eq!(beta_session.last_window_index(), None);
}

#[tokio::test]
async fn swap_window_across_sessions_swaps_linked_slot_metadata() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    let gamma = session_name("gamma");
    create_session(&handler, "alpha").await;
    create_session(&handler, "beta").await;
    create_session(&handler, "gamma").await;

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
        .handle(Request::SwapWindow(SwapWindowRequest {
            source: WindowTarget::with_window(alpha.clone(), 0),
            target: WindowTarget::with_window(beta.clone(), 0),
            detached: false,
        }))
        .await;
    assert_eq!(
        response,
        Response::SwapWindow(rmux_proto::SwapWindowResponse {
            source: WindowTarget::with_window(alpha.clone(), 0),
            target: WindowTarget::with_window(beta.clone(), 0),
        })
    );

    {
        let state = handler.state.lock().await;
        assert_eq!(state.window_link_count(&alpha, 0), 1);
        assert_eq!(state.window_link_count(&beta, 0), 2);
        assert_eq!(state.window_link_count(&gamma, 1), 2);
        assert_eq!(
            state.window_linked_sessions_list(&gamma, 1),
            vec![beta.clone(), gamma.clone()]
        );
    }

    let rename = handler
        .handle(Request::RenameWindow(RenameWindowRequest {
            target: WindowTarget::with_window(gamma.clone(), 1),
            name: "logs".to_owned(),
        }))
        .await;
    assert!(matches!(rename, Response::RenameWindow(_)));

    let state = handler.state.lock().await;
    assert_ne!(
        state
            .sessions
            .session(&alpha)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.name()),
        Some("logs")
    );
    assert_eq!(
        state
            .sessions
            .session(&beta)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.name()),
        Some("logs")
    );
    assert_eq!(
        state
            .sessions
            .session(&gamma)
            .and_then(|session| session.window_at(1))
            .and_then(|window| window.name()),
        Some("logs")
    );
}

#[tokio::test]
async fn swap_window_from_linked_slot_preserves_runtime_owners() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    let gamma = session_name("gamma");
    create_session(&handler, "alpha").await;
    create_session(&handler, "beta").await;
    create_session(&handler, "gamma").await;

    let linked_pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&alpha)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0))
            .map(|pane| pane.id())
            .expect("alpha pane should exist")
    };
    let gamma_pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&gamma)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0))
            .map(|pane| pane.id())
            .expect("gamma pane should exist")
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
        .handle(Request::SwapWindow(SwapWindowRequest {
            source: WindowTarget::with_window(beta.clone(), 1),
            target: WindowTarget::with_window(gamma.clone(), 0),
            detached: false,
        }))
        .await;
    assert_eq!(
        response,
        Response::SwapWindow(rmux_proto::SwapWindowResponse {
            source: WindowTarget::with_window(beta.clone(), 1),
            target: WindowTarget::with_window(gamma.clone(), 0),
        })
    );

    let state = handler.state.lock().await;
    assert_eq!(
        state
            .sessions
            .session(&gamma)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0))
            .map(|pane| pane.id()),
        Some(linked_pane_id)
    );
    assert_eq!(
        state
            .sessions
            .session(&beta)
            .and_then(|session| session.window_at(1))
            .and_then(|window| window.pane(0))
            .map(|pane| pane.id()),
        Some(gamma_pane_id)
    );
    state
        .pane_profile_in_window(&gamma, 0, 0)
        .expect("linked window pane should remain reachable after swap");
    state
        .pane_profile_in_window(&beta, 1, 0)
        .expect("unlinked target pane should move to the source slot runtime");
    assert_eq!(state.window_link_count(&alpha, 0), 2);
    assert_eq!(state.window_link_count(&gamma, 0), 2);
    assert_eq!(state.window_link_count(&beta, 1), 1);
}

#[tokio::test]
async fn swap_window_from_group_peer_swaps_runtime_state() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    let gamma = session_name("gamma");
    create_session(&handler, "alpha").await;
    create_grouped_session(&handler, "beta", &alpha).await;
    create_session(&handler, "gamma").await;

    let grouped_pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&beta)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0))
            .map(|pane| pane.id())
            .expect("grouped pane should exist")
    };
    let gamma_pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&gamma)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0))
            .map(|pane| pane.id())
            .expect("gamma pane should exist")
    };

    let response = handler
        .handle(Request::SwapWindow(SwapWindowRequest {
            source: WindowTarget::with_window(beta.clone(), 0),
            target: WindowTarget::with_window(gamma.clone(), 0),
            detached: false,
        }))
        .await;
    assert_eq!(
        response,
        Response::SwapWindow(rmux_proto::SwapWindowResponse {
            source: WindowTarget::with_window(beta.clone(), 0),
            target: WindowTarget::with_window(gamma.clone(), 0),
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
        Some(gamma_pane_id)
    );
    assert_eq!(
        state
            .sessions
            .session(&beta)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0))
            .map(|pane| pane.id()),
        Some(gamma_pane_id)
    );
    assert_eq!(
        state
            .sessions
            .session(&gamma)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0))
            .map(|pane| pane.id()),
        Some(grouped_pane_id)
    );
    state
        .pane_profile_in_window(&beta, 0, 0)
        .expect("swapped grouped pane terminal should live in the group runtime");
    state
        .pane_profile_in_window(&gamma, 0, 0)
        .expect("swapped target pane terminal should live in gamma");
}

#[tokio::test]
async fn swap_window_from_group_peer_linked_source_moves_link_metadata_to_target() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    let gamma = session_name("gamma");
    let delta = session_name("delta");
    create_session(&handler, "alpha").await;
    create_grouped_session(&handler, "beta", &alpha).await;
    create_session(&handler, "gamma").await;
    create_session(&handler, "delta").await;

    let (linked_pane_id, delta_pane_id) = {
        let state = handler.state.lock().await;
        (
            state
                .sessions
                .session(&beta)
                .and_then(|session| session.window_at(0))
                .and_then(|window| window.pane(0))
                .map(|pane| pane.id())
                .expect("grouped pane should exist"),
            state
                .sessions
                .session(&delta)
                .and_then(|session| session.window_at(0))
                .and_then(|window| window.pane(0))
                .map(|pane| pane.id())
                .expect("delta pane should exist"),
        )
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
        .handle(Request::SwapWindow(SwapWindowRequest {
            source: WindowTarget::with_window(beta.clone(), 0),
            target: WindowTarget::with_window(delta.clone(), 0),
            detached: true,
        }))
        .await;
    assert_eq!(
        response,
        Response::SwapWindow(rmux_proto::SwapWindowResponse {
            source: WindowTarget::with_window(beta.clone(), 0),
            target: WindowTarget::with_window(delta.clone(), 0),
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
        Some(delta_pane_id)
    );
    assert_eq!(
        state
            .sessions
            .session(&beta)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0))
            .map(|pane| pane.id()),
        Some(delta_pane_id)
    );
    assert_eq!(
        state
            .sessions
            .session(&delta)
            .and_then(|session| session.window_at(0))
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
        .pane_profile_in_window(&delta, 0, 0)
        .expect("linked source pane should move to target runtime");
    state
        .pane_profile_in_window(&gamma, 1, 0)
        .expect("surviving linked peer should keep runtime access");
    assert_eq!(state.window_link_count(&alpha, 0), 1);
    assert_eq!(state.window_link_count(&delta, 0), 2);
    assert_eq!(state.window_link_count(&gamma, 1), 2);
}

#[tokio::test]
async fn swap_window_from_group_peer_linked_target_moves_link_metadata_to_source_family() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    let gamma = session_name("gamma");
    let delta = session_name("delta");
    create_session(&handler, "alpha").await;
    create_grouped_session(&handler, "beta", &alpha).await;
    create_session(&handler, "gamma").await;
    create_session(&handler, "delta").await;

    let (grouped_pane_id, linked_pane_id) = {
        let state = handler.state.lock().await;
        (
            state
                .sessions
                .session(&beta)
                .and_then(|session| session.window_at(0))
                .and_then(|window| window.pane(0))
                .map(|pane| pane.id())
                .expect("grouped pane should exist"),
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
        .handle(Request::SwapWindow(SwapWindowRequest {
            source: WindowTarget::with_window(beta.clone(), 0),
            target: WindowTarget::with_window(gamma.clone(), 0),
            detached: true,
        }))
        .await;
    assert_eq!(
        response,
        Response::SwapWindow(rmux_proto::SwapWindowResponse {
            source: WindowTarget::with_window(beta.clone(), 0),
            target: WindowTarget::with_window(gamma.clone(), 0),
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
        Some(linked_pane_id)
    );
    assert_eq!(
        state
            .sessions
            .session(&beta)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0))
            .map(|pane| pane.id()),
        Some(linked_pane_id)
    );
    assert_eq!(
        state
            .sessions
            .session(&gamma)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0))
            .map(|pane| pane.id()),
        Some(grouped_pane_id)
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
        .pane_profile_in_window(&alpha, 0, 0)
        .expect("linked target pane should move to the source family runtime");
    state
        .pane_profile_in_window(&delta, 1, 0)
        .expect("surviving linked peer should keep runtime access");
    assert_eq!(state.window_link_count(&alpha, 0), 2);
    assert_eq!(state.window_link_count(&beta, 0), 2);
    assert_eq!(state.window_link_count(&delta, 1), 2);
    assert_eq!(state.window_link_count(&gamma, 0), 1);
}

#[tokio::test]
async fn rotate_window_synchronizes_every_linked_alias() {
    let handler = RequestHandler::new();
    let alpha = session_name("rotate-linked-alpha");
    let beta = session_name("rotate-linked-beta");
    create_session(&handler, alpha.as_str()).await;
    create_session(&handler, beta.as_str()).await;
    let split = handler
        .handle(Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Session(alpha.clone()),
            direction: rmux_proto::SplitDirection::Vertical,
            before: false,
            environment: None,
        }))
        .await;
    assert!(matches!(split, Response::SplitWindow(_)), "{split:?}");
    let linked = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(alpha.clone(), 0),
            target: WindowTarget::with_window(beta.clone(), 1),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(linked, Response::LinkWindow(_)), "{linked:?}");

    let response = handler
        .handle(Request::RotateWindow(RotateWindowRequest {
            target: WindowTarget::with_window(alpha.clone(), 0),
            direction: RotateWindowDirection::Up,
            restore_zoom: false,
        }))
        .await;
    assert!(
        matches!(response, Response::RotateWindow(_)),
        "{response:?}"
    );

    let state = handler.state.lock().await;
    let alpha_window = state
        .sessions
        .session(&alpha)
        .and_then(|session| session.window_at(0))
        .expect("source alias exists");
    let beta_window = state
        .sessions
        .session(&beta)
        .and_then(|session| session.window_at(1))
        .expect("linked alias exists");
    assert_eq!(beta_window, alpha_window);
}

#[tokio::test]
async fn rotate_window_updates_the_active_pane_after_reordering_the_window() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;

    assert!(matches!(
        handler
            .handle(Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Session(alpha.clone()),
                direction: rmux_proto::SplitDirection::Vertical,
                before: false,
                environment: None,
            }))
            .await,
        Response::SplitWindow(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Session(alpha.clone()),
                direction: rmux_proto::SplitDirection::Vertical,
                before: false,
                environment: None,
            }))
            .await,
        Response::SplitWindow(_)
    ));
    let previous_pane_ids = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&alpha)
            .expect("alpha should exist")
            .window_at(0)
            .expect("window 0 should exist")
            .panes()
            .iter()
            .map(|pane| pane.id())
            .collect::<Vec<_>>()
    };

    let response = handler
        .handle(Request::RotateWindow(RotateWindowRequest {
            target: WindowTarget::with_window(alpha.clone(), 0),
            direction: RotateWindowDirection::Up,
            restore_zoom: false,
        }))
        .await;

    assert_eq!(
        response,
        Response::RotateWindow(rmux_proto::RotateWindowResponse {
            target: WindowTarget::with_window(alpha.clone(), 0),
        })
    );

    let state = handler.state.lock().await;
    let window = state
        .sessions
        .session(&alpha)
        .expect("alpha should exist")
        .window_at(0)
        .expect("window 0 should exist");
    assert_eq!(window.active_pane_index(), 2);
    assert_eq!(
        window
            .panes()
            .iter()
            .map(|pane| pane.index())
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    assert_eq!(
        window
            .panes()
            .iter()
            .map(|pane| pane.id())
            .collect::<Vec<_>>(),
        vec![
            previous_pane_ids[1],
            previous_pane_ids[2],
            previous_pane_ids[0]
        ]
    );
}

#[tokio::test]
async fn rotate_window_down_selects_the_previous_pane_in_window_order() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;

    assert!(matches!(
        handler
            .handle(Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Session(alpha.clone()),
                direction: rmux_proto::SplitDirection::Vertical,
                before: false,
                environment: None,
            }))
            .await,
        Response::SplitWindow(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Session(alpha.clone()),
                direction: rmux_proto::SplitDirection::Vertical,
                before: false,
                environment: None,
            }))
            .await,
        Response::SplitWindow(_)
    ));
    let previous_pane_ids = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&alpha)
            .expect("alpha should exist")
            .window_at(0)
            .expect("window 0 should exist")
            .panes()
            .iter()
            .map(|pane| pane.id())
            .collect::<Vec<_>>()
    };

    let response = handler
        .handle(Request::RotateWindow(RotateWindowRequest {
            target: WindowTarget::with_window(alpha.clone(), 0),
            direction: RotateWindowDirection::Down,
            restore_zoom: false,
        }))
        .await;

    assert_eq!(
        response,
        Response::RotateWindow(rmux_proto::RotateWindowResponse {
            target: WindowTarget::with_window(alpha.clone(), 0),
        })
    );

    let state = handler.state.lock().await;
    let window = state
        .sessions
        .session(&alpha)
        .expect("alpha should exist")
        .window_at(0)
        .expect("window 0 should exist");
    assert_eq!(window.active_pane_index(), 2);
    assert_eq!(
        window
            .panes()
            .iter()
            .map(|pane| pane.index())
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    assert_eq!(
        window
            .panes()
            .iter()
            .map(|pane| pane.id())
            .collect::<Vec<_>>(),
        vec![
            previous_pane_ids[2],
            previous_pane_ids[0],
            previous_pane_ids[1]
        ]
    );
}

#[tokio::test]
async fn move_window_rejects_nonexistent_source() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(alpha.clone(), 99)),
            target: MoveWindowTarget::Window(WindowTarget::with_window(alpha.clone(), 5)),
            renumber: false,
            kill_destination: false,
            detached: false,
            after: false,
            before: false,
        }))
        .await;

    assert!(matches!(response, Response::Error(_)));
}

#[tokio::test]
async fn swap_window_rejects_nonexistent_window() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;

    let response = handler
        .handle(Request::SwapWindow(SwapWindowRequest {
            source: WindowTarget::with_window(alpha.clone(), 0),
            target: WindowTarget::with_window(alpha.clone(), 99),
            detached: false,
        }))
        .await;

    assert!(matches!(response, Response::Error(_)));
}

#[tokio::test]
async fn rotate_window_rejects_nonexistent_window() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;

    let response = handler
        .handle(Request::RotateWindow(RotateWindowRequest {
            target: WindowTarget::with_window(alpha.clone(), 99),
            direction: RotateWindowDirection::Up,
            restore_zoom: false,
        }))
        .await;

    assert!(matches!(response, Response::Error(_)));
}

#[tokio::test]
async fn move_window_same_source_and_destination_is_noop_without_kill() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(alpha.clone(), 0)),
            target: MoveWindowTarget::Window(WindowTarget::with_window(alpha.clone(), 0)),
            renumber: false,
            kill_destination: false,
            detached: false,
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
}

#[tokio::test]
async fn move_window_same_index_noop_does_not_consume_link_hooks() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;

    {
        let mut state = handler.state.lock().await;
        state
            .hooks
            .set(
                ScopeSelector::Global,
                HookName::WindowUnlinked,
                "true".to_owned(),
                HookLifecycle::OneShot,
            )
            .expect("window-unlinked hook set succeeds");
        state
            .hooks
            .set(
                ScopeSelector::Global,
                HookName::WindowLinked,
                "true".to_owned(),
                HookLifecycle::OneShot,
            )
            .expect("window-linked hook set succeeds");
    }

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(alpha.clone(), 0)),
            target: MoveWindowTarget::Window(WindowTarget::with_window(alpha.clone(), 0)),
            renumber: false,
            kill_destination: false,
            detached: false,
            after: false,
            before: false,
        }))
        .await;

    assert!(matches!(response, Response::MoveWindow(_)));
    let state = handler.state.lock().await;
    assert_eq!(
        state.hooks.global_command(HookName::WindowUnlinked),
        Some("true")
    );
    assert_eq!(
        state.hooks.global_command(HookName::WindowLinked),
        Some("true")
    );
}

#[tokio::test]
async fn move_window_same_source_and_destination_with_kill_reports_same_index() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(alpha.clone(), 0)),
            target: MoveWindowTarget::Window(WindowTarget::with_window(alpha.clone(), 0)),
            renumber: false,
            kill_destination: true,
            detached: false,
            after: false,
            before: false,
        }))
        .await;

    assert_eq!(
        response,
        Response::Error(rmux_proto::ErrorResponse {
            error: rmux_proto::RmuxError::Server("same index: 0".to_owned()),
        })
    );
}
