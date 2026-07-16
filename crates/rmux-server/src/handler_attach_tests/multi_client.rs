use super::*;

#[tokio::test]
async fn attached_resize_emits_client_resized_hook_with_client_context() {
    let handler = RequestHandler::new();
    let session = session_name("resize-hook");
    create_session_with_size(
        &handler,
        session.clone(),
        TerminalSize {
            cols: 100,
            rows: 30,
        },
    )
    .await;
    let (_attach_id, _rx) = register_sized_attach(
        &handler,
        101,
        &session,
        TerminalSize {
            cols: 100,
            rows: 30,
        },
    )
    .await;

    let hooked = handler
        .handle(Request::SetHookMutation(
            rmux_proto::SetHookMutationRequest {
                scope: ScopeSelector::Global,
                hook: rmux_proto::HookName::ClientResized,
                command: Some(
                    "if-shell -F '#{==:#{hook_client}:#{hook_session_name},101:resize-hook}' 'set-buffer -b client-resized ok' 'set-buffer -b client-resized bad'".to_owned(),
                ),
                lifecycle: rmux_proto::HookLifecycle::Persistent,
                append: false,
                unset: false,
                run_immediately: false,
                index: None,
            },
        ))
        .await;
    assert!(matches!(hooked, Response::SetHook(_)), "{hooked:?}");

    let mut lifecycle_events = handler.subscribe_lifecycle_events();
    handler
        .handle_attached_resize(
            101,
            TerminalSize {
                cols: 132,
                rows: 37,
            },
        )
        .await
        .expect("client resize succeeds");
    drain_lifecycle_events(&handler, &mut lifecycle_events).await;

    wait_for_named_buffer(&handler, "client-resized", b"ok").await;
}

#[tokio::test]
async fn attached_resize_does_not_emit_client_resized_hook_when_size_is_unchanged() {
    let handler = RequestHandler::new();
    let session = session_name("resize-hook-noop");
    create_session_with_size(
        &handler,
        session.clone(),
        TerminalSize {
            cols: 100,
            rows: 30,
        },
    )
    .await;
    let (_attach_id, _rx) = register_sized_attach(
        &handler,
        101,
        &session,
        TerminalSize {
            cols: 100,
            rows: 30,
        },
    )
    .await;

    let hooked = handler
        .handle(Request::SetHookMutation(
            rmux_proto::SetHookMutationRequest {
                scope: ScopeSelector::Global,
                hook: rmux_proto::HookName::ClientResized,
                command: Some("set-buffer -b client-resized-noop bad".to_owned()),
                lifecycle: rmux_proto::HookLifecycle::Persistent,
                append: false,
                unset: false,
                run_immediately: false,
                index: None,
            },
        ))
        .await;
    assert!(matches!(hooked, Response::SetHook(_)), "{hooked:?}");

    let mut lifecycle_events = handler.subscribe_lifecycle_events();
    handler
        .handle_attached_resize(
            101,
            TerminalSize {
                cols: 100,
                rows: 30,
            },
        )
        .await
        .expect("unchanged client resize succeeds");
    drain_lifecycle_events(&handler, &mut lifecycle_events).await;

    let maybe_content = {
        let state = handler.state.lock().await;
        state
            .buffers
            .show(Some("client-resized-noop"))
            .ok()
            .map(|(_, content)| content.to_vec())
    };
    assert_eq!(
        maybe_content, None,
        "client-resized hook must not run when the client dimensions did not change"
    );
}

#[tokio::test]
async fn window_size_policy_reconciles_attached_client_sizes() {
    let handler = RequestHandler::new();
    let session = session_name("resize-policy");
    create_session_with_size(
        &handler,
        session.clone(),
        TerminalSize {
            cols: 100,
            rows: 30,
        },
    )
    .await;

    let (_large_id, _large_rx) = register_sized_attach(
        &handler,
        101,
        &session,
        TerminalSize {
            cols: 120,
            rows: 40,
        },
    )
    .await;
    assert_eq!(
        attached_session_size(&handler, &session).await,
        TerminalSize {
            cols: 120,
            rows: 40
        },
        "default latest policy should use the most recently attached client"
    );

    let (_small_id, _small_rx) =
        register_sized_attach(&handler, 202, &session, TerminalSize { cols: 80, rows: 20 }).await;
    assert_eq!(
        attached_session_size(&handler, &session).await,
        TerminalSize { cols: 80, rows: 20 },
        "latest policy should follow the newest attach"
    );

    set_window_size_policy(&handler, &session, "largest").await;
    assert_eq!(
        attached_session_size(&handler, &session).await,
        TerminalSize {
            cols: 120,
            rows: 40
        },
        "largest policy must select the largest live attached client"
    );

    set_window_size_policy(&handler, &session, "smallest").await;
    assert_eq!(
        attached_session_size(&handler, &session).await,
        TerminalSize { cols: 80, rows: 20 },
        "smallest policy must select the smallest live attached client"
    );

    set_window_size_policy(&handler, &session, "manual").await;
    handler
        .handle_attached_resize(
            101,
            TerminalSize {
                cols: 140,
                rows: 45,
            },
        )
        .await
        .expect("manual client resize is accepted");
    assert_eq!(
        attached_session_size(&handler, &session).await,
        TerminalSize { cols: 80, rows: 20 },
        "manual policy must not auto-resize the window"
    );

    set_window_size_policy(&handler, &session, "latest").await;
    assert_eq!(
        attached_session_size(&handler, &session).await,
        TerminalSize {
            cols: 140,
            rows: 45
        },
        "latest policy should use the most recently resized client"
    );
}

