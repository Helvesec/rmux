use super::*;
use crate::session_fixture::SessionSetupExt as _;
use rmux_core::command_parser::CommandParser;
use rmux_proto::{SessionName, TerminalSize};
use std::time::Duration;

static SOURCE_ID: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[cfg(unix)]
fn quiet_selection_command() -> Vec<String> {
    ["/bin/sh", "-c", "sleep 60"]
        .into_iter()
        .map(str::to_owned)
        .collect()
}

#[cfg(windows)]
fn quiet_selection_command() -> Vec<String> {
    let system_root =
        std::env::var_os("SystemRoot").unwrap_or_else(|| std::ffi::OsString::from(r"C:\Windows"));
    let cmd = std::path::PathBuf::from(system_root)
        .join("System32")
        .join("cmd.exe");
    vec![
        cmd.to_string_lossy().into_owned(),
        "/d".to_owned(),
        "/q".to_owned(),
        "/c".to_owned(),
        "ping -n 120 127.0.0.1 >NUL".to_owned(),
    ]
}

async fn selection_race(mutation: &str) {
    selection_entry_race(mutation, "queue").await;
}

async fn selection_entry_race(mutation: &str, entry: &str) {
    selection_capture_race(mutation, entry, false, "new-window-existing-selection").await;
}

