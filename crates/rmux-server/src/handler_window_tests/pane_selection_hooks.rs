use super::*;

async fn selection_hook_fixture(label: &str) -> (RequestHandler, SessionName) {
    let handler = RequestHandler::new();
    let session = session_name(label);
    create_session(&handler, session.as_str()).await;
    let split = handler
        .handle(Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Session(session.clone()),
            direction: SplitDirection::Vertical,
            before: false,
            environment: None,
        }))
        .await;
    assert!(matches!(split, Response::SplitWindow(_)), "{split:?}");
    handler.wait_for_initial_panes_for_test().await;
    handler
        .state
        .lock()
        .await
        .sessions
        .session_mut(&session)
        .expect("fixture session exists")
        .select_pane_in_window(0, 0)
        .expect("fixture selects pane zero");
    (handler, session)
}

async fn install_after_select_probe(handler: &RequestHandler, buffer_name: &str) {
    let response = handler
        .handle(Request::SetHookMutation(SetHookMutationRequest {
            scope: ScopeSelector::Global,
            hook: HookName::AfterSelectPane,
            command: Some(format!("set-buffer -b {buffer_name} fired")),
            lifecycle: HookLifecycle::Persistent,
            append: false,
            unset: false,
            run_immediately: false,
            index: None,
        }))
        .await;
    assert!(matches!(response, Response::SetHook(_)), "{response:?}");
    let state = handler.state.lock().await;
    assert!(
        state.buffers.show(Some(buffer_name)).is_err(),
        "probe {buffer_name} fired before the selection under test"
    );
}

async fn assert_probe(handler: &RequestHandler, buffer_name: &str, expected: bool) {
    let state = handler.state.lock().await;
    let probe = state.buffers.show(Some(buffer_name));
    assert_eq!(probe.is_ok(), expected, "buffer {buffer_name}: {probe:?}");
}

async fn run_queued_select(handler: &RequestHandler, command: &str) {
    let parsed = rmux_core::command_parser::CommandParser::new()
        .parse(command)
        .expect("queued select-pane parses");
    handler
        .execute_parsed_commands(
            std::process::id(),
            parsed,
            crate::handler::scripting_support::QueueExecutionContext::without_caller_cwd(),
        )
        .await
        .expect("queued select-pane executes");
}

#[tokio::test]
async fn direct_select_hook_requires_an_active_pane_change() {
    let (handler, session) = selection_hook_fixture("hook-direct-change").await;
    install_after_select_probe(&handler, "hook-direct-change").await;

    let response = handler
        .handle(Request::SelectPane(Box::new(SelectPaneRequest {
            target: PaneTarget::with_window(session, 0, 1),
            title: None,
            style: None,
            input_disabled: None,
            preserve_zoom: false,
        })))
        .await;
    assert!(matches!(response, Response::SelectPane(_)), "{response:?}");
    assert_probe(&handler, "hook-direct-change", true).await;
}

#[tokio::test]
async fn direct_select_noop_title_and_input_mutations_do_not_run_hook() {
    let cases = [
        ("noop", None, None),
        ("title", Some("renamed".to_owned()), None),
        ("input", None, Some(true)),
    ];
    for (label, title, input_disabled) in cases {
        let buffer_name = format!("hook-direct-{label}");
        let (handler, session) = selection_hook_fixture(&buffer_name).await;
        install_after_select_probe(&handler, &buffer_name).await;
        let target_pane = u32::from(title.is_some() || input_disabled.is_some());
        assert_eq!(
            handler
                .state
                .lock()
                .await
                .sessions
                .session(&session)
                .expect("fixture session exists")
                .window()
                .active_pane_index(),
            0,
            "fixture active pane changed before {label}"
        );

        let response = handler
            .handle(Request::SelectPane(Box::new(SelectPaneRequest {
                target: PaneTarget::with_window(session, 0, target_pane),
                title,
                style: None,
                input_disabled,
                preserve_zoom: false,
            })))
            .await;
        assert!(matches!(response, Response::SelectPane(_)), "{response:?}");
        assert_probe(&handler, &buffer_name, false).await;
    }
}

