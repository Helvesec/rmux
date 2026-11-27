use super::*;

#[tokio::test]
async fn control_ignore_size_flags_without_size_reconcile_and_publish_each_transition() {
    // Issue #243 / C1: changing eligibility must immediately select the other
    // declared client size, and restoring eligibility must restore its effect.
    let handler = RequestHandler::new();
    let session = session_name("ignore-size-flags-only");
    create_session(&handler, session.clone(), INITIAL_SIZE).await;
    set_window_size_policy(&handler, &session, "largest").await;
    let control_pid = 92_900;
    let observer_pid = control_pid + 1;
    let (_, mut control_events) =
        register_control_client_with_id(&handler, control_pid, &session).await;
    let (_, mut observer_events) =
        register_control_client_with_id(&handler, observer_pid, &session).await;
    for (pid, size) in [(control_pid, CONTROL_SIZE), (observer_pid, INITIAL_SIZE)] {
        let response = handler
            .handle(Request::RefreshClient(Box::new(
                refresh_client_size_request(pid, size),
            )))
            .await;
        assert!(
            matches!(response, Response::RefreshClient(_)),
            "{response:?}"
        );
    }
    assert_eq!(session_size(&handler, &session).await, CONTROL_SIZE);
    let _ = settle_control_notifications(&mut control_events).await;
    let _ = settle_control_notifications(&mut observer_events).await;
    let window_id = active_window_id(&handler, &session).await;

    for alias in [false, true] {
        for (flags, expected) in [
            ("ignore-size", INITIAL_SIZE),
            ("!ignore-size", CONTROL_SIZE),
        ] {
            let mut request = refresh_client_size_request(control_pid, CONTROL_SIZE);
            request.control_size = None;
            if alias {
                request.flags_alias = Some(flags.to_owned());
            } else {
                request.flags = Some(flags.to_owned());
            }
            let response = handler
                .handle(Request::RefreshClient(Box::new(request)))
                .await;
            assert!(
                matches!(response, Response::RefreshClient(_)),
                "{response:?}"
            );
            assert_eq!(
                session_size(&handler, &session).await,
                expected,
                "alias={alias}, {flags}"
            );
            assert_eq!(
                control_client_size(&handler, control_pid).await,
                CONTROL_SIZE
            );
            {
                let clients = handler.active_control.lock().await;
                let active = clients.by_pid.get(&control_pid).unwrap();
                assert_eq!(
                    crate::handler::format_control_client_flags(active).contains("ignore-size"),
                    flags == "ignore-size"
                );
            }
            for events in [&mut control_events, &mut observer_events] {
                let layouts = settle_control_notifications(events)
                    .await
                    .into_iter()
                    .filter_map(|line| {
                        Some((
                            layout_change_window_id(&line)?,
                            layout_change_geometry(&line)?,
                        ))
                    })
                    .collect::<Vec<_>>();
                assert_eq!(
                    layouts,
                    vec![(window_id, format!("{}x{}", expected.cols, expected.rows))]
                );
            }
        }
    }
}

#[tokio::test]
async fn control_ignore_size_attach_refresh_and_switch_preserve_geometry() {
    // Issue #243 contract, frozen before the production port. A reported size
    // remains observable while ignore-size excludes it from window sizing.
    let handler = RequestHandler::new();
    let source = session_name("ignore-size-source");
    let target = session_name("ignore-size-target");
    create_session(&handler, source.clone(), INITIAL_SIZE).await;
    create_session(&handler, target.clone(), TARGET_SIZE).await;
    let requester_pid = std::process::id();
    let (_control_id, _events) =
        register_control_client_with_id(&handler, requester_pid, &source).await;

    let response = handler
        .handle(Request::AttachSessionExt2(Box::new(
            rmux_proto::request::AttachSessionExt2Request {
                target: Some(source.clone()),
                target_spec: None,
                detach_other_clients: false,
                kill_other_clients: false,
                read_only: false,
                skip_environment_update: true,
                flags: Some(vec!["ignore-size".to_owned()]),
                working_directory: None,
                client_terminal: rmux_proto::ClientTerminalContext::default(),
                client_size: None,
            },
        )))
        .await;
    assert!(!matches!(response, Response::Error(_)), "{response:?}");

    let response = handler
        .handle(Request::RefreshClient(Box::new(
            refresh_client_size_request(requester_pid, CONTROL_SIZE),
        )))
        .await;
    assert!(
        matches!(response, Response::RefreshClient(_)),
        "{response:?}"
    );
    assert_eq!(
        control_client_size(&handler, requester_pid).await,
        CONTROL_SIZE
    );
    assert_eq!(session_size(&handler, &source).await, INITIAL_SIZE);
    {
        let clients = handler.active_control.lock().await;
        let active = clients.by_pid.get(&requester_pid).unwrap();
        assert!(crate::handler::format_control_client_flags(active).contains("ignore-size"));
    }

    let response = handler
        .handle(Request::SwitchClient(SwitchClientRequest {
            target: target.clone(),
        }))
        .await;
    assert!(!matches!(response, Response::Error(_)), "{response:?}");
    assert_eq!(session_size(&handler, &target).await, TARGET_SIZE);

    let mut refresh = refresh_client_size_request(requester_pid, CONTROL_SIZE);
    refresh.flags = Some("!ignore-size".to_owned());
    let response = handler
        .handle(Request::RefreshClient(Box::new(refresh)))
        .await;
    assert!(
        matches!(response, Response::RefreshClient(_)),
        "{response:?}"
    );
    assert_eq!(session_size(&handler, &target).await, CONTROL_SIZE);

    let mut refresh = refresh_client_size_request(requester_pid, TARGET_SIZE);
    refresh.flags_alias = Some("ignore-size".to_owned());
    let response = handler
        .handle(Request::RefreshClient(Box::new(refresh)))
        .await;
    assert!(
        matches!(response, Response::RefreshClient(_)),
        "{response:?}"
    );
    assert_eq!(
        control_client_size(&handler, requester_pid).await,
        TARGET_SIZE
    );
    assert_eq!(session_size(&handler, &target).await, CONTROL_SIZE);
}
