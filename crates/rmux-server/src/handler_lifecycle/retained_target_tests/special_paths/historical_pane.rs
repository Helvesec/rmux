use super::*;

#[path = "historical_pane/tombstone.rs"]
mod tombstone;

#[path = "historical_pane/pane_identity.rs"]
mod pane_identity;

#[tokio::test]
async fn historical_child_kill_pane_never_targets_the_surviving_window() {
    let handler = RequestHandler::new();
    let session_name =
        tombstone::session_with_native_spare_window(&handler, "historical-child-kill-pane").await;

    let result = tombstone::execute_retired_child_command(
        &handler,
        &session_name,
        "if-shell -F 1 'kill-pane'",
    )
    .await;

    let state = handler.state.lock().await;
    assert!(
        state
            .sessions
            .session(&session_name)
            .and_then(|session| session.window_at(1))
            .and_then(|window| window.pane(0))
            .is_some(),
        "a historical child must preserve the unrelated surviving pane"
    );
    assert!(
        result.is_err(),
        "a retired implicit role must report an error"
    );
}

async fn dispatch_historical_pane_hook(
    handler: &RequestHandler,
    session_name: &SessionName,
    command: &str,
) {
    dispatch_pane_hook(handler, session_name, command, true).await;
}

async fn dispatch_pane_hook(
    handler: &RequestHandler,
    session_name: &SessionName,
    command: &str,
    retire: bool,
) {
    let response = handler
        .handle(Request::SetHookMutation(SetHookMutationRequest {
            scope: ScopeSelector::Global,
            hook: HookName::PaneFocusOut,
            command: Some(command.to_owned()),
            lifecycle: HookLifecycle::Persistent,
            append: false,
            unset: false,
            run_immediately: false,
            index: None,
        }))
        .await;
    assert!(matches!(response, Response::SetHook(_)), "{response:?}");

    let event = {
        let mut state = handler.state.lock().await;
        let event = super::super::super::super::prepare_lifecycle_event(
            &mut state,
            &rmux_core::LifecycleEvent::PaneFocusOut {
                target: PaneTarget::with_window(session_name.clone(), 0, 0),
            },
        );
        if retire {
            state
                .sessions
                .session_setup(session_name)
                .expect("historical pane session survives")
                .remove_window_allowing_empty(0)
                .expect("remove historical pane window");
            let lease = event
                .retained_current_target
                .as_ref()
                .expect("pane focus-out captures a retained pane");
            lease.retire_historical();
        }
        event
    };
    handler.dispatch_lifecycle_hook(event).await;
}

#[tokio::test]
async fn run_shell_executes_without_a_live_writer() {
    let handler = RequestHandler::new();
    let session_name = session_with_spare_window(&handler, "historical-run-shell").await;
    let marker = unique_project_path("historical-run-shell.log");
    let shell_command = marker_file_command(&marker);

    dispatch_historical_pane_hook(
        &handler,
        &session_name,
        &format!(
            "run-shell {}",
            crate::test_shell::command_quote(&shell_command)
        ),
    )
    .await;
    let marker_contents = std::fs::read_to_string(&marker);
    let _ = std::fs::remove_file(&marker);
    assert_eq!(marker_contents.as_deref().ok(), Some("HIST"));
}

#[tokio::test]
async fn if_shell_executes_target_independent_branch() {
    let handler = RequestHandler::new();
    let session_name = session_with_spare_window(&handler, "historical-if-shell").await;

    dispatch_historical_pane_hook(
        &handler,
        &session_name,
        "if-shell -F 1 'set-buffer -b historical-if-shell HIST'",
    )
    .await;
    let state = handler.state.lock().await;
    assert_eq!(
        state
            .buffers
            .show(Some("historical-if-shell"))
            .expect("target-independent if-shell branch executes")
            .1,
        b"HIST"
    );
}

#[tokio::test]
async fn source_file_executes_target_independent_commands() {
    let handler = RequestHandler::new();
    let session_name = session_with_spare_window(&handler, "historical-source-file").await;
    let source = unique_project_path("historical-source-file.conf");
    std::fs::write(&source, "set-buffer -b historical-source-file HIST\n")
        .expect("write historical source fixture");

    dispatch_historical_pane_hook(
        &handler,
        &session_name,
        &format!(
            "source-file {}",
            crate::test_shell::command_quote(&source.display().to_string())
        ),
    )
    .await;
    let _ = std::fs::remove_file(source);
    let state = handler.state.lock().await;
    assert_eq!(
        state
            .buffers
            .show(Some("historical-source-file"))
            .expect("target-independent sourced command executes")
            .1,
        b"HIST"
    );
}

