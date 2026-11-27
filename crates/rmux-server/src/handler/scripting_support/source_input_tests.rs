use super::*;
use rmux_core::command_parser::CommandParser;

#[tokio::test]
async fn source_file_new_pane_input_effect() {
    let handler = RequestHandler::new();
    let pid = std::process::id();
    let root = crate::test_shell::ScratchPath::directory("source-input-effect");
    std::fs::write(root.join("input.rmux"), "new-pane -dI -P -F '#{pane_id}'\n").unwrap();
    let parse = |text: &str| CommandParser::new().parse(text).unwrap();
    handler
        .execute_parsed_commands(
            pid,
            parse("new-session -d -s source-input -x80 -y24"),
            QueueExecutionContext::without_caller_cwd(),
        )
        .await
        .unwrap();
    let context = QueueExecutionContext::new(Some(root.to_path_buf()));
    let source = parse("source-file -t source-input:0.0 input.rmux");
    let execution = handler
        .execute_queued_command_scoped(
            pid,
            source.commands()[0].clone(),
            &context,
            QueueMode::Detached,
            None,
            Some(b"source-input-effect".to_vec()),
        )
        .await
        .expect("source-file must admit a distinct finished binary input channel");
    let QueueCommandAction::InsertAfter { batches, .. } = execution.action else {
        panic!("source-file must enqueue its commands");
    };
    let mut stdout = Vec::new();
    for (commands, context) in batches {
        stdout.extend_from_slice(
            handler
                .execute_parsed_commands(pid, commands, context)
                .await
                .expect("new-pane -I must receive the invocation input")
                .stdout(),
        );
    }
    let pane = String::from_utf8(stdout).unwrap();
    let output = handler
        .execute_parsed_commands(
            pid,
            parse(&format!("capture-pane -p -t {}", pane.trim())),
            QueueExecutionContext::without_caller_cwd(),
        )
        .await
        .unwrap();
    assert_eq!(output.stdout(), b"source-input-effect\n\n\n\n\n\n");
    let state = handler.state.lock().await;
    let session = state
        .sessions
        .session(&rmux_proto::SessionName::new("source-input").unwrap())
        .unwrap();
    assert_eq!(session.window_at(0).unwrap().pane_count(), 2);
    assert_eq!(
        session
            .window_at(0)
            .unwrap()
            .floating_stack()
            .placements()
            .len(),
        1
    );
    drop(state);
    handler
        .handle(Request::KillServer(rmux_proto::KillServerRequest))
        .await;
    std::fs::remove_dir_all(root).unwrap();
}
