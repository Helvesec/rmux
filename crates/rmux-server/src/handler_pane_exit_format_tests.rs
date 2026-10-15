use super::RequestHandler;
use rmux_proto::{
    DisplayMessageRequest, NewSessionRequest, OptionName, PaneTarget, Request, RespawnPaneRequest,
    Response, ScopeSelector, SessionName, SetOptionMode, SetOptionRequest, Target, TerminalSize,
};
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
    assert!(matches!(created, Response::NewSession(_)));

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

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let dead = display_pane_dead(&handler, target.clone()).await;
        if dead == "1" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "pane_dead did not flip promptly, last value was {dead:?}"
        );
        sleep(Duration::from_millis(20)).await;
    }
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
    assert!(matches!(
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
        Response::NewSession(_)
    ));
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
