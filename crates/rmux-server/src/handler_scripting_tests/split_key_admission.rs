use super::*;
use rmux_proto::{HandshakeRequest, SplitWindowCommandV1Request, SplitWindowCommandV1Response};

fn offer(arguments: &[&str], payload: Option<Vec<u8>>) -> SplitWindowCommandV1Request {
    SplitWindowCommandV1Request::new(
        arguments.iter().map(|arg| (*arg).to_owned()).collect(),
        None,
        payload,
    )
}

async fn send(
    handler: &RequestHandler,
    offer: SplitWindowCommandV1Request,
) -> SplitWindowCommandV1Response {
    let Response::SplitWindowCommandV1(result) = handler
        .handle(Request::SplitWindowCommandV1(Box::new(offer)))
        .await
    else {
        panic!("split offer result")
    };
    *result
}

#[tokio::test]
async fn sc2_admission_refuses_capability_and_compound_before_effects() {
    let handler = RequestHandler::new();
    let mut missing = offer(&["new-session", "-d", "-s", "must-not-exist"], None);
    missing.handshake = HandshakeRequest::current();
    let mut unsupported = missing.clone();
    unsupported.handshake = HandshakeRequest::requiring(["commands.split_window.key_input.v999"]);
    for request in [
        missing,
        unsupported,
        offer(&["new-session", "-d", "-s", "must-not-exist"], None),
        offer(
            &[
                "set-environment",
                "-g",
                "SC2_BAD",
                "yes;",
                "split-window",
                "-Ik",
            ],
            None,
        ),
        offer(&["SC2_BAD=yes", "split-window", "-Ik"], None),
    ] {
        assert!(matches!(
            send(&handler, request).await,
            SplitWindowCommandV1Response::NotAdmitted { .. }
        ));
        let state = handler.state.lock().await;
        assert!(state.sessions.is_empty());
        assert_eq!(state.environment.global_value("SC2_BAD"), None);
    }
}

#[tokio::test]
async fn sc2_admission_binary_input_and_key_are_one_empty_commit() {
    let handler = RequestHandler::new();
    let alpha = session_name("sc2-binary");
    handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))
        .await;
    let payload = vec![b'A', 0, 255, 27, b'[', b'3', b'1', b'm', b'B', b';', b'C'];
    let result = send(
        &handler,
        offer(
            &[
                "split-window",
                "-IkP",
                "-F",
                "#{pane_dead}:#{pane_pid}:#{remain-on-exit}",
                "-t",
                "sc2-binary:0.0",
            ],
            Some(payload),
        ),
    )
    .await;
    assert_eq!(
        result,
        SplitWindowCommandV1Response::Complete {
            stdout: b"0:0:key\n".to_vec(),
            error: None,
            exit_status: 0
        }
    );
    let state = handler.state.lock().await;
    let pane = state
        .sessions
        .session(&alpha)
        .unwrap()
        .window_at(0)
        .unwrap()
        .pane(1)
        .unwrap();
    assert!(!state.pane_is_dead(&alpha, pane.id()));
    assert_eq!(
        state
            .options
            .resolve_for_pane(&alpha, 0, 1, OptionName::RemainOnExit),
        Some("key")
    );
}

#[tokio::test]
async fn sc2_admission_wrong_payload_shape_and_syntax_do_not_split() {
    let handler = RequestHandler::new();
    let alpha = session_name("sc2-shape");
    handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))
        .await;
    for argv in [
        &["split-window", "-k"][..],
        &["split-window", "-Ik", "true"][..],
    ] {
        assert!(matches!(
            send(&handler, offer(argv, Some(vec![0, 255]))).await,
            SplitWindowCommandV1Response::NotAdmitted { .. }
        ));
    }
    assert!(matches!(
        send(&handler, offer(&["split-window", "-kQ"], None)).await,
        SplitWindowCommandV1Response::Complete {
            error: Some(_),
            exit_status: 1,
            ..
        }
    ));
    assert_eq!(
        handler
            .state
            .lock()
            .await
            .sessions
            .session(&alpha)
            .unwrap()
            .window_at(0)
            .unwrap()
            .pane_count(),
        1
    );
}

