use super::RequestHandler;
use crate::handler::test_support::SERVER_SIGNAL_LIVENESS;
use crate::pane_io::PaneExitEvent;
use rmux_proto::{
    DisplayMessageRequest, NewSessionRequest, OptionName, PaneTarget, Request, RespawnPaneRequest,
    Response, ScopeSelector, SessionName, SetOptionMode, SetOptionRequest, Target, TerminalSize,
};
#[cfg(windows)]
use rmux_proto::{HasSessionRequest, HasSessionResponse, NewSessionExtRequest};
use tokio::time::{sleep, Duration, Instant};

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

#[cfg(windows)]
fn successful_exit_command() -> Vec<String> {
    vec!["exit 0".to_owned()]
}

#[cfg(not(windows))]
fn successful_exit_command() -> Vec<String> {
    vec!["true".to_owned()]
}

#[tokio::test]
async fn display_message_pane_dead_observes_exited_child_promptly() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let target = PaneTarget::with_window(alpha.clone(), 0, 0);

    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))
        .await;
    crate::session_fixture::assert_new_session(&created);

    let remain_on_exit = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Pane(target.clone()),
            option: OptionName::RemainOnExit,
            value: "on".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(remain_on_exit, Response::SetOption(_)));

    let respawned = handler
        .handle(Request::RespawnPane(Box::new(RespawnPaneRequest {
            target: target.clone(),
            kill: true,
            start_directory: None,
            environment: None,
            command: Some(successful_exit_command()),
            process_command: None,
        })))
        .await;
    assert!(matches!(respawned, Response::RespawnPane(_)));

    let deadline = Instant::now() + SERVER_SIGNAL_LIVENESS;
    loop {
        let dead = display_pane_dead(&handler, target.clone()).await;
        if dead == "1" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the kept pane never published its death; last pane_dead was {dead:?}"
        );
        sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn observed_dead_status_still_commits_kept_dead_preparation() {
    let handler = RequestHandler::new();
    let alpha = session_name("observed-before-prepare");
    let target = PaneTarget::with_window(alpha.clone(), 0, 0);

    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))
        .await;
    crate::session_fixture::assert_new_session(&created);

    for (option, value) in [
        (OptionName::RemainOnExit, "on"),
        (OptionName::RemainOnExitFormat, "PREPARED"),
    ] {
        let response = handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Pane(target.clone()),
                option,
                value: value.to_owned(),
                mode: SetOptionMode::Replace,
            }))
            .await;
        assert!(matches!(response, Response::SetOption(_)), "{response:?}");
    }

    let (pane_id, generation) = {
        let mut state = handler.state.lock().await;
        let pane_id = state
            .sessions
            .session(&alpha)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0))
            .map(rmux_core::Pane::id)
            .expect("initial pane exists");
        let generation = state.pane_output_generation_for_target(&target, pane_id);
        state
            .mark_pane_dead_without_exit_details(&target)
            .expect("an observer can record exit metadata before lifecycle preparation");
        assert!(
            state.pane_is_dead(&alpha, pane_id),
            "the precondition is an observed exit"
        );
        (pane_id, generation)
    };

    handler
        .handle_pane_exit_event(PaneExitEvent::eof_published(
            alpha.clone(),
            pane_id,
            Some(generation),
        ))
        .await;

    let state = handler.state.lock().await;
    let transcript = state
        .transcript_handle(&target)
        .expect("kept pane retains its transcript");
    let screen = transcript.lock().clone_screen();
    let mode = screen.mode();
    assert_eq!(
        mode & rmux_core::input::mode::MODE_CURSOR,
        0,
        "committing the kept-dead pane must hide its cursor even when exit was already observed"
    );
    assert!(
        (0..usize::from(screen.size().rows)).any(|row| {
            let mut text = String::new();
            screen.visit_visible_line_cells(row, usize::from(screen.size().cols), |cell| {
                if !cell.is_padding() {
                    text.push_str(cell.text());
                }
            });
            text.contains("PREPARED")
        }),
        "committing the kept-dead pane must draw remain-on-exit status"
    );
}