#[tokio::test]
async fn refresh_client_ignore_size_transitions_reconcile_largest_policy() {
    let handler = RequestHandler::new();
    let session = session_name("refresh-ignore-size-largest");
    create_session_with_size(
        &handler,
        session.clone(),
        TerminalSize {
            cols: 100,
            rows: 30,
        },
    )
    .await;

    let (_large_id, _large_rx) = register_sized_attach(
        &handler,
        101,
        &session,
        TerminalSize {
            cols: 120,
            rows: 40,
        },
    )
    .await;
    let (_small_id, _small_rx) =
        register_sized_attach(&handler, 202, &session, TerminalSize { cols: 80, rows: 20 }).await;
    set_window_size_policy(&handler, &session, "largest").await;
    assert_eq!(
        attached_session_size(&handler, &session).await,
        TerminalSize {
            cols: 120,
            rows: 40
        }
    );

    let ignored = handler
        .handle(refresh_client_flags_request(101, Some("ignore-size"), None))
        .await;
    assert!(matches!(ignored, Response::RefreshClient(_)), "{ignored:?}");
    assert_eq!(
        attached_session_size(&handler, &session).await,
        TerminalSize { cols: 80, rows: 20 },
        "adding ignore-size must immediately remove the client from largest-policy candidates"
    );

    let restored = handler
        .handle(refresh_client_flags_request(
            101,
            None,
            Some("!ignore-size"),
        ))
        .await;
    assert!(
        matches!(restored, Response::RefreshClient(_)),
        "{restored:?}"
    );
    assert_eq!(
        attached_session_size(&handler, &session).await,
        TerminalSize {
            cols: 120,
            rows: 40
        },
        "removing ignore-size through the -F alias must restore the client as a candidate"
    );
}

#[tokio::test]
async fn refresh_client_ignore_size_reconcile_preserves_same_pid_replacement_identity() {
    let handler = RequestHandler::new();
    let alpha = session_name("refresh-ignore-size-race-alpha");
    let beta = session_name("refresh-ignore-size-race-beta");
    create_session_with_size(
        &handler,
        alpha.clone(),
        TerminalSize {
            cols: 100,
            rows: 30,
        },
    )
    .await;
    create_session_with_size(&handler, beta.clone(), TerminalSize { cols: 90, rows: 25 }).await;

    let (original_id, _original_rx) = register_sized_attach(
        &handler,
        303,
        &alpha,
        TerminalSize {
            cols: 120,
            rows: 40,
        },
    )
    .await;
    let (_small_id, _small_rx) =
        register_sized_attach(&handler, 404, &alpha, TerminalSize { cols: 80, rows: 20 }).await;
    set_window_size_policy(&handler, &alpha, "largest").await;

    let pause = handler.install_attached_size_selection_pause();
    let refresh_handler = handler.clone();
    let refresh = tokio::spawn(async move {
        refresh_handler
            .handle(refresh_client_flags_request(303, Some("ignore-size"), None))
            .await
    });
    tokio::time::timeout(ATTACH_LIFECYCLE_TIMEOUT, pause.reached.notified())
        .await
        .expect("refresh-client reaches identity-safe size selection");

    let (replacement_id, _replacement_rx) =
        register_sized_attach(&handler, 303, &beta, TerminalSize { cols: 95, rows: 26 }).await;
    assert_ne!(replacement_id, original_id);
    pause.release.notify_one();

    assert!(matches!(
        refresh.await.expect("refresh-client task joins"),
        Response::Error(_)
    ));
    assert_eq!(
        attached_session_size(&handler, &alpha).await,
        TerminalSize { cols: 80, rows: 20 },
        "the original session must reconcile after its large client is replaced"
    );
    assert_eq!(
        attached_session_size(&handler, &beta).await,
        TerminalSize { cols: 95, rows: 26 },
        "the stale refresh must never resize the replacement client's session"
    );
    let active_attach = handler.active_attach.lock().await;
    let replacement = active_attach
        .by_pid
        .get(&303)
        .expect("same-pid replacement survives");
    assert_eq!(replacement.id, replacement_id);
    assert_eq!(replacement.session_name, beta);
    assert!(!replacement
        .flags
        .contains(super::super::attach_support::ClientFlags::IGNORESIZE));
}

#[tokio::test]
async fn largest_and_smallest_window_size_policies_compose_dimensions_like_tmux() {
    let handler = RequestHandler::new();
    let session = session_name("resize-policy-dimensions");
    create_session_with_size(
        &handler,
        session.clone(),
        TerminalSize {
            cols: 100,
            rows: 30,
        },
    )
    .await;

    let (_wide_id, _wide_rx) = register_sized_attach(
        &handler,
        101,
        &session,
        TerminalSize {
            cols: 120,
            rows: 20,
        },
    )
    .await;
    let (_tall_id, _tall_rx) =
        register_sized_attach(&handler, 202, &session, TerminalSize { cols: 80, rows: 50 }).await;

    set_window_size_policy(&handler, &session, "largest").await;
    assert_eq!(
        attached_session_size(&handler, &session).await,
        TerminalSize {
            cols: 120,
            rows: 50
        },
        "largest policy must take the maximum width and maximum height independently"
    );

    set_window_size_policy(&handler, &session, "smallest").await;
    assert_eq!(
        attached_session_size(&handler, &session).await,
        TerminalSize { cols: 80, rows: 20 },
        "smallest policy must take the minimum width and minimum height independently"
    );
}

