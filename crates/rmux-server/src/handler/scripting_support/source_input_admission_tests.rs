use super::*;
use crate::handler::scripting_support::QueueExecutionContext;
use rmux_core::command_parser::CommandParser;
use rmux_proto::{CommandOutput, HandshakeRequest, Request, SessionName};
use std::path::PathBuf;

#[path = "source_input_diagnostic_tests.rs"]
mod diagnostics;

async fn setup(label: &str) -> (RequestHandler, PathBuf) {
    let handler = RequestHandler::new();
    let root = std::env::current_dir()
        .unwrap()
        .join("target")
        .join("tmp")
        .join(format!("source-input-{label}-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    run(
        &handler,
        "set-option -g status off; new-session -d -s source-input -x80 -y24",
        QueueExecutionContext::without_caller_cwd(),
    )
    .await
    .unwrap();
    (handler, root)
}

async fn run(
    handler: &RequestHandler,
    command: &str,
    context: QueueExecutionContext,
) -> Result<CommandOutput, RmuxError> {
    handler
        .execute_parsed_commands(
            std::process::id(),
            CommandParser::new().parse(command).unwrap(),
            context,
        )
        .await
}

fn offer(root: &std::path::Path, name: &str, payload: Option<Vec<u8>>) -> SourceFileInputV1Request {
    SourceFileInputV1Request::new(
        vec![
            "source-file".into(),
            "-t".into(),
            "source-input:0.0".into(),
            name.into(),
        ],
        Some(root.to_path_buf()),
        payload,
    )
}

async fn send(handler: &RequestHandler, request: SourceFileInputV1Request) -> Response {
    handler
        .handle(Request::SourceFileInputV1(Box::new(request)))
        .await
}

async fn count(handler: &RequestHandler) -> usize {
    handler
        .state
        .lock()
        .await
        .sessions
        .session(&SessionName::new("source-input").unwrap())
        .unwrap()
        .window_at(0)
        .unwrap()
        .pane_count()
}

async fn cleanup(handler: &RequestHandler, root: PathBuf) {
    handler
        .handle(Request::KillServer(rmux_proto::KillServerRequest))
        .await;
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn nested_source_keeps_binary_data_cwd_target_and_an_empty_process() {
    let (handler, root) = setup("nested").await;
    std::fs::write(root.join("outer.rmux"), "source-file child.rmux\n").unwrap();
    std::fs::write(root.join("child.rmux"), "new-pane -dI -P -F '#{pane_id}'\n").unwrap();
    let result = send(
        &handler,
        offer(
            &root,
            "outer.rmux",
            Some(b"source\0\x1b[31m-input-effect\x1b[0m\r\nkill-server\n".to_vec()),
        ),
    )
    .await;
    let Response::SourceFile(result) = result else {
        panic!("{result:?}")
    };
    assert_eq!(result.exit_status().unwrap_or(0), 0, "{result:?}");
    assert!(result.stderr().is_empty(), "{result:?}");
    let pane = String::from_utf8(result.command_output().unwrap().stdout().to_vec()).unwrap();
    let output = run(
        &handler,
        &format!("capture-pane -p -t {}", pane.trim()),
        QueueExecutionContext::without_caller_cwd(),
    )
    .await
    .unwrap();
    assert_eq!(
        output.stdout(),
        b"source-input-effect\nkill-server\n\n\n\n\n"
    );
    let output = run(
        &handler,
        &format!(
            "display-message -p -t {} '#{{pane_pid}}|#{{pane_floating_flag}}'",
            pane.trim()
        ),
        QueueExecutionContext::without_caller_cwd(),
    )
    .await
    .unwrap();
    assert_eq!(output.stdout(), b"0|1\n");
    assert_eq!(count(&handler).await, 2);
    cleanup(&handler, root).await;
}

#[tokio::test]
async fn source_eof_creates_a_float_but_absent_input_and_ordinary_queues_do_not() {
    let (handler, root) = setup("eof").await;
    std::fs::write(root.join("input.rmux"), "new-pane -dI\n").unwrap();
    let result = send(&handler, offer(&root, "input.rmux", None)).await;
    assert!(
        matches!(result, Response::SourceFile(ref r) if r.exit_status() == Some(1)),
        "{result:?}"
    );
    assert_eq!(count(&handler).await, 1);
    assert!(run(
        &handler,
        "new-pane -dI",
        QueueExecutionContext::without_caller_cwd()
    )
    .await
    .is_err());
    let result = send(&handler, offer(&root, "input.rmux", Some(Vec::new()))).await;
    assert!(
        matches!(result, Response::SourceFile(ref r) if r.exit_status().unwrap_or(0) == 0),
        "{result:?}"
    );
    assert_eq!(count(&handler).await, 2);
    cleanup(&handler, root).await;
}

#[tokio::test]
async fn input_is_consumed_after_creation_and_survives_a_failed_creation() {
    let (handler, root) = setup("consume").await;
    let context = QueueExecutionContext::new(Some(root.clone()))
        .with_invocation_input(b"reserved\n".to_vec());
    run(&handler, "new-pane -dE", context.clone())
        .await
        .unwrap();
    assert!(run(&handler, "new-pane -dI -x80 -y6", context.clone())
        .await
        .is_err());
    assert_eq!(count(&handler).await, 2);
    let output = run(&handler, "new-pane -dI -P -F '#{pane_id}'", context.clone())
        .await
        .unwrap();
    let pane = String::from_utf8(output.stdout().to_vec()).unwrap();
    let output = run(
        &handler,
        &format!("capture-pane -p -t {}", pane.trim()),
        QueueExecutionContext::without_caller_cwd(),
    )
    .await
    .unwrap();
    assert_eq!(output.stdout(), b"reserved\n\n\n\n\n\n");
    assert!(run(&handler, "new-pane -dI", context).await.is_err());
    assert_eq!(count(&handler).await, 3);
    cleanup(&handler, root).await;
}

#[tokio::test]
async fn capability_shape_and_source_errors_are_terminal_before_creation() {
    let (handler, root) = setup("refusal").await;
    std::fs::write(root.join("input.rmux"), "new-pane -dI\n").unwrap();
    for required in SOURCE_FILE_INPUT_V1_REQUIRED_CAPABILITIES {
        let mut request = offer(&root, "input.rmux", Some(Vec::new()));
        request
            .handshake
            .required_capabilities
            .retain(|c| c != required);
        assert!(matches!(send(&handler, request).await, Response::Error(_)));
        assert_eq!(count(&handler).await, 1);
    }
    for arguments in [
        vec!["new-pane", "-I"],
        vec!["BAD=yes", "source-file", "input.rmux"],
        vec!["source-file", "input.rmux", ";", "new-pane", "-dI"],
    ] {
        let mut request = offer(&root, "input.rmux", Some(Vec::new()));
        request.arguments = arguments.into_iter().map(str::to_owned).collect();
        assert!(matches!(send(&handler, request).await, Response::Error(_)));
        assert_eq!(count(&handler).await, 1);
    }
    let mut request = offer(&root, "input.rmux", Some(Vec::new()));
    request.handshake = HandshakeRequest::requiring(["commands.source_file.input.v999"]);
    assert!(matches!(send(&handler, request).await, Response::Error(_)));
    let result = send(&handler, offer(&root, "missing.rmux", Some(Vec::new()))).await;
    assert!(
        matches!(result, Response::SourceFile(ref r) if r.exit_status() == Some(1)),
        "{result:?}"
    );
    let mut request = offer(&root, "-", Some(b"new-pane -dE\n".to_vec()));
    request.arguments = vec!["source-file".into(), "-".into()];
    let result = send(&handler, request).await;
    assert!(
        matches!(result, Response::SourceFile(ref r) if r.exit_status() == Some(1)),
        "{result:?}"
    );
    assert_eq!(count(&handler).await, 1);
    cleanup(&handler, root).await;
}

#[tokio::test]
async fn source_input_retains_the_existing_canonical_source_diagnostics() {
    let (handler, root) = setup("diagnostics").await;
    for (name, body) in [
        ("command-tail.rmux", "new-pane -dI printf forbidden\n"),
        ("bad-command.rmux", "not-a-command\n"),
        ("ordinary.rmux", "display-message -p normal-source\n"),
    ] {
        std::fs::write(root.join(name), body).unwrap();
    }
    for name in [
        "command-tail.rmux",
        "bad-command.rmux",
        "ordinary.rmux",
        "missing.rmux",
    ] {
        let request = offer(&root, name, Some(b"source-input-effect".to_vec()));
        let parsed = CommandParser::new()
            .parse_arguments_with_assignments(&request.arguments)
            .unwrap();
        let previous = handler
            .handle(Request::SourceFile(Box::new(SourceFileRequest {
                paths: vec![INTERNAL_CANONICAL_COMMAND_EXECUTION_PATH.to_owned()],
                quiet: false,
                parse_only: false,
                verbose: false,
                expand_paths: false,
                target: None,
                caller_cwd: Some(root.clone()),
                stdin: Some(parsed.to_tmux_reparse_string()),
            })))
            .await;
        let current = send(&handler, request).await;
        assert_eq!(
            bincode::serialize(&current).unwrap(),
            bincode::serialize(&previous).unwrap(),
            "{name}: {current:?} vs {previous:?}"
        );
        assert_eq!(count(&handler).await, 1);
    }
    cleanup(&handler, root).await;
}

#[tokio::test]
async fn negotiated_queue_status_distinguishes_selection_from_internal_errors() {
    let (handler, root) = setup("queue-status").await;
    for (name, body) in [
        ("empty.rmux", ""),
        ("ok.rmux", "display-message -p OK\n"),
        (
            "runtime.rmux",
            "kill-session -t definitely-missing-session\n",
        ),
        (
            "nested.rmux",
            "source-file missing.rmux\ndisplay-message -p AFTER\n",
        ),
        ("unknown.rmux", "not-a-command-ever\n"),
        ("syntax.rmux", "%word\n"),
    ] {
        std::fs::write(root.join(name), body).unwrap();
    }
    std::fs::create_dir(root.join("directory.rmux")).unwrap();
    run(
        &handler,
        "set-option -s command-alias[20] sourcequeue=source-file",
        QueueExecutionContext::without_caller_cwd(),
    )
    .await
    .unwrap();
    for alias in [false, true] {
        for (args, stop, status) in [
            (vec!["missing.rmux"], true, 1),
            (vec!["missing-*.rmux"], true, 1),
            (vec!["-q", "missing.rmux"], false, 0),
            (vec!["empty.rmux"], false, 0),
            (vec!["ok.rmux"], false, 0),
            (vec!["runtime.rmux"], false, 1),
            (vec!["nested.rmux"], false, 1),
            (vec!["unknown.rmux"], false, 1),
            (vec!["syntax.rmux"], false, 1),
            (vec!["missing.rmux", "ok.rmux"], false, 1),
            (vec!["ok.rmux", "missing.rmux"], false, 1),
            (vec!["missing.rmux", "empty.rmux"], false, 1),
            (vec!["directory.rmux"], false, 1),
            (vec!["missing.rmux", "directory.rmux"], false, 1),
        ] {
            let mut request = offer(&root, "unused", Some(vec![0, 255]));
            request.arguments = std::iter::once(if alias { "sourcequeue" } else { "source-file" })
                .chain(args.iter().copied())
                .map(str::to_owned)
                .collect();
            let legacy = send(&handler, request.clone()).await;
            let negotiated = send(&handler, request.requiring_queue_status()).await;
            let Response::SourceFileQueueV1(result) = negotiated else {
                panic!("{args:?}: {negotiated:?}")
            };
            assert_eq!(result.stop_queue, stop, "{args:?}");
            assert_eq!(result.source.exit_status().unwrap_or(0), status, "{args:?}");
            // New metadata never changes the existing stdout/stderr/status payload.
            assert_eq!(
                bincode::serialize(&Response::SourceFile(result.source)).unwrap(),
                bincode::serialize(&legacy).unwrap(),
                "{args:?}"
            );
        }
    }
    cleanup(&handler, root).await;
}

#[tokio::test]
async fn negotiated_queue_status_preserves_nested_binary_input_consumption() {
    let (handler, root) = setup("queue-status-input").await;
    std::fs::write(root.join("outer.rmux"), "source-file child.rmux\n").unwrap();
    std::fs::write(root.join("child.rmux"), "new-pane -dI -P -F '#{pane_id}'\n").unwrap();
    let result = send(
        &handler,
        offer(
            &root,
            "outer.rmux",
            Some(b"source\0\x1b[31m-input-effect\x1b[0m\r\nkill-server\n".to_vec()),
        )
        .requiring_queue_status(),
    )
    .await;
    let Response::SourceFileQueueV1(result) = result else {
        panic!("{result:?}")
    };
    assert!(!result.stop_queue);
    assert_eq!(result.source.exit_status().unwrap_or(0), 0);
    assert!(result.source.stderr().is_empty());
    let pane =
        String::from_utf8(result.source.command_output().unwrap().stdout().to_vec()).unwrap();
    let output = run(
        &handler,
        &format!("capture-pane -p -t {}", pane.trim()),
        QueueExecutionContext::without_caller_cwd(),
    )
    .await
    .unwrap();
    assert_eq!(
        output.stdout(),
        b"source-input-effect\nkill-server\n\n\n\n\n"
    );
    assert_eq!(count(&handler).await, 2);
    cleanup(&handler, root).await;
}

#[tokio::test]
async fn source_input_budget_rejection_keeps_queue_and_legacy_payload() {
    let (handler, root) = setup("queue-budget").await;
    let contents = format!("#{}\n", "a".repeat(1022)).repeat(11 * 1024);
    for name in ["large-a.conf", "large-b.conf", "large-c.conf"] {
        std::fs::write(root.join(name), &contents).unwrap();
    }
    std::fs::write(root.join("nested.conf"), "source-file large-*.conf\n").unwrap();
    run(
        &handler,
        "set-option -s command-alias[20] sourcequeue=source-file",
        QueueExecutionContext::without_caller_cwd(),
    )
    .await
    .unwrap();
    for alias in [false, true] {
        for args in [
            vec!["large-*.conf"],
            vec!["large-a.conf", "large-b.conf", "large-c.conf"],
            vec!["-q", "large-*.conf"],
            vec!["-n", "large-*.conf"],
            vec!["nested.conf"],
        ] {
            let mut request = offer(&root, "unused", Some(vec![0, 255]));
            request.arguments = std::iter::once(if alias { "sourcequeue" } else { "source-file" })
                .chain(args.iter().copied())
                .map(str::to_owned)
                .collect();
            let legacy = send(&handler, request.clone()).await;
            let negotiated = send(&handler, request.requiring_queue_status()).await;
            let Response::SourceFileQueueV1(result) = negotiated else {
                panic!("{args:?}: {negotiated:?}")
            };
            assert!(!result.stop_queue, "{args:?}");
            assert_eq!(result.source.exit_status(), Some(1), "{args:?}");
            let diagnostic = String::from_utf8_lossy(result.source.stderr());
            assert!(
                diagnostic.contains("aggregate bytes"),
                "{args:?}: {diagnostic}"
            );
            assert_eq!(diagnostic.lines().count(), 1, "{args:?}: {diagnostic}");
            assert_eq!(
                bincode::serialize(&Response::SourceFile(result.source)).unwrap(),
                bincode::serialize(&legacy).unwrap(),
                "{args:?}"
            );
        }
    }
    cleanup(&handler, root).await;
}
