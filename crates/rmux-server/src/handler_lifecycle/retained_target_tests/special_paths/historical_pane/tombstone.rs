use super::*;

const MUTATIONS: [&str; 3] = ["kill-pane", "kill-window", "select-pane -m"];
const PREFIXES: [&str; 3] = [
    "",
    "set-buffer -b before HIST ; ",
    "set-buffer -b before HIST\n",
];

pub(super) async fn session_with_native_spare_window(
    handler: &RequestHandler,
    name: &str,
) -> SessionName {
    let session = create_handler_session(handler, name).await;
    let response = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: session.clone(),
            name: None,
            detached: true,
            environment: None,
            command: Some(vec![crate::test_shell::stdin_discard_command()]),
            start_directory: None,
            target_window_index: Some(1),
            insert_at_target: false,
            process_command: None,
        })))
        .await;
    assert!(matches!(response, Response::NewWindow(_)), "{response:?}");
    session
}

pub(super) async fn execute_retired_child_command(
    handler: &RequestHandler,
    session: &SessionName,
    command: &str,
) -> Result<rmux_proto::CommandOutput, rmux_proto::RmuxError> {
    let lease = {
        let mut state = handler.state.lock().await;
        let event = super::super::super::super::super::prepare_lifecycle_event(
            &mut state,
            &rmux_core::LifecycleEvent::PaneFocusOut {
                target: PaneTarget::with_window(session.clone(), 0, 0),
            },
        );
        state
            .sessions
            .session_setup(session)
            .expect("session exists")
            .remove_window_allowing_empty(0)
            .expect("retire historical window");
        let lease = event.retained_current_target.expect("retained pane lease");
        lease.retire_historical();
        lease
    };
    // Exercise admission of an already parsed historical event. Parse-time
    // formatting is a separate path from the child-role admission contract.
    let commands = rmux_core::command_parser::CommandParser::new()
        .parse(command)
        .expect("child entry parses");
    let context = crate::handler::scripting_support::QueueExecutionContext::without_caller_cwd()
        .with_implicit_current_target(None)
        .forbid_missing_current_target_fallback()
        .with_historical_retired_target_work()
        .with_retained_lifecycle_target(Some(lease));
    handler
        .execute_parsed_commands(std::process::id(), commands, context)
        .await
}

pub(super) async fn wrapped_command(
    entry: &str,
    commands: &str,
) -> (String, Option<crate::test_shell::ScratchPath>) {
    match entry {
        "if-shell" => (
            format!(
                "if-shell -F 1 {}",
                crate::test_shell::command_quote(commands)
            ),
            None,
        ),
        "run-shell" => (
            format!(
                "run-shell -C {}",
                crate::test_shell::command_quote(commands)
            ),
            None,
        ),
        "source-file" => {
            let path = unique_project_path("historical-tombstone.conf");
            std::fs::write(&path, commands).expect("write sourced command list");
            (
                format!(
                    "source-file {}",
                    crate::test_shell::command_quote(&path.display().to_string())
                ),
                Some(path),
            )
        }
        _ => unreachable!("declared child entry"),
    }
}

#[tokio::test]
async fn retired_children_reject_implicit_roles_before_any_survivor_fallback() {
    let mut violations = Vec::new();
    for entry in ["if-shell", "run-shell", "source-file"] {
        for mutation in MUTATIONS {
            for prefix in PREFIXES {
                for attached in [false, true] {
                    let handler = RequestHandler::new();
                    let session =
                        session_with_native_spare_window(&handler, "historical-matrix").await;
                    let _receiver = if attached {
                        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
                        handler
                            .register_attach(std::process::id(), session.clone(), sender)
                            .await;
                        Some(receiver)
                    } else {
                        None
                    };
                    let (command, source) =
                        wrapped_command(entry, &format!("{prefix}{mutation}")).await;
                    let result = execute_retired_child_command(&handler, &session, &command).await;
                    if let Some(source) = source {
                        let _ = std::fs::remove_file(source);
                    }
                    let state = handler.state.lock().await;
                    let survives = state
                        .sessions
                        .session(&session)
                        .and_then(|session| session.window_at(1))
                        .and_then(|window| window.pane(0))
                        .is_some();
                    if !survives || state.marked_pane_target().is_some() || result.is_ok() {
                        violations.push(format!(
                            "{entry}: {prefix:?}{mutation}; attached={attached}"
                        ));
                    }
                    if !prefix.is_empty() {
                        assert_eq!(
                            state
                                .buffers
                                .show(Some("before"))
                                .expect("independent predecessor executes")
                                .1,
                            b"HIST"
                        );
                    }
                }
            }
        }
    }
    assert!(
        violations.is_empty(),
        "historical targets escaped to survivors: {violations:?}"
    );
}