#[tokio::test]
async fn attach_session_initial_client_size_respects_window_size_policy() {
    let handler = RequestHandler::new();
    let session = session_name("attach-resize-policy");
    create_session_with_size(
        &handler,
        session.clone(),
        TerminalSize {
            cols: 100,
            rows: 30,
        },
    )
    .await;
    let (_large_id, _large_rx) = register_sized_attach(
        &handler,
        101,
        &session,
        TerminalSize {
            cols: 160,
            rows: 40,
        },
    )
    .await;

    set_window_size_policy(&handler, &session, "largest").await;
    let outcome = handler
        .dispatch(
            202,
            attach_session_request(&session, TerminalSize { cols: 72, rows: 18 }),
        )
        .await;
    assert!(matches!(outcome.response, Response::AttachSession(_)));
    assert!(outcome.attach.is_some());
    assert_eq!(
        attached_session_size(&handler, &session).await,
        TerminalSize {
            cols: 160,
            rows: 40
        },
        "initial attach must not shrink a largest-policy window below the largest live client"
    );

    set_window_size_policy(&handler, &session, "smallest").await;
    let outcome = handler
        .dispatch(
            303,
            attach_session_request(&session, TerminalSize { cols: 72, rows: 18 }),
        )
        .await;
    assert!(matches!(outcome.response, Response::AttachSession(_)));
    assert!(outcome.attach.is_some());
    assert_eq!(
        attached_session_size(&handler, &session).await,
        TerminalSize { cols: 72, rows: 18 },
        "initial attach must be considered by smallest-policy selection"
    );

    let manual = session_name("attach-resize-manual");
    create_session_with_size(
        &handler,
        manual.clone(),
        TerminalSize {
            cols: 100,
            rows: 30,
        },
    )
    .await;
    set_window_size_policy(&handler, &manual, "manual").await;
    let outcome = handler
        .dispatch(
            404,
            attach_session_request(
                &manual,
                TerminalSize {
                    cols: 132,
                    rows: 37,
                },
            ),
        )
        .await;
    assert!(matches!(outcome.response, Response::AttachSession(_)));
    assert!(outcome.attach.is_some());
    assert_eq!(
        attached_session_size(&handler, &manual).await,
        TerminalSize {
            cols: 100,
            rows: 30
        },
        "manual policy must ignore the initial attach client size"
    );
}

#[tokio::test]
async fn latest_window_size_recovers_when_small_client_finishes() {
    let handler = RequestHandler::new();
    let mut events = handler.subscribe_lifecycle_events();
    let session = session_name("resize-finish");
    create_session_with_size(
        &handler,
        session.clone(),
        TerminalSize {
            cols: 100,
            rows: 30,
        },
    )
    .await;

    let (_large_id, _large_rx) = register_sized_attach(
        &handler,
        101,
        &session,
        TerminalSize {
            cols: 160,
            rows: 40,
        },
    )
    .await;
    let (small_id, _small_rx) =
        register_sized_attach(&handler, 202, &session, TerminalSize { cols: 72, rows: 18 }).await;
    assert_eq!(
        attached_session_size(&handler, &session).await,
        TerminalSize { cols: 72, rows: 18 }
    );

    handler.finish_attach(202, small_id).await;

    wait_for_client_detached_event(&mut events, "202").await;
    assert_eq!(
        attached_session_size(&handler, &session).await,
        TerminalSize {
            cols: 160,
            rows: 40
        },
        "latest policy must fall back to the remaining latest live client"
    );
}

#[tokio::test]
async fn refresh_prunes_dead_attach_and_recomputes_latest_size() {
    let handler = RequestHandler::new();
    let mut events = handler.subscribe_lifecycle_events();
    let session = session_name("resize-stale");
    create_session_with_size(
        &handler,
        session.clone(),
        TerminalSize {
            cols: 100,
            rows: 30,
        },
    )
    .await;

    let (_large_id, _large_rx) = register_sized_attach(
        &handler,
        101,
        &session,
        TerminalSize {
            cols: 160,
            rows: 40,
        },
    )
    .await;
    let (_small_id, small_rx) =
        register_sized_attach(&handler, 202, &session, TerminalSize { cols: 72, rows: 18 }).await;
    assert_eq!(
        attached_session_size(&handler, &session).await,
        TerminalSize { cols: 72, rows: 18 }
    );

    drop(small_rx);
    handler.refresh_attached_session(&session).await;

    wait_for_client_detached_event(&mut events, "202").await;
    assert!(
        !handler.active_attach.lock().await.by_pid.contains_key(&202),
        "dead attach must be removed before size reconciliation"
    );
    assert_eq!(
        attached_session_size(&handler, &session).await,
        TerminalSize {
            cols: 160,
            rows: 40
        },
        "stale smallest client must not keep the window stuck small"
    );
}

#[tokio::test]
async fn targeted_refresh_prunes_dead_attach_and_recomputes_latest_size() {
    let handler = RequestHandler::new();
    let mut events = handler.subscribe_lifecycle_events();
    let session = session_name("resize-stale-targeted");
    create_session_with_size(
        &handler,
        session.clone(),
        TerminalSize {
            cols: 100,
            rows: 30,
        },
    )
    .await;

    let (_large_id, _large_rx) = register_sized_attach(
        &handler,
        101,
        &session,
        TerminalSize {
            cols: 160,
            rows: 40,
        },
    )
    .await;
    let (_small_id, small_rx) =
        register_sized_attach(&handler, 202, &session, TerminalSize { cols: 72, rows: 18 }).await;
    assert_eq!(
        attached_session_size(&handler, &session).await,
        TerminalSize { cols: 72, rows: 18 }
    );

    drop(small_rx);
    handler.refresh_attached_client(202, &session).await;

    wait_for_client_detached_event(&mut events, "202").await;
    assert!(
        !handler.active_attach.lock().await.by_pid.contains_key(&202),
        "targeted refresh must remove dead attach clients through the shared stale-client path"
    );
    assert_eq!(
        attached_session_size(&handler, &session).await,
        TerminalSize {
            cols: 160,
            rows: 40
        },
        "targeted refresh must not leave latest policy stuck on the stale client size"
    );
}

