use super::*;

async fn capture(handler: &RequestHandler, request: CapturePaneRequest, raw: bool) -> Response {
    let request = if raw {
        Request::CapturePaneTargetAction(Box::new(CapturePaneTargetActionRequest {
            target: Some(format!("{}:0.0", request.target.session_name())),
            start: request.start,
            end: request.end,
            print: request.print,
            buffer_name: request.buffer_name,
            alternate: request.alternate,
            escape_ansi: request.escape_ansi,
            escape_sequences: request.escape_sequences,
            include_format: request.include_format,
            hyperlinks: request.hyperlinks,
            line_numbers: request.line_numbers,
            join_wrapped: request.join_wrapped,
            use_mode_screen: request.use_mode_screen,
            preserve_trailing_spaces: request.preserve_trailing_spaces,
            do_not_trim_spaces: request.do_not_trim_spaces,
            pending_input: request.pending_input,
            quiet: request.quiet,
            start_is_absolute: request.start_is_absolute,
            end_is_absolute: request.end_is_absolute,
        }))
    } else {
        Request::CapturePane(Box::new(request))
    };
    handler.handle(request).await
}

#[tokio::test]
async fn capture_format_resolved_ranges_cover_direct_raw_main_saved_and_copy() {
    for raw in [false, true] {
        let handler = RequestHandler::new();
        let size = TerminalSize { cols: 20, rows: 2 };
        create_session_with_size(&handler, "ranges", size).await;
        let target = PaneTarget::with_window(session_name("ranges"), 0, 0);
        for surface in ["main", "saved", "copy"] {
            let bytes = if surface == "saved" {
                b"one\r\ntwo\x1b[?1049hALT".as_slice()
            } else {
                b"one\r\ntwo".as_slice()
            };
            replace_transcript_contents(&handler, &target, size, bytes).await;
            if surface == "copy" {
                let response = handler
                    .handle(Request::CopyMode(CopyModeRequest {
                        target: Some(target.clone()),
                        page_down: false,
                        exit_on_scroll: false,
                        hide_position: true,
                        mouse_drag_start: false,
                        cancel_mode: false,
                        scrollbar_scroll: false,
                        source: None,
                        page_up: false,
                    }))
                    .await;
                assert!(matches!(response, Response::CopyMode(_)));
            }
            for (start, end, absolute, expected) in [
                (Some(i64::MAX), Some(0), false, b"0 one\n1 two\n".as_slice()),
                (Some(0), Some(1), false, b"0 one\n1 two\n".as_slice()),
                (Some(1), Some(0), false, b"0 one\n1 two\n".as_slice()),
                (
                    Some(i64::MIN),
                    Some(i64::MAX),
                    false,
                    b"0 one\n1 two\n".as_slice(),
                ),
                (
                    Some(i64::MAX),
                    Some(i64::MIN),
                    true,
                    b"0 one\n1 two\n".as_slice(),
                ),
                (Some(99), Some(99), false, b"1 two\n".as_slice()),
            ] {
                let mut request = capture_pane_request(target.clone(), start, end, true, None);
                request.line_numbers = true;
                request.alternate = surface == "saved";
                request.use_mode_screen = surface == "copy";
                request.start_is_absolute = absolute;
                request.end_is_absolute = absolute;
                assert_eq!(
                    capture_stdout(capture(&handler, request, raw).await),
                    expected,
                    "raw={raw}, surface={surface}, start={start:?}, end={end:?}"
                );
            }
        }
    }
}

#[tokio::test]
async fn capture_format_empty_quiet_and_pending_preserve_buffers_and_events() {
    for raw in [false, true] {
        let handler = RequestHandler::new();
        let size = TerminalSize { cols: 20, rows: 2 };
        // The emptied screen must stay empty: a shell prompt that lands after
        // the replacement fills the row the blank capture reads.
        handler.pin_quiet_default_command_for_test().await;
        create_session_with_size(&handler, "empty", size).await;
        let target = PaneTarget::with_window(session_name("empty"), 0, 0);
        handler.wait_for_pane_startup_output_for_test(&target).await;
        replace_transcript_contents(&handler, &target, size, b"").await;
        assert!(matches!(
            handler
                .handle(Request::SetBuffer(Box::new(set_buffer_request(
                    "keep", b"payload"
                ))))
                .await,
            Response::SetBuffer(_)
        ));
        let mut events = handler.subscribe_lifecycle_events();
        for pending in [false, true] {
            for (numbers, format) in [(true, false), (false, true), (true, true)] {
                for name in [Some("keep"), None] {
                    let mut request = capture_pane_request(target.clone(), None, None, false, name);
                    request.alternate = !pending;
                    request.pending_input = pending;
                    request.quiet = true;
                    request.line_numbers = numbers;
                    request.include_format = format;
                    assert!(matches!(
                        capture(&handler, request, raw).await,
                        Response::CapturePane(_)
                    ));
                    let state = handler.state.lock().await;
                    assert_eq!(state.buffers.get("keep"), Some(b"payload".as_slice()));
                    assert_eq!(state.buffers.len(), 1, "zero rows must not create a buffer");
                    drop(state);
                    assert!(
                        events.try_recv().is_err(),
                        "zero rows must not emit a mutation"
                    );
                }
            }
        }
        let mut missing = capture_pane_request(target.clone(), None, None, true, None);
        missing.alternate = true;
        missing.line_numbers = true;
        assert!(matches!(
            capture(&handler, missing, raw).await,
            Response::Error(_)
        ));
        let mut blank = capture_pane_request(target, Some(0), Some(0), true, None);
        blank.line_numbers = true;
        blank.include_format = true;
        assert_eq!(
            capture_stdout(capture(&handler, blank, raw).await),
            b"0 - \n"
        );
    }
}
