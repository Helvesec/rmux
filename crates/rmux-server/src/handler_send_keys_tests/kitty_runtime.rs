use super::*;

pub(super) async fn setup(
    label: &str,
) -> (RequestHandler, rmux_proto::SessionName, u32, PaneTarget) {
    let handler = RequestHandler::new();
    let session = session_name(label);
    #[cfg(windows)]
    handler
        .state
        .lock()
        .await
        .options
        .set(
            ScopeSelector::Global,
            OptionName::DefaultShell,
            "powershell.exe".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("explicit non-cmd runtime test shell is valid");
    create_send_keys_test_session(&handler, &session).await;
    // These fixtures assert exact key encodings across detach/reattach and do
    // not claim focus. setup_quiet_focused_capture is the fixture that does,
    // and it keeps enabling 1004 and asserting the edge itself.
    handler
        .disable_pane_focus_reports_for_test(&PaneTarget::new(session.clone(), 0))
        .await;
    let pid = std::process::id();
    let (tx, rx) = mpsc::unbounded_channel();
    handler.register_attach(pid, session.clone(), tx).await;
    spawn_accounted_attach_control_drain(&handler, pid, rx).await;
    let target = PaneTarget::new(session.clone(), 0);
    handler
        .state
        .lock()
        .await
        .start_pane_input_capture_for_test(&target);
    (handler, session, pid, target)
}

async fn setup_quiet_focused_capture(
    label: &str,
) -> (
    RequestHandler,
    rmux_proto::SessionName,
    u32,
    PaneTarget,
    PaneId,
    u64,
) {
    let handler = RequestHandler::new();
    let session = session_name(label);
    create_quiet_input_session(&handler, &session).await;
    let target = PaneTarget::new(session.clone(), 0);
    let (pane_id, generation) = {
        let mut state = handler.state.lock().await;
        state
            .append_bytes_to_pane_transcript_for_test(&session, 0, 0, b"\x1b[?1004h")
            .expect("first pane enables focus reporting");
        let transcript = state.transcript_handle(&target).unwrap();
        assert_ne!(transcript.lock().mode() & mode::MODE_FOCUSON, 0);
        let pane_id = state
            .sessions
            .session(&session)
            .unwrap()
            .window()
            .pane(0)
            .unwrap()
            .id();
        let generation = state.pane_output_generation_for_target(&target, pane_id);
        state.start_pane_input_capture_for_test(&target);
        (pane_id, generation)
    };
    let pid = std::process::id();
    let (tx, rx) = mpsc::unbounded_channel();
    handler.register_attach(pid, session.clone(), tx).await;
    spawn_accounted_attach_control_drain(&handler, pid, rx).await;
    {
        let state = handler.state.lock().await;
        let window = state.sessions.session(&session).unwrap().window();
        assert_eq!(window.active_pane().unwrap().id(), pane_id);
        assert_eq!(
            state.pane_output_generation_for_target(&target, pane_id),
            generation
        );
        assert_eq!(
            state.pane_input_capture_for_test(&target),
            Some(b"\x1b[I".to_vec()),
            "initial native registration has one explicit focus edge"
        );
        state.start_pane_input_capture_for_test(&target);
    }
    (handler, session, pid, target, pane_id, generation)
}

pub(super) async fn output(handler: &RequestHandler, target: &PaneTarget, bytes: &[u8]) {
    handler
        .state
        .lock()
        .await
        .append_bytes_to_pane_transcript_for_test(
            target.session_name(),
            target.window_index(),
            target.pane_index(),
            bytes,
        )
        .unwrap();
}

pub(super) async fn input(handler: &RequestHandler, pid: u32, bytes: &[u8]) {
    handler
        .handle_attached_live_input_for_test(pid, bytes)
        .await
        .unwrap();
}

pub(super) async fn captured(handler: &RequestHandler, target: &PaneTarget, expected: &[u8]) {
    assert_eq!(
        handler
            .state
            .lock()
            .await
            .pane_input_capture_for_test(target),
        Some(expected.to_vec())
    );
}

async fn select(handler: &RequestHandler, target: &PaneTarget) {
    assert!(matches!(
        handler
            .handle(Request::SelectPane(Box::new(SelectPaneRequest {
                target: target.clone(),
                title: None,
                style: None,
                input_disabled: None,
                preserve_zoom: false,
            })))
            .await,
        Response::SelectPane(_)
    ));
}

#[tokio::test]
async fn kitty_runtime_acquired_events_follow_negotiation_then_xterm_and_legacy() {
    let (handler, _, pid, target) = setup("kitty-runtime-encoding").await;
    handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::ExtendedKeys,
            value: "on".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    output(&handler, &target, b"\x1b[>4;2m\x1b[=31u").await;
    input(
        &handler,
        pid,
        b"\x1b[97:65:113;2;65u\x1b[97:65:113;2:2;65u\x1b[97;2:3u",
    )
    .await;
    output(&handler, &target, b"\x1b[>1u").await;
    input(&handler, pid, b"\x1b[13;2u\x1b[13;2:3u").await;
    output(&handler, &target, b"\x1b[<99u").await;
    input(&handler, pid, b"\x1b[13;2u\x1b[13;2:3u").await;
    output(&handler, &target, b"\x1b[>4;0m").await;
    input(&handler, pid, b"\x1b[13;2u\x1b[13;2:3u").await;
    captured(
        &handler,
        &target,
        b"\x1b[97:65:113;2;65u\x1b[97:65:113;2:2;65u\x1b[97;2:3u\r\x1b[27;2;13~\r",
    )
    .await;
}

#[tokio::test]
async fn kitty_runtime_held_events_follow_original_pane_and_isolate_clients() {
    let (handler, session, pid, first, first_id, first_generation) =
        setup_quiet_focused_capture("kitty-runtime-route").await;
    let split = handler
        .handle(Request::SplitWindowExt(Box::new(
            rmux_proto::SplitWindowExtRequest {
                target: SplitWindowTarget::Pane(first.clone()),
                direction: SplitDirection::Horizontal,
                before: false,
                environment: None,
                command: Some(quiet_pane_command()),
                process_command: None,
                start_directory: None,
                keep_alive_on_exit: None,
                detached: false,
                size: None,
                preserve_zoom: false,
                full_size: false,
                stdin_payload: None,
            },
        )))
        .await;
    let Response::SplitWindow(split) = split else {
        panic!("expected split-window response: {split:?}")
    };
    let second = split.pane;
    handler
        .wait_for_pane_startup_to_finish_for_test(&second)
        .await;
    output(&handler, &first, b"\x1b[=10u").await;
    output(&handler, &second, b"\x1b[?1004h\x1b[=8u").await;
    let (second_id, second_generation) = {
        let state = handler.state.lock().await;
        let window = state.sessions.session(&session).unwrap().window();
        let second_id = window.pane(1).unwrap().id();
        assert_eq!(window.pane(0).unwrap().id(), first_id);
        assert_eq!(window.active_pane().unwrap().id(), second_id);
        assert_ne!(second_id, first_id);
        assert_eq!(
            state.pane_output_generation_for_target(&first, first_id),
            first_generation
        );
        assert_ne!(
            state.transcript_handle(&first).unwrap().lock().mode() & mode::MODE_FOCUSON,
            0
        );
        assert_ne!(
            state.transcript_handle(&second).unwrap().lock().mode() & mode::MODE_FOCUSON,
            0
        );
        assert_eq!(
            state.pane_input_capture_for_test(&first),
            Some(b"\x1b[O".to_vec()),
            "split moves focus away from the original runtime"
        );
        state.start_pane_input_capture_for_test(&second);
        (
            second_id,
            state.pane_output_generation_for_target(&second, second_id),
        )
    };
    select(&handler, &first).await;
    captured(&handler, &first, b"\x1b[O\x1b[I").await;
    captured(&handler, &second, b"\x1b[O").await;
    input(&handler, pid, b"\x1b[97u").await;
    captured(&handler, &first, b"\x1b[O\x1b[I\x1b[97u").await;
    select(&handler, &second).await;
    captured(&handler, &first, b"\x1b[O\x1b[I\x1b[97u\x1b[O").await;
    captured(&handler, &second, b"\x1b[O\x1b[I").await;
    let other = pid + 10000;
    let (tx, _rx) = mpsc::unbounded_channel();
    handler.register_attach(other, session.clone(), tx).await;
    captured(&handler, &first, b"\x1b[O\x1b[I\x1b[97u\x1b[O").await;
    captured(&handler, &second, b"\x1b[O\x1b[I").await;
    input(&handler, other, b"\x1b[97;1:2u\x1b[97;1:3u").await;
    captured(&handler, &first, b"\x1b[O\x1b[I\x1b[97u\x1b[O").await;
    captured(&handler, &second, b"\x1b[O\x1b[I").await;
    input(&handler, pid, b"\x1b[97;1:2u\x1b[97;1:3u\x1b[98u").await;
    captured(
        &handler,
        &first,
        b"\x1b[O\x1b[I\x1b[97u\x1b[O\x1b[97;1:2u\x1b[97;1:3u",
    )
    .await;
    captured(&handler, &second, b"\x1b[O\x1b[I\x1b[98u").await;
    let state = handler.state.lock().await;
    let window = state.sessions.session(&session).unwrap().window();
    assert_eq!(window.pane(0).unwrap().id(), first_id);
    assert_eq!(window.pane(1).unwrap().id(), second_id);
    assert_eq!(window.active_pane().unwrap().id(), second_id);
    assert_eq!(
        state.pane_output_generation_for_target(&first, first_id),
        first_generation
    );
    assert_eq!(
        state.pane_output_generation_for_target(&second, second_id),
        second_generation
    );
}

#[tokio::test]
async fn kitty_runtime_replacement_attach_discards_held_keys_but_keeps_pane_flags() {
    let (handler, session, pid, target) = setup("kitty-runtime-reattach").await;
    output(&handler, &target, b"\x1b[=10u").await;
    input(&handler, pid, b"\x1b[97u").await;
    let old = handler.active_attach_identity(pid).await.unwrap();
    let (tx, _rx) = mpsc::unbounded_channel();
    handler.register_attach(pid, session, tx).await;
    assert_ne!(old, handler.active_attach_identity(pid).await.unwrap());
    input(
        &handler,
        pid,
        b"\x1b[97;1:2u\x1b[97;1:3u\x1b[98u\x1b[98;1:3u",
    )
    .await;
    captured(&handler, &target, b"\x1b[97u\x1b[98u\x1b[98;1:3u").await;
}

#[tokio::test]
async fn kitty_runtime_fragmentation_paste_and_invalid_frames_preserve_boundaries() {
    let (handler, _, pid, target) = setup("kitty-runtime-fragments").await;
    output(&handler, &target, b"\x1b[=31u\x1b[?2004h").await;
    let mut pending = Vec::new();
    for byte in b"\x1b[128161;1;128161u" {
        handler
            .handle_attached_live_input(pid, &mut pending, &[*byte])
            .await
            .unwrap();
    }
    assert!(pending.is_empty());
    input(
        &handler,
        pid,
        b"\x1b[200~\x1b[97;1:3u\x1b[201~\x1b[97;0u\x1b[1114112uZ",
    )
    .await;
    captured(
        &handler,
        &target,
        b"\x1b[128161;1;128161u\x1b[200~\x1b[97;1:3u\x1b[201~Z",
    )
    .await;
}

#[tokio::test]
async fn kitty_runtime_consumed_prefix_suppresses_repeat_and_release() {
    let (handler, _, pid, target) = setup("kitty-runtime-prefix").await;
    output(&handler, &target, b"\x1b[=10u").await;
    input(&handler, pid, b"\x1b[98;5u\x1b[98;5:2u\x1b[98;5:3u").await;
    captured(&handler, &target, b"").await;
}

#[tokio::test]
async fn kitty_runtime_detach_reattach_keeps_application_stack_without_outer_activation() {
    let (handler, session, pid, target) = setup("kitty-runtime-detach").await;
    output(&handler, &target, b"\x1b[=8u\x1b[>31u").await;
    input(&handler, pid, b"\x1b[97u").await;
    let id = handler.active_attach_identity(pid).await.unwrap();
    handler.finish_attach(pid, id.attach_id()).await;
    assert!(handler.active_attach_identity(pid).await.is_none());
    let (tx, rx) = mpsc::unbounded_channel();
    handler.register_attach(pid, session, tx).await;
    spawn_accounted_attach_control_drain(&handler, pid, rx).await;
    output(&handler, &target, b"\x1b[<u").await;
    input(&handler, pid, b"\x1b[97;1:3u\x1b[98u").await;
    let state = handler.state.lock().await;
    let transcript = state.transcript_handle(&target).unwrap();
    assert_eq!(transcript.lock().keyboard_flags(), 8);
    assert_eq!(transcript.lock().mode() & mode::MODE_KEYS_KITTY, 0);
    drop(state);
    captured(&handler, &target, b"\x1b[97u\x1b[98u").await;
}

#[tokio::test]
async fn kitty_runtime_synchronized_panes_each_use_their_current_encoding() {
    let (handler, session, pid, first, first_id, first_generation) =
        setup_quiet_focused_capture("kitty-runtime-sync").await;
    handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::ExtendedKeys,
            value: "on".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    let split = handler
        .handle(Request::SplitWindowExt(Box::new(
            rmux_proto::SplitWindowExtRequest {
                target: SplitWindowTarget::Pane(first.clone()),
                direction: SplitDirection::Horizontal,
                before: false,
                environment: None,
                command: Some(quiet_pane_command()),
                process_command: None,
                start_directory: None,
                keep_alive_on_exit: None,
                detached: false,
                size: None,
                preserve_zoom: false,
                full_size: false,
                stdin_payload: None,
            },
        )))
        .await;
    let Response::SplitWindow(split) = split else {
        panic!("expected split-window response: {split:?}")
    };
    let second = split.pane;
    handler
        .wait_for_pane_startup_to_finish_for_test(&second)
        .await;
    captured(&handler, &first, b"\x1b[O").await;
    output(&handler, &first, b"\x1b[=10u").await;
    output(&handler, &second, b"\x1b[?1004h\x1b[>4;2m").await;
    let (second_id, second_generation) = {
        let state = handler.state.lock().await;
        let window = state.sessions.session(&session).unwrap().window();
        let second_id = window.pane(1).unwrap().id();
        assert_eq!(window.pane(0).unwrap().id(), first_id);
        assert_eq!(window.active_pane().unwrap().id(), second_id);
        assert_ne!(second_id, first_id);
        assert_eq!(
            state.pane_output_generation_for_target(&first, first_id),
            first_generation
        );
        let first_transcript = state.transcript_handle(&first).unwrap();
        let first_transcript = first_transcript.lock();
        assert_ne!(first_transcript.mode() & mode::MODE_FOCUSON, 0);
        assert_eq!(first_transcript.keyboard_flags(), 10);
        drop(first_transcript);
        let second_transcript = state.transcript_handle(&second).unwrap();
        let second_transcript = second_transcript.lock();
        let second_mode = second_transcript.mode();
        assert_ne!(second_mode & mode::MODE_FOCUSON, 0);
        assert_ne!(second_mode & mode::MODE_KEYS_EXTENDED_2, 0);
        assert_eq!(second_transcript.keyboard_flags(), 0);
        assert_eq!(
            state.pane_input_capture_for_test(&first),
            Some(b"\x1b[O".to_vec())
        );
        state.start_pane_input_capture_for_test(&second);
        (
            second_id,
            state.pane_output_generation_for_target(&second, second_id),
        )
    };
    assert!(matches!(
        handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Window(WindowTarget::with_window(session.clone(), 0)),
                option: OptionName::SynchronizePanes,
                value: "on".into(),
                mode: SetOptionMode::Replace,
            }))
            .await,
        Response::SetOption(_)
    ));
    select(&handler, &first).await;
    {
        let state = handler.state.lock().await;
        assert_eq!(
            state
                .options
                .resolve_for_window(&session, 0, OptionName::SynchronizePanes),
            Some("on")
        );
    }
    captured(&handler, &first, b"\x1b[O\x1b[I").await;
    captured(&handler, &second, b"\x1b[O").await;
    input(&handler, pid, b"\x1b[13;2u\x1b[13;2:2u\x1b[13;2:3u").await;
    captured(
        &handler,
        &first,
        b"\x1b[O\x1b[I\x1b[13;2u\x1b[13;2:2u\x1b[13;2:3u",
    )
    .await;
    captured(&handler, &second, b"\x1b[O\x1b[27;2;13~\x1b[27;2;13~").await;
    let state = handler.state.lock().await;
    let window = state.sessions.session(&session).unwrap().window();
    assert_eq!(window.pane(0).unwrap().id(), first_id);
    assert_eq!(window.pane(1).unwrap().id(), second_id);
    assert_eq!(window.active_pane().unwrap().id(), first_id);
    assert_eq!(
        state.pane_output_generation_for_target(&first, first_id),
        first_generation
    );
    assert_eq!(
        state.pane_output_generation_for_target(&second, second_id),
        second_generation
    );
}