async fn selection_capture_race(mutation: &str, entry: &str, detached: bool, boundary: &str) {
    let handler = RequestHandler::new();
    let name = SessionName::new("selection-race").unwrap();
    let size = TerminalSize { cols: 80, rows: 24 };
    let holder = SessionName::new("selection-holder").unwrap();
    if mutation == "relink" {
        for session_name in [&name, &holder] {
            let response = handler
                .handle(Request::NewSession(rmux_proto::NewSessionRequest {
                    session_name: session_name.clone(),
                    detached: true,
                    size: Some(size),
                    environment: None,
                }))
                .await;
            crate::session_fixture::assert_new_session(&response);
        }
        let response = handler
            .handle(Request::NewWindow(Box::new(NewWindowRequest {
                target: name.clone(),
                name: Some("found".to_owned()),
                detached: true,
                environment: None,
                command: Some(quiet_selection_command()),
                process_command: None,
                start_directory: None,
                target_window_index: Some(1),
                insert_at_target: false,
            })))
            .await;
        assert!(matches!(response, Response::NewWindow(_)), "{response:?}");
        link_selection_window(&handler, &name, &holder).await;
    } else {
        let mut state = handler.state.lock().await;
        state.sessions.create_session(name.clone(), size).unwrap();
        let mut session = state.sessions.session_setup(&name).unwrap();
        session.insert_window_with_initial_pane(1, size).unwrap();
        session.rename_window(1, "found".to_owned()).unwrap();
    }
    let pause = super::super::install_queue_exact_target_capture_pause(&handler, boundary);
    let runner = handler.clone();
    let command = format!(
        "new-window -S {} -t selection-race -n found",
        if detached { "-d" } else { "" }
    );
    let source_path = (entry == "source").then(|| {
        let path = crate::test_shell::ScratchPath::reserve(
            &format!(
                "selection-race-source-{}",
                SOURCE_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ),
            ".conf",
        );
        std::fs::write(&path, &command).unwrap();
        path
    });
    let queued_command = source_path.as_ref().map_or_else(
        || command.to_owned(),
        |path| format!("source-file '{}'", path.display()),
    );
    let binding = entry == "binding";
    let canonical = entry == "canonical";
    let control = entry == "control";
    let _control_events = if control {
        use crate::control::{ControlModeUpgrade, CONTROL_SERVER_EVENT_CAPACITY};
        let (tx, rx) = tokio::sync::mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
        handler
            .register_control_with_closing(
                std::process::id(),
                ControlModeUpgrade {
                    initial_command_count: 0,
                    mode: rmux_proto::ControlMode::Plain,
                    terminal_context: crate::outer_terminal::OuterTerminalContext::default(),
                },
                tx,
                std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            )
            .await;
        handler
            .set_control_session(std::process::id(), Some(name.clone()))
            .await
            .unwrap();
        Some(rx)
    } else {
        None
    };
    let task = tokio::spawn(async move {
        if canonical {
            let response = runner
                .handle(Request::SourceFile(Box::new(
                    rmux_proto::SourceFileRequest {
                        paths: vec![
                            rmux_proto::INTERNAL_CANONICAL_COMMAND_EXECUTION_PATH.to_owned()
                        ],
                        quiet: false,
                        parse_only: false,
                        verbose: false,
                        expand_paths: false,
                        target: None,
                        caller_cwd: None,
                        stdin: Some(queued_command),
                    },
                )))
                .await;
            match response {
                Response::Error(error) => Err(error.error),
                Response::SourceFile(response) => {
                    if response.exit_status.unwrap_or(0) == 0 {
                        assert!(response.stderr.is_empty(), "{response:?}");
                        Ok(())
                    } else {
                        Err(RmuxError::Server(
                            String::from_utf8(response.stderr).unwrap(),
                        ))
                    }
                }
                other => panic!("unexpected canonical response: {other:?}"),
            }
        } else if control {
            let commands = runner
                .parse_control_commands(&queued_command)
                .await
                .unwrap();
            runner
                .execute_control_commands(std::process::id(), commands)
                .await
                .error
                .map_or(Ok(()), Err)
        } else if binding {
            runner
                .execute_hook_command_with_target_binding(
                    std::process::id(),
                    &queued_command,
                    Some(Target::Session(SessionName::new("selection-race").unwrap())),
                    None,
                )
                .await
                .map(|_| ())
        } else {
            runner
                .execute_parsed_commands_for_test(
                    std::process::id(),
                    CommandParser::new().parse(&queued_command).unwrap(),
                )
                .await
                .map(|_| ())
        }
    });
    tokio::time::timeout(Duration::from_secs(5), pause.reached.notified())
        .await
        .unwrap();
    {
        let mut state = handler.state.lock().await;
        let mut session = state.sessions.session_setup(&name).unwrap();
        match mutation {
            "replace" => {
                session.remove_window_allowing_empty(1).unwrap();
                session.insert_window_with_initial_pane(1, size).unwrap();
                session.rename_window(1, "found".to_owned()).unwrap();
            }
            "remove" => {
                session.remove_window_allowing_empty(1).unwrap();
            }
            "session-replace" => {
                drop(session);
                let _ = state.sessions.remove_session(&name).unwrap();
                state.sessions.create_session(name.clone(), size).unwrap();
                let mut replacement = state.sessions.session_setup(&name).unwrap();
                replacement
                    .insert_window_with_initial_pane(1, size)
                    .unwrap();
                replacement.rename_window(1, "found".to_owned()).unwrap();
            }
            "duplicate" => {
                session.insert_window_with_initial_pane(2, size).unwrap();
                session.rename_window(2, "found".to_owned()).unwrap();
            }
            "rename" => {
                session.rename_window(1, "renamed".to_owned()).unwrap();
            }
            "rekey" => {
                let _ = state
                    .sessions
                    .move_window(&name, 1, 2, false, false)
                    .unwrap();
            }
            "unchanged" | "relink" => {}
            _ => unreachable!(),
        }
        state.retire_removed_lifecycle_targets();
    }
    if mutation == "relink" {
        let (window_id, occurrence) = {
            let state = handler.state.lock().await;
            (
                state
                    .sessions
                    .session(&name)
                    .unwrap()
                    .window_at(1)
                    .unwrap()
                    .id(),
                state.window_link_occurrence_id(&name, 1).unwrap(),
            )
        };
        let response = handler
            .handle(Request::UnlinkWindow(rmux_proto::UnlinkWindowRequest {
                target: WindowTarget::with_window(name.clone(), 1),
                kill_if_last: false,
            }))
            .await;
        assert!(
            matches!(response, Response::UnlinkWindow(_)),
            "{response:?}"
        );
        link_selection_window(&handler, &holder, &name).await;
        let state = handler.state.lock().await;
        assert_eq!(
            state
                .sessions
                .session(&name)
                .unwrap()
                .window_at(1)
                .unwrap()
                .id(),
            window_id
        );
        assert_ne!(state.window_link_occurrence_id(&name, 1), Some(occurrence));
    }
    let windows_after_conflict = window_snapshot(&handler, &name).await;
    pause.release.notify_one();
    let result = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
    if let Some(path) = source_path {
        std::fs::remove_file(path).unwrap();
    }
    assert_eq!(
        window_snapshot(&handler, &name).await,
        windows_after_conflict,
        "selection must not create or mutate windows: {entry}/{mutation}/{detached}/{boundary}"
    );
    let state = handler.state.lock().await;
    let active = state.sessions.session(&name).unwrap().active_window_index();
    if matches!(
        mutation,
        "replace" | "remove" | "rekey" | "session-replace" | "relink"
    ) {
        let error = result.expect_err("invalidated selection must fail");
        // A control client's lost session authority is rejected by access
        // policy before the retained window guard. Keep that precedence.
        let expected = if entry == "control" && mutation == "session-replace" {
            "client is read-only"
        } else {
            "replaced"
        };
        assert!(
            error.to_string().contains(expected),
            "{entry}/{mutation}/{detached}/{boundary}: {error}"
        );
        assert_eq!(active, 0, "must not select replacement");
    } else {
        assert!(result.is_ok(), "same object remains selectable: {result:?}");
        assert_eq!(active, if detached { 0 } else { 1 });
    }
    drop(state);
    if mutation == "relink" {
        for target in [name, holder] {
            let response = handler
                .handle(Request::KillSession(rmux_proto::KillSessionRequest {
                    target,
                    kill_all_except_target: false,
                    clear_alerts: false,
                    kill_group: false,
                }))
                .await;
            assert!(matches!(response, Response::KillSession(_)), "{response:?}");
        }
    }
}