#[tokio::test]
async fn adjacent_select_hook_runs_only_when_the_active_pane_changes() {
    let (handler, session) = selection_hook_fixture("hook-adjacent").await;
    install_after_select_probe(&handler, "hook-adjacent-change").await;

    let response = handler
        .handle(Request::SelectPaneAdjacent(SelectPaneAdjacentRequest {
            target: PaneTarget::with_window(session.clone(), 0, 0),
            direction: SelectPaneDirection::Down,
            preserve_zoom: false,
        }))
        .await;
    assert!(matches!(response, Response::SelectPane(_)), "{response:?}");
    assert_probe(&handler, "hook-adjacent-change", true).await;

    let (noop_handler, noop_session) = selection_hook_fixture("hook-adjacent-noop").await;
    install_after_select_probe(&noop_handler, "hook-adjacent-noop").await;
    let response = noop_handler
        .handle(Request::SelectPaneAdjacent(SelectPaneAdjacentRequest {
            target: PaneTarget::with_window(noop_session, 0, 0),
            direction: SelectPaneDirection::Right,
            preserve_zoom: false,
        }))
        .await;
    assert!(matches!(response, Response::SelectPane(_)), "{response:?}");
    assert_probe(&noop_handler, "hook-adjacent-noop", false).await;
}

#[tokio::test]
async fn last_pane_does_not_run_after_select_pane_hook() {
    let (handler, session) = selection_hook_fixture("hook-last-pane").await;
    install_after_select_probe(&handler, "hook-last-pane").await;

    let response = handler
        .handle(Request::LastPane(LastPaneRequest {
            target: WindowTarget::with_window(session, 0),
            preserve_zoom: false,
            input_disabled: None,
        }))
        .await;
    assert!(matches!(response, Response::LastPane(_)), "{response:?}");
    assert_probe(&handler, "hook-last-pane", false).await;
}

#[tokio::test]
async fn sdk_select_hook_requires_an_active_pane_change() {
    let (handler, session) = selection_hook_fixture("hook-sdk").await;
    let pane_one_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&session)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(1))
            .expect("pane one exists")
            .id()
    };
    install_after_select_probe(&handler, "hook-sdk-change").await;
    let response = handler
        .handle(Request::PaneSelect(PaneSelectRequest {
            target: PaneTargetRef::by_id(session.clone(), pane_one_id),
            title: None,
        }))
        .await;
    assert!(matches!(response, Response::SelectPane(_)), "{response:?}");
    assert_probe(&handler, "hook-sdk-change", true).await;

    let (title_handler, title_session) = selection_hook_fixture("hook-sdk-title").await;
    let title_pane_id = {
        let state = title_handler.state.lock().await;
        state
            .sessions
            .session(&title_session)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(1))
            .expect("pane one exists")
            .id()
    };
    install_after_select_probe(&title_handler, "hook-sdk-title").await;
    let response = title_handler
        .handle(Request::PaneSelect(PaneSelectRequest {
            target: PaneTargetRef::by_id(title_session, title_pane_id),
            title: Some("renamed".to_owned()),
        }))
        .await;
    assert!(matches!(response, Response::SelectPane(_)), "{response:?}");
    assert_probe(&title_handler, "hook-sdk-title", false).await;
}

#[tokio::test]
async fn queued_select_hook_uses_the_same_change_gate() {
    let (change_handler, change_session) = selection_hook_fixture("hook-queue-change").await;
    install_after_select_probe(&change_handler, "hook-queue-change").await;
    run_queued_select(
        &change_handler,
        &format!("select-pane -D -t {change_session}:0.0"),
    )
    .await;
    assert_probe(&change_handler, "hook-queue-change", true).await;

    let (noop_handler, noop_session) = selection_hook_fixture("hook-queue-noop").await;
    install_after_select_probe(&noop_handler, "hook-queue-noop").await;
    run_queued_select(
        &noop_handler,
        &format!("select-pane -R -t {noop_session}:0.0"),
    )
    .await;
    assert_probe(&noop_handler, "hook-queue-noop", false).await;
}
