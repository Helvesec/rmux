use super::*;
use rmux_proto::MoveWindowTarget;

#[tokio::test]
async fn move_window_without_s_moves_current_window_to_first_free_index() {
    let (handler, session) = handler_with_three_windows("move-current-free").await;
    let source_pane_id = pane_id_at(&handler, &session, 2).await;

    let parsed = CommandParser::new()
        .parse("move-window")
        .expect("commands parse");
    handler
        .execute_parsed_commands(
            std::process::id(),
            parsed,
            current_window_context(&session, 2),
        )
        .await
        .expect("move-window should use current window source and first free index");

    let state = handler.state.lock().await;
    let session_state = state.sessions.session(&session).expect("session exists");
    assert_eq!(
        session_state
            .windows()
            .map(|(index, _)| index)
            .collect::<Vec<_>>(),
        vec![0, 1, 3]
    );
    assert_eq!(pane_id_in(session_state, 3), source_pane_id);
}

#[tokio::test]
async fn move_window_after_uses_current_window_as_source() {
    let (handler, session) = handler_with_three_windows("move-current-after").await;
    let source_pane_id = pane_id_at(&handler, &session, 2).await;

    let parsed = CommandParser::new()
        .parse("move-window -a -t move-current-after:0")
        .expect("commands parse");
    handler
        .execute_parsed_commands(
            std::process::id(),
            parsed,
            current_window_context(&session, 2),
        )
        .await
        .expect("move-window -a should use current window source");

    let state = handler.state.lock().await;
    let session_state = state.sessions.session(&session).expect("session exists");
    assert_eq!(pane_id_in(session_state, 1), source_pane_id);
}

#[tokio::test]
async fn move_window_before_uses_current_window_as_source() {
    let (handler, session) = handler_with_three_windows("move-current-before").await;
    let source_pane_id = pane_id_at(&handler, &session, 2).await;

    let parsed = CommandParser::new()
        .parse("move-window -b -t move-current-before:0")
        .expect("commands parse");
    handler
        .execute_parsed_commands(
            std::process::id(),
            parsed,
            current_window_context(&session, 2),
        )
        .await
        .expect("move-window -b should use current window source");

    let state = handler.state.lock().await;
    let session_state = state.sessions.session(&session).expect("session exists");
    assert_eq!(pane_id_in(session_state, 0), source_pane_id);
}

#[tokio::test]
async fn move_window_relative_collision_uses_tmux_error_shape() {
    let (handler, session) = handler_with_three_windows("move-current-collision").await;

    let parsed = CommandParser::new()
        .parse("move-window -t move-current-collision:1")
        .expect("commands parse");
    let error = handler
        .execute_parsed_commands(
            std::process::id(),
            parsed,
            current_window_context(&session, 2),
        )
        .await
        .expect_err("occupied relative target should fail");

    assert_eq!(
        error,
        rmux_proto::RmuxError::Server("index in use: 1".to_owned())
    );
}

#[tokio::test]
async fn move_window_trailing_colon_target_uses_first_free_index() {
    let handler = RequestHandler::new();
    let alpha = session_name("move-colon-alpha");
    let beta = session_name("move-colon-beta");
    for session in [&alpha, &beta] {
        crate::session_fixture::assert_new_session(
            &handler
                .handle(Request::NewSession(NewSessionRequest {
                    session_name: session.clone(),
                    detached: true,
                    size: Some(TerminalSize { cols: 80, rows: 24 }),
                    environment: None,
                }))
                .await,
        );
    }

    let parsed = CommandParser::new()
        .parse("move-window -s move-colon-alpha:0 -t move-colon-beta:")
        .expect("commands parse");
    assert_eq!(
        parsed.commands()[0].to_tmux_string(),
        "move-window -s move-colon-alpha:0 -t move-colon-beta:"
    );
    {
        let state = handler.state.lock().await;
        let request = crate::handler::scripting_support::parse_request_from_parts(
            "move-window".to_owned(),
            vec![
                "-s".to_owned(),
                "move-colon-alpha:0".to_owned(),
                "-t".to_owned(),
                "move-colon-beta:".to_owned(),
            ],
            None,
            &state.sessions,
            &state.options,
            &TargetFindContext::new(Some(Target::Pane(PaneTarget::with_window(
                beta.clone(),
                0,
                0,
            )))),
        )
        .expect("request parses");
        let Request::MoveWindow(request) = request else {
            panic!("expected move-window request");
        };
        assert_eq!(
            request.target,
            MoveWindowTarget::Session(beta.clone()),
            "trailing colon should stay a session target until execution"
        );
    }
    handler
        .execute_parsed_commands(std::process::id(), parsed, current_window_context(&beta, 0))
        .await
        .expect("trailing colon target should use first free window index");

    let state = handler.state.lock().await;
    let beta_state = state.sessions.session(&beta).expect("beta session exists");
    assert_eq!(
        beta_state
            .windows()
            .map(|(index, _)| index)
            .collect::<Vec<_>>(),
        vec![0, 1]
    );
}

