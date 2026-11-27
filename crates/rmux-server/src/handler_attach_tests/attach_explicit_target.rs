//! SC1-N1: an attached attach-session must commit its explicit window/pane.
//! Native tmux 3.7b measurements are retained in the `acquired` record and the
//! bounded N1 matrix. A session-only target still keeps its current pane.
use super::*;

const PID: u32 = 96_101;

async fn fixture(handler: &RequestHandler) -> (SessionName, SessionName) {
    let alpha = session_name("attach-explicit-alpha");
    let beta = session_name("attach-explicit-beta");
    create_quiet_session(handler, &alpha).await;
    create_quiet_session(handler, &beta).await;
    let commands = handler
        .parse_control_commands(&format!(
            "split-window -I -t {alpha}:0.0 ; link-window -d -s {alpha}:0 -t {beta}:5"
        ))
        .await
        .unwrap();
    handler
        .execute_parsed_commands_for_test(std::process::id(), commands)
        .await
        .expect("Empty split and linked alias");
    assert_eq!(selected(handler, &alpha).await, (0, 1));
    (alpha, beta)
}

async fn selected(handler: &RequestHandler, session: &SessionName) -> (u32, u32) {
    let state = handler.state.lock().await;
    let session = state.sessions.session(session).unwrap();
    (
        session.active_window_index(),
        session.window().active_pane().unwrap().index(),
    )
}

fn attach(session: &SessionName, spec: &str, render: bool) -> Request {
    let request = AttachSessionExt2Request {
        target: Some(session.clone()),
        target_spec: Some(spec.to_owned()),
        detach_other_clients: false,
        kill_other_clients: false,
        read_only: false,
        skip_environment_update: true,
        flags: None,
        working_directory: None,
        client_terminal: rmux_proto::ClientTerminalContext::default(),
        client_size: Some(TerminalSize { cols: 80, rows: 24 }),
    };
    if render {
        Request::AttachSessionExt3(Box::new(AttachSessionExt3Request::from_ext2(
            request,
            vec![CAPABILITY_ATTACH_RENDER.to_owned()],
        )))
    } else {
        Request::AttachSessionExt2(Box::new(request))
    }
}

async fn register(
    handler: &RequestHandler,
    session: &SessionName,
) -> mpsc::UnboundedReceiver<AttachControl> {
    let (tx, rx) = mpsc::unbounded_channel();
    handler.register_attach(PID, session.clone(), tx).await;
    rx
}

#[test]
fn attach_explicit_target_prompt_and_queue_commit_pane_and_continuation() {
    // Pin Rust's default test stack even when a release gate supplies a larger one.
    std::thread::Builder::new()
        .name("attach-explicit-target-stack".to_owned())
        .stack_size(2 * 1024 * 1024)
        .spawn(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .expect("runtime builds")
                .block_on(exercise_attach_explicit_target_prompt_queue_binding());
        })
        .expect("stack regression thread starts")
        .join()
        .expect("stack regression thread completes");
}