#[tokio::test]
async fn targeted_base_refresh_prunes_dead_attach_and_recomputes_latest_size() {
    let handler = RequestHandler::new();
    let mut events = handler.subscribe_lifecycle_events();
    let session = session_name("resize-stale-base");
    create_session_with_size(
        &handler,
        session.clone(),
        TerminalSize {
            cols: 100,
            rows: 30,
        },
    )
    .await;

    let (_large_id, _large_rx) = register_sized_attach(
        &handler,
        101,
        &session,
        TerminalSize {
            cols: 160,
            rows: 40,
        },
    )
    .await;
    let (_small_id, small_rx) =
        register_sized_attach(&handler, 202, &session, TerminalSize { cols: 72, rows: 18 }).await;
    assert_eq!(
        attached_session_size(&handler, &session).await,
        TerminalSize { cols: 72, rows: 18 }
    );

    drop(small_rx);
    handler
        .refresh_attached_client_base_only(202, &session)
        .await;

    wait_for_client_detached_event(&mut events, "202").await;
    assert!(
        !handler.active_attach.lock().await.by_pid.contains_key(&202),
        "targeted base refresh must remove dead attach clients through the shared stale-client path"
    );
    assert_eq!(
        attached_session_size(&handler, &session).await,
        TerminalSize {
            cols: 160,
            rows: 40
        },
        "targeted base refresh must not leave latest policy stuck on the stale client size"
    );
}

#[tokio::test]
async fn ignore_size_clients_update_their_render_size_without_resizing_session() {
    let handler = RequestHandler::new();
    let session = session_name("resize-ignore");
    create_session_with_size(
        &handler,
        session.clone(),
        TerminalSize {
            cols: 100,
            rows: 30,
        },
    )
    .await;

    let (_large_id, _large_rx) = register_sized_attach(
        &handler,
        101,
        &session,
        TerminalSize {
            cols: 160,
            rows: 40,
        },
    )
    .await;
    let (_ignored_id, _ignored_rx) = register_sized_attach_with_flags(
        &handler,
        202,
        &session,
        TerminalSize { cols: 90, rows: 22 },
        super::super::attach_support::ClientFlags::IGNORESIZE,
    )
    .await;
    assert_eq!(
        attached_session_size(&handler, &session).await,
        TerminalSize {
            cols: 160,
            rows: 40
        },
        "ignore-size attach must not become the latest window-size candidate"
    );

    handler
        .handle_attached_resize(202, TerminalSize { cols: 72, rows: 18 })
        .await
        .expect("ignore-size client resize still updates client metadata");

    {
        let active_attach = handler.active_attach.lock().await;
        let ignored = active_attach
            .by_pid
            .get(&202)
            .expect("ignore-size client remains attached");
        assert_eq!(ignored.client_size, TerminalSize { cols: 72, rows: 18 });
    }
    assert_eq!(
        attached_session_size(&handler, &session).await,
        TerminalSize {
            cols: 160,
            rows: 40
        },
        "ignore-size client resize must not resize the session"
    );
}

#[tokio::test]
async fn read_only_initial_attach_size_is_not_a_window_size_candidate() {
    let handler = RequestHandler::new();
    let session = session_name("attach-readonly-size");
    create_session_with_size(
        &handler,
        session.clone(),
        TerminalSize {
            cols: 100,
            rows: 30,
        },
    )
    .await;

    let (_large_id, _large_rx) = register_sized_attach(
        &handler,
        101,
        &session,
        TerminalSize {
            cols: 160,
            rows: 40,
        },
    )
    .await;
    let outcome = handler
        .dispatch(
            202,
            attach_session_request_with_read_only(
                &session,
                TerminalSize { cols: 72, rows: 18 },
                true,
            ),
        )
        .await;
    assert!(matches!(outcome.response, Response::AttachSession(_)));
    assert!(outcome.attach.is_some());
    assert_eq!(
        attached_session_size(&handler, &session).await,
        TerminalSize {
            cols: 160,
            rows: 40
        },
        "read-only implies ignore-size and must not shrink the session during attach"
    );
}

#[tokio::test]
async fn detach_client_recomputes_window_size_before_detached_event() {
    let handler = RequestHandler::new();
    let mut events = handler.subscribe_lifecycle_events();
    let session = session_name("resize-detach-command");
    create_session_with_size(
        &handler,
        session.clone(),
        TerminalSize {
            cols: 100,
            rows: 30,
        },
    )
    .await;

    let (_large_id, _large_rx) = register_sized_attach(
        &handler,
        101,
        &session,
        TerminalSize {
            cols: 160,
            rows: 40,
        },
    )
    .await;
    let (_small_id, mut small_rx) =
        register_sized_attach(&handler, 202, &session, TerminalSize { cols: 72, rows: 18 }).await;
    assert_eq!(
        attached_session_size(&handler, &session).await,
        TerminalSize { cols: 72, rows: 18 }
    );

    let response = handler
        .handle(Request::DetachClientExt(DetachClientExtRequest {
            target_client: Some("202".to_owned()),
            all_other_clients: false,
            target_session: None,
            kill_on_detach: false,
            exec_command: None,
        }))
        .await;
    assert_eq!(
        response,
        Response::DetachClient(rmux_proto::DetachClientResponse)
    );
    let _ = recv_matching_attach_control(&mut small_rx, "detach control", |control| {
        matches!(control, AttachControl::Detach)
    })
    .await;
    wait_for_client_detached_event(&mut events, "202").await;
    assert_eq!(
        attached_session_size(&handler, &session).await,
        TerminalSize {
            cols: 160,
            rows: 40
        },
        "closing detached clients must be excluded from latest-policy resize selection immediately"
    );
}

