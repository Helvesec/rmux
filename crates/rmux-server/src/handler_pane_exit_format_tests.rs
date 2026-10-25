use super::RequestHandler;
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
    assert!(matches!(created, Response::NewSession(_)));
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

    let deadline = Instant::now() + Duration::from_secs(5);
    while display_pane_dead(&handler, target.clone()).await != "1" {
        assert!(Instant::now() < deadline, "the kept pane never died");
        sleep(Duration::from_millis(20)).await;
    }
    let capture = run_commands(&handler, "capture-pane -p -t alpha:0.0").await;
    assert!(capture.contains("Pane is dead ("), "{capture:?}");
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
    assert!(matches!(created, Response::NewSession(_)), "{created:?}");

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
