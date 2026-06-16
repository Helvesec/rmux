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
        .handle(Request::RespawnPane(RespawnPaneRequest {
            target: target.clone(),
            kill: true,
            start_directory: None,
            environment: None,
            command: Some(successful_exit_command()),
            process_command: None,
        }))
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