#[test]
fn program_status_unparsed_reports_cannot_cross_foreground_groups() {
    for (before, queued) in [
        (&b"\x1b]7501;state=wor"[..], &b"king:app=pi\x07"[..]),
        (
            &b"ordinary output\r\n"[..],
            &b"\x1b]7501;state=working:app=pi\x07"[..],
        ),
    ] {
        let mut transcript =
            crate::pane_transcript::PaneTranscript::new(100, TerminalSize { cols: 80, rows: 24 });
        transcript.observe_foreground_group(Some(100));
        transcript.append_bytes(before);
        assert!(transcript.program_status().records().is_empty());
        transcript.observe_foreground_group(Some(200));
        assert!(transcript.program_status_transition_pending());
        transcript.append_bytes(queued);
        assert!(transcript.program_status().records().is_empty());
        transcript.finish_program_status_drain();
        transcript.append_bytes(b"\x1b]7501;state=working:app=pi\x07");
        assert_eq!(transcript.program_status().records().len(), 1);
    }
}

#[test]
fn program_status_foreground_change_expires_active_without_output() {
    let mut transcript =
        crate::pane_transcript::PaneTranscript::new(100, TerminalSize { cols: 80, rows: 24 });
    transcript.observe_foreground_group(Some(100));
    transcript
        .append_bytes(b"\x1b]7501;state=working:app=pi\x07\x1b]7501;state=done:id=result\x07");
    transcript.observe_foreground_group(None); // Inspection failure is not an exit.
    assert_eq!(transcript.program_status().records().len(), 2);
    transcript.observe_foreground_group(Some(100));
    assert_eq!(transcript.program_status().records().len(), 2);
    transcript.observe_foreground_group(Some(200));
    assert_eq!(transcript.program_status().records().len(), 1);
    assert!(transcript.program_status().observed());
    // Queued reports from the former group cannot undo the invalidation.
    transcript.append_bytes(b"\x1b]7501;state=working:app=pi\x07");
    assert_eq!(transcript.program_status().records().len(), 1);
    transcript.append_bytes(b"\x1b]7501;state=wor");
    transcript.finish_program_status_drain(); // Queue empty, but sequence incomplete.
    assert!(transcript.program_status_transition_pending());
    transcript.append_bytes(b"king:app=pi\x07");
    assert_eq!(transcript.program_status().records().len(), 1);
    transcript.finish_program_status_drain();
    assert!(!transcript.program_status_transition_pending());
    transcript.append_bytes(b"\x1b]7501;state=working:app=pi\x07");
    assert_eq!(transcript.program_status().records().len(), 2); // New owner can report.
}

#[test]
fn program_status_exit_retains_results_without_late_active_resurrection() {
    let mut transcript =
        crate::pane_transcript::PaneTranscript::new(100, TerminalSize { cols: 80, rows: 24 });
    let replies = transcript.append_bytes_and_take_replies(b"\x1b]7501;?\x1b\\\x1b[c");
    assert!(replies.replies.starts_with(b"\x1b]7501;?:states="));
    assert!(replies.replies.ends_with(b"\x1b[?1;2c"));
    transcript.append_bytes(b"\x1b]7501;state=working:app=pi\x07\x1b]7501;state=done:id=child\x07");
    transcript.expire_program_status();
    assert_eq!(transcript.program_status().records().len(), 1);
    transcript.append_bytes(b"\x1b]7501;state=blocked:kind=auth\x07");
    assert_eq!(transcript.program_status().records().len(), 1);
    transcript.append_bytes(b"\x1b]7501;state=error:app=pi\x07");
    assert_eq!(transcript.program_status().records().len(), 2);
    assert_eq!(
        transcript
            .program_status()
            .summary()
            .unwrap()
            .state
            .as_str(),
        "error"
    );
}

