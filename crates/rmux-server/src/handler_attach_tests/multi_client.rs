use super::*;

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
                "switch-client: 2 clients attached (101, 303); pick one with \
                 `-t <client>` or address a whole session with `-s <session>`"
                    .to_owned(),
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
                "detach-client: 2 clients attached (101, 303); pick one with \
                 `-t <client>` or address a whole session with `-s <session>`"
                    .to_owned(),
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
            Request::SwitchClientExt2(SwitchClientExt2Request {
                target: Some(beta.clone()),
                key_table: None,
                last_session: false,
                next_session: false,
                previous_session: false,
                toggle_read_only: false,
                flags: None,
                sort_order: None,
                skip_environment_update: false,
            }),
        )
        .await
        .response;
    assert_eq!(
        switched,
        Response::SwitchClient(rmux_proto::SwitchClientResponse {
            session_name: beta.clone(),
        })
    );
    assert!(matches!(
        control_rx.try_recv(),
        Ok(AttachControl::Switch(_))
    ));

    let switched_back = handler
        .dispatch(
            requester_pid,
            Request::SwitchClientExt2(SwitchClientExt2Request {
                target: None,
                key_table: None,
                last_session: true,
                next_session: false,
                previous_session: false,
                toggle_read_only: false,
                flags: None,
                sort_order: None,
                skip_environment_update: false,
            }),
        )
        .await
        .response;
    assert_eq!(
        switched_back,
        Response::SwitchClient(rmux_proto::SwitchClientResponse {
            session_name: alpha,
        })
    );
    assert!(matches!(
        control_rx.try_recv(),
        Ok(AttachControl::Switch(_))
    ));
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
            Request::SwitchClientExt2(SwitchClientExt2Request {
                target: Some(beta.clone()),
                key_table: None,
                last_session: false,
                next_session: false,
                previous_session: false,
                toggle_read_only: false,
                flags: None,
                sort_order: None,
                skip_environment_update: false,
            }),
        )
        .await
        .response;
    assert_eq!(
        switched,
        Response::SwitchClient(rmux_proto::SwitchClientResponse {
            session_name: beta.clone(),
        })
    );
    assert!(matches!(
        control_rx.try_recv(),
        Ok(AttachControl::Switch(_))
    ));

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