#[tokio::test]
async fn special_children_keep_the_retired_target_guard() {
    let source = unique_project_path("historical-guard-source.conf");
    std::fs::write(&source, "rename-window stale-source\n").expect("write guarded source fixture");
    let cases = [
        (
            "run-shell-commands",
            "run-shell -C 'rename-window stale-run-shell'".to_owned(),
            "stale-run-shell",
        ),
        (
            "if-shell",
            "if-shell -F 1 'rename-window stale-if-shell'".to_owned(),
            "stale-if-shell",
        ),
        (
            "source-file",
            format!(
                "source-file {}",
                crate::test_shell::command_quote(&source.display().to_string())
            ),
            "stale-source",
        ),
    ];

    for (case, command, forbidden_name) in cases {
        let handler = RequestHandler::new();
        let session_name =
            session_with_spare_window(&handler, &format!("historical-guard-{case}")).await;
        dispatch_historical_pane_hook(&handler, &session_name, &command).await;
        let state = handler.state.lock().await;
        assert_ne!(
            state
                .sessions
                .session(&session_name)
                .and_then(|session| session.window_at(1))
                .and_then(rmux_core::Window::name),
            Some(forbidden_name),
            "historical child must not fall back to the surviving window"
        );
    }
    let _ = std::fs::remove_file(source);
}

fn unique_project_path(label: &str) -> crate::test_shell::ScratchPath {
    crate::test_shell::ScratchPath::reserve(label, "")
}

#[cfg(unix)]
fn marker_file_command(marker: &std::path::Path) -> String {
    format!("printf HIST > {}", crate::test_shell::sh_quote_path(marker))
}

#[cfg(windows)]
fn marker_file_command(marker: &std::path::Path) -> String {
    crate::test_shell::powershell_encoded_command(&format!(
        "[IO.File]::WriteAllText({}, 'HIST')",
        crate::test_shell::powershell_quote_path(marker)
    ))
}

#[tokio::test]
async fn historical_if_shell_formats_never_substitute_a_surviving_pane() {
    let mut failures = Vec::new();
    for (mode, flags) in [
        ("format", "-F"),
        ("background-format", "-bF"),
        ("shell", ""),
        ("background-shell", "-b"),
    ] {
        for retire in [true, false] {
            for explicit in [false, true] {
                let handler = RequestHandler::new();
                let session_name = session_with_spare_window(&handler, "historical-format").await;
                let (retired_id, survivor_id, retired_window, survivor_window) = {
                    let state = handler.state.lock().await;
                    let session = state
                        .sessions
                        .session(&session_name)
                        .expect("session exists");
                    let original = session.window_at(0).unwrap();
                    let survivor = session.window_at(1).unwrap();
                    (
                        original.pane_id(0).unwrap(),
                        survivor.pane_id(0).unwrap(),
                        original.id(),
                        survivor.id(),
                    )
                };
                assert_ne!(
                    retired_id, survivor_id,
                    "the survivor must have a distinct stable ID"
                );
                let expected_pane = if explicit { survivor_id } else { retired_id };
                let expected_window = if explicit {
                    survivor_window.to_string()
                } else if retire {
                    String::new()
                } else {
                    retired_window.to_string()
                };
                // pane_id and hook_* come from the event snapshot. window_id has
                // no retained value and must never come from a substitute pane.
                let predicate = format!(
                    "#{{==:#{{pane_id}}|#{{window_id}}|#{{hook_pane}}|#{{hook_session_name}},{expected_pane}|{expected_window}|{retired_id}|{session_name}}}"
                );
                let condition = if flags.contains('F') {
                    predicate
                } else {
                    format_predicate_shell(&predicate)
                };
                let target = if explicit {
                    format!("-t {session_name}:1.0")
                } else {
                    String::new()
                };
                let command = format!(
                    "if-shell {flags} {target} {} 'set-buffer -b historical-format SAFE' 'set-buffer -b historical-format LEAK'",
                    crate::test_shell::command_quote(&condition)
                );
                dispatch_pane_hook(&handler, &session_name, &command, retire).await;
                let result = tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        if let Ok((_, bytes)) = handler
                            .state
                            .lock()
                            .await
                            .buffers
                            .show(Some("historical-format"))
                        {
                            break bytes.to_vec();
                        }
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                })
                .await
                .expect("selected independent branch completes");
                if result != b"SAFE" {
                    failures.push(format!(
                        "{mode}, retired={retire}, explicit={explicit}: {result:?}"
                    ));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[cfg(unix)]
fn format_predicate_shell(predicate: &str) -> String {
    format!("test '{predicate}' = 1")
}

#[cfg(windows)]
fn format_predicate_shell(predicate: &str) -> String {
    format!(
        "powershell.exe -NoProfile -NonInteractive -Command \"if ('{predicate}' -eq '1') {{ exit 0 }} else {{ exit 1 }}\""
    )
}
