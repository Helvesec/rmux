use super::*;
use rmux_proto::SetBufferRequest;

#[tokio::test]
async fn stdin_stdout_budget_rejects_before_publishing_and_keeps_error_guard() {
    const BUFFER_BYTES: usize = 2_097_153;
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

    let (server, mut client) = UnixStream::pair().expect("private stdin transport");
    let (_shutdown_tx, shutdown_rx) = watch::channel(());
    let (_events_tx, events_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let (shutdown_handle, _shutdown_requests) = ShutdownHandle::new();
    let task = tokio::spawn(forward_control(
        server,
        Arc::clone(&handler),
        48_111,
        ControlUpgradeInput::new(Vec::new(), 0),
        shutdown_rx,
        events_rx,
        ControlLifecycle {
            closing: Arc::new(AtomicBool::new(false)),
            shutdown_handle,
        },
    ));
    client
        .write_all(b"show-buffer -b b ; show-buffer -b b\n")
        .await
        .expect("live stdin command writes");
    client.shutdown().await.expect("stdin EOF");
    let mut output = Vec::new();
    output_drain::read_to_eof(&mut client, &mut output).await;
    task.await
        .expect("transport joins")
        .expect("transport succeeds");

    assert_eq!(
        output.iter().filter(|byte| **byte == b'Z').count(),
        BUFFER_BYTES - 1,
        "only the accepted first buffer may reach the actual sink"
    );
    let rendered = String::from_utf8(output).expect("UTF-8 response");
    let transcript = parse_strict_control_transcript(&rendered);
    assert_eq!(transcript.frames.len(), 3);
    assert_eq!(transcript.frames[0].terminal, TestGuardTerminal::End);
    assert!(transcript.frames[0].payload.is_empty());
    assert_eq!(transcript.frames[1].terminal, TestGuardTerminal::End);
    assert_eq!(transcript.frames[1].payload.len(), 1);
    assert_eq!(transcript.frames[1].payload[0].len(), BUFFER_BYTES - 1);
    assert_eq!(transcript.frames[2].terminal, TestGuardTerminal::Error);
    assert_eq!(transcript.frames[2].payload.len(), 1);
    assert_eq!(
        transcript.frames[2].payload[0],
        "control command stdout exceeds 4194304 bytes"
    );
    assert_eq!(transcript.asynchronous_notifications, ["%exit"]);
}