#[tokio::test]
async fn aggressive_resize_tracks_only_linked_windows_that_are_current() {
    let handler = RequestHandler::new();
    let alpha = session_name("aggr-alpha");
    let beta = session_name("aggr-beta");
    create_session_with_size(
        &handler,
        alpha.clone(),
        TerminalSize {
            cols: 100,
            rows: 30,
        },
    )
    .await;
    create_session_with_size(
        &handler,
        beta.clone(),
        TerminalSize {
            cols: 100,
            rows: 30,
        },
    )
    .await;
    assert!(matches!(
        handler
            .handle(Request::LinkWindow(LinkWindowRequest {
                source: WindowTarget::with_window(alpha.clone(), 0),
                target: WindowTarget::with_window(beta.clone(), 1),
                after: false,
                before: false,
                kill_destination: false,
                detached: false,
            }))
            .await,
        Response::LinkWindow(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::SelectWindow(SelectWindowRequest {
                target: WindowTarget::with_window(beta.clone(), 1),
            }))
            .await,
        Response::SelectWindow(_)
    ));
    let (_alpha_id, _alpha_rx) = register_sized_attach(
        &handler,
        101,
        &alpha,
        TerminalSize {
            cols: 160,
            rows: 40,
        },
    )
    .await;
    let (_beta_id, _beta_rx) =
        register_sized_attach(&handler, 202, &beta, TerminalSize { cols: 72, rows: 18 }).await;

    set_window_size_policy(&handler, &alpha, "smallest").await;
    set_window_option(&handler, &alpha, OptionName::AggressiveResize, "on").await;
    handler
        .reconcile_attached_session_size_and_emit(&alpha)
        .await
        .expect("aggressive linked current sessions reconcile");
    assert_eq!(
        attached_session_size(&handler, &alpha).await,
        TerminalSize { cols: 72, rows: 18 },
        "aggressive-resize must include other sessions where the linked window is current"
    );

    assert!(matches!(
        handler
            .handle(Request::SelectWindow(SelectWindowRequest {
                target: WindowTarget::with_window(beta.clone(), 0),
            }))
            .await,
        Response::SelectWindow(_)
    ));
    assert_eq!(
        attached_session_size(&handler, &alpha).await,
        TerminalSize {
            cols: 160,
            rows: 40
        },
        "selecting away from the linked window must recompute affected aggressive-resize sessions"
    );
}

#[tokio::test]
async fn different_requester_pids_reject_ambiguous_cross_process_attach_control() {
    let handler = RequestHandler::new();
    let first_owner_pid = 101;
    let second_owner_pid = 303;
    let intruder_pid = 202;
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    let gamma = session_name("gamma");

    for session_name in [alpha.clone(), beta.clone(), gamma.clone()] {
        let created = handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name,
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await;
        assert!(matches!(created, Response::NewSession(_)));
    }

    let (first_tx, mut first_rx) = mpsc::unbounded_channel();
    let _first_attach_id = handler
        .register_attach(first_owner_pid, alpha, first_tx)
        .await;
    let (second_tx, mut second_rx) = mpsc::unbounded_channel();
    let _second_attach_id = handler
        .register_attach(second_owner_pid, beta, second_tx)
        .await;

    let switched = handler
        .dispatch(
            intruder_pid,
            Request::SwitchClient(SwitchClientRequest { target: gamma }),
        )
        .await
        .response;
    assert_eq!(
        switched,
        Response::Error(rmux_proto::ErrorResponse {
            error: rmux_proto::RmuxError::Server(
                "switch-client requires an unambiguous attached client".to_owned(),
            ),
        })
    );

    let detached = handler
        .dispatch(
            intruder_pid,
            Request::DetachClient(rmux_proto::DetachClientRequest),
        )
        .await
        .response;
    assert_eq!(
        detached,
        Response::Error(rmux_proto::ErrorResponse {
            error: rmux_proto::RmuxError::Server(
                "detach-client requires an unambiguous attached client".to_owned(),
            ),
        })
    );

    assert!(matches!(first_rx.try_recv(), Err(TryRecvError::Empty)));
    assert!(matches!(second_rx.try_recv(), Err(TryRecvError::Empty)));
}

#[tokio::test]
async fn attach_session_without_target_prefers_an_unattached_session() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");

    for session_name in [alpha.clone(), beta.clone()] {
        let created = handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name,
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await;
        assert!(matches!(created, Response::NewSession(_)));
    }

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler.register_attach(101, alpha, control_tx).await;

    let outcome = handler
        .dispatch(
            202,
            Request::AttachSessionExt(AttachSessionExtRequest {
                target: None,
                detach_other_clients: false,
                kill_other_clients: false,
                read_only: false,
                skip_environment_update: false,
                flags: None,
            }),
        )
        .await;

    assert_eq!(
        outcome.response,
        Response::AttachSession(AttachSessionResponse { session_name: beta })
    );
    assert!(outcome.attach.is_some());
}

#[tokio::test]
async fn attach_session_without_target_prefers_the_most_recent_unattached_session() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");

    for session_name in [alpha.clone(), beta.clone()] {
        let created = handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name,
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await;
        assert!(matches!(created, Response::NewSession(_)));
    }

    sleep(Duration::from_secs(1)).await;
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let attach_id = handler.register_attach(101, beta.clone(), control_tx).await;
    handler.finish_attach(101, attach_id).await;

    let outcome = handler
        .dispatch(
            202,
            Request::AttachSessionExt(AttachSessionExtRequest {
                target: None,
                detach_other_clients: false,
                kill_other_clients: false,
                read_only: false,
                skip_environment_update: false,
                flags: None,
            }),
        )
        .await;

    assert_eq!(
        outcome.response,
        Response::AttachSession(AttachSessionResponse { session_name: beta })
    );
    assert!(outcome.attach.is_some());
}

