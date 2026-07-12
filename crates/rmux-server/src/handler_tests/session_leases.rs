use super::*;

#[tokio::test]
async fn session_lease_rejects_too_short_ttl() {
    let handler = RequestHandler::new();
    let alpha = session_name("lease-short");

    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: None,
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));

    let created_lease = handler
        .handle(Request::CreateSessionLease(
            rmux_proto::CreateSessionLeaseRequest {
                session_name: alpha,
                ttl_millis: rmux_proto::MIN_SESSION_LEASE_TTL_MILLIS - 1,
            },
        ))
        .await;
    let Response::Error(error) = created_lease else {
        panic!("expected too-short lease ttl to be rejected");
    };
    assert!(
        error.error.to_string().contains("must be at least"),
        "unexpected lease ttl error: {}",
        error.error
    );
}

#[tokio::test]
async fn session_lease_reaper_kills_unrenewed_session() {
    let handler = RequestHandler::new();
    let alpha = session_name("leased");

    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: None,
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));

    let created_lease = handler
        .handle(Request::CreateSessionLease(
            rmux_proto::CreateSessionLeaseRequest {
                session_name: alpha.clone(),
                ttl_millis: 600,
            },
        ))
        .await;
    assert!(matches!(created_lease, Response::CreateSessionLease(_)));

    tokio::time::sleep(Duration::from_millis(800)).await;

    let exists = handler
        .handle(Request::HasSession(HasSessionRequest {
            target: alpha.clone(),
        }))
        .await;
    assert_eq!(
        exists,
        Response::HasSession(rmux_proto::HasSessionResponse { exists: false })
    );
}

#[tokio::test]
async fn session_lease_renew_and_release_preserves_session() {
    let handler = RequestHandler::new();
    let alpha = session_name("renewed");

    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: None,
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));

    let created_lease = handler
        .handle(Request::CreateSessionLease(
            rmux_proto::CreateSessionLeaseRequest {
                session_name: alpha.clone(),
                ttl_millis: 600,
            },
        ))
        .await;
    let Response::CreateSessionLease(created_lease) = created_lease else {
        panic!("expected lease create response");
    };

    let renewed = handler
        .handle(Request::RenewSessionLease(
            rmux_proto::RenewSessionLeaseRequest {
                session_name: alpha.clone(),
                token: created_lease.token,
                ttl_millis: 600,
            },
        ))
        .await;
    assert_eq!(
        renewed,
        Response::RenewSessionLease(rmux_proto::RenewSessionLeaseResponse { renewed: true })
    );

    let released = handler
        .handle(Request::ReleaseSessionLease(
            rmux_proto::ReleaseSessionLeaseRequest {
                session_name: alpha.clone(),
                token: created_lease.token,
            },
        ))
        .await;
    assert_eq!(
        released,
        Response::ReleaseSessionLease(rmux_proto::ReleaseSessionLeaseResponse { released: true })
    );

    tokio::time::sleep(Duration::from_millis(700)).await;

    let exists = handler
        .handle(Request::HasSession(HasSessionRequest {
            target: alpha.clone(),
        }))
        .await;
    assert_eq!(
        exists,
        Response::HasSession(rmux_proto::HasSessionResponse { exists: true })
    );
}

#[tokio::test]
async fn renamed_session_lease_renews_and_releases_under_the_new_name() {
    let handler = RequestHandler::new();
    let old_name = session_name("lease-before-rename");
    let new_name = session_name("lease-after-rename");

    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: old_name.clone(),
            detached: true,
            size: None,
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));

    let created_lease = handler
        .handle(Request::CreateSessionLease(
            rmux_proto::CreateSessionLeaseRequest {
                session_name: old_name.clone(),
                ttl_millis: 600,
            },
        ))
        .await;
    let Response::CreateSessionLease(created_lease) = created_lease else {
        panic!("expected lease create response");
    };

    let renamed = handler
        .handle(Request::RenameSession(RenameSessionRequest {
            target: old_name.clone(),
            new_name: new_name.clone(),
        }))
        .await;
    assert_eq!(
        renamed,
        Response::RenameSession(rmux_proto::RenameSessionResponse {
            session_name: new_name.clone(),
        })
    );

    let renewed_old_name = handler
        .handle(Request::RenewSessionLease(
            rmux_proto::RenewSessionLeaseRequest {
                session_name: old_name,
                token: created_lease.token,
                ttl_millis: 600,
            },
        ))
        .await;
    assert!(matches!(renewed_old_name, Response::Error(_)));

    let renewed = handler
        .handle(Request::RenewSessionLease(
            rmux_proto::RenewSessionLeaseRequest {
                session_name: new_name.clone(),
                token: created_lease.token,
                ttl_millis: 600,
            },
        ))
        .await;
    assert_eq!(
        renewed,
        Response::RenewSessionLease(rmux_proto::RenewSessionLeaseResponse { renewed: true })
    );

    let released = handler
        .handle(Request::ReleaseSessionLease(
            rmux_proto::ReleaseSessionLeaseRequest {
                session_name: new_name.clone(),
                token: created_lease.token,
            },
        ))
        .await;
    assert_eq!(
        released,
        Response::ReleaseSessionLease(rmux_proto::ReleaseSessionLeaseResponse { released: true })
    );

    tokio::time::sleep(Duration::from_millis(700)).await;
    wait_for_session_state(&handler, new_name, true).await;
}