#[tokio::test]
async fn kitty_runtime_respawn_resets_stacks_and_rejects_old_key_remainders() {
    let (handler, _, pid, target) = setup("kitty-runtime-respawn").await;
    handler.wait_for_initial_panes_for_test().await;
    output(&handler, &target, b"\x1b[=10u").await;
    input(&handler, pid, b"\x1b[97u").await;
    let response = handler
        .handle(Request::RespawnPane(Box::new(
            rmux_proto::RespawnPaneRequest {
                target: target.clone(),
                kill: true,
                start_directory: None,
                environment: None,
                command: Some(quiet_pane_command()),
                process_command: None,
            },
        )))
        .await;
    assert!(matches!(response, Response::RespawnPane(_)), "{response:?}");
    handler.wait_for_initial_panes_for_test().await;
    {
        let state = handler.state.lock().await;
        assert_eq!(
            state
                .transcript_handle(&target)
                .unwrap()
                .lock()
                .keyboard_flags(),
            0
        );
        state.start_pane_input_capture_for_test(&target);
    }
    output(&handler, &target, b"\x1b[=10u").await;
    input(&handler, pid, b"\x1b[97;1:2u\x1b[97;1:3u\x1b[98u").await;
    captured(&handler, &target, b"\x1b[98u").await;
}

