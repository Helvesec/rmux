use super::*;

async fn create_attached_live_session(
    handler: &RequestHandler,
    name: &rmux_proto::SessionName,
    requester_pid: u32,
) -> mpsc::UnboundedReceiver<crate::pane_io::AttachControl> {
    #[cfg(unix)]
    {
        let mut state = handler.state.lock().await;
        state
            .options
            .set(
                ScopeSelector::Global,
                OptionName::DefaultShell,
                "/bin/bash".to_owned(),
                SetOptionMode::Replace,
            )
            .expect("test default-shell is valid");
    }

    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: name.clone(),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));

    let (control_tx, control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, name.clone(), control_tx)
        .await;
    control_rx
}

#[tokio::test]
async fn live_attach_unterminated_bracketed_paste_is_bounded_without_pane_leak() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();
    let _control_rx = create_attached_live_session(&handler, &alpha, requester_pid).await;

    #[cfg(windows)]
    let capture_target = {
        let target = PaneTarget::new(alpha.clone(), 0);
        let state = handler.state.lock().await;
        state.start_pane_input_capture_for_test(&target);
        target
    };

    let mut pending_input = Vec::new();
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"\x1b[200~")
        .await
        .expect("bracketed paste start is retained");
    assert_eq!(pending_input, b"\x1b[200~");

    let overflow = vec![b'a'; DEFAULT_MAX_FRAME_LENGTH - pending_input.len() + 1];
    let err = handler
        .handle_attached_live_input(requester_pid, &mut pending_input, &overflow)
        .await
        .expect_err("unterminated bracketed paste should be bounded");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    assert!(err.to_string().contains("live bracketed paste"));
    assert!(pending_input.is_empty());

    #[cfg(windows)]
    {
        let state = handler.state.lock().await;
        assert_eq!(
            state.pane_input_capture_for_test(&capture_target),
            Some(Vec::new())
        );
    }
}

#[tokio::test]
async fn timed_out_bracketed_paste_forwards_only_scrubbed_body_then_resumes_input() {
    let handler = RequestHandler::new();
    let alpha = session_name("timed-out-bracketed-paste");
    let requester_pid = std::process::id();
    let _control_rx = create_attached_live_session(&handler, &alpha, requester_pid).await;
    let capture = RawPaneInputProbe::start(
        &handler,
        &alpha,
        "timed-out-bracketed-paste",
        b"\x02[prefix\x1b[Atail!".len(),
    )
    .await;

    // Removing the embedded marker makes the surrounding fragments form a
    // second opener. The fixed-point scrub must remove that too, otherwise a
    // timed-out paste can poison the child terminal parser again.
    let retained = b"\x1b[200~\x02[prefix\x1b[\x1b[200~200~\x1b[Atail";
    let mut pending_input = Vec::new();
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, retained)
        .await
        .expect("unterminated bracketed paste is retained");
    assert_eq!(pending_input, retained);

    handler
        .flush_attached_pending_escape_input(requester_pid, &mut pending_input)
        .await
        .expect("timed-out bracketed paste is neutralized");
    assert!(pending_input.is_empty());
    assert!(
        !handler
            .target_is_in_copy_mode(&PaneTarget::new(alpha.clone(), 0))
            .await
            .expect("copy-mode state resolves"),
        "the pasted C-b [ bytes must remain literal rather than run a prefix binding"
    );

    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"!")
        .await
        .expect("ordinary input resumes after the timeout");
    assert!(pending_input.is_empty());

    capture.finish(&handler, &alpha).await;
    capture
        .assert_contents(&handler, b"\x02[prefix\x1b[Atail!")
        .await;
}