#[tokio::test]
async fn renamed_session_lease_expiration_kills_the_renamed_session() {
    let handler = RequestHandler::new();
    let old_name = session_name("lease-expire-before-rename");
    let new_name = session_name("lease-expire-after-rename");

    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: old_name.clone(),
            detached: true,
            size: None,
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));

    let created_lease = handler
        .handle(Request::CreateSessionLease(
            rmux_proto::CreateSessionLeaseRequest {
                session_name: old_name.clone(),
                ttl_millis: 600,
            },
        ))
        .await;
    assert!(matches!(created_lease, Response::CreateSessionLease(_)));

    let renamed = handler
        .handle(Request::RenameSession(RenameSessionRequest {
            target: old_name,
            new_name: new_name.clone(),
        }))
        .await;
    assert!(matches!(renamed, Response::RenameSession(_)));

    tokio::time::sleep(Duration::from_millis(800)).await;
    wait_for_session_state(&handler, new_name, false).await;
}

#[tokio::test]
async fn expired_session_lease_reaper_follows_rename_after_expiration_extraction() {
    let handler = RequestHandler::new();
    let old_name = session_name("lease-reap-race-before-rename");
    let new_name = session_name("lease-reap-race-after-rename");

    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: old_name.clone(),
            detached: true,
            size: None,
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)), "{created:?}");
    let session_id = handler
        .state
        .lock()
        .await
        .sessions
        .session(&old_name)
        .expect("leased session exists")
        .id();
    let pause = handler.install_expired_session_lease_reap_pause(old_name.clone(), session_id);

    let created_lease = handler
        .handle(Request::CreateSessionLease(
            rmux_proto::CreateSessionLeaseRequest {
                session_name: old_name.clone(),
                ttl_millis: 600,
            },
        ))
        .await;
    assert!(
        matches!(created_lease, Response::CreateSessionLease(_)),
        "{created_lease:?}"
    );

    tokio::time::timeout(Duration::from_secs(2), pause.reached.notified())
        .await
        .expect("lease reaper extracts the expired stable session identity");
    let renamed = handler
        .handle(Request::RenameSession(RenameSessionRequest {
            target: old_name,
            new_name: new_name.clone(),
        }))
        .await;
    assert!(matches!(renamed, Response::RenameSession(_)), "{renamed:?}");
    pause.release.notify_one();

    wait_for_session_state(&handler, new_name, false).await;
}

#[tokio::test]
async fn session_destroyed_by_last_pane_kill_clears_stale_lease() {
    let handler = RequestHandler::new();
    let alpha = session_name("lease-pane-kill");

    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: None,
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));

    let created_lease = handler
        .handle(Request::CreateSessionLease(
            rmux_proto::CreateSessionLeaseRequest {
                session_name: alpha.clone(),
                ttl_millis: 600,
            },
        ))
        .await;
    assert!(matches!(created_lease, Response::CreateSessionLease(_)));

    let killed = handler
        .handle(Request::KillPane(KillPaneRequest {
            target: PaneTarget::new(alpha.clone(), 0),
            kill_all_except: false,
        }))
        .await;
    assert_eq!(
        killed,
        Response::KillPane(rmux_proto::KillPaneResponse {
            target: PaneTarget::new(alpha.clone(), 0),
            window_destroyed: true,
        })
    );

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
        "same session name should be reusable after final-pane kill: {recreated:?}"
    );

    tokio::time::sleep(Duration::from_millis(800)).await;

    let exists = handler
        .handle(Request::HasSession(HasSessionRequest {
            target: alpha.clone(),
        }))
        .await;
    assert_eq!(
        exists,
        Response::HasSession(rmux_proto::HasSessionResponse { exists: true })
    );
}

#[tokio::test]
async fn session_destroyed_by_last_pane_exit_clears_stale_lease() {
    let handler = RequestHandler::new();
    let alpha = session_name("lease-pane-exit");

    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: None,
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));

    let created_lease = handler
        .handle(Request::CreateSessionLease(
            rmux_proto::CreateSessionLeaseRequest {
                session_name: alpha.clone(),
                ttl_millis: 600,
            },
        ))
        .await;
    assert!(matches!(created_lease, Response::CreateSessionLease(_)));

    let respawned = handler
        .handle(Request::RespawnPane(Box::new(
            rmux_proto::RespawnPaneRequest {
                target: PaneTarget::new(alpha.clone(), 0),
                kill: true,
                start_directory: None,
                environment: None,
                command: None,
                process_command: Some(rmux_proto::ProcessCommand::Shell("exit 0".to_owned())),
            },
        )))
        .await;
    assert!(matches!(respawned, Response::RespawnPane(_)));
    wait_for_session_state(&handler, alpha.clone(), false).await;

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
        "same session name should be reusable after final-pane exit: {recreated:?}"
    );

    tokio::time::sleep(Duration::from_millis(800)).await;
    wait_for_session_state(&handler, alpha, true).await;
}

async fn wait_for_session_state(
    handler: &RequestHandler,
    session_name: rmux_proto::SessionName,
    expected: bool,
) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let exists = handler
            .handle(Request::HasSession(HasSessionRequest {
                target: session_name.clone(),
            }))
            .await;
        if exists == Response::HasSession(rmux_proto::HasSessionResponse { exists: expected }) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "session {session_name} did not reach exists={expected}; last response: {exists:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
