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
            // Replacing the screen also leaves the previous copy mode.
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

/// A capture flag set, how to apply it, and what tmux 3.7b prints for it.
type CaptureCase = (&'static str, fn(&mut CapturePaneRequest), &'static [u8]);

#[tokio::test]
async fn copy_mode_capture_reads_backing_on_every_capture_path_like_tmux_3_7b() {
    // tmux 3.7b reads `capture-pane -M` from the copy-mode backing, so the
    // one-row scroll of `copy-mode -u` hides neither line5 nor the empty row
    // below it, and line3's hyperlink stays out of the default range.
    let cases: [CaptureCase; 5] = [
        ("plain", |_| {}, b"line4\nline5\n\n"),
        ("-F", |r| r.include_format = true, b"- line4\n- line5\n- \n"),
        (
            "-F -J",
            |r| {
                r.include_format = true;
                r.join_wrapped = true;
            },
            b"- line4\n- line5\n- \n",
        ),
        (
            "-L -e",
            |r| {
                r.line_numbers = true;
                r.escape_ansi = true;
            },
            b"0 line4\n1 line5\n2 \n",
        ),
        ("-H", |r| r.hyperlinks = true, b"\n"),
    ];
    for raw in [false, true] {
        let handler = RequestHandler::new();
        let size = TerminalSize { cols: 24, rows: 3 };
        create_session_with_size(&handler, "backing", size).await;
        let target = PaneTarget::with_window(session_name("backing"), 0, 0);
        replace_transcript_contents(
            &handler,
            &target,
            size,
            b"line1\r\nline2\r\n\x1b]8;;http://x/\x1b\\line3\x1b]8;;\x1b\\\r\nline4\r\nline5\r\n",
        )
        .await;
        let response = handler
            .handle(Request::CopyMode(CopyModeRequest {
                target: Some(target.clone()),
                page_down: false,
                exit_on_scroll: false,
                hide_position: false,
                mouse_drag_start: false,
                cancel_mode: false,
                scrollbar_scroll: false,
                source: None,
                page_up: true,
            }))
            .await;
        assert!(matches!(response, Response::CopyMode(_)));
        for (flags, configure, expected) in cases {
            let mut request = capture_pane_request(target.clone(), None, None, true, None);
            request.use_mode_screen = true;
            configure(&mut request);
            assert_eq!(
                capture_stdout(capture(&handler, request, raw).await),
                expected,
                "raw={raw}, -M {flags}"
            );
        }
    }
}

#[tokio::test]
async fn alternate_capture_takes_precedence_over_copy_mode_screen_like_tmux_3_7b() {
    // tmux 3.7b tests `-a` before `-M`: the saved main screen wins over the
    // copy-mode backing, and without one the capture fails unless `-q`.
    for raw in [false, true] {
        let handler = RequestHandler::new();
        let size = TerminalSize { cols: 24, rows: 3 };
        create_session_with_size(&handler, "precedence", size).await;
        let target = PaneTarget::with_window(session_name("precedence"), 0, 0);
        for alternate_on in [true, false] {
            let bytes = if alternate_on {
                b"main1\r\nmain2\r\nmain3\r\nmain4\r\n\x1b[?1049hALT1\r\nALT2\r\n".as_slice()
            } else {
                b"line1\r\nline2\r\n".as_slice()
            };
            // Replacing the screen also leaves the previous copy mode.
            replace_transcript_contents(&handler, &target, size, bytes).await;
            let response = handler
                .handle(Request::CopyMode(CopyModeRequest {
                    target: Some(target.clone()),
                    page_down: false,
                    exit_on_scroll: false,
                    hide_position: false,
                    mouse_drag_start: false,
                    cancel_mode: false,
                    scrollbar_scroll: false,
                    source: None,
                    page_up: alternate_on,
                }))
                .await;
            assert!(matches!(response, Response::CopyMode(_)));
            for (include_format, quiet) in [(false, false), (true, false), (false, true)] {
                let mut request = capture_pane_request(target.clone(), None, None, true, None);
                request.use_mode_screen = true;
                request.alternate = true;
                request.include_format = include_format;
                request.join_wrapped = include_format;
                request.quiet = quiet;
                let response = capture(&handler, request, raw).await;
                let context = format!(
                    "raw={raw}, alternate_on={alternate_on}, -F={include_format}, -q={quiet}"
                );
                match (alternate_on, include_format, quiet) {
                    (true, false, _) => {
                        assert_eq!(capture_stdout(response), b"main3\nmain4\n\n", "{context}")
                    }
                    (true, true, _) => assert_eq!(
                        capture_stdout(response),
                        b"- main3\n- main4\n- \n",
                        "{context}"
                    ),
                    (false, _, true) => assert_eq!(capture_stdout(response), b"\n", "{context}"),
                    (false, _, false) => {
                        assert!(
                            matches!(response, Response::Error(_)),
                            "{context}: {response:?}"
                        )
                    }
                }
            }
            if alternate_on {
                let mut mode_only = capture_pane_request(target.clone(), None, None, true, None);
                mode_only.use_mode_screen = true;
                assert_eq!(
                    capture_stdout(capture(&handler, mode_only, raw).await),
                    b"ALT1\nALT2\n\n",
                    "raw={raw}"
                );
            }
        }
    }
}
#[tokio::test]
async fn capture_format_alternate_history_wrap_matches_tmux_on_all_dispatch_paths() {
    let handler = RequestHandler::new();
    let size = TerminalSize { cols: 8, rows: 2 };
    create_session_with_size(&handler, "wrap-flags", size).await;
    let target = PaneTarget::with_window(session_name("wrap-flags"), 0, 0);
    replace_transcript_contents(
        &handler,
        &target,
        size,
        b"\x1b]133;A\x07abcdefghijkl\r\n\x1b[?1049h\x1b[HVIM",
    )
    .await;
    for joined in [false, true] {
        for history_only in [false, true] {
            let expected = if history_only {
                b"P abcdefgh\n".as_slice()
            } else {
                b"P abcdefgh\n- VIM\n- \n".as_slice()
            };
            for raw in [false, true] {
                let mut request = capture_pane_request(
                    target.clone(),
                    Some(-1),
                    history_only.then_some(-1),
                    true,
                    None,
                );
                request.include_format = true;
                request.join_wrapped = joined;
                assert_eq!(
                    capture_stdout(capture(&handler, request, raw).await),
                    expected
                );
            }
            let join = if joined { "-J" } else { "" };
            let end = if history_only { "-E -1" } else { "" };
            let queued = handler
                .parse_control_commands(&format!(
                    "capture-pane -p -F -S -1 {join} {end} -t wrap-flags:0.0"
                ))
                .await
                .expect("capture parses");
            let response = handler
                .execute_parsed_commands_for_test(std::process::id(), queued)
                .await
                .expect("capture executes");
            assert_eq!(response.stdout(), expected);
        }
    }
}
