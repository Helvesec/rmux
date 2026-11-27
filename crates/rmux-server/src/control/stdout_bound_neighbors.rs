use super::*;
use rmux_proto::SetBufferRequest;

const BUFFER_BYTES: usize = 2_097_153;
const BOUND_ERROR: &str = "control command stdout exceeds 4194304 bytes";

async fn handler_with_buffer() -> Arc<RequestHandler> {
    let handler = Arc::new(RequestHandler::new());
    let mut content = vec![b'Z'; BUFFER_BYTES];
    content[BUFFER_BYTES - 1] = b'\n';
    let response = handler
        .handle(Request::SetBuffer(Box::new(SetBufferRequest {
            name: Some("b".to_owned()),
            content,
            append: false,
            new_name: None,
            set_clipboard: false,
            target_client: None,
        })))
        .await;
    assert!(matches!(response, Response::SetBuffer(_)), "{response:?}");
    handler
}

#[tokio::test]
async fn control_stdout_budget_preserves_initial_source_and_if_shell_guards() {
    let source = control_message_test_config(
        "stdout-bound-source",
        "show-buffer -b b ; show-buffer -b b\n",
    );
    for (route, command, initial_count, frame_count) in [
        (
            "initial",
            "show-buffer -b b ; show-buffer -b b".to_owned(),
            1,
            1,
        ),
        ("source", format!("source-file {}", source.display()), 0, 4),
        (
            "if-shell",
            "if-shell -F 1 { show-buffer -b b ; show-buffer -b b }".to_owned(),
            0,
            4,
        ),
    ] {
        let rendered = run_registered_initial_control_batch(
            handler_with_buffer().await,
            48_112,
            format!("{command}\n\n").into_bytes(),
            initial_count,
        )
        .await;
        assert_eq!(
            rendered.bytes().filter(|byte| *byte == b'Z').count(),
            BUFFER_BYTES - 1,
            "{route}"
        );
        let transcript = parse_strict_control_transcript(&rendered);
        assert_eq!(transcript.frames.len(), frame_count, "{route}");
        let rejected = transcript.frames.last().expect("rejection frame");
        assert_eq!(rejected.terminal, TestGuardTerminal::Error, "{route}");
        assert_eq!(
            rejected.payload.last().map(String::as_str),
            Some(BOUND_ERROR),
            "{route}"
        );
        assert!(
            transcript.frames[..frame_count - 1]
                .iter()
                .all(|frame| frame.terminal == TestGuardTerminal::End),
            "{route}"
        );
        assert_eq!(transcript.asynchronous_notifications, ["%exit"], "{route}");
    }
    std::fs::remove_file(source).expect("remove private source fixture");
}

#[tokio::test]
async fn control_stdout_budget_rejects_insert_after_output_before_children() {
    // source-file -v returns its verbose listing in InsertAfter before the child.
    // Rejecting that listing must neither publish it nor execute the child.
    let source = control_message_test_config(
        "stdout-bound-verbose",
        &format!("set-buffer -b must-not-run {}\n", "Y".repeat(16 * 1024)).repeat(128),
    );
    let handler = handler_with_buffer().await;
    let rendered = run_registered_initial_control_batch(
        Arc::clone(&handler),
        48_113,
        format!("show-buffer -b b ; source-file -v {}\n\n", source.display()).into_bytes(),
        0,
    )
    .await;
    assert_eq!(
        rendered.bytes().filter(|byte| *byte == b'Z').count(),
        BUFFER_BYTES - 1
    );
    assert!(
        !rendered.contains('Y'),
        "the refused InsertAfter listing escaped"
    );
    let transcript = parse_strict_control_transcript(&rendered);
    assert_eq!(transcript.frames.len(), 3);
    assert_eq!(transcript.frames[1].terminal, TestGuardTerminal::End);
    assert_eq!(transcript.frames[2].terminal, TestGuardTerminal::Error);
    assert_eq!(transcript.frames[2].payload, [BOUND_ERROR]);
    let response = handler
        .handle(Request::ShowBuffer(ShowBufferRequest {
            name: Some("must-not-run".to_owned()),
        }))
        .await;
    assert!(
        matches!(response, Response::Error(_)),
        "a refused insertion executed its child"
    );
    std::fs::remove_file(source).expect("remove private verbose fixture");
}
