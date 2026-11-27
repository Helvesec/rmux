use super::*;
use crate::handler::test_support::SERVER_SIGNAL_LIVENESS;

#[tokio::test]
async fn respawn_window_retains_custom_title_after_remain_on_exit() {
    let handler = RequestHandler::new();
    let name = session_name("respawn-title");
    create_session(&handler, name.as_str()).await;
    let target = PaneTarget::with_window(name.clone(), 0, 0);
    let title = "retained.respawn.example";
    let response = handler
        .handle(Request::SelectPane(Box::new(SelectPaneRequest {
            target: target.clone(),
            title: Some(title.to_owned()),
            input_disabled: None,
            preserve_zoom: false,
            style: None,
        })))
        .await;
    assert!(matches!(response, Response::SelectPane(_)), "{response:?}");
    let response = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Pane(target.clone()),
            option: OptionName::RemainOnExit,
            value: "on".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(response, Response::SetOption(_)), "{response:?}");
    let response = handler
        .handle(Request::RespawnPane(Box::new(
            rmux_proto::RespawnPaneRequest {
                target: target.clone(),
                kill: true,
                start_directory: None,
                environment: None,
                command: Some(vec!["exit 0".to_owned()]),
                process_command: None,
            },
        )))
        .await;
    assert!(matches!(response, Response::RespawnPane(_)), "{response:?}");
    timeout(SERVER_SIGNAL_LIVENESS, async {
        loop {
            if pane_title_state(&handler, target.clone(), "#{pane_dead}").await == "1" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("child exits and remain-on-exit keeps the pane");
    assert_eq!(
        pane_title_state(&handler, target.clone(), "#{pane_title}").await,
        title
    );

    let response = handler
        .handle(Request::RespawnWindow(Box::new(RespawnWindowRequest {
            target: WindowTarget::with_window(name, 0),
            kill: true,
            start_directory: None,
            environment: None,
            command: Some(quiet_window_test_command()),
        })))
        .await;
    assert!(
        matches!(response, Response::RespawnWindow(_)),
        "{response:?}"
    );
    assert_eq!(
        pane_title_state(&handler, target, "#{pane_title}").await,
        title
    );
}

async fn pane_title_state(handler: &RequestHandler, target: PaneTarget, format: &str) -> String {
    let response = handler
        .handle(Request::DisplayMessage(DisplayMessageRequest {
            target: Some(Target::Pane(target)),
            print: true,
            message: Some(format.to_owned()),
            empty_target_context: false,
        }))
        .await;
    let Response::DisplayMessage(response) = response else {
        panic!("expected display-message response, got {response:?}");
    };
    String::from_utf8(
        response
            .command_output()
            .expect("printed output")
            .stdout()
            .to_vec(),
    )
    .expect("UTF-8 output")
    .trim_end_matches('\n')
    .to_owned()
}