#[tokio::test]
async fn extended_keys_option_mutations_reach_live_application_requests() {
    let (handler, _, pid, target) = setup("extended-option-policy").await;
    for (value, expected) in [
        (Some("off"), b"\x01".as_slice()),
        (Some("always"), b"\x1b[27;5;97~".as_slice()),
        (Some("on"), b"\x1b[27;5;97~".as_slice()),
        (None, b"\x01".as_slice()),
    ] {
        assert!(matches!(
            handler
                .handle(Request::SetOptionByName(Box::new(
                    rmux_proto::SetOptionByNameRequest {
                        scope: rmux_proto::OptionScopeSelector::ServerGlobal,
                        name: "extended-keys".into(),
                        value: value.map(str::to_owned),
                        mode: SetOptionMode::Replace,
                        only_if_unset: false,
                        unset: value.is_none(),
                        unset_pane_overrides: false,
                        format: false,
                        format_target: None,
                    }
                )))
                .await,
            Response::SetOptionByName(_)
        ));
        handler
            .state
            .lock()
            .await
            .start_pane_input_capture_for_test(&target);
        output(&handler, &target, b"\x1b[>4n\x1b[>4;2m").await;
        input(&handler, pid, b"\x01").await;
        captured(&handler, &target, expected).await;
    }
    assert!(matches!(
        handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Global,
                option: OptionName::ExtendedKeys,
                value: "always".into(),
                mode: SetOptionMode::Replace,
            }))
            .await,
        Response::SetOption(_)
    ));
    handler
        .state
        .lock()
        .await
        .start_pane_input_capture_for_test(&target);
    output(&handler, &target, b"\x1b[>4n\x1b[>4;2m").await;
    input(&handler, pid, b"\x01").await;
    captured(&handler, &target, b"\x1b[27;5;97~").await;
}

