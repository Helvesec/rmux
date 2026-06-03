use super::*;

#[tokio::test]
async fn parsed_queue_bind_key_accepts_command_blocks() {
    let handler = RequestHandler::new();
    let bind = CommandParser::new()
        .parse("bind-key x { display-message -p -- from-block }")
        .expect("bind-key block parses");
    handler
        .execute_parsed_commands_for_test(std::process::id(), bind)
        .await
        .expect("bind-key block executes");

    let list = CommandParser::new()
        .parse("list-keys -T prefix x")
        .expect("list-keys parses");
    let output = handler
        .execute_parsed_commands_for_test(std::process::id(), list)
        .await
        .expect("list-keys executes");
    let stdout = std::str::from_utf8(output.stdout()).expect("list-keys utf8");

    assert!(
        stdout.contains("display-message -p -- from-block"),
        "{stdout}"
    );
}

#[tokio::test]
async fn parsed_queue_set_hook_accepts_command_blocks() {
    let handler = RequestHandler::new();
    let parsed = CommandParser::new()
        .parse("set-hook -g after-new-window { display-message -p -- hook-block }")
        .expect("set-hook block parses");
    handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect("set-hook block executes");

    let state = handler.state.lock().await;
    assert_eq!(
        state
            .hooks
            .global_command(rmux_proto::HookName::AfterNewWindow),
        Some("display-message -p -- hook-block")
    );
}

#[tokio::test]
async fn parsed_queue_set_hook_resolves_relative_targets_before_block_parse() {
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

    let parsed = CommandParser::new()
        .parse("set-hook -t . after-new-window { display-message -p -- hook-block }")
        .expect("set-hook block parses");
    handler
        .execute_parsed_commands(
            std::process::id(),
            parsed,
            QueueExecutionContext::without_caller_cwd()
                .with_current_target(Some(Target::Pane(PaneTarget::with_window(alpha, 0, 0)))),
        )
        .await
        .expect("set-hook -t . should execute");
}