#[tokio::test]
async fn switch_client_last_session_recalls_the_previous_session() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let beta = session_name("beta");

    for session_name in [alpha.clone(), beta.clone()] {
        let created = handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name,
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await;
        assert!(matches!(created, Response::NewSession(_)));
    }

    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let switched = handler
        .dispatch(
            requester_pid,
            Request::SwitchClientExt2(Box::new(SwitchClientExt2Request {
                target: Some(beta.clone()),
                key_table: None,
                last_session: false,
                next_session: false,
                previous_session: false,
                toggle_read_only: false,
                flags: None,
                sort_order: None,
                skip_environment_update: false,
            })),
        )
        .await
        .response;
    assert_eq!(
        switched,
        Response::SwitchClient(rmux_proto::SwitchClientResponse {
            session_name: beta.clone(),
        })
    );
    let _ = recv_matching_attach_control(&mut control_rx, "switch to beta", |control| {
        matches!(control, AttachControl::Switch(_))
    })
    .await;

    let switched_back = handler
        .dispatch(
            requester_pid,
            Request::SwitchClientExt2(Box::new(SwitchClientExt2Request {
                target: None,
                key_table: None,
                last_session: true,
                next_session: false,
                previous_session: false,
                toggle_read_only: false,
                flags: None,
                sort_order: None,
                skip_environment_update: false,
            })),
        )
        .await
        .response;
    assert_eq!(
        switched_back,
        Response::SwitchClient(rmux_proto::SwitchClientResponse {
            session_name: alpha,
        })
    );
    let _ = recv_matching_attach_control(&mut control_rx, "switch back to alpha", |control| {
        matches!(control, AttachControl::Switch(_))
    })
    .await;
}

#[tokio::test]
async fn kill_session_clears_attached_last_session_references() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let beta = session_name("beta");

    for session_name in [alpha.clone(), beta.clone()] {
        let created = handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name,
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await;
        assert!(matches!(created, Response::NewSession(_)));
    }

    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let switched = handler
        .dispatch(
            requester_pid,
            Request::SwitchClientExt2(Box::new(SwitchClientExt2Request {
                target: Some(beta.clone()),
                key_table: None,
                last_session: false,
                next_session: false,
                previous_session: false,
                toggle_read_only: false,
                flags: None,
                sort_order: None,
                skip_environment_update: false,
            })),
        )
        .await
        .response;
    assert_eq!(
        switched,
        Response::SwitchClient(rmux_proto::SwitchClientResponse {
            session_name: beta.clone(),
        })
    );
    let _ = recv_matching_attach_control(&mut control_rx, "switch to beta", |control| {
        matches!(control, AttachControl::Switch(_))
    })
    .await;

    {
        let active_attach = handler.active_attach.lock().await;
        assert_eq!(
            active_attach
                .last_session_for_client(requester_pid)
                .expect("attached client exists"),
            Some(alpha.clone())
        );
    }

    let response = handler
        .handle(Request::KillSession(KillSessionRequest {
            target: alpha,
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        }))
        .await;
    assert_eq!(
        response,
        Response::KillSession(rmux_proto::KillSessionResponse { existed: true })
    );

    let active_attach = handler.active_attach.lock().await;
    assert_eq!(
        active_attach
            .last_session_for_client(requester_pid)
            .expect("attached client survives on beta"),
        None
    );
}

#[tokio::test]
async fn kill_session_detach_on_destroy_off_switches_every_attached_client() {
    let handler = RequestHandler::new();
    let gamma = session_name("destroy-switch-gamma");
    let beta = session_name("destroy-switch-beta");
    let alpha = session_name("destroy-switch-alpha");
    for session in [gamma.clone(), beta.clone(), alpha.clone()] {
        create_session_with_size(&handler, session, TerminalSize { cols: 80, rows: 24 }).await;
    }
    let response = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Session(alpha.clone()),
            option: OptionName::DetachOnDestroy,
            value: "off".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(response, Response::SetOption(_)), "{response:?}");

    let (_, mut first_rx) = register_sized_attach(
        &handler,
        91_801,
        &alpha,
        TerminalSize { cols: 90, rows: 30 },
    )
    .await;
    let (_, mut second_rx) = register_sized_attach(
        &handler,
        91_802,
        &alpha,
        TerminalSize {
            cols: 100,
            rows: 35,
        },
    )
    .await;
    while first_rx.try_recv().is_ok() {}
    while second_rx.try_recv().is_ok() {}

    let response = handler
        .handle(Request::KillSession(KillSessionRequest {
            target: alpha.clone(),
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        }))
        .await;
    assert!(matches!(response, Response::KillSession(_)), "{response:?}");

    for (receiver, label) in [(&mut first_rx, "first"), (&mut second_rx, "second")] {
        let target = recv_switch_target(receiver, label).await;
        assert_eq!(target.session_name, beta);
    }
    let active_attach = handler.active_attach.lock().await;
    for attach_pid in [91_801, 91_802] {
        let active = active_attach
            .by_pid
            .get(&attach_pid)
            .expect("destroy switch preserves attached client");
        assert_eq!(active.session_name, beta);
        assert_ne!(active.session_id, rmux_proto::SessionId::new(0));
        assert!(!active.closing.load(Ordering::SeqCst));
    }
}

