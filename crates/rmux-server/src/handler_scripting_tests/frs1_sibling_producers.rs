use super::frs1::{cleanup, queue, window};
use super::*;
use crate::handler::display_message_tests::spawn_requester_with_environment;
pub(super) use crate::handler::display_message_tests::ChildGuard;
use crate::handler::ControlRegistration;
use rmux_os::identity::UserIdentity;
use tokio::sync::mpsc;

/// Starts a requester carrying `RMUX` and `RMUX_PANE`, returned only once it
/// runs its own code, as a CLI client always does before the daemon reads it.
/// On Windows a helper still in start-up showed its environment once and lost
/// it at the next read, so a dispatch then ran without the inherited context.
pub(super) async fn spawn_requester(rmux: &str, pane: &str) -> ChildGuard {
    spawn_requester_with(rmux, pane, &[]).await
}

pub(super) async fn spawn_requester_with(
    rmux: &str,
    pane: &str,
    extra: &[(&str, String)],
) -> ChildGuard {
    let mut environment = vec![("RMUX", rmux.to_owned()), ("RMUX_PANE", pane.to_owned())];
    environment.extend_from_slice(extra);
    let requester = spawn_requester_with_environment(&environment).await;
    wait_for_requester(&requester, rmux, pane).await;
    requester
}

