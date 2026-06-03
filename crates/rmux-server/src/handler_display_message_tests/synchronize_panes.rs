use super::*;

#[tokio::test]
async fn display_message_pane_synchronized_reflects_window_option() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");

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
    assert!(matches!(
        handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Window(WindowTarget::with_window(alpha.clone(), 0)),
                option: OptionName::SynchronizePanes,
                value: "on".to_owned(),
                mode: SetOptionMode::Replace,
            }))
            .await,
        Response::SetOption(_)
    ));

    let response = handler
        .handle(Request::DisplayMessage(DisplayMessageRequest {
            target: Some(Target::Pane(PaneTarget::with_window(alpha, 0, 0))),
            print: true,
            message: Some("#{pane_synchronized}".to_owned()),
        }))
        .await;

    let Response::DisplayMessage(response) = response else {
        panic!("expected display-message response");
    };
    let output = response
        .command_output()
        .expect("display-message -p returns output");
    assert_eq!(output.stdout(), b"1\n");
}
