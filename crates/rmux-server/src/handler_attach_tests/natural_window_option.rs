use super::*;
use rmux_core::command_parser::CommandParser;

#[tokio::test]
async fn config_parse_natural_window_option_binding_keeps_current_window() {
    let handler = RequestHandler::new();
    let pid = u32::MAX - 6251;
    let alpha = session_name("alpha");
    let _control = create_attached_session(&handler, pid, &alpha).await;
    let response = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: alpha.clone(),
            name: None,
            command: None,
            process_command: None,
            detached: false,
            start_directory: None,
            environment: None,
            target_window_index: None,
            insert_at_target: false,
        })))
        .await;
    assert!(matches!(response, Response::NewWindow(_)), "{response:?}");
    handler.execute_parsed_commands_for_test(pid, CommandParser::new().parse(
        "set-option -w -t alpha:0 window-status-format ZERO; set-option -w -t alpha:1 window-status-format ONE; bind-key Y { set-option -t alpha window-status-format MARKER; set-option -g @natural-window-done yes }"
    ).expect("binding fixture parses")).await.expect("binding fixture executes");
    handler
        .handle_attached_live_input_for_test(pid, b"\x02Y")
        .await
        .expect("binding dispatches");
    let completed = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let output = handler
                .execute_parsed_commands_for_test(
                    pid,
                    CommandParser::new()
                        .parse("show-options -gqv @natural-window-done")
                        .unwrap(),
                )
                .await
                .expect("completion query");
            if output.stdout() == b"yes\n" {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    let result = handler.execute_parsed_commands_for_test(pid, CommandParser::new().parse(
        "show-options -wv -t alpha:0 window-status-format; show-options -wv -t alpha:1 window-status-format"
    ).unwrap()).await;
    handler
        .handle(Request::KillSession(KillSessionRequest {
            target: alpha,
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        }))
        .await;
    completed.expect("binding completed");
    assert_eq!(
        result.expect("read window options").stdout(),
        b"ZERO\nMARKER\n"
    );
}