#[tokio::test]
async fn timed_out_paste_partial_delimiters_never_reach_the_pane() {
    let handler = RequestHandler::new();
    let alpha = session_name("timed-out-paste-partial-delimiters");
    let requester_pid = std::process::id();
    let _control_rx = create_attached_live_session(&handler, &alpha, requester_pid).await;
    let cuts = [
        b"\x1b".as_slice(),
        b"\x1b[".as_slice(),
        b"\x1b[2".as_slice(),
        b"\x1b[20".as_slice(),
        b"\x1b[200".as_slice(),
        b"\x1b[201".as_slice(),
    ];
    let expected = b"body!".repeat(cuts.len());
    let capture = RawPaneInputProbe::start(
        &handler,
        &alpha,
        "timed-out-paste-partial-delimiters",
        expected.len(),
    )
    .await;
    let mut pending_input = Vec::new();

    for cut in cuts {
        let mut retained = b"\x1b[200~body".to_vec();
        retained.extend_from_slice(cut);
        handler
            .handle_attached_live_input(requester_pid, &mut pending_input, &retained)
            .await
            .expect("retain paste with a cut closing delimiter");
        assert_eq!(pending_input, retained);

        handler
            .flush_attached_pending_escape_input(requester_pid, &mut pending_input)
            .await
            .expect("sanitize the timed-out paste delimiter");
        assert!(pending_input.is_empty());
        handler
            .handle_attached_live_input(requester_pid, &mut pending_input, b"!")
            .await
            .expect("ordinary input resumes after the paste timeout");
        assert!(pending_input.is_empty());
    }

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, &expected).await;
}

#[tokio::test]
async fn timed_out_overlong_sgr_mouse_is_discarded_then_resumes_input() {
    let handler = RequestHandler::new();
    let alpha = session_name("timed-out-overlong-sgr-mouse");
    let requester_pid = std::process::id();
    let _control_rx = create_attached_live_session(&handler, &alpha, requester_pid).await;
    let capture =
        RawPaneInputProbe::start(&handler, &alpha, "timed-out-overlong-sgr-mouse", 3).await;

    let mut pending_input = Vec::new();
    for (field, retained, next_input) in [
        ("button", b"\x1b[<700000".as_slice(), b"b".as_slice()),
        ("x", b"\x1b[<1;700000".as_slice(), b"x".as_slice()),
        ("y", b"\x1b[<1;1;700000".as_slice(), b"y".as_slice()),
    ] {
        handler
            .handle_attached_live_input(requester_pid, &mut pending_input, retained)
            .await
            .unwrap_or_else(|error| panic!("retain overflowing {field} field: {error}"));
        assert_eq!(pending_input, retained, "{field} overflow is retained");

        let forwarded = handler
            .flush_attached_pending_escape_input(requester_pid, &mut pending_input)
            .await
            .unwrap_or_else(|error| panic!("discard timed-out {field} overflow: {error}"));
        assert!(!forwarded, "discarding invalid {field} writes no bytes");
        assert!(pending_input.is_empty());

        handler
            .handle_attached_live_input(requester_pid, &mut pending_input, next_input)
            .await
            .unwrap_or_else(|error| panic!("ordinary input resumes after {field}: {error}"));
        assert!(pending_input.is_empty());
    }

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, b"bxy").await;
}

#[tokio::test]
async fn complete_overflowing_sgr_mouse_is_discarded_before_plain_tail() {
    let handler = RequestHandler::new();
    let alpha = session_name("complete-overflowing-sgr-mouse");
    let requester_pid = std::process::id();
    let _control_rx = create_attached_live_session(&handler, &alpha, requester_pid).await;
    let capture =
        RawPaneInputProbe::start(&handler, &alpha, "complete-overflowing-sgr-mouse", 3).await;
    let mut pending_input = Vec::new();

    for input in [
        b"\x1b[<700000;1;1Mb".as_slice(),
        b"\x1b[<1;700000;1Mx".as_slice(),
        b"\x1b[<1;1;700000My".as_slice(),
    ] {
        handler
            .handle_attached_live_input(requester_pid, &mut pending_input, input)
            .await
            .expect("discard complete overflow and reroute its plain tail");
        assert!(pending_input.is_empty());
    }

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, b"bxy").await;
}