#[tokio::test]
async fn break_pane_without_t_lands_in_the_current_session_not_the_source_one() {
    // tmux 3.7b resolves an omitted -t from the current session; -a/-b anchor
    // on that session's active window.
    for (command, expected_windows, moved_index) in [
        ("break-pane -d -s bp-source:0.0", vec![0, 1], 1),
        ("break-pane -d -b -s bp-source:0.0", vec![0, 1], 0),
    ] {
        let handler = RequestHandler::new();
        let source = session_name("bp-source");
        let current = session_name("bp-current");
        for session in [&source, &current] {
            crate::session_fixture::assert_new_session(
                &handler
                    .handle(Request::NewSession(NewSessionRequest {
                        session_name: session.clone(),
                        detached: true,
                        size: Some(TerminalSize { cols: 80, rows: 24 }),
                        environment: None,
                    }))
                    .await,
            );
        }
        assert!(matches!(
            handler
                .handle(Request::SplitWindow(SplitWindowRequest {
                    target: SplitWindowTarget::Pane(PaneTarget::with_window(source.clone(), 0, 0)),
                    direction: SplitDirection::Vertical,
                    environment: None,
                    before: false,
                }))
                .await,
            Response::SplitWindow(_)
        ));
        let moved_pane_id = pane_id_at(&handler, &source, 0).await;

        let parsed = CommandParser::new().parse(command).expect("command parses");
        handler
            .execute_parsed_commands(
                std::process::id(),
                parsed,
                current_window_context(&current, 0),
            )
            .await
            .unwrap_or_else(|error| panic!("{command} should succeed: {error:?}"));

        let state = handler.state.lock().await;
        let current_state = state.sessions.session(&current).expect("current session");
        assert_eq!(
            current_state
                .windows()
                .map(|(index, _)| index)
                .collect::<Vec<_>>(),
            expected_windows,
            "{command}"
        );
        assert_eq!(
            pane_id_in(current_state, moved_index),
            moved_pane_id,
            "{command}"
        );
        let source_state = state.sessions.session(&source).expect("source session");
        assert_eq!(source_state.windows().count(), 1, "{command}");
    }
}

#[tokio::test]
async fn link_window_after_without_t_keeps_a_pinned_current_window_anchor() {
    // A queue state that pins its current target (hook, key binding) anchors
    // -a on that window even when the session's active window is another one:
    // tmux 3.7b shuffles after target->wl, the item's current window. Only a
    // current derived from the requesting client becomes the session's curw.
    let (handler, session) = handler_with_three_windows("link-pinned-anchor").await;
    let other = session_name("link-pinned-other");
    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: other.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
    );
    let linked_pane_id = pane_id_at(&handler, &other, 0).await;

    let parsed = CommandParser::new()
        .parse("link-window -d -a -s link-pinned-other:0")
        .expect("command parses");
    handler
        .execute_parsed_commands(
            std::process::id(),
            parsed,
            current_window_context(&session, 1),
        )
        .await
        .expect("link-window -a should anchor on the pinned current window");

    let state = handler.state.lock().await;
    let session_state = state.sessions.session(&session).expect("session exists");
    assert_eq!(session_state.active_window_index(), 0);
    assert_eq!(
        session_state
            .windows()
            .map(|(index, _)| index)
            .collect::<Vec<_>>(),
        vec![0, 1, 2, 3]
    );
    assert_eq!(pane_id_in(session_state, 2), linked_pane_id);
}

fn current_window_context(session: &SessionName, window_index: u32) -> QueueExecutionContext {
    QueueExecutionContext::without_caller_cwd().with_current_target(Some(Target::Pane(
        PaneTarget::with_window(session.clone(), window_index, 0),
    )))
}

async fn handler_with_three_windows(name: &str) -> (RequestHandler, SessionName) {
    let handler = RequestHandler::new();
    let session = session_name(name);
    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
    );
    for window_name in ["b", "c"] {
        assert!(matches!(
            handler
                .handle(Request::NewWindow(Box::new(NewWindowRequest {
                    target: session.clone(),
                    name: Some(window_name.to_owned()),
                    detached: true,
                    start_directory: None,
                    environment: None,
                    command: None,
                    process_command: None,
                    target_window_index: None,
                    insert_at_target: false,
                })))
                .await,
            Response::NewWindow(_)
        ));
    }
    (handler, session)
}

async fn pane_id_at(
    handler: &RequestHandler,
    session_name: &SessionName,
    window_index: u32,
) -> Option<rmux_core::PaneId> {
    let state = handler.state.lock().await;
    state
        .sessions
        .session(session_name)
        .and_then(|session| pane_id_in(session, window_index))
}

fn pane_id_in(
    session: rmux_core::ManagedSessionView<'_>,
    window_index: u32,
) -> Option<rmux_core::PaneId> {
    session
        .window_at(window_index)
        .and_then(|window| window.pane(0))
        .map(|pane| pane.id())
}