async fn wait_for_requester(requester: &ChildGuard, rmux: &str, pane: &str) {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if let Some(environment) = rmux_os::process::environment(requester.0.id()) {
                if environment.get("RMUX").map(String::as_str) == Some(rmux)
                    && environment.get("RMUX_PANE").map(String::as_str) == Some(pane)
                {
                    return;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("requester installs inherited environment");
}

pub(super) async fn grant_unattached_write_access(
    handler: &RequestHandler,
    requester_pid: u32,
) -> mpsc::Receiver<ControlServerEvent> {
    let (event_tx, event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    handler
        .register_control_with_access(
            requester_pid,
            ControlModeUpgrade {
                initial_command_count: 0,
                mode: rmux_proto::ControlMode::Plain,
                terminal_context: OuterTerminalContext::default(),
            },
            ControlRegistration {
                event_tx,
                closing: Arc::new(AtomicBool::new(false)),
                uid: 1000,
                user: UserIdentity::Uid(1000),
                can_write: true,
            },
        )
        .await
        .expect("unattached requester receives write access");
    event_rx
}

pub(super) async fn setup_sibling_fixture() -> (RequestHandler, SessionName) {
    let handler = RequestHandler::new();
    use_platform_test_shell(&handler).await;
    run_hook_probe_queue(&handler, "set-option -g status off").await;
    #[cfg(unix)]
    run_hook_probe_queue(&handler, "set-option -g default-shell /bin/sh").await;
    let primary = session_name("frs-sibling-primary");
    create_background_identity_session(&handler, primary.clone()).await;
    queue(
        &handler,
        "new-pane -d -t frs-sibling-primary:0.0 -x30 -y8 -X3 -Y4",
    )
    .await
    .expect("floating sibling pane");
    (handler, primary)
}

pub(super) async fn setup_tiled_source_fixture() -> (RequestHandler, SessionName) {
    let handler = RequestHandler::new();
    use_platform_test_shell(&handler).await;
    run_hook_probe_queue(&handler, "set-option -g status off").await;
    #[cfg(unix)]
    run_hook_probe_queue(&handler, "set-option -g default-shell /bin/sh").await;
    let primary = session_name("frs-sibling-primary");
    create_background_identity_session(&handler, primary.clone()).await;
    queue(
        &handler,
        "split-window -d -h -l30 -t frs-sibling-primary:0.0",
    )
    .await
    .expect("tiled sibling pane");
    (handler, primary)
}

fn command_mode_run_shell(command: &str, target: Option<PaneTarget>) -> Request {
    Request::RunShell(Box::new(RunShellRequest {
        command: command.to_owned(),
        arguments: Vec::new(),
        background: false,
        as_commands: true,
        show_stderr: false,
        delay_seconds: None,
        start_directory: None,
        target,
        source_depth: None,
    }))
}

fn extended_session(name: SessionName, cwd: String) -> Request {
    Request::NewSessionExt(Box::new(NewSessionExtRequest {
        session_name: Some(name),
        working_directory: Some(cwd),
        detached: true,
        size: Some(TerminalSize { cols: 80, rows: 24 }),
        environment: None,
        group_target: None,
        attach_if_exists: false,
        detach_other_clients: false,
        kill_other_clients: false,
        flags: None,
        window_name: None,
        print_session_info: false,
        print_format: None,
        command: None,
        process_command: None,
        client_environment: None,
        skip_environment_update: false,
    }))
}

async fn assert_width(handler: &RequestHandler, session: &SessionName, expected: i32) {
    assert_eq!(
        i32::from(
            window(handler, session)
                .await
                .pane(1)
                .expect("pane")
                .geometry()
                .cols()
        ),
        expected
    );
}

#[tokio::test]
async fn sibling_source_runtime_run_shell_and_cwd_keep_inherited_occurrence() {
    let (handler, primary) = setup_tiled_source_fixture().await;
    let linked = session_name("frs-sibling-linked");
    let unrelated = session_name("frs-sibling-unrelated");
    create_background_identity_session(&handler, linked.clone()).await;
    create_background_identity_session(&handler, unrelated.clone()).await;
    let (primary_id, pane_id, unrelated_id) = {
        let state = handler.state.lock().await;
        let primary_session = state.sessions.session(&primary).expect("primary session");
        let primary_window = primary_session.window_at(0).expect("primary window");
        (
            primary_session.id(),
            primary_window.pane(1).expect("tiled pane").id(),
            state
                .sessions
                .session(&unrelated)
                .expect("unrelated session")
                .id(),
        )
    };
    assert!(matches!(
        handler
            .handle(Request::LinkWindow(LinkWindowRequest {
                source: WindowTarget::with_window(primary.clone(), 0),
                target: WindowTarget::with_window(linked.clone(), 1),
                after: false,
                before: false,
                kill_destination: false,
                detached: true,
            }))
            .await,
        Response::LinkWindow(_)
    ));
    let lure_before = window(&handler, &linked).await;
    let lure_id = lure_before.id();
    let lure_active_pane = lure_before.active_pane_index();
    let lure_pane = lure_before.pane(0).expect("lure pane");
    let lure_pane_id = lure_pane.id();
    let lure_geometry = lure_pane.geometry();
    let lure_floating_stack = lure_before.floating_stack().clone();

    let root = temp_root("sibling-producers");
    fs::create_dir_all(&root).expect("private test root");
    let socket_path = root.join("server.sock");
    handler.set_socket_path(&socket_path);
    let pane_environment = pane_id.to_string();
    let valid_rmux = format!("{},1,{}", socket_path.display(), primary_id.as_u32());
    let valid = spawn_requester(&valid_rmux, &pane_environment).await;
    let _valid_events = grant_unattached_write_access(&handler, valid.0.id()).await;
    let source_path = root.join("sibling.conf");
    write_config(&source_path, "resize-pane -x31\n");
    assert_eq!(
        fs::read(&source_path).expect("read compact source fixture"),
        b"resize-pane -x31\n"
    );
    let source = handler
        .dispatch(
            valid.0.id(),
            source_file_request(
                vec![source_path.to_string_lossy().into_owned()],
                Some(root.clone()),
            ),
        )
        .await
        .response;
    let Response::SourceFile(source) = source else {
        panic!("expected source-file response: {source:?}");
    };
    assert_eq!(source.exit_status(), None, "{source:?}");
    assert!(source.stderr().is_empty(), "{source:?}");
    assert!(
        source.command_output().is_none(),
        "non-verbose source-file has no stdout payload: {source:?}"
    );
    assert_width(&handler, &primary, 31).await;

    let runtime = handler
        .dispatch(
            valid.0.id(),
            execution_request(&["resize-pane", "-x", "32"]),
        )
        .await
        .response;
    assert!(matches!(
        execution_result(runtime),
        RuntimeCommandExecutionResponse::Complete {
            exit_status: 0,
            error: None,
            ..
        }
    ));
    assert_width(&handler, &primary, 32).await;

    let run_shell = handler
        .dispatch(
            valid.0.id(),
            command_mode_run_shell("resize-pane -x 33", None),
        )
        .await
        .response;
    assert!(matches!(run_shell, Response::RunShell(_)), "{run_shell:?}");
    assert_width(&handler, &primary, 33).await;
    let lure_after_valid = window(&handler, &linked).await;
    assert_eq!(lure_after_valid.id(), lure_id);
    assert_eq!(lure_after_valid.active_pane_index(), lure_active_pane);
    assert_eq!(
        lure_after_valid.pane(0).expect("lure pane").id(),
        lure_pane_id
    );
    assert_eq!(
        lure_after_valid.pane(0).expect("lure pane").geometry(),
        lure_geometry
    );
    assert_eq!(
        lure_after_valid.floating_stack(),
        lure_floating_stack.clone()
    );

    let expected_cwd = root.join(primary.as_str());
    fs::create_dir_all(&expected_cwd).expect("formatted cwd target");
    let cwd_template = root.join("#{session_name}").to_string_lossy().into_owned();
    let cwd_session = session_name("frs-sibling-cwd-valid");
    let cwd_response = handler
        .dispatch(
            valid.0.id(),
            extended_session(cwd_session.clone(), cwd_template.clone()),
        )
        .await
        .response;
    crate::session_fixture::assert_new_session(&cwd_response);
    {
        let state = handler.state.lock().await;
        let pane_id = state
            .sessions
            .session(&cwd_session)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0))
            .expect("cwd session pane")
            .id();
        assert_eq!(
            state
                .pane_lifecycle(pane_id)
                .expect("cwd pane lifecycle")
                .working_directory(),
            Some(expected_cwd.as_path())
        );
    }

    let stale_rmux = format!("{},1,{}", socket_path.display(), unrelated_id.as_u32());
    let stale = spawn_requester(&stale_rmux, &pane_environment).await;
    let _stale_events = grant_unattached_write_access(&handler, stale.0.id()).await;

    let stale_source = handler
        .dispatch(
            stale.0.id(),
            source_file_request(
                vec![source_path.to_string_lossy().into_owned()],
                Some(root.clone()),
            ),
        )
        .await
        .response;
    assert!(
        matches!(stale_source, Response::Error(_)),
        "{stale_source:?}"
    );
    let stale_runtime = execution_result(
        handler
            .dispatch(
                stale.0.id(),
                execution_request(&["resize-pane", "-x", "34"]),
            )
            .await
            .response,
    );
    assert_eq!(stale_runtime, RuntimeCommandExecutionResponse::Fallback);
    let stale_run_shell = handler
        .dispatch(
            stale.0.id(),
            command_mode_run_shell("resize-pane -x 34", None),
        )
        .await
        .response;
    assert!(
        matches!(stale_run_shell, Response::Error(_)),
        "{stale_run_shell:?}"
    );
    let stale_cwd = handler
        .dispatch(
            stale.0.id(),
            extended_session(session_name("frs-sibling-cwd-stale"), cwd_template),
        )
        .await
        .response;
    assert!(matches!(stale_cwd, Response::Error(_)), "{stale_cwd:?}");
    assert_width(&handler, &primary, 33).await;
    let lure_after_stale = window(&handler, &linked).await;
    assert_eq!(lure_after_stale.id(), lure_id);
    assert_eq!(lure_after_stale.active_pane_index(), lure_active_pane);
    assert_eq!(
        lure_after_stale.pane(0).expect("lure pane").id(),
        lure_pane_id
    );
    assert_eq!(
        lure_after_stale.pane(0).expect("lure pane").geometry(),
        lure_geometry
    );
    assert_eq!(lure_after_stale.floating_stack(), lure_floating_stack);

    let explicit_display = handler
        .dispatch(
            stale.0.id(),
            execution_request(&[
                "display-message",
                "-p",
                "-t",
                "frs-sibling-primary:0.1",
                "#{pane_id}",
            ]),
        )
        .await
        .response;
    assert!(
        matches!(
            execution_result(explicit_display),
            RuntimeCommandExecutionResponse::Complete {
                ref stdout,
                exit_status: 0,
                error: None,
                ..
            } if stdout == format!("{pane_id}\n").as_bytes()
        ),
        "qualified display-message must ignore a stale implicit pair"
    );

    let explicit_runtime = handler
        .dispatch(
            stale.0.id(),
            execution_request(&["resize-pane", "-t", "frs-sibling-primary:0.1", "-x", "34"]),
        )
        .await
        .response;
    assert!(matches!(
        execution_result(explicit_runtime),
        RuntimeCommandExecutionResponse::Complete {
            exit_status: 0,
            error: None,
            ..
        }
    ));
    assert_width(&handler, &primary, 34).await;

    let last_relative = execution_result(
        handler
            .dispatch(
                valid.0.id(),
                execution_request(&[
                    "resize-pane",
                    "-t",
                    "frs-sibling-primary:0.1",
                    "-t",
                    ".",
                    "-x31",
                ]),
            )
            .await
            .response,
    );
    assert!(matches!(
        last_relative,
        RuntimeCommandExecutionResponse::Complete {
            exit_status: 0,
            error: None,
            ..
        }
    ));
    assert_width(&handler, &primary, 31).await;

    let stale_last_relative = execution_result(
        handler
            .dispatch(
                stale.0.id(),
                execution_request(&[
                    "resize-pane",
                    "-t",
                    "frs-sibling-primary:0.1",
                    "-t",
                    ".",
                    "-x32",
                ]),
            )
            .await
            .response,
    );
    assert_eq!(
        stale_last_relative,
        RuntimeCommandExecutionResponse::Fallback
    );
    assert_width(&handler, &primary, 31).await;

    let stale_last_qualified = execution_result(
        handler
            .dispatch(
                stale.0.id(),
                execution_request(&[
                    "resize-pane",
                    "-t",
                    ".",
                    "-tfrs-sibling-primary:0.1",
                    "-x32",
                ]),
            )
            .await
            .response,
    );
    assert!(matches!(
        stale_last_qualified,
        RuntimeCommandExecutionResponse::Complete {
            exit_status: 0,
            error: None,
            ..
        }
    ));
    assert_width(&handler, &primary, 32).await;
    let lure_after_precedence = window(&handler, &linked).await;
    assert_eq!(lure_after_precedence.id(), lure_id);
    assert_eq!(lure_after_precedence.active_pane_index(), lure_active_pane);
    assert_eq!(
        lure_after_precedence.pane(0).expect("lure pane").id(),
        lure_pane_id
    );
    assert_eq!(
        lure_after_precedence.pane(0).expect("lure pane").geometry(),
        lure_geometry
    );
    assert_eq!(lure_after_precedence.floating_stack(), lure_floating_stack);

    let pinned = CommandParser::new()
        .parse("resize-pane -t frs-sibling-unrelated:0.0 -t . -x33")
        .expect("pinned repeated-target queue parses");
    handler
        .execute_parsed_commands(
            valid.0.id(),
            pinned,
            QueueExecutionContext::without_caller_cwd().with_current_target(Some(Target::Pane(
                PaneTarget::with_window(primary.clone(), 0, 1),
            ))),
        )
        .await
        .expect("last relative target retains the pinned current target");
    assert_width(&handler, &primary, 33).await;
    let lure_after_pinned = window(&handler, &linked).await;
    assert_eq!(lure_after_pinned.id(), lure_id);
    assert_eq!(lure_after_pinned.active_pane_index(), lure_active_pane);
    assert_eq!(
        lure_after_pinned.pane(0).expect("lure pane").id(),
        lure_pane_id
    );
    assert_eq!(
        lure_after_pinned.pane(0).expect("lure pane").geometry(),
        lure_geometry
    );
    assert_eq!(lure_after_pinned.floating_stack(), lure_floating_stack);

    let literal_cwd = root.join("literal");
    fs::create_dir_all(&literal_cwd).expect("literal cwd");
    let literal_session = session_name("frs-sibling-cwd-literal");
    let literal_response = handler
        .dispatch(
            stale.0.id(),
            extended_session(
                literal_session.clone(),
                literal_cwd.to_string_lossy().into_owned(),
            ),
        )
        .await
        .response;
    crate::session_fixture::assert_new_session(&literal_response);
    {
        let state = handler.state.lock().await;
        let pane_id = state
            .sessions
            .session(&literal_session)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0))
            .expect("literal cwd pane")
            .id();
        assert_eq!(
            state
                .pane_lifecycle(pane_id)
                .expect("literal cwd lifecycle")
                .working_directory(),
            Some(literal_cwd.as_path())
        );
    }

    cleanup(&handler).await;
    drop(stale);
    drop(valid);
    let _ = fs::remove_dir_all(root);
}