#[tokio::test]
async fn fragmented_overlong_sgr_mouse_consumes_its_opener_at_the_fixed_bound() {
    let handler = RequestHandler::new();
    let alpha = session_name("bounded-fragmented-overlong-sgr-mouse");
    let requester_pid = std::process::id();
    let _control_rx = create_attached_live_session(&handler, &alpha, requester_pid).await;
    let capture =
        RawPaneInputProbe::start(&handler, &alpha, "bounded-fragmented-overlong-sgr-mouse", 1)
            .await;
    let mut pending_input = Vec::new();

    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"\x1b[<")
        .await
        .expect("retain SGR mouse opener");
    assert_eq!(pending_input.len(), 3);
    for total_len in 4..=MAX_SGR_MOUSE_FRAME_BYTES {
        handler
            .handle_attached_live_input(requester_pid, &mut pending_input, b"9")
            .await
            .expect("process one overflowing decimal fragment");
        if total_len < MAX_SGR_MOUSE_FRAME_BYTES {
            assert_eq!(pending_input.len(), total_len);
        } else {
            assert!(
                pending_input.is_empty(),
                "the fixed bound consumes the opener instead of retaining an unbounded field"
            );
        }
    }

    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"!")
        .await
        .expect("ordinary input resumes after the bounded discard");
    assert!(pending_input.is_empty());

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, b"!").await;
}

#[tokio::test]
async fn live_attach_chunked_sgr_mouse_sequence_still_dispatches() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();
    let _control_rx = create_attached_live_session(&handler, &alpha, requester_pid).await;
    let mouse_enabled = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::Mouse,
            value: "on".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(mouse_enabled, Response::SetOption(_)));

    {
        let mut state = handler.state.lock().await;
        state
            .append_bytes_to_pane_transcript_for_test(&alpha, 0, 0, b"\x1b[?1003h\x1b[?1006h")
            .expect("mouse any and sgr transcript update");
    }

    #[cfg(windows)]
    let expected = encode_mouse_event(
        mode::MODE_MOUSE_ALL | mode::MODE_MOUSE_SGR,
        &MouseForwardEvent {
            b: 64,
            lb: 0,
            x: 1,
            y: 1,
            lx: 0,
            ly: 0,
            sgr_b: 64,
            sgr_type: 'M',
            ignore: false,
        },
        1,
        1,
    )
    .expect("sgr wheel encodes");

    #[cfg(windows)]
    let capture = RawPaneInputProbe::start(
        &handler,
        &alpha,
        "live-attach-chunked-sgr-wheel",
        expected.len(),
    )
    .await;

    let mut pending_input = Vec::new();
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"\x1b[<64;2")
        .await
        .expect("first sgr mouse chunk");
    assert_eq!(pending_input, b"\x1b[<64;2");

    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b";2M")
        .await
        .expect("second sgr mouse chunk");
    assert!(pending_input.is_empty());

    let active_attach = handler.active_attach.lock().await;
    let event = active_attach
        .by_pid
        .get(&requester_pid)
        .and_then(|active| active.mouse.current_event.as_ref())
        .expect("current chunked wheel event");
    assert_eq!(event.location, MouseLocation::Pane);
    assert_eq!(event.raw.b, 64);
    drop(active_attach);

    #[cfg(windows)]
    {
        capture.finish(&handler, &alpha).await;
        capture.assert_contents(&handler, &expected).await;
    }
}

