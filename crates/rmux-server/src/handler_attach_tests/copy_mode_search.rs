use super::*;

async fn set_vi_mode_keys(handler: &RequestHandler, session: &SessionName) {
    assert!(matches!(
        handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Window(WindowTarget::with_window(session.clone(), 0)),
                option: OptionName::ModeKeys,
                value: "vi".to_owned(),
                mode: SetOptionMode::Replace,
            }))
            .await,
        Response::SetOption(_)
    ));
}

async fn enter_copy_mode_with_search_seed(handler: &RequestHandler, target: &PaneTarget) -> String {
    replace_transcript_contents(
        handler,
        target,
        TerminalSize { cols: 80, rows: 24 },
        b"alpha beta gamma\r\nsecond beta line\r\nthird alpha marker\r\nfourth beta marker\r\nfifth beta tail\r\n",
    )
    .await;
    assert!(matches!(
        handler
            .handle(Request::CopyMode(CopyModeRequest {
                target: Some(target.clone()),
                page_down: false,
                exit_on_scroll: false,
                hide_position: false,
                mouse_drag_start: false,
                cancel_mode: false,
                scrollbar_scroll: false,
                source: None,
                page_up: false,
            }))
            .await,
        Response::CopyMode(_)
    ));
    copy_search_status(handler, target.clone()).await
}

async fn copy_search_status(handler: &RequestHandler, target: PaneTarget) -> String {
    display_target_format(
        handler,
        target,
        "#{pane_in_mode}:#{copy_cursor_x},#{copy_cursor_y}:#{search_match}",
    )
    .await
}

async fn send_copy_search_key(
    handler: &RequestHandler,
    requester_pid: u32,
    pending_input: &mut Vec<u8>,
    bytes: &[u8],
) {
    let forwarded_to_pane = handler
        .handle_attached_live_input_inner(requester_pid, pending_input, bytes)
        .await
        .expect("copy-mode search input");
    assert!(
        !forwarded_to_pane,
        "copy-mode search keys must be consumed instead of forwarded to pane IO"
    );
    assert!(
        pending_input.is_empty(),
        "copy-mode search input should fully decode and leave no pending bytes"
    );
}

#[tokio::test]
async fn copy_mode_search_prompt_consumes_query_without_pane_leak() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let mut control_rx = create_quiet_attached_session(&handler, requester_pid, &alpha).await;
    let target = PaneTarget::new(alpha.clone(), 0);
    set_vi_mode_keys(&handler, &alpha).await;

    assert_eq!(
        enter_copy_mode_with_search_seed(&handler, &target).await,
        "1:0,5:\n"
    );
    drain_attach_controls(&mut control_rx);
    let before_capture = capture_pane_print(&handler, target.clone()).await;

    let mut pending_input = Vec::new();
    send_copy_search_key(&handler, requester_pid, &mut pending_input, b"/").await;
    let prompt = handler
        .attached_prompt_render(requester_pid)
        .await
        .expect("vi slash opens a copy-mode search prompt");
    assert!(
        prompt.prompt.contains("(search down)"),
        "copy-mode search prompt must be distinct from the shell prompt, got {prompt:?}"
    );
    drain_attach_controls(&mut control_rx);

    send_copy_search_key(&handler, requester_pid, &mut pending_input, b"beta\r").await;

    assert_eq!(
        capture_pane_print(&handler, target).await,
        before_capture,
        "copy-mode search query bytes must not mutate the pane screen"
    );
}

