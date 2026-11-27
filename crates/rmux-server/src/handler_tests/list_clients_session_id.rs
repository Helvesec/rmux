use super::*;

const CLIENT_SESSION_FORMAT: &str =
    "#{session_id}|#{session_name}|#{client_session}|#{client_control_mode}";

async fn list_clients(handler: &RequestHandler, format: &str, filter: Option<&str>) -> String {
    let response = handler
        .handle(Request::ListClients(Box::new(
            rmux_proto::ListClientsRequest {
                format: Some(format.to_owned()),
                target_session: None,
                filter: filter.map(str::to_owned),
                sort_order: None,
                reversed: false,
            },
        )))
        .await;
    let Response::ListClients(response) = response else {
        panic!("expected list-clients response");
    };
    String::from_utf8(response.output.stdout().to_vec()).expect("utf-8 list-clients output")
}

async fn register_control(handler: &RequestHandler, control_pid: u32, target: &SessionName) {
    let (event_tx, _event_rx) = mpsc::channel(crate::control::CONTROL_SERVER_EVENT_CAPACITY);
    handler
        .register_control_with_closing(
            control_pid,
            ControlModeUpgrade {
                initial_command_count: 0,
                mode: ControlMode::Plain,
                terminal_context: crate::outer_terminal::OuterTerminalContext::default(),
            },
            event_tx,
            Arc::new(AtomicBool::new(false)),
        )
        .await;
    handler
        .set_control_session(control_pid, Some(target.clone()))
        .await
        .expect("control client attaches to the session");
}

#[tokio::test]
async fn list_clients_uses_stable_session_identity_for_attach_and_control_formats() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: None,
            environment: None,
        }))
        .await;
    crate::session_fixture::assert_new_session(&created);

    let session_id = handler
        .state
        .lock()
        .await
        .sessions
        .session(&alpha)
        .expect("alpha exists")
        .id();
    let attach_pid = 93_401;
    let (attach_tx, _attach_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(attach_pid, alpha.clone(), attach_tx)
        .await;
    register_control(&handler, 93_402, &alpha).await;

    let expected = format!("{session_id}|alpha|alpha|0\n{session_id}|alpha|alpha|1\n");
    assert_eq!(
        list_clients(&handler, CLIENT_SESSION_FORMAT, None).await,
        expected
    );
    assert_eq!(
        list_clients(
            &handler,
            "#{?session_id,#{session_id},missing}|#{session_name}",
            None,
        )
        .await,
        format!("{session_id}|alpha\n{session_id}|alpha\n")
    );
    assert_eq!(
        list_clients(
            &handler,
            CLIENT_SESSION_FORMAT,
            Some(&format!("#{{==:#{{session_id}},{session_id}}}")),
        )
        .await,
        expected
    );

    let renamed = session_name("renamed");
    let response = handler
        .handle(Request::RenameSession(RenameSessionRequest {
            target: alpha,
            new_name: renamed,
        }))
        .await;
    assert!(
        matches!(response, Response::RenameSession(_)),
        "{response:?}"
    );
    assert_eq!(
        list_clients(&handler, CLIENT_SESSION_FORMAT, None).await,
        format!(
            "{session_id}|renamed|renamed|0\n\
             {session_id}|renamed|renamed|1\n"
        )
    );
}