#[tokio::test]
async fn existing_selection_rejects_replacement() {
    selection_race("replace").await;
}
#[tokio::test]
async fn existing_selection_rejects_removal() {
    selection_race("remove").await;
}
#[tokio::test]
async fn existing_selection_allows_rename() {
    selection_race("rename").await;
}
#[tokio::test]
async fn existing_selection_allows_unchanged() {
    selection_race("unchanged").await;
}

#[tokio::test]
async fn existing_selection_rejects_rekey() {
    selection_race("rekey").await;
}
#[tokio::test]
async fn existing_selection_source_rejects_replacement() {
    selection_entry_race("replace", "source").await;
}
#[tokio::test]
async fn existing_selection_binding_rejects_replacement() {
    selection_entry_race("replace", "binding").await;
}

// One controlled schedule per cell: resolve/capture -> conflicting actor commits
// -> resume selection. The earlier boundary is before access-policy awaits;
// the later boundary is immediately before selection/no-op validation.
#[tokio::test]
async fn existing_selection_before_wait_matrix() {
    let mut schedules = 0;
    for entry in ["queue", "source", "binding", "canonical", "control"] {
        for detached in [false, true] {
            for mutation in [
                "replace",
                "remove",
                "session-replace",
                "rekey",
                "relink",
                "rename",
                "duplicate",
                "unchanged",
            ] {
                selection_capture_race(mutation, entry, detached, "new-window").await;
                schedules += 1;
            }
        }
    }
    assert_eq!(schedules, 80);
}

#[tokio::test]
async fn existing_selection_detached_validates_after_wait() {
    for entry in ["queue", "source", "binding", "canonical", "control"] {
        selection_capture_race("replace", entry, true, "new-window-existing-selection").await;
    }
}

async fn window_snapshot(handler: &RequestHandler, name: &SessionName) -> String {
    let state = handler.state.lock().await;
    let session = state.sessions.session(name).unwrap();
    format!(
        "{:?}",
        (
            session.id(),
            session
                .windows()
                .map(|(index, window)| (
                    index,
                    window.id(),
                    window
                        .panes()
                        .iter()
                        .map(|pane| pane.id())
                        .collect::<Vec<_>>()
                ))
                .collect::<Vec<_>>()
        )
    )
}

async fn link_selection_window(
    handler: &RequestHandler,
    source: &SessionName,
    target: &SessionName,
) {
    let response = handler
        .handle(Request::LinkWindow(rmux_proto::LinkWindowRequest {
            source: WindowTarget::with_window(source.clone(), 1),
            target: WindowTarget::with_window(target.clone(), 1),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(response, Response::LinkWindow(_)), "{response:?}");
}