#[tokio::test]
async fn retired_children_preserve_fully_independent_explicit_targets() {
    for entry in ["if-shell", "run-shell", "source-file"] {
        for mutation in MUTATIONS {
            for prefix in PREFIXES {
                let handler = RequestHandler::new();
                let session =
                    session_with_native_spare_window(&handler, "historical-explicit").await;
                let (command, source) =
                    wrapped_command(entry, &format!("{prefix}{mutation} -t {session}:1.0")).await;
                execute_retired_child_command(&handler, &session, &command)
                    .await
                    .expect("independent explicit target executes");
                if let Some(source) = source {
                    let _ = std::fs::remove_file(source);
                }
                let state = handler.state.lock().await;
                if mutation == "select-pane -m" {
                    assert_eq!(
                        state.marked_pane_target(),
                        Some(PaneTarget::with_window(session.clone(), 1, 0)),
                        "{entry} {prefix:?}{mutation}"
                    );
                } else {
                    assert!(
                        state
                            .sessions
                            .session(&session)
                            .and_then(|s| s.window_at(1))
                            .is_none(),
                        "explicit {entry} {prefix:?}{mutation} executes"
                    );
                }
            }
        }
    }
}

#[tokio::test]
async fn retired_children_preserve_independent_global_commands() {
    for entry in ["if-shell", "run-shell", "source-file"] {
        let handler = RequestHandler::new();
        let session = session_with_native_spare_window(&handler, "historical-global").await;
        let (command, source) = wrapped_command(
            entry,
            "set-option -g @historical-safe yes ; set-buffer -b independent HIST",
        )
        .await;
        execute_retired_child_command(&handler, &session, &command)
            .await
            .expect("independent global commands execute");
        if let Some(source) = source {
            let _ = std::fs::remove_file(source);
        }
        let state = handler.state.lock().await;
        assert_eq!(
            state
                .buffers
                .show(Some("independent"))
                .expect("global commands execute")
                .1,
            b"HIST",
            "{entry}"
        );
    }
}

#[tokio::test]
async fn retired_children_keep_independent_ids_and_window_destinations() {
    for entry in ["if-shell", "run-shell", "source-file"] {
        for independent in [false, true] {
            let handler = RequestHandler::new();
            let session = session_with_native_spare_window(&handler, "historical-index").await;
            let destination = if independent {
                format!("{session}:7")
            } else {
                ":".to_owned()
            };
            let (command, source) =
                wrapped_command(entry, &format!("new-window -d -t {destination}")).await;
            let result = execute_retired_child_command(&handler, &session, &command).await;
            if let Some(source) = source {
                let _ = std::fs::remove_file(source);
            }
            assert_eq!(result.is_ok(), independent, "{entry}: {result:?}");
            let state = handler.state.lock().await;
            let session = state.sessions.session(&session).expect("surviving session");
            assert_eq!(session.window_at(7).is_some(), independent);
            assert!(
                session.window_at(0).is_none(),
                "relative insertion cannot reuse the retired slot"
            );
        }
        let handler = RequestHandler::new();
        let session = session_with_native_spare_window(&handler, "historical-id").await;
        let id = {
            let state = handler.state.lock().await;
            state
                .sessions
                .session(&session)
                .and_then(|s| s.window_at(1))
                .and_then(|w| w.pane(0))
                .expect("live independent pane")
                .id()
        };
        let (command, source) = wrapped_command(entry, &format!("select-pane -m -t {id}")).await;
        execute_retired_child_command(&handler, &session, &command)
            .await
            .expect("absolute pane id is independent");
        if let Some(source) = source {
            let _ = std::fs::remove_file(source);
        }
        assert_eq!(
            handler.state.lock().await.marked_pane_target(),
            Some(PaneTarget::with_window(session, 1, 0))
        );
    }
}
