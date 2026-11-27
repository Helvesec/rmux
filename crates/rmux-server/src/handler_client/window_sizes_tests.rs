use super::*;
use rmux_proto::WindowId;

async fn setup(policy: &str) -> (RequestHandler, SessionName, Vec<WindowId>) {
    let handler = RequestHandler::new();
    let session = session_name("window-sizes");
    create_session(&handler, session.clone(), INITIAL_SIZE).await;
    let response = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: session.clone(),
            name: None,
            detached: true,
            start_directory: None,
            environment: None,
            command: None,
            process_command: None,
            target_window_index: Some(1),
            insert_at_target: false,
        })))
        .await;
    assert!(matches!(response, Response::NewWindow(_)), "{response:?}");
    for index in [0, 1] {
        set_window_size_policy_for_window(&handler, &session, index, policy).await;
    }
    let state = handler.state.lock().await;
    let ids = state
        .sessions
        .session(&session)
        .unwrap()
        .windows()
        .map(|(_, w)| w.id())
        .collect();
    drop(state);
    (handler, session, ids)
}

async fn declare(handler: &RequestHandler, pid: u32, value: &str) -> Response {
    let mut request = refresh_client_size_request(pid, INITIAL_SIZE);
    request.control_size = Some(value.to_owned());
    handler
        .handle(Request::RefreshClient(Box::new(request)))
        .await
}

async fn good(handler: &RequestHandler, pid: u32, value: &str) {
    let response = declare(handler, pid, value).await;
    assert!(
        matches!(response, Response::RefreshClient(_)),
        "{value}: {response:?}"
    );
}

fn size(cols: u16, rows: u16) -> TerminalSize {
    TerminalSize { cols, rows }
}
fn value(id: WindowId, dimensions: &str) -> String {
    format!("@{}:{dimensions}", id.as_u32())
}

#[tokio::test]
async fn control_window_sizes_select_and_clamp_both_dimensions_under_every_policy() {
    // tmux 3.7b native freeze 2026-09-08: declarations replace a client's
    // vote and then cap both dimensions, even under largest/latest/manual.
    for (policy, first, removed) in [
        ("latest", size(70, 24), size(90, 50)),
        ("largest", size(70, 24), size(100, 50)),
        ("smallest", size(70, 24), size(90, 40)),
        ("manual", size(70, 24), INITIAL_SIZE),
    ] {
        let (handler, session, ids) = setup(policy).await;
        let (_id_a, _events_a) = register_control_client_with_id(&handler, 93_001, &session).await;
        good(&handler, 93_001, "100x40").await;
        let (_id_b, _events_b) = register_control_client_with_id(&handler, 93_002, &session).await;
        good(&handler, 93_002, "90,50").await;
        good(&handler, 93_001, &value(ids[0], "70x55")).await;
        good(&handler, 93_002, &value(ids[0], "85x24")).await;
        assert_eq!(
            session_window_sizes(&handler, &session).await[0],
            first,
            "{policy}"
        );
        assert_eq!(control_client_size(&handler, 93_001).await, size(100, 40));
        assert_eq!(control_client_size(&handler, 93_002).await, size(90, 50));
        good(&handler, 93_001, &value(ids[0], "")).await;
        good(&handler, 93_002, &value(ids[0], "")).await;
        assert_eq!(
            session_window_sizes(&handler, &session).await,
            vec![removed, removed],
            "{policy}"
        );
    }
}