#[tokio::test]
async fn no_detach_on_destroy_client_flag_overrides_default_detach() {
    let handler = RequestHandler::new();
    let beta = session_name("destroy-flag-beta");
    let alpha = session_name("destroy-flag-alpha");
    for session in [beta.clone(), alpha.clone()] {
        create_session_with_size(&handler, session, TerminalSize { cols: 80, rows: 24 }).await;
    }
    let (_, mut control_rx) = register_sized_attach_with_flags(
        &handler,
        91_803,
        &alpha,
        TerminalSize { cols: 90, rows: 30 },
        super::super::attach_support::ClientFlags::NO_DETACH_ON_DESTROY,
    )
    .await;
    while control_rx.try_recv().is_ok() {}

    let response = handler
        .handle(Request::KillSession(KillSessionRequest {
            target: alpha,
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        }))
        .await;
    assert!(matches!(response, Response::KillSession(_)), "{response:?}");
    let target = recv_switch_target(&mut control_rx, "no-detach client flag").await;
    assert_eq!(target.session_name, beta);
}

#[tokio::test]
async fn destroy_switch_target_name_reuse_fails_closed() {
    let handler = RequestHandler::new();
    let gamma = session_name("destroy-reuse-gamma");
    let beta = session_name("destroy-reuse-beta");
    let alpha = session_name("destroy-reuse-alpha");
    for session in [gamma, beta.clone(), alpha.clone()] {
        create_session_with_size(&handler, session, TerminalSize { cols: 80, rows: 24 }).await;
    }
    let response = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Session(alpha.clone()),
            option: OptionName::DetachOnDestroy,
            value: "off".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(response, Response::SetOption(_)), "{response:?}");
    let (_, mut control_rx) = register_sized_attach(
        &handler,
        91_804,
        &alpha,
        TerminalSize { cols: 90, rows: 30 },
    )
    .await;
    while control_rx.try_recv().is_ok() {}

    let original_beta_id = handler
        .state
        .lock()
        .await
        .sessions
        .session(&beta)
        .expect("original beta exists")
        .id();
    let pause = handler.install_attached_size_selection_pause();
    let kill_handler = handler.clone();
    let kill_alpha = tokio::spawn(async move {
        kill_handler
            .handle(Request::KillSession(KillSessionRequest {
                target: alpha,
                kill_all_except_target: false,
                clear_alerts: false,
                kill_group: false,
            }))
            .await
    });
    pause.reached.notified().await;

    let response = handler
        .handle(Request::KillSession(KillSessionRequest {
            target: beta.clone(),
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        }))
        .await;
    assert!(matches!(response, Response::KillSession(_)), "{response:?}");
    create_session_with_size(&handler, beta.clone(), TerminalSize { cols: 80, rows: 24 }).await;
    let replacement_beta_id = handler
        .state
        .lock()
        .await
        .sessions
        .session(&beta)
        .expect("replacement beta exists")
        .id();
    assert_ne!(replacement_beta_id, original_beta_id);

    pause.release.notify_one();
    let response = kill_alpha.await.expect("kill alpha task joins");
    assert!(matches!(response, Response::KillSession(_)), "{response:?}");
    assert!(matches!(
        recv_attach_control(&mut control_rx, "stale destroy target fallback").await,
        AttachControl::Exited
    ));
    assert!(!handler
        .active_attach
        .lock()
        .await
        .by_pid
        .contains_key(&91_804));
}

#[tokio::test]
async fn concurrent_manual_switch_wins_over_destroy_switch() {
    let handler = RequestHandler::new();
    let gamma = session_name("destroy-race-gamma");
    let beta = session_name("destroy-race-beta");
    let alpha = session_name("destroy-race-alpha");
    for session in [gamma.clone(), beta, alpha.clone()] {
        create_session_with_size(&handler, session, TerminalSize { cols: 80, rows: 24 }).await;
    }
    let response = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Session(alpha.clone()),
            option: OptionName::DetachOnDestroy,
            value: "off".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(response, Response::SetOption(_)), "{response:?}");
    let (_, mut control_rx) = register_sized_attach(
        &handler,
        91_805,
        &alpha,
        TerminalSize { cols: 90, rows: 30 },
    )
    .await;
    while control_rx.try_recv().is_ok() {}

    let pause = handler.install_attached_size_selection_pause();
    let kill_handler = handler.clone();
    let kill_alpha = tokio::spawn(async move {
        kill_handler
            .handle(Request::KillSession(KillSessionRequest {
                target: alpha,
                kill_all_except_target: false,
                clear_alerts: false,
                kill_group: false,
            }))
            .await
    });
    pause.reached.notified().await;

    let switched = handler
        .dispatch(
            91_805,
            Request::SwitchClientExt2(Box::new(SwitchClientExt2Request {
                target: Some(gamma.clone()),
                key_table: None,
                last_session: false,
                next_session: false,
                previous_session: false,
                toggle_read_only: false,
                flags: None,
                sort_order: None,
                skip_environment_update: false,
            })),
        )
        .await
        .response;
    assert!(
        matches!(switched, Response::SwitchClient(_)),
        "{switched:?}"
    );
    let target = recv_switch_target(&mut control_rx, "manual switch during destroy").await;
    assert_eq!(target.session_name, gamma);

    pause.release.notify_one();
    let response = kill_alpha.await.expect("kill alpha task joins");
    assert!(matches!(response, Response::KillSession(_)), "{response:?}");
    assert!(matches!(control_rx.try_recv(), Err(TryRecvError::Empty)));
    let active_attach = handler.active_attach.lock().await;
    let active = active_attach
        .by_pid
        .get(&91_805)
        .expect("manual switch preserves attached client");
    assert_eq!(active.session_name, gamma);
    assert!(!active.closing.load(Ordering::SeqCst));
}