#[tokio::test]
async fn copy_mode_question_search_prompt_consumes_query_without_pane_leak() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let mut control_rx = create_quiet_attached_session(&handler, requester_pid, &alpha).await;
    let target = PaneTarget::new(alpha.clone(), 0);
    set_vi_mode_keys(&handler, &alpha).await;

    assert_eq!(
        enter_copy_mode_with_search_seed(&handler, &target).await,
        "1:0,5:\n"
    );
    drain_attach_controls(&mut control_rx);
    let before_capture = capture_pane_print(&handler, target.clone()).await;

    let mut pending_input = Vec::new();
    send_copy_search_key(&handler, requester_pid, &mut pending_input, b"?").await;
    let prompt = handler
        .attached_prompt_render(requester_pid)
        .await
        .expect("vi question mark opens a copy-mode search prompt");
    assert!(
        prompt.prompt.contains("(search up)"),
        "copy-mode backward search prompt must be distinct from the shell prompt, got {prompt:?}"
    );
    drain_attach_controls(&mut control_rx);

    send_copy_search_key(&handler, requester_pid, &mut pending_input, b"beta\r").await;
    tokio::task::yield_now().await;
    assert_eq!(
        copy_search_status(&handler, target.clone()).await,
        "1:6,4:beta\n",
        "primary ? search must land on the previous beta match from the copy cursor"
    );

    assert_eq!(
        capture_pane_print(&handler, target).await,
        before_capture,
        "? search query bytes must not mutate the pane screen"
    );
}

#[tokio::test]
async fn copy_mode_search_prompt_bounds_unterminated_sgr_mouse_without_pane_leak() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let mut control_rx = create_quiet_attached_session(&handler, requester_pid, &alpha).await;
    let target = PaneTarget::new(alpha.clone(), 0);
    set_vi_mode_keys(&handler, &alpha).await;

    let _ = enter_copy_mode_with_search_seed(&handler, &target).await;
    drain_attach_controls(&mut control_rx);
    let before_capture = capture_pane_print(&handler, target.clone()).await;

    let mut pending_input = Vec::new();
    send_copy_search_key(&handler, requester_pid, &mut pending_input, b"/").await;
    assert!(
        handler
            .attached_prompt_render(requester_pid)
            .await
            .is_some(),
        "slash should leave a search prompt active before the partial-input guard"
    );

    let partial = bounded_unterminated_sgr_mouse_input();
    let forwarded = handler
        .handle_attached_live_input_inner(requester_pid, &mut pending_input, &partial)
        .await
        .expect("bounded invalid mouse input is consumed");
    assert!(
        !forwarded,
        "invalid prompt mouse input must not reach pane IO"
    );
    assert!(
        pending_input.is_empty(),
        "overflowing partial prompt input should be consumed at the syntax bound"
    );
    assert_eq!(
        capture_pane_print(&handler, target.clone()).await,
        before_capture,
        "unterminated prompt control input must not mutate the pane screen"
    );

    let recovered = handler
        .handle_attached_live_input_inner(requester_pid, &mut pending_input, b"\x1b")
        .await
        .expect("escape should still be handled after the bounded discard");
    assert!(
        !recovered,
        "search prompt escape must not be forwarded to pane IO"
    );
    assert_eq!(
        pending_input, b"\x1b",
        "a lone Escape remains ambiguous with a fragmented terminal response until escape-time"
    );
    let recovered = handler
        .flush_attached_pending_escape_input(requester_pid, &mut pending_input)
        .await
        .expect("escape-time should resolve the retained Escape after the bounded discard");
    assert!(
        !recovered,
        "the resolved search prompt Escape must not be forwarded to pane IO"
    );
    assert!(pending_input.is_empty());
    assert!(
        handler
            .attached_prompt_render(requester_pid)
            .await
            .is_none(),
        "search prompt should remain recoverable after the bounded discard"
    );
}