#[tokio::test]
async fn control_window_sizes_enable_placeholder_fallback_and_keep_global_sizes_separate() {
    let (handler, session, ids) = setup("largest").await;
    let (_id, _events) = register_control_client_with_id(&handler, 93_011, &session).await;
    force_window_size(&handler, &session, 0, size(110, 35)).await;
    force_window_size(&handler, &session, 1, size(110, 35)).await;
    good(&handler, 93_011, &value(ids[1], "65x25")).await;
    assert_eq!(
        session_window_sizes(&handler, &session).await,
        vec![INITIAL_SIZE, size(65, 25)]
    );
    assert_eq!(control_client_size(&handler, 93_011).await, INITIAL_SIZE);
    good(&handler, 93_011, "100,40").await;
    assert_eq!(
        session_window_sizes(&handler, &session).await,
        vec![size(100, 40), size(65, 25)]
    );
    good(&handler, 93_011, &value(ids[1], "120x45")).await;
    assert_eq!(
        session_window_sizes(&handler, &session).await,
        vec![size(100, 40), size(120, 45)]
    );
    good(&handler, 93_011, &value(ids[1], "")).await;
    assert_eq!(
        session_window_sizes(&handler, &session).await,
        vec![size(100, 40); 2]
    );
}

#[tokio::test]
async fn control_window_sizes_echo_in_order_but_absent_removal_and_errors_have_no_effect() {
    let (handler, session, ids) = setup("largest").await;
    let (_id, mut events) = register_control_client_with_id(&handler, 93_021, &session).await;
    let _ = settle_control_notifications(&mut events).await;
    good(&handler, 93_021, &value(ids[1], "")).await;
    assert!(settle_control_notifications(&mut events)
        .await
        .iter()
        .all(|s| !s.starts_with("%layout-change")));
    for dimensions in ["65x25", "65x25", "", ""] {
        good(&handler, 93_021, &value(ids[1], dimensions)).await;
        let actual = settle_control_notifications(&mut events).await;
        let actual = actual
            .iter()
            .filter_map(|s| layout_change_window_id(s))
            .collect::<Vec<_>>();
        assert_eq!(actual, window_ids(&handler, &session).await);
    }
    good(&handler, 93_021, &value(ids[1], "65x25")).await;
    let _ = settle_control_notifications(&mut events).await;
    let before = session_window_sizes(&handler, &session).await;
    for bad in [
        value(ids[1], "bad"),
        value(ids[1], "1,1"),
        value(ids[1], "65536x1"),
        value(ids[1], "0x1"),
        "@4294967296:".to_owned(),
        "@9999:80x24".to_owned(),
    ] {
        assert!(
            matches!(declare(&handler, 93_021, &bad).await, Response::Error(_)),
            "{bad}"
        );
        assert_eq!(session_window_sizes(&handler, &session).await, before);
        assert!(settle_control_notifications(&mut events)
            .await
            .iter()
            .all(|s| !s.starts_with("%layout-change")));
    }
}

#[tokio::test]
async fn control_window_sizes_departure_reconciles_inactive_windows_and_manual_baseline() {
    for policy in ["largest", "manual"] {
        let (handler, session, ids) = setup(policy).await;
        let (id, _events) = register_control_client_with_id(&handler, 93_031, &session).await;
        let (_observer, _observer_events) =
            register_control_client_with_id(&handler, 93_032, &session).await;
        good(&handler, 93_032, "100x40").await;
        good(&handler, 93_031, &value(ids[1], "60x15")).await;
        assert_eq!(
            session_window_sizes(&handler, &session).await[1],
            TARGET_SIZE
        );
        handler.finish_control(93_031, id).await;
        assert_eq!(
            session_window_sizes(&handler, &session).await[1],
            if policy == "manual" {
                INITIAL_SIZE
            } else {
                size(100, 40)
            }
        );
    }
}

#[tokio::test]
async fn control_window_sizes_invalidate_only_selections_whose_effective_candidates_changed() {
    let (handler, session, ids) = setup("largest").await;
    let (_id, _events) = register_control_client_with_id(&handler, 93_041, &session).await;
    good(&handler, 93_041, "100x40").await;
    let target = WindowTarget::with_window(session.clone(), 1);
    let selection = handler
        .selected_attached_window_size(&target, None)
        .await
        .unwrap();
    good(&handler, 93_041, &value(ids[0], "60x15")).await;
    {
        let state = handler.state.lock().await;
        let attaches = handler.active_attach.lock().await;
        let controls = handler.active_control.lock().await;
        assert!(handler.attached_size_selection_is_current(
            &state, &attaches, &controls, &session, &selection, false
        ));
    }
    good(&handler, 93_041, &value(ids[1], "60x15")).await;
    let state = handler.state.lock().await;
    let attaches = handler.active_attach.lock().await;
    let controls = handler.active_control.lock().await;
    assert!(!handler.attached_size_selection_is_current(
        &state, &attaches, &controls, &session, &selection, false
    ));
}