// The target shell selects cmd's Ctrl+A emulation even when application
// extended-key requests are ignored; other shells retain legacy Ctrl+A.
#[cfg(windows)]
#[tokio::test]
async fn extended_keys_off_application_request_keeps_select_all_with_explicit_cmd_shell() {
    assert_extended_keys_off_ctrl_a_with_shell("cmd.exe", b"\x1b[1;5H\x1b[1;2F").await;
}

#[cfg(windows)]
#[tokio::test]
async fn extended_keys_off_application_request_keeps_ctrl_a_with_explicit_non_cmd_shell() {
    assert_extended_keys_off_ctrl_a_with_shell("powershell.exe", b"\x01").await;
}

#[cfg(windows)]
async fn assert_extended_keys_off_ctrl_a_with_shell(shell: &str, expected: &[u8]) {
    let handler = RequestHandler::new();
    let session = session_name("extended-option-explicit-shell");
    handler
        .state
        .lock()
        .await
        .options
        .set(
            ScopeSelector::Global,
            OptionName::DefaultShell,
            shell.to_owned(),
            SetOptionMode::Replace,
        )
        .expect("explicit test shell is valid");
    create_send_keys_test_session(&handler, &session).await;
    let target = PaneTarget::new(session.clone(), 0);
    handler
        .wait_for_pane_startup_to_finish_for_test(&target)
        .await;
    handler.disable_pane_focus_reports_for_test(&target).await;
    let pid = std::process::id();
    let (tx, rx) = mpsc::unbounded_channel();
    handler.register_attach(pid, session, tx).await;
    spawn_accounted_attach_control_drain(&handler, pid, rx).await;
    assert!(matches!(
        handler
            .handle(Request::SetOptionByName(Box::new(
                rmux_proto::SetOptionByNameRequest {
                    scope: rmux_proto::OptionScopeSelector::ServerGlobal,
                    name: "extended-keys".into(),
                    value: Some("off".into()),
                    mode: SetOptionMode::Replace,
                    only_if_unset: false,
                    unset: false,
                    unset_pane_overrides: false,
                    format: false,
                    format_target: None,
                }
            )))
            .await,
        Response::SetOptionByName(_)
    ));
    for request in [b"".as_slice(), b"\x1b[>4n\x1b[>4;2m".as_slice()] {
        handler
            .state
            .lock()
            .await
            .start_pane_input_capture_for_test(&target);
        output(&handler, &target, request).await;
        {
            let state = handler.state.lock().await;
            let transcript = state.transcript_handle(&target).unwrap();
            assert_eq!(transcript.lock().mode() & mode::EXTENDED_KEY_MODES, 0);
        }
        input(&handler, pid, b"\x01").await;
        captured(&handler, &target, expected).await;
    }
}