#[tokio::test]
async fn copy_mode_search_repeat_next_and_previous_match_tmux_order() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let mut control_rx = create_quiet_attached_session(&handler, requester_pid, &alpha).await;
    let target = PaneTarget::new(alpha.clone(), 0);
    set_vi_mode_keys(&handler, &alpha).await;
    let _ = enter_copy_mode_with_search_seed(&handler, &target).await;
    drain_attach_controls(&mut control_rx);
    let before_capture = capture_pane_print(&handler, target.clone()).await;

    handler
        .execute_copy_mode_command(
            requester_pid,
            target.clone(),
            "search-forward",
            &["--".to_owned(), "beta".to_owned()],
            1,
        )
        .await
        .expect("direct primary search-forward setup");
    assert_eq!(
        copy_search_status(&handler, target.clone()).await,
        "1:6,0:beta\n",
        "primary search-forward must match tmux oracle before testing n/N"
    );

    let mut pending_input = Vec::new();
    send_copy_search_key(&handler, requester_pid, &mut pending_input, b"n").await;
    assert_eq!(
        copy_search_status(&handler, target.clone()).await,
        "1:7,1:beta\n",
        "n must repeat the primary forward search direction"
    );

    send_copy_search_key(&handler, requester_pid, &mut pending_input, b"N").await;
    assert_eq!(
        copy_search_status(&handler, target.clone()).await,
        "1:6,0:beta\n",
        "N must reverse the primary forward search direction for one step"
    );

    assert_eq!(
        capture_pane_print(&handler, target.clone()).await,
        before_capture,
        "copy-mode search repeat keys must not reach or mutate pane IO"
    );

    send_copy_search_key(&handler, requester_pid, &mut pending_input, b"q").await;
    assert_eq!(
        copy_search_status(&handler, target.clone()).await,
        "0:,:\n",
        "q must exit copy-mode after search repeat navigation"
    );
    assert!(
        !capture_pane_print(&handler, target.clone())
            .await
            .contains("\nq"),
        "q must not appear in the pane capture after copy-mode search dismiss"
    );

    let forwarded_to_pane = handler
        .handle_attached_live_input_inner(
            requester_pid,
            &mut pending_input,
            b"RMUX_AFTER_COPY_SEARCH",
        )
        .await
        .expect("normal input resumes after copy-mode search");
    assert!(
        forwarded_to_pane,
        "normal pane input should resume after copy-mode search dismiss"
    );
}

#[tokio::test]
async fn copy_mode_question_search_repeat_next_and_reverse_match_tmux_order() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let mut control_rx = create_quiet_attached_session(&handler, requester_pid, &alpha).await;
    let target = PaneTarget::new(alpha.clone(), 0);
    set_vi_mode_keys(&handler, &alpha).await;
    let _ = enter_copy_mode_with_search_seed(&handler, &target).await;
    drain_attach_controls(&mut control_rx);
    let before_capture = capture_pane_print(&handler, target.clone()).await;

    handler
        .execute_copy_mode_command(
            requester_pid,
            target.clone(),
            "search-backward",
            &["--".to_owned(), "beta".to_owned()],
            1,
        )
        .await
        .expect("direct primary search-backward setup");
    assert_eq!(
        copy_search_status(&handler, target.clone()).await,
        "1:6,4:beta\n",
        "primary search-backward must match tmux oracle before testing n/N"
    );

    let mut pending_input = Vec::new();
    send_copy_search_key(&handler, requester_pid, &mut pending_input, b"n").await;
    assert_eq!(
        copy_search_status(&handler, target.clone()).await,
        "1:7,3:beta\n",
        "n must repeat the primary backward search direction"
    );

    send_copy_search_key(&handler, requester_pid, &mut pending_input, b"N").await;
    assert_eq!(
        copy_search_status(&handler, target.clone()).await,
        "1:6,4:beta\n",
        "N must reverse the primary backward search direction for one step"
    );

    assert_eq!(
        capture_pane_print(&handler, target.clone()).await,
        before_capture,
        "? search repeat keys must not reach or mutate pane IO"
    );

    send_copy_search_key(&handler, requester_pid, &mut pending_input, b"q").await;
    assert_eq!(
        copy_search_status(&handler, target.clone()).await,
        "0:,:\n",
        "q must exit copy-mode after ? search repeat navigation"
    );
    assert!(
        !capture_pane_print(&handler, target.clone())
            .await
            .contains("\nq"),
        "q must not appear in the pane capture after ? search dismiss"
    );

    let forwarded_to_pane = handler
        .handle_attached_live_input_inner(
            requester_pid,
            &mut pending_input,
            b"RMUX_AFTER_COPY_QUESTION_SEARCH",
        )
        .await
        .expect("normal input resumes after ? copy-mode search");
    assert!(
        forwarded_to_pane,
        "normal pane input should resume after ? copy-mode search dismiss"
    );
}
