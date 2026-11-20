use super::*;

type JoinedCaptureScene<'a> = (&'a [u8], i64, &'a [u8], &'a [u8]);

// tmux 3.7b, 8x4, S1210 oracle-ascii-measurement.json (2026-11-03).
// Compare every entry/surface/escape combination, even when one loses spaces.
async fn assert_joined_ascii_suffix_spaces(wrapped: bool, print: bool) {
    let scenes: &[JoinedCaptureScene<'_>] = if wrapped {
        &[
            (b"abc def gh", 0, b"0 abc def \n", b"0 W abc def \n"),
            (
                b"abc def gh",
                1,
                b"0 abc def 1 gh\n",
                b"0 W abc def 1 - gh\n",
            ),
        ]
    } else {
        &[
            (b"abc\r\nnext", 0, b"0 abc\n", b"0 - abc\n"),
            (b"abc \r\nnext", 0, b"0 abc \n", b"0 - abc \n"),
            (b"abc  \r\nnext", 0, b"0 abc  \n", b"0 - abc  \n"),
        ]
    };
    assert_joined_physical_rows(8, scenes, print).await;
}

async fn assert_joined_physical_rows(width: u16, scenes: &[JoinedCaptureScene<'_>], print: bool) {
    let mut failures = Vec::new();
    for raw in [false, true] {
        let handler = RequestHandler::new();
        let size = TerminalSize {
            cols: width,
            rows: 4,
        };
        // The replaced screen must stay the only content: a shell prompt that
        // lands after it rewrites the rows these scenes compare.
        handler.pin_quiet_default_command_for_test().await;
        create_session_with_size(&handler, "ascii-suffix", size).await;
        let target = PaneTarget::with_window(session_name("ascii-suffix"), 0, 0);
        handler.wait_for_pane_startup_output_for_test(&target).await;
        for surface in ["main", "saved", "copy"] {
            for &(payload, end, numbered, formatted) in scenes {
                let mut input = payload.to_vec();
                if surface == "saved" {
                    input.extend_from_slice(b"\x1b[?1049hALT");
                }
                replace_transcript_contents(&handler, &target, size, &input).await;
                if surface == "copy" {
                    assert!(matches!(
                        handler
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
                            .await,
                        Response::CopyMode(_)
                    ));
                }
                for (include_format, oracle) in [(false, numbered), (true, formatted)] {
                    for escape_ansi in [false, true] {
                        let mut request = capture_pane_request(
                            target.clone(),
                            Some(0),
                            Some(end),
                            print,
                            Some("ascii-spaces"),
                        );
                        request.line_numbers = true;
                        request.include_format = include_format;
                        request.join_wrapped = true;
                        request.escape_ansi = escape_ansi;
                        request.alternate = surface == "saved";
                        request.use_mode_screen = surface == "copy";
                        let response = capture(&handler, request, raw).await;
                        let actual = if print {
                            capture_stdout(response)
                        } else {
                            assert!(matches!(response, Response::CapturePane(_)));
                            handler
                                .state
                                .lock()
                                .await
                                .buffers
                                .get("ascii-spaces")
                                .unwrap()
                                .to_vec()
                        };
                        // Only the inherited formatted-buffer final LF is omitted.
                        let expected = if print {
                            oracle
                        } else {
                            oracle.strip_suffix(b"\n").unwrap()
                        };
                        if actual != expected {
                            failures.push(format!(
                            "raw={raw}, print={print}, surface={surface}, F={include_format}, e={escape_ansi}, \
                             payload={payload:?}, end={end}: actual={actual:?}, oracle={expected:?}"
                        ));
                        }
                    }
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[tokio::test]
async fn capture_format_join_ascii_hard_suffix_print() {
    assert_joined_ascii_suffix_spaces(false, true).await;
}

#[tokio::test]
async fn capture_format_join_ascii_hard_suffix_buffer() {
    assert_joined_ascii_suffix_spaces(false, false).await;
}

#[tokio::test]
async fn capture_format_join_ascii_wrapped_suffix_print() {
    assert_joined_ascii_suffix_spaces(true, true).await;
}

#[tokio::test]
async fn capture_format_join_ascii_wrapped_suffix_buffer() {
    assert_joined_ascii_suffix_spaces(true, false).await;
}

// S1212 remeasurement: identical joined text, distinct physical boundaries.
async fn assert_joined_width_collisions(print: bool) {
    for (width, numbered, formatted) in [
        (
            8,
            b"0 abcdefgh1 ijk\n".as_slice(),
            b"0 W abcdefgh1 - ijk\n".as_slice(),
        ),
        (
            9,
            b"0 abcdefghi1 jk\n".as_slice(),
            b"0 W abcdefghi1 - jk\n".as_slice(),
        ),
    ] {
        assert_joined_physical_rows(width, &[(b"abcdefghijk", 1, numbered, formatted)], print)
            .await;
    }
}

#[tokio::test]
async fn capture_format_join_width_collisions_print() {
    assert_joined_width_collisions(true).await;
}

#[tokio::test]
async fn capture_format_join_width_collisions_buffer() {
    assert_joined_width_collisions(false).await;
}

pub(super) async fn capture(
    handler: &RequestHandler,
    request: CapturePaneRequest,
    raw: bool,
) -> Response {
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
async fn capture_format_history_join_pending_and_hyperlink_neighbors() {
    for raw in [false, true] {
        let handler = RequestHandler::new();
        let size = TerminalSize { cols: 8, rows: 3 };
        create_session_with_size(&handler, "neighbors", size).await;
        let target = PaneTarget::with_window(session_name("neighbors"), 0, 0);
        replace_transcript_contents(&handler, &target, size, b"old\r\nabcdefghijk\r\nlast").await;
        for (start, end, joined, format, expected) in [
            (
                None,
                None,
                true,
                false,
                b"0 abcdefgh1 ijk\n2 last\n".as_slice(),
            ),
            (Some(99), Some(99), false, false, b"2 last\n".as_slice()),
        ] {
            let mut request = capture_pane_request(target.clone(), start, end, true, None);
            request.line_numbers = true;
            request.join_wrapped = joined;
            request.include_format = format;
            assert_eq!(
                capture_stdout(capture(&handler, request, raw).await),
                expected,
                "raw={raw}, start={start:?}, end={end:?}, J={joined}, F={format}"
            );
        }
        let mut stored = capture_pane_request(target.clone(), None, None, false, Some("numbered"));
        stored.line_numbers = true;
        stored.join_wrapped = true;
        assert!(matches!(
            capture(&handler, stored, raw).await,
            Response::CapturePane(_)
        ));
        assert_eq!(
            handler.state.lock().await.buffers.get("numbered"),
            Some(b"0 abcdefgh1 ijk\n2 last".as_slice())
        );

        replace_transcript_contents(&handler, &target, size, b"").await;
        handler
            .state
            .lock()
            .await
            .append_bytes_to_pane_transcript_for_test(
                target.session_name(),
                target.window_index(),
                target.pane_index(),
                b"\x1b[",
            )
            .unwrap();
        let transcript = handler
            .state
            .lock()
            .await
            .transcript_handle(&target)
            .unwrap();
        assert_eq!(transcript.lock().pending_bytes(), b"\x1b[");
        let mut pending =
            capture_pane_request(target.clone(), Some(i64::MAX), Some(i64::MIN), true, None);
        pending.pending_input = true;
        pending.line_numbers = true;
        pending.include_format = true;
        assert_eq!(
            capture_stdout(capture(&handler, pending, raw).await),
            b"0 - \x1b[\n"
        );
        handler
            .state
            .lock()
            .await
            .append_bytes_to_pane_transcript_for_test(
                target.session_name(),
                target.window_index(),
                target.pane_index(),
                b"m",
            )
            .unwrap();
        assert!(transcript.lock().pending_bytes().is_empty());

        replace_transcript_contents(
            &handler,
            &target,
            size,
            b"\x1b]8;id=neighbor;https://capture.test\x1b\\abcdefghijkl\x1b]8;;\x1b\\",
        )
        .await;
        let mut links = capture_pane_request(target, None, None, true, None);
        links.hyperlinks = true;
        links.join_wrapped = true;
        let expected = capture_stdout(capture(&handler, links.clone(), raw).await);
        links.line_numbers = true;
        assert_eq!(
            capture_stdout(capture(&handler, links, raw).await),
            expected
        );
    }
}

async fn assert_history_numbers_match_oracle(
    start: Option<i64>,
    end: Option<i64>,
    joined: bool,
    format: bool,
    expected: &[u8],
) {
    let mut outputs = Vec::new();
    for raw in [false, true] {
        let handler = RequestHandler::new();
        let size = TerminalSize { cols: 8, rows: 3 };
        create_session_with_size(&handler, "oracle-history", size).await;
        let target = PaneTarget::with_window(session_name("oracle-history"), 0, 0);
        replace_transcript_contents(&handler, &target, size, b"old\r\nabcdefghijk\r\nlast").await;
        let mut request = capture_pane_request(target, start, end, true, None);
        request.line_numbers = true;
        request.join_wrapped = joined;
        request.include_format = format;
        outputs.push(capture_stdout(capture(&handler, request, raw).await));
    }
    assert_eq!(
        outputs,
        vec![expected.to_vec(), expected.to_vec()],
        "tmux 3.7b history_size=1; outputs ordered typed then raw target action"
    );
}

// Measured with tmux 3.7b, an 8x3 pane and one history row, 2026-11-03.
// S-D568-LOT13-ORACLE-EXPECTATIONS-1201/preuves/FAITS.md records argv and bytes.
#[tokio::test]
async fn capture_format_oracle_history_line_numbers() {
    // capture-pane -p -L
    assert_history_numbers_match_oracle(None, None, false, false, b"0 abcdefgh\n1 ijk\n2 last\n")
        .await;
}

#[tokio::test]
async fn capture_format_oracle_history_format_join_numbers() {
    // capture-pane -p -L -F -J
    assert_history_numbers_match_oracle(None, None, true, true, b"0 W abcdefgh1 - ijk\n2 - last\n")
        .await;
}

#[tokio::test]
async fn capture_format_oracle_history_range_numbers() {
    // capture-pane -p -L -S -1 -E 0
    assert_history_numbers_match_oracle(Some(-1), Some(0), false, false, b"-1 old\n0 abcdefgh\n")
        .await;
}

// Re-measured with tmux 3.7b for S1207, 2026-11-03; the same 8x3 scene.
// Prefixes belong to physical rows, even when -J removes their separator.
#[tokio::test]
async fn capture_format_oracle_history_join_numbers() {
    // capture-pane -p -L -J
    assert_history_numbers_match_oracle(None, None, true, false, b"0 abcdefgh1 ijk\n2 last\n")
        .await;
}

#[tokio::test]
async fn capture_format_oracle_history_clamped_numbers() {
    // capture-pane -p -L -S 99 -E 99
    assert_history_numbers_match_oracle(Some(99), Some(99), false, false, b"2 last\n").await;
}

#[tokio::test]
async fn capture_format_oracle_history_join_buffer() {
    // capture-pane -L -J -b numbered; save-buffer -b numbered -
    // The existing RMUX formatted-buffer contract omits the final LF. Only
    // that terminator is removed from the measured oracle bytes here.
    let oracle = b"0 abcdefgh1 ijk\n2 last\n";
    let expected = oracle.strip_suffix(b"\n").unwrap();
    let mut outputs = Vec::new();
    for raw in [false, true] {
        let handler = RequestHandler::new();
        let size = TerminalSize { cols: 8, rows: 3 };
        create_session_with_size(&handler, "joined-buffer", size).await;
        let target = PaneTarget::with_window(session_name("joined-buffer"), 0, 0);
        replace_transcript_contents(&handler, &target, size, b"old\r\nabcdefghijk\r\nlast").await;
        let mut request = capture_pane_request(target, None, None, false, Some("numbered"));
        request.line_numbers = true;
        request.join_wrapped = true;
        assert!(matches!(
            capture(&handler, request, raw).await,
            Response::CapturePane(_)
        ));
        outputs.push(
            handler
                .state
                .lock()
                .await
                .buffers
                .get("numbered")
                .unwrap()
                .to_vec(),
        );
    }
    assert_eq!(outputs, vec![expected.to_vec(), expected.to_vec()]);
}

#[tokio::test]
async fn capture_format_join_preserves_physical_prefixes_across_surfaces_and_ranges() {
    // Expected bytes are the S1207 tmux measurements, also applied to identical
    // saved/copy snapshots. Unnumbered -J and -F -J remain neighboring controls.
    for raw in [false, true] {
        let handler = RequestHandler::new();
        let size = TerminalSize { cols: 8, rows: 3 };
        create_session_with_size(&handler, "join-surfaces", size).await;
        let target = PaneTarget::with_window(session_name("join-surfaces"), 0, 0);
        for surface in ["main", "saved", "copy"] {
            let bytes = if surface == "saved" {
                b"old\r\nabcdefghijk\r\nlast\x1b[?1049hALT".as_slice()
            } else {
                b"old\r\nabcdefghijk\r\nlast".as_slice()
            };
            replace_transcript_contents(&handler, &target, size, bytes).await;
            if surface == "copy" {
                assert!(matches!(
                    handler
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
                        .await,
                    Response::CopyMode(_)
                ));
            }
            for (start, end, numbers, format, expected) in [
                (
                    None,
                    None,
                    true,
                    false,
                    b"0 abcdefgh1 ijk\n2 last\n".as_slice(),
                ),
                (None, None, false, false, b"abcdefghijk\nlast\n".as_slice()),
                (
                    None,
                    None,
                    false,
                    true,
                    b"W abcdefgh- ijk\n- last\n".as_slice(),
                ),
                (
                    None,
                    None,
                    true,
                    true,
                    b"0 W abcdefgh1 - ijk\n2 - last\n".as_slice(),
                ),
                (Some(0), Some(0), true, false, b"0 abcdefgh\n".as_slice()),
                (Some(1), Some(2), true, false, b"1 ijk\n2 last\n".as_slice()),
                (
                    Some(0),
                    Some(1),
                    true,
                    false,
                    b"0 abcdefgh1 ijk\n".as_slice(),
                ),
            ] {
                let mut request = capture_pane_request(target.clone(), start, end, true, None);
                request.line_numbers = numbers;
                request.include_format = format;
                request.join_wrapped = true;
                request.alternate = surface == "saved";
                request.use_mode_screen = surface == "copy";
                assert_eq!(
                    capture_stdout(capture(&handler, request, raw).await), expected,
                    "raw={raw}, surface={surface}, start={start:?}, end={end:?}, L={numbers}, F={format}"
                );
            }
        }
    }
    // History belongs to the selected surface: the oracle's saved screen
    // clamps negative bounds, so exercise this history range on main only.
    assert_history_numbers_match_oracle(
        Some(-1),
        Some(1),
        true,
        false,
        b"-1 old\n0 abcdefgh1 ijk\n",
    )
    .await;
}

#[tokio::test]
async fn capture_format_numbers_follow_capture_time_history_strip() {
    for raw in [false, true] {
        let handler = RequestHandler::new();
        let size = TerminalSize { cols: 20, rows: 2 };
        create_session_with_size(&handler, "dead-numbered", size).await;
        let target = PaneTarget::with_window(session_name("dead-numbered"), 0, 0);
        replace_transcript_contents(&handler, &target, size, b"exit\r\none\r\ntwo").await;
        {
            let mut state = handler.state.lock().await;
            state
                .record_attached_submitted_text(&target, b"exit")
                .unwrap();
            state.mark_pane_dead_without_exit_details(&target).unwrap();
        }
        let mut request = capture_pane_request(target, Some(99), Some(0), true, None);
        request.line_numbers = true;
        assert_eq!(
            capture_stdout(capture(&handler, request, raw).await),
            b"0 one\n1 two\n"
        );
    }
}