#[tokio::test]
async fn program_status_formats_are_pane_local_and_session_list_is_batched() {
    let handler = RequestHandler::new();
    let alpha = session_name("native-status");
    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
    );
    {
        let mut state = handler.state.lock().await;
        let pane_id = state
            .sessions
            .session(&alpha)
            .unwrap()
            .active_pane()
            .unwrap()
            .id();
        state
            .append_bytes_to_runtime_pane_transcript(
                &alpha,
                pane_id,
                b"\x1b]7501;state=blocked:app=pi:kind=question:msg=SGVsbG8=\x07",
            )
            .unwrap();
    }
    let response = handler
        .handle(Request::DisplayMessage(DisplayMessageRequest {
            target: Some(Target::Pane(PaneTarget::with_window(alpha, 0, 0))),
            print: true,
            message: Some(
                "#{pane_program_status}|#{pane_program_status_kind}|#{pane_program_status_json}"
                    .to_owned(),
            ),
            empty_target_context: false,
        }))
        .await;
    let Response::DisplayMessage(response) = response else {
        panic!("display-message response");
    };
    let output = String::from_utf8(response.command_output().unwrap().stdout().to_vec()).unwrap();
    let json = output.strip_prefix("blocked|question|").unwrap();
    let value: serde_json::Value = serde_json::from_str(json).unwrap();
    assert_eq!(value["version"], 1);
    assert_eq!(value["summary"]["message"], "Hello");
    assert_eq!(value["records"].as_array().unwrap().len(), 1);
    let response = handler
        .handle(Request::ListSessions(rmux_proto::ListSessionsRequest {
            format: Some("#{session_program_status_json}".to_owned()),
            filter: None,
            sort_order: None,
            reversed: false,
        }))
        .await;
    let Response::ListSessions(response) = response else {
        panic!("list-sessions response");
    };
    let value: serde_json::Value =
        serde_json::from_slice(response.command_output().stdout()).unwrap();
    assert_eq!(value["panes"][0]["summary"]["app"], "pi");
    assert_eq!(value["panes"][0]["summary"]["state"], "blocked");
    assert!(value["panes"][0]["pane_id"]
        .as_str()
        .unwrap()
        .starts_with('%'));
}

#[tokio::test]
async fn pane_dead_query_leaves_a_kept_pane_to_its_exit_commit() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let target = PaneTarget::with_window(alpha.clone(), 0, 0);
    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))
        .await;
    crate::session_fixture::assert_new_session(&created);
    run_commands(&handler, "set-option -p -t alpha:0.0 remain-on-exit on").await;
    let respawned = handler
        .handle(Request::RespawnPane(Box::new(RespawnPaneRequest {
            target: target.clone(),
            kill: true,
            start_directory: None,
            environment: None,
            command: Some(successful_exit_command()),
            process_command: None,
        })))
        .await;
    assert!(matches!(respawned, Response::RespawnPane(_)));

    {
        let mut state = handler.state.lock().await;
        // Hold the lock past the child's exit so the query runs before the commit.
        std::thread::sleep(Duration::from_secs(1));
        let observed = state
            .refresh_pane_exit_status(&target)
            .expect("query reads the pane");
        assert_eq!(observed, None, "the query marked the kept pane dead");
    }

    let deadline = Instant::now() + SERVER_SIGNAL_LIVENESS;
    while display_pane_dead(&handler, target.clone()).await != "1" {
        assert!(Instant::now() < deadline, "the kept pane never died");
        sleep(Duration::from_millis(20)).await;
    }
    let capture = run_commands(&handler, "capture-pane -p -t alpha:0.0").await;
    assert!(capture.contains("Pane is dead ("), "{capture:?}");
}

#[tokio::test]
async fn pane_dead_publication_is_atomic_with_its_footer() {
    for policy in ["on", "failed"] {
        let handler = RequestHandler::new();
        let alpha = session_name("alpha");
        let target = PaneTarget::with_window(alpha.clone(), 0, 0);
        let created = handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha,
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await;
        crate::session_fixture::assert_new_session(&created);
        run_commands(
            &handler,
            &format!("set-option -p -t alpha:0.0 remain-on-exit {policy}"),
        )
        .await;
        run_commands(
            &handler,
            "set-option -p -t alpha:0.0 remain-on-exit-format 'EXIT_COMMITTED:#{pane_dead}:#{pane_dead_status}'",
        )
        .await;
        let pause = handler.install_pane_exit_commit_pause();
        let respawned = handler
            .handle(Request::RespawnPane(Box::new(RespawnPaneRequest {
                target: target.clone(),
                kill: true,
                start_directory: None,
                environment: None,
                command: Some(vec!["exit 7".to_owned()]),
                process_command: None,
            })))
            .await;
        assert!(matches!(respawned, Response::RespawnPane(_)));
        tokio::time::timeout(SERVER_SIGNAL_LIVENESS, pause.reached.notified())
            .await
            .expect("exit is observed and its kept-pane commit is paused");

        let displayed = run_commands(
            &handler,
            "display-message -p -t alpha:0.0 '#{pane_dead}:#{pane_dead_status}'",
        )
        .await;
        let listed = run_commands(
            &handler,
            "list-panes -t alpha:0 -F '#{pane_dead}:#{pane_dead_status}'",
        )
        .await;
        let before = run_commands(&handler, "capture-pane -p -t alpha:0.0").await;
        pause.release.notify_one();
        let deadline = Instant::now() + SERVER_SIGNAL_LIVENESS;
        while display_pane_dead(&handler, target.clone()).await != "1" {
            assert!(Instant::now() < deadline, "kept exit never commits");
            sleep(Duration::from_millis(20)).await;
        }
        let after = run_commands(&handler, "capture-pane -p -t alpha:0.0").await;
        assert_eq!(
            displayed.trim(),
            "0:",
            "display published before footer ({policy})"
        );
        assert_eq!(
            listed.trim(),
            "0:",
            "list published before footer ({policy})"
        );
        assert!(!before.contains("EXIT_COMMITTED"), "{before:?}");
        assert_eq!(after.matches("EXIT_COMMITTED:1:7").count(), 1, "{after:?}");
    }
}