async fn exercise_attach_explicit_target_prompt_queue_binding() {
    for entry in ["prompt", "queue", "binding"] {
        let handler = RequestHandler::new();
        let (alpha, _) = fixture(&handler).await;
        let _rx = register(&handler, &alpha).await;
        let command =
            format!("attach-session -t {alpha}:0.0 ; set-buffer -b n1-tail '#{{pane_index}}'");
        match entry {
            "prompt" => {
                for input in [b"\x02:".as_slice(), command.as_bytes(), b"\r"] {
                    handler
                        .handle_attached_live_input_for_test(PID, input)
                        .await
                        .unwrap();
                }
            }
            "binding" => {
                let commands = handler
                    .parse_control_commands(&format!("bind-key x {{ {command} }}"))
                    .await
                    .unwrap();
                handler
                    .execute_parsed_commands_for_test(std::process::id(), commands)
                    .await
                    .unwrap();
                handler
                    .handle_attached_live_input_for_test(PID, b"\x02x")
                    .await
                    .unwrap();
            }
            "queue" => {
                let identity = handler.active_attach_identity_for_test(PID).await;
                let commands = handler.parse_control_commands(&command).await.unwrap();
                crate::handler::with_expected_attach_and_session_identity(
                    identity,
                    alpha.clone(),
                    identity.session_id(),
                    handler.execute_parsed_commands_for_test(PID, commands),
                )
                .await
                .unwrap();
            }
            _ => unreachable!(),
        }
        // The tail proves asynchronous prompt/binding execution completed.
        let deadline = tokio::time::Instant::now() + ATTACH_LIFECYCLE_TIMEOUT;
        loop {
            let response = handler
                .handle(Request::ShowBuffer(rmux_proto::ShowBufferRequest {
                    name: Some("n1-tail".to_owned()),
                }))
                .await;
            if matches!(response, Response::ShowBuffer(_)) {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "{entry}: missing queue tail"
            );
            sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(selected(&handler, &alpha).await, (0, 0), "{entry}");
    }
}

#[tokio::test]
async fn attach_explicit_target_typed_window_pane_alias_and_session_witness() {
    for render in [false, true] {
        for suffix in ["", ":5", ":5.0"] {
            let handler = RequestHandler::new();
            let (alpha, beta) = fixture(&handler).await;
            let _rx = register(&handler, &alpha).await;
            let response = handler
                .dispatch(PID, attach(&beta, &format!("{beta}{suffix}"), render))
                .await
                .response;
            assert!(
                matches!(response, Response::SwitchClient(_)),
                "{response:?}"
            );
            let expected = match suffix {
                "" => (0, 0),
                ":5" => (5, 1),
                _ => (5, 0),
            };
            assert_eq!(
                selected(&handler, &beta).await,
                expected,
                "{suffix} render={render}"
            );
            let active = handler.active_attach.lock().await;
            assert_eq!(active.by_pid.get(&PID).unwrap().session_name, beta);
        }
    }
}

#[tokio::test]
async fn attach_explicit_target_initial_upgrade_keeps_window_and_pane() {
    for render in [false, true] {
        let handler = RequestHandler::new();
        let (_, beta) = fixture(&handler).await;
        let outcome = handler
            .dispatch(PID, attach(&beta, &format!("{beta}:5.0"), render))
            .await;
        assert!(
            matches!(outcome.response, Response::AttachSession(_)),
            "{:?}",
            outcome.response
        );
        assert!(outcome.attach.is_some());
        assert_eq!(selected(&handler, &beta).await, (5, 0));
    }
}

#[tokio::test]
async fn attach_explicit_target_control_queue_preserves_selection() {
    for suffix in [":5", ":5.0"] {
        let handler = RequestHandler::new();
        let (alpha, beta) = fixture(&handler).await;
        let (tx, _rx) = mpsc::channel(crate::control::CONTROL_SERVER_EVENT_CAPACITY);
        let id = handler
            .register_control_with_closing(
                PID,
                crate::control::ControlModeUpgrade {
                    mode: rmux_proto::ControlMode::Plain,
                    terminal_context: OuterTerminalContext::default(),
                    initial_command_count: 0,
                },
                tx,
                Arc::new(AtomicBool::new(false)),
            )
            .await;
        handler.set_control_session(PID, Some(alpha)).await.unwrap();
        let commands = handler.parse_control_commands(&format!(
            "attach-session -t {beta}{suffix} ; display-message -p '#{{window_index}}.#{{pane_index}}'"
        )).await.unwrap();
        let result = handler
            .execute_control_commands_identity(PID, id, commands)
            .await;
        assert!(result.error.is_none(), "{result:?}");
        let expected = if suffix == ":5" { (5, 1) } else { (5, 0) };
        assert_eq!(selected(&handler, &beta).await, expected);
        assert_eq!(
            result.stdout,
            format!("{}.{}\n", expected.0, expected.1).into_bytes()
        );
    }
}
