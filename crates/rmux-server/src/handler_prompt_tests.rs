use super::RequestHandler;
use crate::pane_io::AttachControl;
use rmux_core::command_parser::CommandParser;
use rmux_proto::{NewSessionRequest, Request, Response, RmuxError, SessionName, TerminalSize};
use tokio::sync::mpsc;
use tokio::time::{timeout, Duration};

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

async fn create_attached_session(
    handler: &RequestHandler,
    requester_pid: u32,
    name: &str,
) -> mpsc::UnboundedReceiver<AttachControl> {
    let session_name = session_name(name);
    assert!(matches!(
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session_name.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
        Response::NewSession(_)
    ));

    let (control_tx, control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, session_name, control_tx)
        .await;
    control_rx
}

async fn recv_switch_frame(control_rx: &mut mpsc::UnboundedReceiver<AttachControl>) -> String {
    loop {
        let control = timeout(Duration::from_secs(1), control_rx.recv())
            .await
            .expect("prompt refresh timeout")
            .expect("attached refresh");
        if let AttachControl::Switch(target) = control {
            return String::from_utf8(target.render_frame).expect("render frame is utf-8");
        }
    }
}

async fn recv_switch_frame_containing(
    control_rx: &mut mpsc::UnboundedReceiver<AttachControl>,
    needle: &str,
) -> String {
    loop {
        let frame = recv_switch_frame(control_rx).await;
        if frame.contains(needle) {
            return frame;
        }
    }
}

fn parse_command(command: &str) -> rmux_core::command_parser::ParsedCommands {
    CommandParser::new()
        .parse_one_group(command)
        .expect("command parses")
}

#[tokio::test]
async fn active_command_prompt_accepts_one_megabyte_frame_with_bounded_work() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let mut control_rx = create_attached_session(&handler, requester_pid, "prompt-bulk").await;
    let parsed = parse_command("command-prompt -b -pbulk");
    handler
        .execute_parsed_commands_for_test(requester_pid, parsed)
        .await
        .expect("background command prompt starts");
    let frame = recv_switch_frame_containing(&mut control_rx, "bulk ").await;
    assert!(frame.contains("bulk "), "{frame}");

    let input = vec![b'a'; 1_000_000];
    timeout(
        if cfg!(windows) {
            Duration::from_secs(30)
        } else {
            Duration::from_secs(2)
        },
        handler.handle_attached_live_input_for_test(requester_pid, &input),
    )
    .await
    .expect("one-megabyte prompt frame must have bounded processing work")
    .expect("one-megabyte prompt input succeeds");

    let prompt = handler
        .attached_prompt_render(requester_pid)
        .await
        .expect("command prompt remains active");
    assert_eq!(prompt.input.len(), input.len());
    assert!(prompt.input.bytes().all(|byte| byte == b'a'));
    handler.clear_prompt_for_attach(requester_pid).await;
}

#[tokio::test]
async fn batched_prompt_text_preserves_split_utf8_at_the_input_boundary() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let mut control_rx =
        create_attached_session(&handler, requester_pid, "prompt-split-utf8").await;
    let parsed = parse_command("command-prompt -b -pbulk");
    handler
        .execute_parsed_commands_for_test(requester_pid, parsed)
        .await
        .expect("background command prompt starts");
    let _ = recv_switch_frame_containing(&mut control_rx, "bulk ").await;

    let mut pending_input = Vec::new();
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"a\xe6\x97")
        .await
        .expect("partial prompt UTF-8 succeeds");
    assert_eq!(pending_input, b"\xe6\x97");

    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"\xa5b")
        .await
        .expect("completed prompt UTF-8 succeeds");
    assert!(pending_input.is_empty());
    let prompt = handler
        .attached_prompt_render(requester_pid)
        .await
        .expect("command prompt remains active");
    assert_eq!(prompt.input, "a日b");
    assert_eq!(prompt.cursor, 3);
    handler.clear_prompt_for_attach(requester_pid).await;
}

