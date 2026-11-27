//! SC1-R1: the selected pane can have a transcript without a native terminal.
use super::super::attach_support::{
    attach_target_for_session, attach_target_for_session_switch, AttachSessionRenderOptions,
    AttachSessionSwitchRenderOptions,
};
use super::super::client_support::SwitchTargetSelection;
use super::*;
use rmux_core::command_parser::CommandParser;
use rmux_proto::{AttachSessionRequest, RespawnPaneRequest, SplitDirection};

const PAYLOAD: &[u8] = b"EMPTY-ATTACH-SNAPSHOT\n";

async fn create_empty(handler: &RequestHandler, session: &SessionName) -> PaneTarget {
    create_quiet_session(handler, session).await;
    let response = handler
        .handle(Request::SplitWindowExt(Box::new(SplitWindowExtRequest {
            target: SplitWindowTarget::Session(session.clone()),
            direction: SplitDirection::Vertical,
            before: false,
            environment: None,
            command: None,
            process_command: None,
            start_directory: None,
            keep_alive_on_exit: None,
            detached: false,
            size: None,
            preserve_zoom: false,
            full_size: false,
            stdin_payload: Some(PAYLOAD.to_vec()),
        })))
        .await;
    match response {
        Response::SplitWindow(response) => response.pane,
        other => panic!("empty split: {other:?}"),
    }
}

fn assert_empty_target(target: &crate::pane_io::AttachTarget) {
    assert!(target.pane_master.is_none(), "no fabricated master");
    assert!(target.active_pane_geometry.cols() > 0);
    assert!(target.active_pane_geometry.rows() > 0);
    let frame = String::from_utf8_lossy(&target.render_frame);
    assert!(frame.contains("EMPTY-ATTACH-SNAPSHOT"), "{frame:?}");
}

fn extended_attach(session: &SessionName, spec: &str) -> AttachSessionExt2Request {
    AttachSessionExt2Request {
        target: Some(session.clone()),
        target_spec: Some(spec.to_owned()),
        detach_other_clients: false,
        kill_other_clients: false,
        read_only: false,
        skip_environment_update: false,
        flags: None,
        working_directory: None,
        client_terminal: rmux_proto::ClientTerminalContext::default(),
        client_size: Some(TerminalSize {
            cols: 100,
            rows: 40,
        }),
    }
}

#[tokio::test]
async fn empty_attach_initial_typed_upgrade_keeps_snapshot_and_live_subscription() {
    let handler = RequestHandler::new();
    let alpha = session_name("empty-attach-typed");
    let pane = create_empty(&handler, &alpha).await;
    let outcome = handler
        .dispatch(
            95_801,
            Request::AttachSession(AttachSessionRequest {
                target: alpha.clone(),
            }),
        )
        .await;
    assert!(
        matches!(outcome.response, Response::AttachSession(_)),
        "{:?}",
        outcome.response
    );
    let mut target = outcome.attach.expect("initial upgrade").target;
    assert_empty_target(&target);
    let sender = {
        let state = handler.state.lock().await;
        assert_eq!(
            state
                .pane_pid_in_window(&alpha, 0, pane.pane_index())
                .unwrap(),
            0
        );
        assert!(state
            .clone_pane_master(&alpha, 0, pane.pane_index())
            .is_err());
        state
            .pane_output_for_target(&alpha, 0, pane.pane_index())
            .unwrap()
    };
    let sequence = sender.send(b"after-snapshot".to_vec());
    assert_eq!(sequence, target.pane_output_start_sequence);
    let event = tokio::time::timeout(ATTACH_LIFECYCLE_TIMEOUT, target.pane_output.recv())
        .await
        .expect("subscription remains live");
    let rmux_core::events::OutputCursorItem::Event(event) = event else {
        panic!("expected live output")
    };
    assert_eq!(event.bytes(), b"after-snapshot");
}

#[tokio::test]
async fn empty_attach_window_and_linked_initial_render_capabilities() {
    for render_stream in [false, true] {
        let handler = RequestHandler::new();
        let alpha = session_name("empty-attach-source");
        let beta = session_name("empty-attach-alias");
        create_empty(&handler, &alpha).await;
        create_quiet_session(&handler, &beta).await;
        assert!(matches!(
            handler
                .handle(Request::LinkWindow(LinkWindowRequest {
                    source: WindowTarget::with_window(alpha.clone(), 0),
                    target: WindowTarget::with_window(beta.clone(), 5),
                    after: false,
                    before: false,
                    kill_destination: false,
                    detached: true,
                }))
                .await,
            Response::LinkWindow(_)
        ));
        for (session, window) in [(&alpha, 0), (&beta, 5)] {
            let spec = format!("{session}:{window}");
            let ext = extended_attach(session, &spec);
            let request = if render_stream {
                Request::AttachSessionExt3(Box::new(AttachSessionExt3Request::from_ext2(
                    ext,
                    vec![CAPABILITY_ATTACH_RENDER.to_owned()],
                )))
            } else {
                Request::AttachSessionExt2(Box::new(ext))
            };
            let outcome = handler.dispatch(95_802, request).await;
            assert!(
                matches!(outcome.response, Response::AttachSession(_)),
                "{:?}",
                outcome.response
            );
            let upgrade = outcome.attach.expect("window/linked upgrade");
            assert_eq!(upgrade.render_stream, render_stream);
            assert_empty_target(&upgrade.target);
            assert!(upgrade.target.live_pane.is_some());
        }
    }
}

