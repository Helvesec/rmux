use super::capture_format_neighbors::capture;
use super::*;

#[tokio::test]
async fn capture_format_empty_stdout_and_real_blank_named_buffer_are_distinct() {
    for raw in [false, true] {
        let handler = RequestHandler::new();
        let size = TerminalSize { cols: 8, rows: 2 };
        create_session_with_size(&handler, "empty-print", size).await;
        let target = PaneTarget::with_window(session_name("empty-print"), 0, 0);
        replace_transcript_contents(&handler, &target, size, b"").await;
        for pending in [false, true] {
            for (numbers, format) in [(true, false), (false, true), (true, true)] {
                let mut request = capture_pane_request(target.clone(), None, None, true, None);
                request.alternate = !pending;
                request.pending_input = pending;
                request.quiet = true;
                request.line_numbers = numbers;
                request.include_format = format;
                assert_eq!(capture_stdout(capture(&handler, request, raw).await), b"\n");
            }
        }
        let mut blank = capture_pane_request(target, Some(0), Some(0), false, Some("blank"));
        blank.line_numbers = true;
        blank.include_format = true;
        assert!(matches!(
            capture(&handler, blank, raw).await,
            Response::CapturePane(_)
        ));
        assert_eq!(
            handler.state.lock().await.buffers.get("blank"),
            Some(b"0 - ".as_slice())
        );
    }
}