#[tokio::test]
async fn control_window_sizes_native_failure_rolls_back_declaration_and_geometry() {
    let (handler, session, ids) = setup("largest").await;
    let (_id, _events) = register_control_client_with_id(&handler, 93_051, &session).await;
    good(&handler, 93_051, "100x40").await;
    handler.state.lock().await.fail_resizes_for_test(1);
    assert!(matches!(
        declare(&handler, 93_051, &value(ids[1], "60x15")).await,
        Response::Error(_)
    ));
    assert_eq!(
        session_window_sizes(&handler, &session).await,
        vec![size(100, 40); 2]
    );
    let controls = handler.active_control.lock().await;
    assert_eq!(controls.by_pid[&93_051].window_sizes.get(ids[1]), None);
}

#[tokio::test]
async fn control_window_sizes_public_queues_share_set_remove_and_strict_parsing() {
    let (handler, session, ids) = setup("largest").await;
    let (id, _events) = register_control_client_with_id(&handler, 93_061, &session).await;
    for (declaration, expected) in [("60x15", TARGET_SIZE), ("", INITIAL_SIZE)] {
        let command = format!("refresh-client -C{}", value(ids[1], declaration));
        let parsed = CommandParser::new().parse(&command).unwrap();
        let result = handler
            .execute_control_commands_identity(93_061, id, parsed)
            .await;
        assert!(result.error.is_none(), "{:?}", result.error);
        assert_eq!(session_window_sizes(&handler, &session).await[1], expected);
    }
    let command = format!(
        "refresh-client -t 93061 -C {}; refresh-client -t 93061 -C {}",
        value(ids[1], "60x15"),
        value(ids[0], "75x20")
    );
    let parsed = handler.parse_control_commands(&command).await.unwrap();
    let (external_id, _external_events) =
        register_control_client_with_id(&handler, 93_062, &session).await;
    let result = handler
        .execute_control_commands_identity(93_062, external_id, parsed)
        .await;
    assert!(result.error.is_none(), "{:?}", result.error);
    assert_eq!(
        session_window_sizes(&handler, &session).await,
        vec![size(75, 20), TARGET_SIZE]
    );
    let before = session_window_sizes(&handler, &session).await;
    let parsed = handler
        .parse_control_commands(&format!("refresh-client -C {}", value(ids[1], "garbage")))
        .await
        .unwrap();
    let result = handler
        .execute_control_commands_identity(93_061, id, parsed)
        .await;
    assert!(result.error.is_some());
    assert_eq!(session_window_sizes(&handler, &session).await, before);
}

#[tokio::test]
async fn control_window_sizes_replacement_cannot_mutate_or_inherit_old_registration() {
    use crate::handler::control_support::ControlClientIdentity;
    let (handler, session, ids) = setup("largest").await;
    let (old, _old_events) = register_control_client_with_id(&handler, 93_071, &session).await;
    good(&handler, 93_071, &value(ids[1], "60x15")).await;
    let (new, _new_events) = register_control_client_with_id(&handler, 93_071, &session).await;
    assert_ne!(old, new);
    let result = handler
        .apply_control_size(
            ControlClientIdentity::new(93_071, old),
            rmux_core::command_parser::ControlClientSize::RemoveWindow(ids[1]),
        )
        .await;
    assert!(result.is_err());
    let controls = handler.active_control.lock().await;
    let active = &controls.by_pid[&93_071];
    assert_eq!(active.id, new);
    assert!(!active.window_sizes.declared());
    assert!(active.window_sizes.get(ids[1]).is_none());
    drop(controls);
    handler.finish_control(93_071, old).await;
    assert_eq!(handler.active_control.lock().await.by_pid[&93_071].id, new);
}

