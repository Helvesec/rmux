use super::*;

#[tokio::test]
async fn new_session_uses_the_default_size_when_request_omits_geometry() {
    let handler = RequestHandler::new();
    let response = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name("alpha"),
            detached: true,
            size: None,

            environment: None,
        }))
        .await;

    assert_eq!(
        response,
        Response::NewSession(rmux_proto::NewSessionResponse {
            session_name: session_name("alpha"),
            detached: true,
            output: None,
        })
    );

    let exists = handler
        .handle(Request::HasSession(HasSessionRequest {
            target: session_name("alpha"),
        }))
        .await;
    assert_eq!(
        exists,
        Response::HasSession(rmux_proto::HasSessionResponse { exists: true })
    );

    let removed = handler
        .handle(Request::KillSession(KillSessionRequest {
            target: session_name("alpha"),
            kill_all_except_target: false,
            clear_alerts: false,
        }))
        .await;
    assert_eq!(
        removed,
        Response::KillSession(rmux_proto::KillSessionResponse { existed: true })
    );

    let recreated = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name("alpha"),
            detached: true,
            size: Some(DEFAULT_SESSION_SIZE),

            environment: None,
        }))
        .await;
    assert_eq!(
        recreated,
        Response::NewSession(rmux_proto::NewSessionResponse {
            session_name: session_name("alpha"),
            detached: true,
            output: None,
        })
    );
}

#[tokio::test]
async fn duplicate_new_session_returns_the_duplicate_session_error() {
    let handler = RequestHandler::new();
    let request = Request::NewSession(NewSessionRequest {
        session_name: session_name("alpha"),
        detached: false,
        size: Some(TerminalSize {
            cols: 100,
            rows: 30,
        }),

        environment: None,
    });

    let first = handler.handle(request.clone()).await;
    let duplicate = handler.handle(request).await;

    assert!(matches!(first, Response::NewSession(_)));
    assert_eq!(
        duplicate,
        Response::Error(rmux_proto::ErrorResponse {
            error: RmuxError::DuplicateSession("alpha".to_owned()),
        })
    );
}

#[tokio::test]
async fn grouped_new_session_without_explicit_name_uses_tmux_suffix_shape() {
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
    assert!(matches!(created, Response::NewSession(_)));

    let grouped = handler
        .handle(Request::NewSessionExt(NewSessionExtRequest {
            session_name: None,
            working_directory: None,
            detached: true,
            size: None,
            environment: None,
            group_target: Some(alpha.clone()),
            attach_if_exists: false,
            detach_other_clients: false,
            kill_other_clients: false,
            flags: None,
            window_name: None,
            print_session_info: true,
            print_format: Some("#{session_name}".to_owned()),
            command: None,
            process_command: None,
            passthrough: false,
        }))
        .await;

    assert_eq!(
        grouped,
        Response::NewSession(rmux_proto::NewSessionResponse {
            session_name: session_name("alpha-1"),
            detached: true,
            output: Some(rmux_proto::CommandOutput::from_stdout(
                b"alpha-1\n".to_vec()
            )),
        })
    );

    let listed = handler
        .handle(Request::ListSessions(ListSessionsRequest {
            format: Some("#{session_name}".to_owned()),
            filter: None,
            sort_order: None,
            reversed: false,
        }))
        .await;
    let Response::ListSessions(listed) = listed else {
        panic!("list-sessions should succeed after grouped creation");
    };
    let stdout = std::str::from_utf8(listed.output.stdout()).expect("utf-8 stdout");
    assert_eq!(stdout, "alpha\nalpha-1\n");
}

#[tokio::test]
async fn auto_named_session_uses_next_global_session_id_after_named_sessions() {
    let handler = RequestHandler::new();
    for name in ["0", "1", "bob"] {
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

    let unnamed = handler
        .handle(Request::NewSessionExt(NewSessionExtRequest {
            session_name: None,
            working_directory: None,
            detached: true,
            size: None,
            environment: None,
            group_target: None,
            attach_if_exists: false,
            detach_other_clients: false,
            kill_other_clients: false,
            flags: None,
            window_name: None,
            print_session_info: true,
            print_format: Some("#{session_name}".to_owned()),
            command: None,
            process_command: None,
            passthrough: false,
        }))
        .await;

    assert_eq!(
        unnamed,
        Response::NewSession(rmux_proto::NewSessionResponse {
            session_name: session_name("3"),
            detached: true,
            output: Some(rmux_proto::CommandOutput::from_stdout(b"3\n".to_vec())),
        })
    );
}

#[tokio::test]
async fn grouped_new_session_rejects_shell_command_like_tmux() {
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
    assert!(matches!(created, Response::NewSession(_)));

    let grouped = handler
        .handle(Request::NewSessionExt(NewSessionExtRequest {
            session_name: Some(session_name("peer")),
            working_directory: None,
            detached: true,
            size: None,
            environment: None,
            group_target: Some(alpha),
            attach_if_exists: false,
            detach_other_clients: false,
            kill_other_clients: false,
            flags: None,
            window_name: None,
            print_session_info: false,
            print_format: None,
            command: Some(vec!["cat".to_owned()]),
            process_command: None,
            passthrough: false,
        }))
        .await;

    assert!(
        matches!(grouped, Response::Error(ErrorResponse { error: RmuxError::Server(ref message) }) if message == "command or window name given with target"),
        "expected grouped new-session command rejection, got {grouped:?}"
    );
}

#[tokio::test]
async fn grouped_new_session_uses_next_global_session_id_suffix_when_group_is_new() {
    let handler = RequestHandler::new();
    for name in ["0", "1", "bob"] {
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

    let grouped = handler
        .handle(Request::NewSessionExt(NewSessionExtRequest {
            session_name: None,
            working_directory: None,
            detached: true,
            size: None,
            environment: None,
            group_target: Some(session_name("stacy")),
            attach_if_exists: false,
            detach_other_clients: false,
            kill_other_clients: false,
            flags: None,
            window_name: None,
            print_session_info: true,
            print_format: Some("#{session_name}:#{session_group}".to_owned()),
            command: None,
            process_command: None,
            passthrough: false,
        }))
        .await;

    assert_eq!(
        grouped,
        Response::NewSession(rmux_proto::NewSessionResponse {
            session_name: session_name("stacy-3"),
            detached: true,
            output: Some(rmux_proto::CommandOutput::from_stdout(
                b"stacy-3:stacy\n".to_vec(),
            )),
        })
    );
}