#[tokio::test]
async fn command_prompt_renders_prompt_and_executes_substituted_command() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let mut control_rx = create_attached_session(&handler, requester_pid, "alpha").await;
    let parsed = parse_command("command-prompt -pname { display-message -p -- 'value=%%' }");
    let handler_task = handler.clone();
    let join = tokio::spawn(async move {
        handler_task
            .execute_parsed_commands_for_test(requester_pid, parsed)
            .await
    });

    let frame = recv_switch_frame_containing(&mut control_rx, "name ").await;
    assert!(frame.contains("name "), "{frame}");

    handler
        .handle_attached_live_input_for_test(requester_pid, b"delta\r")
        .await
        .expect("prompt input");

    let output = join
        .await
        .expect("prompt task join")
        .expect("prompt command output");
    assert_eq!(output.stdout(), b"value=delta\n");
}

#[tokio::test]
async fn command_prompt_default_label_uses_first_template_command_name() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let mut control_rx = create_attached_session(&handler, requester_pid, "alpha").await;
    let parsed = parse_command("command-prompt -I'#W' { rename-window -- '%%' }");
    let handler_task = handler.clone();
    let join = tokio::spawn(async move {
        handler_task
            .execute_parsed_commands_for_test(requester_pid, parsed)
            .await
    });

    let frame = recv_switch_frame_containing(&mut control_rx, "(rename-window) ").await;
    assert!(frame.contains("(rename-window) "), "{frame}");
    assert!(!frame.contains("(rename-window --"), "{frame}");

    handler
        .handle_attached_live_input_for_test(requester_pid, b"delta\r")
        .await
        .expect("prompt input");

    let output = join
        .await
        .expect("prompt task join")
        .expect("rename prompt output");
    assert!(output.stdout().is_empty());
}

#[tokio::test]
async fn command_prompt_multi_prompt_substitutes_percent_indices() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let mut control_rx = create_attached_session(&handler, requester_pid, "alpha").await;
    let parsed = parse_command(
        "command-prompt -pfirst,second { display-message -p -- 'first=%1 second=%2 default=%%' }",
    );
    let handler_task = handler.clone();
    let join = tokio::spawn(async move {
        handler_task
            .execute_parsed_commands_for_test(requester_pid, parsed)
            .await
    });

    let first_frame = recv_switch_frame_containing(&mut control_rx, "first ").await;
    assert!(first_frame.contains("first "), "{first_frame}");
    handler
        .handle_attached_live_input_for_test(requester_pid, b"alpha\r")
        .await
        .expect("first prompt input");

    let second_frame = recv_switch_frame_containing(&mut control_rx, "second ").await;
    assert!(second_frame.contains("second "), "{second_frame}");
    handler
        .handle_attached_live_input_for_test(requester_pid, b"beta\r")
        .await
        .expect("second prompt input");

    let output = join
        .await
        .expect("prompt task join")
        .expect("prompt command output");
    assert_eq!(output.stdout(), b"first=alpha second=beta default=alpha\n");
}

#[tokio::test]
async fn confirm_before_accepts_enter_with_default_yes_and_skips_on_decline() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let mut control_rx = create_attached_session(&handler, requester_pid, "alpha").await;
    let parsed = parse_command("confirm-before -y -psure { display-message -p -- 'confirmed' }");
    let handler_task = handler.clone();
    let accept_join = tokio::spawn(async move {
        handler_task
            .execute_parsed_commands_for_test(requester_pid, parsed)
            .await
    });

    let frame = recv_switch_frame_containing(&mut control_rx, "sure ").await;
    assert!(frame.contains("sure "), "{frame}");
    handler
        .handle_attached_live_input_for_test(requester_pid, b"\r")
        .await
        .expect("confirm input");

    let accepted = accept_join
        .await
        .expect("confirm task join")
        .expect("confirm command output");
    assert_eq!(accepted.stdout(), b"confirmed\n");

    let parsed = parse_command("confirm-before -pstop { display-message -p -- 'blocked' }");
    let handler_task = handler.clone();
    let decline_join = tokio::spawn(async move {
        handler_task
            .execute_parsed_commands_for_test(requester_pid, parsed)
            .await
    });

    let frame = recv_switch_frame_containing(&mut control_rx, "stop ").await;
    assert!(frame.contains("stop "), "{frame}");
    handler
        .handle_attached_live_input_for_test(requester_pid, b"n")
        .await
        .expect("decline input");

    let declined = decline_join
        .await
        .expect("decline task join")
        .expect("decline prompt output");
    assert!(declined.stdout().is_empty());
}

