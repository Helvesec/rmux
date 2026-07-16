use super::*;

#[tokio::test]
async fn mutate_session_rolls_back_when_the_mutation_returns_an_error() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");

    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: Some(TerminalSize {
                cols: 120,
                rows: 40,
            }),

            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));

    let previous_session = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&alpha)
            .expect("session exists")
            .clone()
    };

    let result = {
        let mut state = handler.state.lock().await;
        state.mutate_session_and_resize_terminals(&alpha, |session| {
            session.split_active_pane()?;
            Err::<(), RmuxError>(RmuxError::Server("forced mutation failure".to_owned()))
        })
    };
    assert_eq!(
        result,
        Err(RmuxError::Server("forced mutation failure".to_owned()))
    );

    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("session exists");
    assert_eq!(session, &previous_session);
    assert_eq!(
        state.ensure_panes_exist(&alpha, &[rmux_core::PaneId::new(1)]),
        Err(RmuxError::Server(format!(
            "missing pane terminal for pane id 1 in session {}",
            alpha
        )))
    );
}

#[tokio::test]
async fn rename_session_missing_source_returns_session_not_found() {
    let handler = RequestHandler::new();

    let response = handler
        .handle(Request::RenameSession(RenameSessionRequest {
            target: session_name("missing"),
            new_name: session_name("gamma"),
        }))
        .await;

    assert_eq!(
        response,
        Response::Error(ErrorResponse {
            error: RmuxError::SessionNotFound("missing".to_owned()),
        })
    );
}

#[tokio::test]
async fn rename_session_to_existing_name_returns_duplicate_session() {
    let handler = RequestHandler::new();
    for name in ["alpha", "beta"] {
        let created = handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session_name(name),
                detached: true,
                size: None,

                environment: None,
            }))
            .await;
        assert!(matches!(created, Response::NewSession(_)));
    }

    let response = handler
        .handle(Request::RenameSession(RenameSessionRequest {
            target: session_name("alpha"),
            new_name: session_name("beta"),
        }))
        .await;

    assert_eq!(
        response,
        Response::Error(ErrorResponse {
            error: RmuxError::DuplicateSession("beta".to_owned()),
        })
    );

    assert_eq!(
        handler
            .handle(Request::HasSession(HasSessionRequest {
                target: session_name("alpha"),
            }))
            .await,
        Response::HasSession(rmux_proto::HasSessionResponse { exists: true })
    );
}

#[tokio::test]
async fn rename_session_to_same_name_returns_success_without_mutation() {
    let handler = RequestHandler::new();
    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name("alpha"),
            detached: true,
            size: None,

            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));

    let response = handler
        .handle(Request::RenameSession(RenameSessionRequest {
            target: session_name("alpha"),
            new_name: session_name("alpha"),
        }))
        .await;

    assert_eq!(
        response,
        Response::RenameSession(rmux_proto::RenameSessionResponse {
            session_name: session_name("alpha"),
        })
    );
    assert_eq!(
        handler
            .handle(Request::HasSession(HasSessionRequest {
                target: session_name("alpha"),
            }))
            .await,
        Response::HasSession(rmux_proto::HasSessionResponse { exists: true })
    );
}

#[tokio::test]
async fn rename_session_happy_path_migrates_session() {
    let handler = RequestHandler::new();
    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name("alpha"),
            detached: true,
            size: None,

            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));

    let renamed = handler
        .handle(Request::RenameSession(RenameSessionRequest {
            target: session_name("alpha"),
            new_name: session_name("gamma"),
        }))
        .await;

    assert_eq!(
        renamed,
        Response::RenameSession(rmux_proto::RenameSessionResponse {
            session_name: session_name("gamma"),
        })
    );

    assert_eq!(
        handler
            .handle(Request::HasSession(HasSessionRequest {
                target: session_name("alpha"),
            }))
            .await,
        Response::HasSession(rmux_proto::HasSessionResponse { exists: false })
    );
    assert_eq!(
        handler
            .handle(Request::HasSession(HasSessionRequest {
                target: session_name("gamma"),
            }))
            .await,
        Response::HasSession(rmux_proto::HasSessionResponse { exists: true })
    );
}