#[tokio::test]
async fn completed_pending_mouse_does_not_charge_large_plain_tail_to_retained_bound() {
    let handler = RequestHandler::new();
    let alpha = session_name("mouse-before-large-tail");
    let requester_pid = std::process::id();
    let _control_rx = create_attached_live_session(&handler, &alpha, requester_pid).await;

    let mouse_continuation = b"0;1;1M";
    let mut continuation_and_tail = Vec::with_capacity(DEFAULT_MAX_FRAME_LENGTH);
    continuation_and_tail.extend_from_slice(mouse_continuation);
    continuation_and_tail.resize(DEFAULT_MAX_FRAME_LENGTH, b'x');
    let expected_tail = &continuation_and_tail[mouse_continuation.len()..];
    let capture = RawPaneInputProbe::start(
        &handler,
        &alpha,
        "completed-mouse-before-large-tail",
        expected_tail.len(),
    )
    .await;

    let mut pending_input = Vec::new();
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"\x1b[<")
        .await
        .expect("partial mouse opener is retained");

    tokio::time::timeout(
        if cfg!(windows) {
            Duration::from_secs(60)
        } else {
            // This is the maximum transport frame, not an interactive-latency
            // assertion. Keep a bounded budget that survives full-suite CPU
            // contention while still catching pathological decode work.
            Duration::from_secs(8)
        },
        handler.handle_attached_live_input(
            requester_pid,
            &mut pending_input,
            &continuation_and_tail,
        ),
    )
    .await
    .expect("valid mouse plus a maximum-size plain tail must remain bounded")
    .expect("the plain tail is not retained control input");

    assert!(pending_input.is_empty());
    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, expected_tail).await;
}

#[tokio::test]
async fn live_attach_invalid_sgr_mouse_is_forwarded_instead_of_retained() {
    let handler = RequestHandler::new();
    let alpha = session_name("invalid-sgr-mouse");
    let requester_pid = std::process::id();
    let _control_rx = create_attached_live_session(&handler, &alpha, requester_pid).await;
    let expected = b"\x1b[<a";
    let capture = RawPaneInputProbe::start(
        &handler,
        &alpha,
        "live-attach-invalid-sgr-mouse",
        expected.len(),
    )
    .await;

    let mut pending_input = Vec::new();
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, expected)
        .await
        .expect("invalid SGR mouse input");

    assert!(
        pending_input.is_empty(),
        "invalid mouse syntax must not poison subsequent input"
    );
    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, expected).await;
}

#[tokio::test]
async fn live_attach_commits_plain_prefix_before_retained_escape_suffix() {
    for (label, suffix) in [
        ("csi", b"\x1b[".as_slice()),
        ("double-escape", b"\x1b\x1b".as_slice()),
    ] {
        let handler = RequestHandler::new();
        let alpha = session_name(&format!("plain-before-{label}"));
        let requester_pid = std::process::id();
        let _control_rx = create_attached_live_session(&handler, &alpha, requester_pid).await;
        let mut expected = b"text".to_vec();
        expected.extend_from_slice(suffix);
        let capture = RawPaneInputProbe::start(
            &handler,
            &alpha,
            &format!("live-attach-plain-before-{label}"),
            expected.len(),
        )
        .await;

        let mut pending_input = Vec::new();
        handler
            .handle_attached_live_input(requester_pid, &mut pending_input, &expected)
            .await
            .expect("plain input followed by an escape prefix");
        assert_eq!(
            pending_input, suffix,
            "only the ambiguous escape suffix may remain pending"
        );

        handler
            .flush_attached_pending_escape_input(requester_pid, &mut pending_input)
            .await
            .expect("flush retained escape suffix");
        assert!(pending_input.is_empty());
        capture.finish(&handler, &alpha).await;
        capture.assert_contents(&handler, &expected).await;
    }
}

#[tokio::test]
async fn live_attach_unterminated_sgr_mouse_is_bounded_without_pane_leak() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();
    let _control_rx = create_attached_live_session(&handler, &alpha, requester_pid).await;

    let capture = RawPaneInputProbe::start(
        &handler,
        &alpha,
        "one-shot-bounded-unterminated-sgr-mouse",
        0,
    )
    .await;

    let mut pending_input = Vec::new();
    let mut bounded = b"\x1b[<".to_vec();
    bounded.resize(MAX_SGR_MOUSE_FRAME_BYTES, b'9');
    let forwarded = handler
        .handle_attached_live_input_inner(requester_pid, &mut pending_input, &bounded)
        .await
        .expect("unterminated SGR mouse is consumed at the fixed syntax bound");
    assert!(!forwarded, "bounded discard must not write to pane IO");
    assert!(pending_input.is_empty());

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, b"").await;
}