#[tokio::test]
async fn control_window_sizes_aggressive_resize_excludes_issuer_until_window_is_selected() {
    let (handler, session, ids) = setup("largest").await;
    let (_id, _events) = register_control_client_with_id(&handler, 93_081, &session).await;
    good(&handler, 93_081, &value(ids[1], "65x25")).await;
    let response = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Window(WindowTarget::with_window(session.clone(), 1)),
            option: OptionName::AggressiveResize,
            value: "on".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(response, Response::SetOption(_)));
    good(&handler, 93_081, &value(ids[1], "60x15")).await;
    assert_eq!(
        session_window_sizes(&handler, &session).await[1],
        size(65, 25)
    );
    let parsed = handler
        .parse_control_commands("switch-client -t window-sizes:1")
        .await
        .unwrap();
    let result = handler.execute_control_commands(93_081, parsed).await;
    assert!(result.error.is_none(), "{:?}", result.error);
    assert_eq!(
        session_window_sizes(&handler, &session).await[1],
        TARGET_SIZE
    );
}

#[tokio::test]
async fn control_window_sizes_later_native_failure_compensates_earlier_window() {
    let (handler, session, ids) = setup("largest").await;
    let (_id, _events) = register_control_client_with_id(&handler, 93_091, &session).await;
    let original = size(110, 35);
    for index in [0, 1] {
        force_window_size(&handler, &session, index, original).await;
    }
    let master = {
        let mut state = handler.state.lock().await;
        let master = state.clone_pane_master_if_alive(&session, 0, 0).unwrap();
        let missing = state
            .sessions
            .session(&session)
            .unwrap()
            .window_at(1)
            .unwrap()
            .pane(0)
            .unwrap()
            .id();
        assert!(state.remove_pane_terminal(&session, missing));
        state.take_applied_window_resizes();
        master
    };
    // The first declaration also enables the global 80x24 fallback on window 0.
    // Its native resize succeeds before the missing window-1 terminal fails.
    let response = declare(&handler, 93_091, &value(ids[1], "60x15")).await;
    assert!(matches!(response, Response::Error(_)), "{response:?}");
    assert_eq!(
        session_window_sizes(&handler, &session).await,
        vec![original; 2]
    );
    let native = master.size().unwrap();
    assert_eq!(size(native.cols, native.rows), original);
    assert!(handler
        .state
        .lock()
        .await
        .take_applied_window_resizes()
        .is_empty());
    let controls = handler.active_control.lock().await;
    let active = &controls.by_pid[&93_091];
    assert!(!active.size_declared);
    assert!(!active.window_sizes.declared());
    assert_eq!(active.window_sizes.get(ids[1]), None);
}

#[tokio::test]
async fn control_window_sizes_idempotent_inactive_declaration_does_not_call_native_resize() {
    let (handler, session, ids) = setup("largest").await;
    let (_id, _events) = register_control_client_with_id(&handler, 93_101, &session).await;
    good(&handler, 93_101, "100x40").await;
    good(&handler, 93_101, &value(ids[1], "60x15")).await;
    handler.state.lock().await.fail_resizes_for_test(1);
    good(&handler, 93_101, &value(ids[1], "60x15")).await;
    let response = declare(&handler, 93_101, &value(ids[1], "61x15")).await;
    assert!(
        matches!(response, Response::Error(_)),
        "injection remains armed until a real resize: {response:?}"
    );
    assert_eq!(
        session_window_sizes(&handler, &session).await,
        vec![size(100, 40), TARGET_SIZE]
    );
}