#[tokio::test]
async fn rename_session_resolves_unique_prefix_targets() {
    let handler = RequestHandler::new();
    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name("alpha"),
            detached: true,
            size: None,
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));

    let renamed = handler
        .handle(Request::RenameSession(RenameSessionRequest {
            target: session_name("alp"),
            new_name: session_name("gamma"),
        }))
        .await;

    assert_eq!(
        renamed,
        Response::RenameSession(rmux_proto::RenameSessionResponse {
            session_name: session_name("gamma"),
        })
    );
    assert_eq!(
        handler
            .handle(Request::HasSession(HasSessionRequest {
                target: session_name("gamma"),
            }))
            .await,
        Response::HasSession(rmux_proto::HasSessionResponse { exists: true })
    );
}

#[tokio::test]
async fn rename_session_fails_closed_when_source_name_is_recreated_after_resolution() {
    let handler = RequestHandler::new();
    let alpha = session_name("rename-identity-alpha");
    let beta = session_name("rename-identity-beta");
    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: None,
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)), "{created:?}");
    let old_session_id = handler
        .state
        .lock()
        .await
        .sessions
        .session(&alpha)
        .expect("old alpha exists")
        .id();

    let pause = handler.install_rename_session_identity_pause(alpha.clone());
    let rename_handler = handler.clone();
    let rename_alpha = alpha.clone();
    let rename_beta = beta.clone();
    let rename = tokio::spawn(async move {
        rename_handler
            .handle(Request::RenameSession(RenameSessionRequest {
                target: rename_alpha,
                new_name: rename_beta,
            }))
            .await
    });

    pause.reached.notified().await;
    let killed = handler
        .handle(Request::KillSession(KillSessionRequest {
            target: alpha.clone(),
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        }))
        .await;
    assert!(matches!(killed, Response::KillSession(_)), "{killed:?}");
    let recreated = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: None,
            environment: None,
        }))
        .await;
    assert!(
        matches!(recreated, Response::NewSession(_)),
        "{recreated:?}"
    );
    let new_session_id = handler
        .state
        .lock()
        .await
        .sessions
        .session(&alpha)
        .expect("recreated alpha exists")
        .id();
    assert_ne!(new_session_id, old_session_id);
    pause.release.notify_one();

    assert_eq!(
        rename.await.expect("rename task joins"),
        Response::Error(ErrorResponse {
            error: RmuxError::SessionNotFound(alpha.to_string()),
        })
    );
    let state = handler.state.lock().await;
    assert_eq!(
        state.sessions.session(&alpha).map(rmux_core::Session::id),
        Some(new_session_id)
    );
    assert!(!state.sessions.contains_session(&beta));
}

