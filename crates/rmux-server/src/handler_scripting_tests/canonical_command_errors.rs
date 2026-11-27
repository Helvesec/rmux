use super::*;

fn canonical_commands(input: &str) -> Request {
    Request::SourceFile(Box::new(SourceFileRequest {
        paths: vec![INTERNAL_CANONICAL_COMMAND_EXECUTION_PATH.to_owned()],
        quiet: false,
        parse_only: false,
        verbose: false,
        expand_paths: false,
        target: None,
        caller_cwd: None,
        stdin: Some(input.to_owned()),
    }))
}

fn source_streams(response: Response) -> (Vec<u8>, Vec<u8>, Option<i32>) {
    let Response::SourceFile(source) = response else {
        panic!("canonical execution should answer as source-file: {response:?}");
    };
    let stdout = source
        .command_output()
        .map(|output| output.stdout().to_vec())
        .unwrap_or_default();
    (stdout, source.stderr().to_vec(), source.exit_status())
}

#[tokio::test]
async fn canonical_command_validation_error_reaches_stderr_not_stdout() {
    let handler = RequestHandler::new();
    let alpha = session_name("canonical-stderr");
    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha,
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
    );

    // tmux 3.7b: `display-message -I -t <busy pane>` prints only to stderr, rc 1.
    let (stdout, stderr, exit_status) = source_streams(
        handler
            .handle(canonical_commands(
                "display-message -I -t canonical-stderr:0.0 hello",
            ))
            .await,
    );

    assert_eq!(String::from_utf8_lossy(&stdout), "");
    assert_eq!(String::from_utf8_lossy(&stderr), "pane is not empty\n");
    assert_eq!(exit_status, Some(1));
}

#[tokio::test]
async fn canonical_nested_source_file_diagnostic_keeps_located_stdout() {
    let handler = RequestHandler::new();
    let root = temp_root("canonical-nested-diagnostic");
    let config = root.join("nested.conf");
    write_config(&config, "new-window -Q\n");

    let (stdout, stderr, exit_status) = source_streams(
        handler
            .handle(canonical_commands(&format!(
                "source-file '{}'",
                config.display()
            )))
            .await,
    );

    let stdout = String::from_utf8_lossy(&stdout);
    assert!(
        stdout.ends_with(":1: command new-window: unknown flag -Q\n"),
        "{stdout:?}"
    );
    assert_eq!(String::from_utf8_lossy(&stderr), "");
    assert_eq!(exit_status, Some(1));
}