#[tokio::test]
async fn prompt_commands_return_tmux_style_errors_for_unknown_target_clients() {
    let handler = RequestHandler::new();
    assert!(matches!(
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session_name("alpha"),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
        Response::NewSession(_)
    ));

    let prompt_error = handler
        .execute_parsed_commands_for_test(
            std::process::id(),
            parse_command("command-prompt -t 99999 -pname { display-message -p -- 'value=%%' }"),
        )
        .await
        .expect_err("unknown target client should fail");
    assert_eq!(
        prompt_error,
        RmuxError::Message("can't find client: 99999".to_owned())
    );

    let confirm_error = handler
        .execute_parsed_commands_for_test(
            std::process::id(),
            parse_command("confirm-before -t 99999 -psure { display-message -p -- 'confirmed' }"),
        )
        .await
        .expect_err("unknown target client should fail");
    assert_eq!(
        confirm_error,
        RmuxError::Message("can't find client: 99999".to_owned())
    );
}

#[tokio::test]
async fn second_prompt_request_returns_immediately_while_prompt_is_active() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let mut control_rx = create_attached_session(&handler, requester_pid, "alpha").await;
    let first = parse_command("command-prompt -pfirst { display-message -p -- 'first=%%' }");
    let handler_task = handler.clone();
    let first_join = tokio::spawn(async move {
        handler_task
            .execute_parsed_commands_for_test(requester_pid, first)
            .await
    });

    let frame = recv_switch_frame_containing(&mut control_rx, "first ").await;
    assert!(frame.contains("first "), "{frame}");

    let second = parse_command("command-prompt -psecond { display-message -p -- 'second=%%' }");
    let second_output = timeout(
        Duration::from_millis(250),
        handler.execute_parsed_commands_for_test(requester_pid, second),
    )
    .await
    .expect("second prompt returns immediately")
    .expect("second prompt output");
    assert!(second_output.stdout().is_empty());

    handler
        .handle_attached_live_input_for_test(requester_pid, b"done\r")
        .await
        .expect("first prompt input");
    let first_output = first_join
        .await
        .expect("first prompt task join")
        .expect("first prompt output");
    assert_eq!(first_output.stdout(), b"first=done\n");
}

#[tokio::test]
async fn show_prompt_history_renders_tmux_sections_in_prompt_type_order() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let mut control_rx = create_attached_session(&handler, requester_pid, "alpha").await;

    let parsed = parse_command("command-prompt -pcommand { display-message -p -- 'command=%%' }");
    let handler_task = handler.clone();
    let command_join = tokio::spawn(async move {
        handler_task
            .execute_parsed_commands_for_test(requester_pid, parsed)
            .await
    });

    let frame = recv_switch_frame_containing(&mut control_rx, "command ").await;
    assert!(frame.contains("command "), "{frame}");
    handler
        .handle_attached_live_input_for_test(requester_pid, b"first\r")
        .await
        .expect("command prompt input");
    let command_output = command_join
        .await
        .expect("command prompt task join")
        .expect("command prompt output");
    assert_eq!(command_output.stdout(), b"command=first\n");

    let parsed =
        parse_command("command-prompt -T search -psearch { display-message -p -- 'search=%%' }");
    let handler_task = handler.clone();
    let search_join = tokio::spawn(async move {
        handler_task
            .execute_parsed_commands_for_test(requester_pid, parsed)
            .await
    });

    let frame = recv_switch_frame_containing(&mut control_rx, "search ").await;
    assert!(frame.contains("search "), "{frame}");
    handler
        .handle_attached_live_input_for_test(requester_pid, b"needle\r")
        .await
        .expect("search prompt input");
    let search_output = search_join
        .await
        .expect("search prompt task join")
        .expect("search prompt output");
    assert_eq!(search_output.stdout(), b"search=needle\n");

    let history = handler
        .execute_parsed_commands_for_test(requester_pid, parse_command("show-prompt-history"))
        .await
        .expect("show-prompt-history succeeds");
    assert_eq!(
        history.stdout(),
        b"History for command:\n\n1: first\n\nHistory for search:\n\n1: needle\n\nHistory for target:\n\n\nHistory for window-target:\n\n\n"
    );

    let search_history = handler
        .execute_parsed_commands_for_test(
            requester_pid,
            parse_command("show-prompt-history -T search"),
        )
        .await
        .expect("show-prompt-history -T search succeeds");
    assert_eq!(
        search_history.stdout(),
        b"History for search:\n\n1: needle\n\n"
    );
}