#[tokio::test]
async fn rename_session_serializes_timer_rekey_before_source_name_reuse() {
    let handler = RequestHandler::new();
    let alpha = session_name("rename-timer-alpha");
    let beta = session_name("rename-timer-beta");
    let monitor = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::MonitorSilence,
            value: "60".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(monitor, Response::SetOption(_)), "{monitor:?}");
    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: None,
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)), "{created:?}");
    let old_target = rmux_proto::WindowTarget::with_window(alpha.clone(), 0);
    let old_snapshot = handler
        .silence_timer_snapshot_for_test(&old_target)
        .expect("old alpha timer is armed");
    let renamed_target = rmux_proto::WindowTarget::with_window(beta.clone(), 0);

    let pause = handler.install_rename_session_control_commit_pause(alpha.clone());
    let rename_handler = handler.clone();
    let rename_alpha = alpha.clone();
    let rename_beta = beta.clone();
    let rename = tokio::spawn(async move {
        rename_handler
            .handle(Request::RenameSession(RenameSessionRequest {
                target: rename_alpha,
                new_name: rename_beta,
            }))
            .await
    });
    pause.reached.notified().await;

    assert_eq!(
        handler.silence_timer_snapshot_for_test(&old_target),
        None,
        "the old timer key must be removed inside the rename transaction"
    );
    let renamed_snapshot = handler
        .silence_timer_snapshot_for_test(&renamed_target)
        .expect("renamed session timer follows beta before the transaction commits");
    assert_eq!(
        renamed_snapshot.1, old_snapshot.1,
        "rename must preserve the old session's absolute silence deadline"
    );

    let recreate_handler = handler.clone();
    let recreate_alpha = alpha.clone();
    let recreate = tokio::spawn(async move {
        recreate_handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: recreate_alpha,
                detached: true,
                size: None,
                environment: None,
            }))
            .await
    });
    tokio::task::yield_now().await;
    assert!(
        !recreate.is_finished(),
        "source-name reuse must wait for the atomic rename transaction"
    );
    assert_eq!(
        handler.silence_timer_snapshot_for_test(&old_target),
        None,
        "blocked source-name reuse must not arm a timer early"
    );
    pause.release.notify_one();

    let renamed = rename.await.expect("rename task joins");
    assert!(matches!(renamed, Response::RenameSession(_)), "{renamed:?}");
    let recreated = recreate.await.expect("new alpha task joins");
    assert!(
        matches!(recreated, Response::NewSession(_)),
        "{recreated:?}"
    );
    assert_eq!(
        handler
            .silence_timer_snapshot_for_test(&renamed_target)
            .expect("renamed session keeps its timer"),
        renamed_snapshot,
        "source-name reuse must not replace the renamed session's timer"
    );
    assert!(
        handler
            .silence_timer_snapshot_for_test(&old_target)
            .is_some(),
        "reused source name arms its own timer after rename commit"
    );
}

#[tokio::test]
async fn stale_timer_expiry_and_cancel_fail_closed_after_session_name_reuse() {
    let handler = RequestHandler::new();
    let alpha = session_name("timer-aba-alpha");
    let monitor = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::MonitorSilence,
            value: "60".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(monitor, Response::SetOption(_)), "{monitor:?}");
    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: None,
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)), "{created:?}");
    let target = rmux_proto::WindowTarget::with_window(alpha.clone(), 0);
    let old_identity = handler
        .silence_timer_identity_for_test(&target)
        .expect("old incarnation timer identity exists");

    let killed = handler
        .handle(Request::KillSession(KillSessionRequest {
            target: alpha.clone(),
            kill_all_except_target: false,
            kill_group: false,
            clear_alerts: false,
        }))
        .await;
    assert!(matches!(killed, Response::KillSession(_)), "{killed:?}");
    let recreated = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: None,
            environment: None,
        }))
        .await;
    assert!(
        matches!(recreated, Response::NewSession(_)),
        "{recreated:?}"
    );
    let new_identity = handler
        .silence_timer_identity_for_test(&target)
        .expect("new incarnation timer identity exists");
    assert_ne!(old_identity.0, new_identity.0, "SessionId must change");

    handler
        .expire_silence_timer_for_test(
            target.clone(),
            old_identity.0,
            old_identity.1,
            old_identity.2,
        )
        .await;
    assert_eq!(
        handler.silence_timer_identity_for_test(&target),
        Some(new_identity),
        "stale expiry must not claim the new incarnation's timer"
    );
    {
        let state = handler.state.lock().await;
        assert!(
            state
                .sessions
                .session(&alpha)
                .expect("new alpha exists")
                .winlink_alert_flags(0)
                .is_empty(),
            "stale expiry must not alert the reused session name"
        );
    }

    handler.cancel_session_silence_timers(&alpha).await;
    assert_eq!(
        handler.silence_timer_identity_for_test(&target),
        Some(new_identity),
        "late retired-session cancellation must preserve current SessionId timers"
    );
}
