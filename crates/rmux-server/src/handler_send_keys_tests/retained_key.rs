use super::*;
use crate::handler::test_support::SERVER_SIGNAL_LIVENESS;
use rmux_proto::{SessionName, SplitWindowExtRequest};

async fn dead_key_pane(handler: &RequestHandler, session: &SessionName) -> (PaneTarget, PaneId) {
    create_send_keys_test_session(handler, session).await;
    split_dead_key_pane(handler, session).await
}

async fn split_dead_key_pane(
    handler: &RequestHandler,
    session: &SessionName,
) -> (PaneTarget, PaneId) {
    let response = handler
        .handle(Request::SplitWindowExt(Box::new(SplitWindowExtRequest {
            target: SplitWindowTarget::Pane(PaneTarget::with_window(session.clone(), 0, 0)),
            direction: SplitDirection::Vertical,
            before: false,
            environment: None,
            command: Some(vec!["exit 7".into()]),
            process_command: None,
            start_directory: None,
            keep_alive_on_exit: Some(true),
            detached: false,
            size: None,
            preserve_zoom: false,
            full_size: false,
            stdin_payload: None,
        })))
        .await;
    let Response::SplitWindow(split) = response else {
        panic!("{response:?}")
    };
    let target = split.pane;
    let deadline = tokio::time::Instant::now() + SERVER_SIGNAL_LIVENESS;
    let id = loop {
        let state = handler.state.lock().await;
        let id = state
            .sessions
            .session(session)
            .unwrap()
            .window_at(0)
            .unwrap()
            .pane(target.pane_index())
            .unwrap()
            .id();
        if state.pane_is_dead(session, id) {
            break id;
        }
        drop(state);
        assert!(
            tokio::time::Instant::now() < deadline,
            "the kept `exit 7` pane never published its death"
        );
        sleep(Duration::from_millis(10)).await;
    };
    assert!(matches!(
        handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Pane(target.clone()),
                option: OptionName::RemainOnExit,
                value: "key".into(),
                mode: SetOptionMode::Replace,
            }))
            .await,
        Response::SetOption(_)
    ));
    (target, id)
}

struct FocusedDeadKeyFixture {
    live: PaneTarget,
    live_id: PaneId,
    live_generation: u64,
    dead: PaneTarget,
    dead_id: PaneId,
}

async fn focused_dead_key_fixture(
    handler: &RequestHandler,
    session: &SessionName,
) -> FocusedDeadKeyFixture {
    create_quiet_input_session(handler, session).await;
    let (dead, dead_id) = split_dead_key_pane(handler, session).await;
    let live = PaneTarget::with_window(session.clone(), 0, 0);
    let (live_id, live_generation) = {
        let mut state = handler.state.lock().await;
        state
            .append_bytes_to_pane_transcript_for_test(session, 0, 0, b"\x1b[?1004h")
            .expect("live pane enables focus reporting");
        let transcript = state.transcript_handle(&live).unwrap();
        assert_ne!(transcript.lock().mode() & mode::MODE_FOCUSON, 0);
        let window = state.sessions.session(session).unwrap().window();
        let live_id = window.pane(0).unwrap().id();
        assert_eq!(window.active_pane().unwrap().id(), dead_id);
        assert_eq!(window.pane(dead.pane_index()).unwrap().id(), dead_id);
        let generation = state.pane_output_generation_for_target(&live, live_id);
        state.start_pane_input_capture_for_test(&live);
        (live_id, generation)
    };
    FocusedDeadKeyFixture {
        live,
        live_id,
        live_generation,
        dead,
        dead_id,
    }
}

async fn assert_dead_key_preinput(
    handler: &RequestHandler,
    session: &SessionName,
    fixture: &FocusedDeadKeyFixture,
    synchronized: bool,
) {
    let state = handler.state.lock().await;
    let window = state.sessions.session(session).unwrap().window();
    assert_eq!(window.pane(0).unwrap().id(), fixture.live_id);
    assert_eq!(
        window.pane(fixture.dead.pane_index()).unwrap().id(),
        fixture.dead_id
    );
    assert_eq!(window.active_pane().unwrap().id(), fixture.dead_id);
    assert!(state.pane_is_dead(session, fixture.dead_id));
    assert_eq!(
        state.pane_output_generation_for_target(&fixture.live, fixture.live_id),
        fixture.live_generation
    );
    assert_ne!(
        state
            .transcript_handle(&fixture.live)
            .unwrap()
            .lock()
            .mode()
            & mode::MODE_FOCUSON,
        0
    );
    assert_eq!(
        state.pane_input_capture_for_test(&fixture.live),
        Some(Vec::new()),
        "registration targets the active Dead/key pane, not the live survivor"
    );
    if synchronized {
        assert_eq!(
            state
                .options
                .resolve_for_window(session, 0, OptionName::SynchronizePanes),
            Some("on")
        );
    }
}