#[tokio::test]
async fn control_window_sizes_commit_echo_survives_later_geometry_and_rejects_replaced_observer() {
    use crate::handler::control_support::ControlClientIdentity;
    let (handler, session, ids) = setup("largest").await;
    let (_owner_id, mut owner_events) =
        register_control_client_with_id(&handler, 93_111, &session).await;
    let (old_observer, _old_events) =
        register_control_client_with_id(&handler, 93_112, &session).await;
    let _ = settle_control_notifications(&mut owner_events).await;
    let pause = handler.install_window_lifecycle_emit_pause();
    let requester = handler.clone();
    let declaration = value(ids[1], "60x15");
    let first = tokio::spawn(async move { declare(&requester, 93_111, &declaration).await });
    tokio::time::timeout(Duration::from_secs(5), pause.reached.notified())
        .await
        .unwrap();
    force_window_size(&handler, &session, 1, size(75, 20)).await;
    let session_id = handler
        .state
        .lock()
        .await
        .sessions
        .session(&session)
        .unwrap()
        .id();
    let (tx, mut replacement_events) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let replacing = handler.clone();
    let registration = tokio::spawn(async move {
        replacing
            .register_control_with_closing(
                93_112,
                ControlModeUpgrade {
                    initial_command_count: 0,
                    mode: ControlMode::Plain,
                    terminal_context: crate::outer_terminal::OuterTerminalContext::default(),
                },
                tx,
                Arc::new(AtomicBool::new(false)),
            )
            .await
    });
    // Replacement commits before its detach event can pass the paused C ticket.
    let replacement = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let id = handler.active_control.lock().await.by_pid[&93_112].id;
            if id != old_observer {
                break id;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    handler
        .set_control_subscription_identity_for_test(
            ControlClientIdentity::new(93_112, replacement),
            session.clone(),
            session_id,
        )
        .await;
    pause.release.notify_one();
    let response = tokio::time::timeout(Duration::from_secs(5), first)
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(response, Response::RefreshClient(_)),
        "{response:?}"
    );
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), registration)
            .await
            .unwrap()
            .unwrap(),
        replacement
    );
    let lines = settle_control_notifications(&mut owner_events).await;
    let echo = lines
        .iter()
        .find(|line| line.starts_with(&format!("%layout-change {} ", ids[1])))
        .unwrap();
    assert_eq!(layout_change_geometry(echo).as_deref(), Some("60x15"));
    assert_eq!(
        session_window_sizes(&handler, &session).await[1],
        size(75, 20)
    );
    let replacement_lines = settle_control_notifications(&mut replacement_events).await;
    // The later forced mutation may publish its own 75x20 event when the
    // request finishes. Neither of the earlier C echoes belongs to this client.
    assert!(
        replacement_lines
            .iter()
            .filter(|line| line.starts_with("%layout-change "))
            .all(|line| {
                line.starts_with(&format!("%layout-change {} ", ids[1]))
                    && layout_change_geometry(line).as_deref() == Some("75x20")
            }),
        "{replacement_lines:?}"
    );
}

#[tokio::test]
async fn control_window_sizes_explicit_resize_keeps_cap_and_transactional_manual_baseline() {
    let (handler, session, ids) = setup("manual").await;
    let (_id, _events) = register_control_client_with_id(&handler, 93_121, &session).await;
    good(&handler, 93_121, &value(ids[1], "60x15")).await;
    let commands = handler
        .parse_control_commands("resize-window -t window-sizes:1 -x 95 -y 32")
        .await
        .unwrap();
    let result = handler.execute_control_commands(93_121, commands).await;
    assert!(result.error.is_none(), "{:?}", result.error);
    assert_eq!(
        session_window_sizes(&handler, &session).await[1],
        TARGET_SIZE
    );
    handler.state.lock().await.fail_resizes_for_test(1);
    let commands = handler
        .parse_control_commands("resize-window -t window-sizes:1 -x 110 -y 40")
        .await
        .unwrap();
    let result = handler.execute_control_commands(93_121, commands).await;
    assert!(result.error.is_some());
    good(&handler, 93_121, &value(ids[1], "")).await;
    assert_eq!(
        session_window_sizes(&handler, &session).await[1],
        size(95, 32)
    );
}