#[tokio::test]
async fn empty_attach_attached_switch_uses_requested_pane_not_previous_selection() {
    for use_attach in [false, true] {
        let handler = RequestHandler::new();
        let alpha = session_name("empty-attach-from");
        let beta = session_name("empty-attach-to");
        create_quiet_session(&handler, &alpha).await;
        create_empty(&handler, &beta).await;
        let (tx, mut rx) = mpsc::unbounded_channel();
        handler.register_attach(95_803, alpha, tx).await;
        let request = if use_attach {
            Request::AttachSession(AttachSessionRequest {
                target: beta.clone(),
            })
        } else {
            Request::SwitchClient(SwitchClientRequest {
                target: beta.clone(),
            })
        };
        let outcome = handler.dispatch(95_803, request).await;
        assert!(
            matches!(outcome.response, Response::SwitchClient(_)),
            "{:?}",
            outcome.response
        );
        assert_empty_target(&recv_switch_target(&mut rx, "switch into Empty").await);
        drain_attach_controls(&mut rx);

        // Pin the pre-commit constructor itself. The canonical selection stays
        // Empty while the presentation requests either pane; ordinary refresh
        // controls emitted around a dispatch are not master handovers.
        let state = handler.state.lock().await;
        let session = state.sessions.session(&beta).unwrap();
        assert_eq!(session.active_pane_index(), 1);
        for index in [0, 1] {
            let selection = SwitchTargetSelection::Pane {
                target: PaneTarget::with_window(beta.clone(), 0, index),
                window_id: session.window().id(),
                pane_id: session.window().pane(index).unwrap().id(),
                pane_output_generation: None,
                zoom: false,
            };
            let target = attach_target_for_session_switch(
                &state,
                &beta,
                AttachSessionSwitchRenderOptions {
                    attached_count: 1,
                    key_table: None,
                    previous_title: None,
                    client: None,
                    terminal_context: &OuterTerminalContext::default(),
                    socket_path: &handler.socket_path(),
                    render_stream: false,
                    selection: Some(&selection),
                    window_size_override: None,
                    client_loop_snapshots: None,
                },
            )
            .expect("staged switch target");
            assert_eq!(target.pane_master.is_some(), index == 0);
            if index == 1 {
                assert_empty_target(&target);
            }
        }
    }
}

#[tokio::test]
async fn empty_attach_control_command_binds_session_without_native_master() {
    let handler = RequestHandler::new();
    let alpha = session_name("empty-attach-control");
    create_empty(&handler, &alpha).await;
    let (tx, _rx) = mpsc::channel(crate::control::CONTROL_SERVER_EVENT_CAPACITY);
    let id = handler
        .register_control_with_closing(
            95_804,
            crate::control::ControlModeUpgrade {
                mode: rmux_proto::ControlMode::Plain,
                terminal_context: OuterTerminalContext::default(),
                initial_command_count: 0,
            },
            tx,
            Arc::new(AtomicBool::new(false)),
        )
        .await;
    let commands = CommandParser::new()
        .parse(&format!("attach-session -t {alpha}"))
        .unwrap();
    let result = handler
        .execute_control_commands_identity(95_804, id, commands)
        .await;
    assert!(result.error.is_none(), "{:?}", result.error);
    let controls = handler.active_control.lock().await;
    assert_eq!(
        controls.by_pid.get(&95_804).unwrap().session_name.as_ref(),
        Some(&alpha)
    );
}

#[tokio::test]
async fn empty_attach_after_respawn_requires_the_real_master_again() {
    let handler = RequestHandler::new();
    let alpha = session_name("empty-attach-respawn");
    let pane = create_empty(&handler, &alpha).await;
    assert!(matches!(
        handler
            .handle(Request::RespawnPane(Box::new(RespawnPaneRequest {
                target: pane,
                kill: false,
                start_directory: None,
                environment: None,
                command: Some(quiet_attached_command()),
                process_command: None,
            })))
            .await,
        Response::RespawnPane(_)
    ));
    let outcome = handler
        .dispatch(
            95_805,
            Request::AttachSession(AttachSessionRequest { target: alpha }),
        )
        .await;
    assert!(
        matches!(outcome.response, Response::AttachSession(_)),
        "{:?}",
        outcome.response
    );
    assert!(outcome.attach.unwrap().target.pane_master.is_some());
}

#[tokio::test]
async fn empty_attach_missing_lifecycle_does_not_suppress_a_clone_error() {
    let handler = RequestHandler::new();
    let alpha = session_name("empty-attach-clone-error");
    create_empty(&handler, &alpha).await;
    let mut state = handler.state.lock().await;
    let id = state
        .sessions
        .session(&alpha)
        .unwrap()
        .active_pane()
        .unwrap()
        .id();
    // Keep the transcript and resource-less terminal but remove its explicit
    // Empty authority. PID zero / clone failure alone must never grant attach.
    state.forget_pane_lifecycle_for_test(id);
    let expected = state.active_pane_master(&alpha).unwrap_err();
    let actual = attach_target_for_session(
        &state,
        &alpha,
        AttachSessionRenderOptions {
            attached_count: 1,
            terminal_context: &OuterTerminalContext::default(),
            previous_title: None,
            client: None,
            socket_path: &handler.socket_path(),
            client_loop_snapshots: None,
        },
    )
    .unwrap_err();
    assert_eq!(actual, expected);
    assert!(actual.to_string().contains("failed to clone pane terminal"));
}