async fn assert_dead_key_postinput(
    handler: &RequestHandler,
    session: &SessionName,
    fixture: &FocusedDeadKeyFixture,
    expected: &[u8],
) {
    let state = handler.state.lock().await;
    let window = state.sessions.session(session).unwrap().window();
    assert!(!window
        .panes()
        .iter()
        .any(|pane| pane.id() == fixture.dead_id));
    assert_eq!(window.pane_count(), 1);
    assert_eq!(window.pane(0).unwrap().id(), fixture.live_id);
    assert_eq!(window.active_pane().unwrap().id(), fixture.live_id);
    assert_eq!(
        state.pane_output_generation_for_target(&fixture.live, fixture.live_id),
        fixture.live_generation
    );
    assert_eq!(
        state.pane_input_capture_for_test(&fixture.live),
        Some(expected.to_vec())
    );
}

#[tokio::test]
async fn sc2_consumer_attached_press_removes_dead_key_without_forwarding() {
    let handler = RequestHandler::new();
    let session = session_name("sc2-consumer");
    let fixture = focused_dead_key_fixture(&handler, &session).await;
    let pid = 961201;
    let (tx, _rx) = mpsc::unbounded_channel();
    handler.register_attach(pid, session.clone(), tx).await;
    assert_dead_key_preinput(&handler, &session, &fixture, false).await;
    handler
        .handle_attached_live_input(pid, &mut Vec::new(), b"x")
        .await
        .unwrap();
    assert_dead_key_postinput(&handler, &session, &fixture, b"\x1b[I").await;
}