#[tokio::test]
async fn clear_prompt_history_clears_selected_type_without_touching_others() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let mut control_rx = create_attached_session(&handler, requester_pid, "alpha").await;

    let parsed = parse_command("command-prompt -pcommand { display-message -p -- 'command=%%' }");
    let handler_task = handler.clone();
    let command_join = tokio::spawn(async move {
        handler_task
            .execute_parsed_commands_for_test(requester_pid, parsed)
            .await
    });

    let frame = recv_switch_frame_containing(&mut control_rx, "command ").await;
    assert!(frame.contains("command "), "{frame}");
    handler
        .handle_attached_live_input_for_test(requester_pid, b"keep\r")
        .await
        .expect("command prompt input");
    let command_output = command_join
        .await
        .expect("command prompt task join")
        .expect("command prompt output");
    assert_eq!(command_output.stdout(), b"command=keep\n");

    let parsed =
        parse_command("command-prompt -T search -psearch { display-message -p -- 'search=%%' }");
    let handler_task = handler.clone();
    let search_join = tokio::spawn(async move {
        handler_task
            .execute_parsed_commands_for_test(requester_pid, parsed)
            .await
    });

    let frame = recv_switch_frame_containing(&mut control_rx, "search ").await;
    assert!(frame.contains("search "), "{frame}");
    handler
        .handle_attached_live_input_for_test(requester_pid, b"drop\r")
        .await
        .expect("search prompt input");
    let search_output = search_join
        .await
        .expect("search prompt task join")
        .expect("search prompt output");
    assert_eq!(search_output.stdout(), b"search=drop\n");

    let cleared = handler
        .execute_parsed_commands_for_test(
            requester_pid,
            parse_command("clear-prompt-history -T search"),
        )
        .await
        .expect("clear-prompt-history -T search succeeds");
    assert!(cleared.stdout().is_empty());

    let remaining = handler
        .execute_parsed_commands_for_test(requester_pid, parse_command("show-prompt-history"))
        .await
        .expect("show-prompt-history succeeds");
    assert_eq!(
        remaining.stdout(),
        b"History for command:\n\n1: keep\n\nHistory for search:\n\n\nHistory for target:\n\n\nHistory for window-target:\n\n\n"
    );

    let cleared_all = handler
        .execute_parsed_commands_for_test(requester_pid, parse_command("clear-prompt-history"))
        .await
        .expect("clear-prompt-history succeeds");
    assert!(cleared_all.stdout().is_empty());

    let empty = handler
        .execute_parsed_commands_for_test(requester_pid, parse_command("show-prompt-history"))
        .await
        .expect("show-prompt-history after clear succeeds");
    assert_eq!(
        empty.stdout(),
        b"History for command:\n\n\nHistory for search:\n\n\nHistory for target:\n\n\nHistory for window-target:\n\n\n"
    );
}