async fn create_session_with_size(
    handler: &RequestHandler,
    session: SessionName,
    size: TerminalSize,
) {
    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session,
            detached: true,
            size: Some(size),
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));
}

async fn register_sized_attach(
    handler: &RequestHandler,
    requester_pid: u32,
    session: &SessionName,
    size: TerminalSize,
) -> (u64, mpsc::UnboundedReceiver<AttachControl>) {
    register_sized_attach_with_flags(
        handler,
        requester_pid,
        session,
        size,
        super::super::attach_support::ClientFlags::default(),
    )
    .await
}

async fn register_sized_attach_with_flags(
    handler: &RequestHandler,
    requester_pid: u32,
    session: &SessionName,
    size: TerminalSize,
    flags: super::super::attach_support::ClientFlags,
) -> (u64, mpsc::UnboundedReceiver<AttachControl>) {
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    let uid = current_owner_uid();
    let attach_id = handler
        .register_attach_with_access(
            requester_pid,
            session.clone(),
            None,
            AttachRegistration {
                control_tx,
                control_backlog: Arc::new(AtomicUsize::new(0)),
                closing: Arc::new(AtomicBool::new(false)),
                persistent_overlay_epoch: Arc::new(AtomicU64::new(0)),
                terminal_context: OuterTerminalContext::default(),
                flags,
                render_stream: false,
                uid,
                user: rmux_os::identity::UserIdentity::Uid(uid),
                can_write: true,
                client_size: Some(size),
            },
        )
        .await
        .expect("attach registration succeeds");
    handler
        .handle_attached_resize(requester_pid, size)
        .await
        .expect("initial attached client size is accepted");
    (attach_id, control_rx)
}

fn refresh_client_flags_request(
    target_pid: u32,
    flags: Option<&str>,
    flags_alias: Option<&str>,
) -> Request {
    Request::RefreshClient(Box::new(rmux_proto::request::RefreshClientRequest {
        target_client: Some(target_pid.to_string()),
        adjustment: None,
        clear_pan: false,
        pan_left: false,
        pan_right: false,
        pan_up: false,
        pan_down: false,
        status_only: false,
        clipboard_query: false,
        flags: flags.map(str::to_owned),
        flags_alias: flags_alias.map(str::to_owned),
        subscriptions: Vec::new(),
        subscriptions_format: Vec::new(),
        control_size: None,
        colour_report: None,
    }))
}

async fn set_window_size_policy(handler: &RequestHandler, session: &SessionName, value: &str) {
    set_window_option(handler, session, OptionName::WindowSize, value).await;
}

async fn set_window_option(
    handler: &RequestHandler,
    session: &SessionName,
    option: OptionName,
    value: &str,
) {
    let response = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Window(WindowTarget::with_window(session.clone(), 0)),
            option,
            value: value.to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(response, Response::SetOption(_)), "{response:?}");
}

fn attach_session_request(session: &SessionName, client_size: TerminalSize) -> Request {
    attach_session_request_with_read_only(session, client_size, false)
}

fn attach_session_request_with_read_only(
    session: &SessionName,
    client_size: TerminalSize,
    read_only: bool,
) -> Request {
    Request::AttachSessionExt2(Box::new(AttachSessionExt2Request {
        target: Some(session.clone()),
        target_spec: Some(session.to_string()),
        detach_other_clients: false,
        kill_other_clients: false,
        read_only,
        skip_environment_update: false,
        flags: None,
        working_directory: None,
        client_terminal: rmux_proto::ClientTerminalContext::default(),
        client_size: Some(client_size),
    }))
}

async fn attached_session_size(handler: &RequestHandler, session: &SessionName) -> TerminalSize {
    let state = handler.state.lock().await;
    state
        .sessions
        .session(session)
        .expect("session exists")
        .window()
        .size()
}

async fn wait_for_client_detached_event(
    events: &mut tokio::sync::broadcast::Receiver<
        super::super::lifecycle_support::QueuedLifecycleEvent,
    >,
    client_name: &str,
) {
    tokio::time::timeout(ATTACH_LIFECYCLE_TIMEOUT, async {
        loop {
            let event = events.recv().await.expect("lifecycle event");
            if matches!(
                event.event,
                rmux_core::LifecycleEvent::ClientDetached { client_name: Some(ref name), .. }
                    if name == client_name
            ) {
                return;
            }
        }
    })
    .await
    .expect("timed out waiting for client-detached event");
}

async fn wait_for_named_buffer(handler: &RequestHandler, name: &str, expected: &[u8]) {
    let deadline = tokio::time::Instant::now() + ATTACH_LIFECYCLE_TIMEOUT;
    loop {
        let maybe_content = {
            let state = handler.state.lock().await;
            state
                .buffers
                .show(Some(name))
                .ok()
                .map(|(_, content)| content.to_vec())
        };
        if maybe_content.as_deref() == Some(expected) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for buffer {name:?} to contain {expected:?}; got {maybe_content:?}"
        );
        sleep(Duration::from_millis(25)).await;
    }
}

async fn drain_lifecycle_events(
    handler: &RequestHandler,
    events: &mut tokio::sync::broadcast::Receiver<
        super::super::lifecycle_support::QueuedLifecycleEvent,
    >,
) {
    loop {
        match events.try_recv() {
            Ok(event) => handler.dispatch_lifecycle_hook(event).await,
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
            | Err(tokio::sync::broadcast::error::TryRecvError::Closed) => break,
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => continue,
        }
    }
}