#[tokio::test]
async fn sc2_api_boolean_keep_remains_on_off_and_none_inherits() {
    for entry in 0..3 {
        for (keep, expected) in [(Some(true), "on"), (Some(false), "off"), (None, "key")] {
            let handler = RequestHandler::new();
            let alpha = session_name("sc2-api");
            handler
                .handle(Request::NewSession(NewSessionRequest {
                    session_name: alpha.clone(),
                    detached: true,
                    size: Some(TerminalSize { cols: 80, rows: 24 }),
                    environment: None,
                }))
                .await;
            handler
                .handle(Request::SetOption(SetOptionRequest {
                    scope: ScopeSelector::Global,
                    option: OptionName::RemainOnExit,
                    value: "key".to_owned(),
                    mode: SetOptionMode::Replace,
                }))
                .await;
            let request = rmux_proto::SplitWindowExtRequest {
                target: rmux_proto::SplitWindowTarget::Pane(PaneTarget::new(alpha.clone(), 0)),
                direction: rmux_proto::SplitDirection::Vertical,
                before: false,
                environment: None,
                command: None,
                process_command: None,
                start_directory: None,
                keep_alive_on_exit: keep,
                detached: false,
                size: None,
                preserve_zoom: false,
                full_size: false,
                stdin_payload: Some(vec![]),
            };
            let request = if entry == 0 {
                Request::SplitWindowExt(Box::new(request))
            } else {
                let action = rmux_proto::SplitWindowTargetActionRequest {
                    target: Some(format!("{}:0.0", alpha.as_str())),
                    direction: request.direction,
                    before: request.before,
                    environment: request.environment,
                    command: request.command,
                    process_command: request.process_command,
                    start_directory: request.start_directory,
                    keep_alive_on_exit: request.keep_alive_on_exit,
                    detached: request.detached,
                    size: request.size,
                    preserve_zoom: request.preserve_zoom,
                    full_size: request.full_size,
                    stdin_payload: request.stdin_payload,
                };
                if entry == 1 {
                    Request::SplitWindowTargetAction(Box::new(action))
                } else {
                    Request::SplitWindowIdentity(Box::new(rmux_proto::SplitWindowIdentityRequest {
                        action,
                    }))
                }
            };
            assert!(matches!(
                handler.handle(request).await,
                Response::SplitWindow(_) | Response::SplitWindowIdentity(_)
            ));
            assert_eq!(
                handler.state.lock().await.options.resolve_for_pane(
                    &alpha,
                    0,
                    1,
                    OptionName::RemainOnExit
                ),
                Some(expected)
            );
        }
    }
}

#[tokio::test]
async fn sc2_admission_revalidates_captured_target_before_policy_and_payload_commit() {
    let handler = RequestHandler::new();
    let alpha = session_name("sc2-target-race");
    handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))
        .await;
    let target = PaneTarget::new(alpha.clone(), 0);
    let pause = crate::handler::scripting_support::install_queue_exact_target_capture_pause(
        &handler,
        "split-window",
    );
    let queued_handler = handler.clone();
    let queued = tokio::spawn(async move {
        send(
            &queued_handler,
            offer(
                &["split-window", "-Ik", "-t", "sc2-target-race:0.0"],
                Some(b"must-not-publish".to_vec()),
            ),
        )
        .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(3), pause.reached.notified())
        .await
        .unwrap();
    handler
        .handle(Request::RespawnPane(Box::new(
            rmux_proto::RespawnPaneRequest {
                target,
                kill: true,
                start_directory: None,
                environment: None,
                command: Some(vec![crate::test_shell::stdin_discard_command()]),
                process_command: None,
            },
        )))
        .await;
    pause.release.notify_one();
    let result = tokio::time::timeout(std::time::Duration::from_secs(3), queued)
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(
            result,
            SplitWindowCommandV1Response::Complete {
                error: Some(_),
                exit_status: 1,
                ..
            }
        ),
        "{result:?}"
    );
    assert_eq!(
        handler
            .state
            .lock()
            .await
            .sessions
            .session(&alpha)
            .unwrap()
            .window()
            .pane_count(),
        1
    );
}