async fn run_commands(handler: &RequestHandler, commands: &str) -> String {
    let parsed = handler
        .parse_control_commands(commands)
        .await
        .expect("commands parse");
    let output = handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect("commands run");
    String::from_utf8_lossy(output.stdout()).into_owned()
}

#[cfg(windows)]
#[tokio::test]
async fn pane_exiting_during_deferred_start_closes_its_session_before_the_start_ends() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let created = handler
        .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
            session_name: Some(alpha.clone()),
            working_directory: None,
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
            command: Some(vec!["exit 3".to_owned()]),
            process_command: None,
            client_environment: None,
            skip_environment_update: false,
        })))
        .await;
    crate::session_fixture::assert_new_session(&created);

    // The deferred start keeps its input queued for 2 s.
    let deadline = Instant::now() + Duration::from_millis(1500);
    loop {
        let response = handler
            .handle(Request::HasSession(HasSessionRequest {
                target: alpha.clone(),
            }))
            .await;
        if matches!(
            response,
            Response::HasSession(HasSessionResponse { exists: false })
        ) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "session of a pane that exited during its start stayed, last {response:?}"
        );
        sleep(Duration::from_millis(20)).await;
    }
}

async fn display_pane_dead(handler: &RequestHandler, target: PaneTarget) -> String {
    let response = handler
        .handle(Request::DisplayMessage(DisplayMessageRequest {
            target: Some(Target::Pane(target)),
            print: true,
            message: Some("#{pane_dead}".to_owned()),
            empty_target_context: false,
        }))
        .await;
    let Response::DisplayMessage(response) = response else {
        panic!("expected display-message response");
    };
    let output = response
        .command_output()
        .expect("display-message -p returns output");
    String::from_utf8(output.stdout().to_vec())
        .expect("pane_dead output is utf8")
        .trim()
        .to_owned()
}

#[tokio::test]
async fn program_status_expires_when_handler_records_pane_exit() {
    let handler = RequestHandler::new();
    let alpha = session_name("exit-status");
    let target = PaneTarget::with_window(alpha.clone(), 0, 0);
    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))
        .await;
    crate::session_fixture::assert_new_session(&created);
    let mut state = handler.state.lock().await;
    let pane_id = state
        .sessions
        .session(&alpha)
        .and_then(|session| session.active_pane())
        .map(rmux_core::Pane::id)
        .expect("initial pane exists");
    state
        .append_bytes_to_runtime_pane_transcript(
            &alpha,
            pane_id,
            b"\x1b]7501;state=working:app=pi\x07\x1b]7501;state=done:id=result\x07",
        )
        .expect("pane transcript accepts output");
    let transcript = state
        .transcript_handle(&target)
        .expect("pane has a transcript");
    assert_eq!(transcript.lock().program_status().records().len(), 2);

    state
        .mark_pane_dead_without_exit_details(&target)
        .expect("exit can be recorded");
    assert_eq!(
        transcript.lock().program_status().records().len(),
        1,
        "recording the pane exit must expire its active program status"
    );
    assert!(state
        .observe_runtime_pane_exit(&alpha, pane_id)
        .expect("recorded exit is observable")
        .is_some());
    assert_eq!(transcript.lock().program_status().records().len(), 1);
}