#[tokio::test]
async fn sc2_consumer_burst_consumes_only_closing_press_before_sync_fanout() {
    let handler = RequestHandler::new();
    let session = session_name("sc2-burst");
    let fixture = focused_dead_key_fixture(&handler, &session).await;
    handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Window(rmux_proto::WindowTarget::with_window(session.clone(), 0)),
            option: OptionName::SynchronizePanes,
            value: "on".into(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    let pid = 961202;
    let (tx, rx) = mpsc::unbounded_channel();
    handler.register_attach(pid, session.clone(), tx).await;
    spawn_accounted_attach_control_drain(&handler, pid, rx).await;
    assert_dead_key_preinput(&handler, &session, &fixture, true).await;
    handler
        .handle_attached_live_input(pid, &mut Vec::new(), b"xy")
        .await
        .unwrap();
    assert_dead_key_postinput(&handler, &session, &fixture, b"\x1b[Iy").await;
}

#[tokio::test]
async fn sc2_consumer_acquired_remainders_never_close_or_leak_after_consumed_press() {
    let handler = RequestHandler::new();
    let session = session_name("sc2-held");
    let fixture = focused_dead_key_fixture(&handler, &session).await;
    let pid = 961203;
    let (tx, rx) = mpsc::unbounded_channel();
    handler.register_attach(pid, session.clone(), tx).await;
    spawn_accounted_attach_control_drain(&handler, pid, rx).await;
    assert_dead_key_preinput(&handler, &session, &fixture, false).await;
    handler
        .handle_attached_live_input(pid, &mut Vec::new(), b"\x1b[120;1:2u\x1b[120;1:3u")
        .await
        .unwrap();
    assert!(handler
        .state
        .lock()
        .await
        .pane_is_dead(&session, fixture.dead_id));
    assert_dead_key_preinput(&handler, &session, &fixture, false).await;
    handler
        .handle_attached_live_input(
            pid,
            &mut Vec::new(),
            b"\x1b[120;1:1u\x1b[120;1:2u\x1b[120;1:3u",
        )
        .await
        .unwrap();
    assert_dead_key_postinput(&handler, &session, &fixture, b"\x1b[I").await;
}

#[tokio::test]
async fn sc2_consumer_reports_paste_and_send_keys_do_not_dismiss() {
    let handler = RequestHandler::new();
    let session = session_name("sc2-reports");
    let (target, id) = dead_key_pane(&handler, &session).await;
    let pid = 961204;
    let (tx, rx) = mpsc::unbounded_channel();
    handler.register_attach(pid, session.clone(), tx).await;
    spawn_accounted_attach_control_drain(&handler, pid, rx).await;
    for input in [
        b"\x1b[I".as_slice(),
        b"\x1b[O",
        b"\x1b[?1;2c",
        b"\x1b[>0;276;0c",
        b"\x1b[?997;1n",
        b"\x1b]10;rgb:ffff/ffff/ffff\x07",
        b"\x1b[200~x\x1b[201~",
    ] {
        handler
            .handle_attached_live_input(pid, &mut Vec::new(), input)
            .await
            .unwrap();
        assert!(
            handler.state.lock().await.pane_is_dead(&session, id),
            "{input:?}"
        );
    }
    handler
        .handle(Request::SendKeys(SendKeysRequest {
            target,
            keys: vec!["x".into()],
        }))
        .await;
    assert!(handler.state.lock().await.pane_is_dead(&session, id));
}

#[tokio::test]
async fn sc2_consumer_readonly_and_bound_keys_are_not_closing_presses() {
    let handler = RequestHandler::new();
    let session = session_name("sc2-readonly-binding");
    let (_, id) = dead_key_pane(&handler, &session).await;
    let pid = 961205;
    let (tx, rx) = mpsc::unbounded_channel();
    handler.register_attach(pid, session.clone(), tx).await;
    spawn_accounted_attach_control_drain(&handler, pid, rx).await;
    {
        let mut attaches = handler.active_attach.lock().await;
        attaches
            .by_pid
            .get_mut(&pid)
            .unwrap()
            .flags
            .insert(crate::client_flags::ClientFlags::READONLY);
    }
    handler
        .handle_attached_live_input(pid, &mut Vec::new(), b"x\x1b[120;1u")
        .await
        .unwrap();
    assert!(handler.state.lock().await.pane_is_dead(&session, id));
    {
        let mut attaches = handler.active_attach.lock().await;
        attaches
            .by_pid
            .get_mut(&pid)
            .unwrap()
            .flags
            .remove(crate::client_flags::ClientFlags::READONLY);
    }
    handler
        .handle(Request::BindKey(Box::new(BindKeyRequest {
            table_name: "root".into(),
            key: "x".into(),
            note: None,
            repeat: false,
            command: Some(vec!["send-keys".into(), "x".into()]),
        })))
        .await;
    handler
        .handle_attached_live_input(pid, &mut Vec::new(), b"x\x1b[120;1u")
        .await
        .unwrap();
    assert!(handler.state.lock().await.pane_is_dead(&session, id));
}

#[tokio::test]
async fn sc2_consumer_retirement_does_not_invent_process_or_command_hooks() {
    let handler = RequestHandler::new();
    let session = session_name("sc2-hooks");
    dead_key_pane(&handler, &session).await;
    for hook in [
        HookName::PaneDied,
        HookName::PaneExited,
        HookName::AfterKillPane,
    ] {
        handler
            .handle(Request::SetHook(SetHookRequest {
                scope: ScopeSelector::Global,
                hook,
                command: "set-environment -g SC2_BAD_HOOK yes".into(),
                lifecycle: HookLifecycle::Persistent,
            }))
            .await;
    }
    let pid = 961206;
    let (tx, rx) = mpsc::unbounded_channel();
    handler.register_attach(pid, session.clone(), tx).await;
    spawn_accounted_attach_control_drain(&handler, pid, rx).await;
    handler
        .handle_attached_live_input(pid, &mut Vec::new(), b"x")
        .await
        .unwrap();
    let state = handler.state.lock().await;
    assert_eq!(
        state
            .sessions
            .session(&session)
            .unwrap()
            .window()
            .pane_count(),
        1
    );
    assert_eq!(state.environment.global_value("SC2_BAD_HOOK"), None);
}

#[tokio::test]
async fn sc2_consumer_timed_out_escape_is_an_ordinary_closing_press() {
    let handler = RequestHandler::new();
    let session = session_name("sc2-escape");
    dead_key_pane(&handler, &session).await;
    let pid = 961207;
    let (tx, rx) = mpsc::unbounded_channel();
    handler.register_attach(pid, session.clone(), tx).await;
    spawn_accounted_attach_control_drain(&handler, pid, rx).await;
    let mut pending = Vec::new();
    handler
        .handle_attached_live_input(pid, &mut pending, b"\x1b")
        .await
        .unwrap();
    assert_eq!(
        handler
            .state
            .lock()
            .await
            .sessions
            .session(&session)
            .unwrap()
            .window()
            .pane_count(),
        2
    );
    handler
        .flush_attached_pending_escape_input(pid, &mut pending)
        .await
        .unwrap();
    assert_eq!(
        handler
            .state
            .lock()
            .await
            .sessions
            .session(&session)
            .unwrap()
            .window()
            .pane_count(),
        1
    );
}
