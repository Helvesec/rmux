use super::*;
use crate::handler::overlay_support::ClientOverlayState;

async fn open_help(
    handler: &RequestHandler,
    attach_pid: u32,
    control_rx: &mut mpsc::UnboundedReceiver<AttachControl>,
) -> String {
    let mut pending_input = Vec::new();
    let forwarded = handler
        .handle_attached_live_input_inner(attach_pid, &mut pending_input, b"\x02?")
        .await
        .expect("prefix help input succeeds");
    assert!(!forwarded, "prefix help must not reach the pane");
    assert!(
        pending_input.is_empty(),
        "prefix help must be fully consumed"
    );
    recv_overlay_frame(control_rx, "prefix help overlay").await
}

async fn navigate_help(
    handler: &RequestHandler,
    attach_pid: u32,
    control_rx: &mut mpsc::UnboundedReceiver<AttachControl>,
    key: &[u8],
    context: &str,
) -> String {
    let mut pending_input = Vec::new();
    let forwarded = handler
        .handle_attached_live_input_inner(attach_pid, &mut pending_input, key)
        .await
        .unwrap_or_else(|error| panic!("{context} failed: {error}"));
    assert!(!forwarded, "{context} must not reach the pane");
    assert!(pending_input.is_empty(), "{context} must be fully consumed");
    recv_overlay_frame(control_rx, context).await
}

async fn list_key_note_lines(handler: &RequestHandler, requester_pid: u32) -> Vec<String> {
    let commands = handler
        .parse_command_string_one_group("list-keys -N")
        .await
        .expect("list-keys -N parses");
    let output = handler
        .execute_parsed_commands_for_test(requester_pid, commands)
        .await
        .expect("list-keys -N executes");
    String::from_utf8(output.stdout().to_vec())
        .expect("list-keys output is utf-8")
        .lines()
        .map(str::to_owned)
        .collect()
}

async fn help_scroll_state(handler: &RequestHandler, attach_pid: u32) -> (usize, usize) {
    let active_attach = handler.active_attach.lock().await;
    let active = active_attach
        .by_pid
        .get(&attach_pid)
        .expect("attached client remains registered");
    let Some(ClientOverlayState::Popup(popup)) = active.overlay.as_ref() else {
        panic!("expected help popup");
    };
    let text = popup
        .scrollable_text
        .as_ref()
        .expect("help popup owns static text");
    (text.offset(), text.line_count())
}

async fn show_help_directly(handler: &RequestHandler, attach_pid: u32) -> bool {
    let (identity, session_name, session_id, target) = {
        let active_attach = handler.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get(&attach_pid)
            .expect("attached client");
        (
            active.identity(attach_pid),
            active.session_name.clone(),
            active.session_id,
            Target::Pane(PaneTarget::new(active.session_name.clone(), 0)),
        )
    };
    handler
        .show_attached_key_help_popup(
            crate::handler::overlay_support::AttachedHelpContext {
                attach_pid,
                requester_pid: attach_pid,
                expected_identity: Some(identity),
                expected_session_name: &session_name,
                expected_session_id: session_id,
                target: &target,
            },
            &rmux_proto::CommandOutput::from_stdout(b"first\nlast\n".to_vec()),
        )
        .await
        .expect("direct help publication succeeds")
}

#[tokio::test]
async fn attached_help_scrolls_a_short_terminal_from_first_to_last_line() {
    let handler = RequestHandler::new();
    let attach_pid = std::process::id();
    let alpha = session_name("help-scroll");
    let mut control_rx = create_quiet_attached_session(&handler, attach_pid, &alpha).await;
    handler
        .handle_attached_resize(attach_pid, TerminalSize { cols: 160, rows: 8 })
        .await
        .expect("short attach resize succeeds");
    drain_attach_controls(&mut control_rx);

    let lines = list_key_note_lines(&handler, attach_pid).await;
    assert!(lines.len() > 8, "fixture must exceed the terminal viewport");
    let first = lines.first().expect("first help line");
    let second = lines.get(1).expect("second help line");
    let last = lines.last().expect("last help line");

    let frame = open_help(&handler, attach_pid, &mut control_rx).await;
    assert!(
        frame.contains(first),
        "initial help must expose the first line"
    );
    assert!(
        !frame.contains(last),
        "short initial viewport must omit the last line"
    );

    let mut fragmented_arrow = Vec::new();
    handler
        .handle_attached_live_input(attach_pid, &mut fragmented_arrow, b"\x1b")
        .await
        .expect("fragmented Down escape is retained");
    assert_eq!(fragmented_arrow, b"\x1b");
    assert!(
        control_rx.try_recv().is_err(),
        "partial Down must not refresh or close help"
    );
    handler
        .handle_attached_live_input(attach_pid, &mut fragmented_arrow, b"[B")
        .await
        .expect("fragmented Down sequence completes");
    assert!(fragmented_arrow.is_empty());
    let frame = recv_overlay_frame(&mut control_rx, "fragmented help Down").await;
    assert!(frame.contains(second), "Down must reveal the second line");
    assert_eq!(help_scroll_state(&handler, attach_pid).await.0, 1);

    let frame = navigate_help(&handler, attach_pid, &mut control_rx, b"\x1b[A", "help Up").await;
    assert!(frame.contains(first), "Up must return to the first line");
    assert_eq!(help_scroll_state(&handler, attach_pid).await.0, 0);

    let mut fragmented_ss3_arrow = Vec::new();
    handler
        .handle_attached_live_input(attach_pid, &mut fragmented_ss3_arrow, b"\x1b")
        .await
        .expect("fragmented SS3 Down escape is retained");
    assert_eq!(fragmented_ss3_arrow, b"\x1b");
    assert!(
        control_rx.try_recv().is_err(),
        "partial SS3 Down must not refresh or close help"
    );
    handler
        .handle_attached_live_input(attach_pid, &mut fragmented_ss3_arrow, b"OB")
        .await
        .expect("fragmented SS3 Down sequence completes");
    assert!(fragmented_ss3_arrow.is_empty());
    let frame = recv_overlay_frame(&mut control_rx, "fragmented SS3 help Down").await;
    assert!(
        frame.contains(second),
        "SS3 Down must reveal the second line"
    );
    assert_eq!(help_scroll_state(&handler, attach_pid).await.0, 1);

    let frame = navigate_help(
        &handler,
        attach_pid,
        &mut control_rx,
        b"\x1bOA",
        "SS3 help Up",
    )
    .await;
    assert!(
        frame.contains(first),
        "SS3 Up must return to the first line"
    );
    assert_eq!(help_scroll_state(&handler, attach_pid).await.0, 0);

    let _ = navigate_help(
        &handler,
        attach_pid,
        &mut control_rx,
        b"\x1b[6~",
        "help PageDown",
    )
    .await;
    assert!(help_scroll_state(&handler, attach_pid).await.0 > 1);
    let frame = navigate_help(
        &handler,
        attach_pid,
        &mut control_rx,
        b"\x1b[5~",
        "help PageUp",
    )
    .await;
    assert!(
        frame.contains(first),
        "PageUp must return to the first page"
    );

    let frame = navigate_help(&handler, attach_pid, &mut control_rx, b"\x1b[F", "help End").await;
    assert!(frame.contains(last), "End must expose the final help line");
    let (end_offset, line_count) = help_scroll_state(&handler, attach_pid).await;
    assert!(end_offset > 0 && line_count == lines.len());

    let frame = navigate_help(
        &handler,
        attach_pid,
        &mut control_rx,
        b"\x1b[H",
        "help Home",
    )
    .await;
    assert!(
        frame.contains(first),
        "Home must expose the first help line"
    );
    assert_eq!(help_scroll_state(&handler, attach_pid).await.0, 0);

    let _ = navigate_help(&handler, attach_pid, &mut control_rx, b"j", "help vi j").await;
    assert_eq!(help_scroll_state(&handler, attach_pid).await.0, 1);
    let _ = navigate_help(&handler, attach_pid, &mut control_rx, b"k", "help vi k").await;
    assert_eq!(help_scroll_state(&handler, attach_pid).await.0, 0);
    let _ = navigate_help(
        &handler,
        attach_pid,
        &mut control_rx,
        b"\x06",
        "help emacs C-f",
    )
    .await;
    assert!(help_scroll_state(&handler, attach_pid).await.0 > 1);
    let _ = navigate_help(
        &handler,
        attach_pid,
        &mut control_rx,
        b"\x02",
        "help emacs C-b",
    )
    .await;
    assert_eq!(help_scroll_state(&handler, attach_pid).await.0, 0);
    let frame = navigate_help(&handler, attach_pid, &mut control_rx, b"G", "help vi G").await;
    assert!(frame.contains(last), "vi G must expose the final help line");
    let frame = navigate_help(&handler, attach_pid, &mut control_rx, b"g", "help vi g").await;
    assert!(
        frame.contains(first),
        "vi g must expose the first help line"
    );

    let clear = navigate_help(&handler, attach_pid, &mut control_rx, b"q", "help q close").await;
    assert!(clear.is_empty(), "q must clear the help overlay");
}

#[tokio::test]
async fn attached_help_escape_closes_without_leaking_to_the_pane() {
    let handler = RequestHandler::new();
    let attach_pid = std::process::id();
    let alpha = session_name("help-escape");
    let mut control_rx = create_attached_session(&handler, attach_pid, &alpha).await;
    drain_attach_controls(&mut control_rx);
    let _ = open_help(&handler, attach_pid, &mut control_rx).await;
    let before = capture_pane_print(&handler, PaneTarget::new(alpha, 0)).await;

    let mut pending_input = Vec::new();
    handler
        .handle_attached_live_input(attach_pid, &mut pending_input, b"\x1b")
        .await
        .expect("help Escape is retained");
    handler
        .flush_attached_pending_escape_input(attach_pid, &mut pending_input)
        .await
        .expect("help Escape flush succeeds");
    assert!(pending_input.is_empty());
    let clear = recv_overlay_frame(&mut control_rx, "help Escape close").await;
    assert!(clear.is_empty(), "Escape must clear the help overlay");
    let after = capture_pane_print(&handler, PaneTarget::new(session_name("help-escape"), 0)).await;
    assert_eq!(after, before, "Escape must not reach the underlying pane");
}

#[tokio::test]
async fn attached_help_viewports_are_independent_per_attach() {
    let handler = RequestHandler::new();
    let alpha = session_name("help-multi");
    let first_pid = 71_001;
    let second_pid = 71_002;
    let mut first_rx = create_attached_session(&handler, first_pid, &alpha).await;
    let (second_tx, mut second_rx) = mpsc::unbounded_channel();
    handler.register_attach(second_pid, alpha, second_tx).await;
    drain_attach_controls(&mut first_rx);
    drain_attach_controls(&mut second_rx);

    let _ = open_help(&handler, first_pid, &mut first_rx).await;
    let _ = open_help(&handler, second_pid, &mut second_rx).await;
    assert_eq!(help_scroll_state(&handler, first_pid).await.0, 0);
    assert_eq!(help_scroll_state(&handler, second_pid).await.0, 0);
    drain_attach_controls(&mut second_rx);

    let _ = navigate_help(
        &handler,
        first_pid,
        &mut first_rx,
        b"\x1b[F",
        "first help End",
    )
    .await;
    assert!(help_scroll_state(&handler, first_pid).await.0 > 0);
    assert_eq!(help_scroll_state(&handler, second_pid).await.0, 0);
    assert!(
        second_rx.try_recv().is_err(),
        "scrolling one help viewport must not refresh the other attach"
    );
}

#[tokio::test]
async fn attached_help_fails_closed_over_existing_overlay_and_prompt() {
    let handler = RequestHandler::new();
    let attach_pid = std::process::id();
    let alpha = session_name("help-surface-conflict");
    let mut control_rx = create_attached_session(&handler, attach_pid, &alpha).await;
    drain_attach_controls(&mut control_rx);

    let popup = handler
        .parse_command_string_one_group("display-popup -N -w 20 -h 6")
        .await
        .expect("popup parses");
    handler
        .execute_parsed_commands_for_test(attach_pid, popup)
        .await
        .expect("popup opens");
    let existing_overlay_id = {
        let active_attach = handler.active_attach.lock().await;
        active_attach.by_pid[&attach_pid]
            .overlay
            .as_ref()
            .expect("existing overlay")
            .id()
    };
    assert!(!show_help_directly(&handler, attach_pid).await);
    {
        let active_attach = handler.active_attach.lock().await;
        assert_eq!(
            active_attach.by_pid[&attach_pid]
                .overlay
                .as_ref()
                .expect("existing overlay remains")
                .id(),
            existing_overlay_id
        );
    }
    handler
        .clear_interactive_overlay(attach_pid, true)
        .await
        .expect("existing overlay closes");
    drain_attach_controls(&mut control_rx);

    handler
        .handle_attached_live_input_for_test(attach_pid, b"\x02:")
        .await
        .expect("command prompt opens");
    let prompt_before = {
        let active_attach = handler.active_attach.lock().await;
        active_attach.by_pid[&attach_pid]
            .prompt
            .as_ref()
            .map(|prompt| std::ptr::from_ref(prompt).addr())
            .expect("existing prompt")
    };
    assert!(!show_help_directly(&handler, attach_pid).await);
    let active_attach = handler.active_attach.lock().await;
    assert!(active_attach.by_pid[&attach_pid].overlay.is_none());
    let prompt_after = active_attach.by_pid[&attach_pid]
        .prompt
        .as_ref()
        .map(|prompt| std::ptr::from_ref(prompt).addr())
        .expect("existing prompt remains");
    assert_eq!(
        prompt_after, prompt_before,
        "rejected help must preserve the existing prompt"
    );
}

#[tokio::test]
async fn stale_help_close_cannot_clear_a_replacement_overlay() {
    let handler = RequestHandler::new();
    let attach_pid = std::process::id();
    let alpha = session_name("help-stale-close");
    let mut control_rx = create_attached_session(&handler, attach_pid, &alpha).await;
    drain_attach_controls(&mut control_rx);
    let _ = open_help(&handler, attach_pid, &mut control_rx).await;
    let stale_help_id = {
        let active_attach = handler.active_attach.lock().await;
        active_attach.by_pid[&attach_pid]
            .overlay
            .as_ref()
            .expect("help overlay")
            .id()
    };
    handler
        .clear_interactive_overlay(attach_pid, true)
        .await
        .expect("help overlay closes");

    let popup = handler
        .parse_command_string_one_group("display-popup -N -w 20 -h 6")
        .await
        .expect("replacement popup parses");
    handler
        .execute_parsed_commands_for_test(attach_pid, popup)
        .await
        .expect("replacement popup opens");
    let replacement_id = {
        let active_attach = handler.active_attach.lock().await;
        active_attach.by_pid[&attach_pid]
            .overlay
            .as_ref()
            .expect("replacement overlay")
            .id()
    };
    assert_ne!(replacement_id, stale_help_id);

    assert!(handler
        .handle_scrollable_popup_key_input(
            attach_pid,
            None,
            stale_help_id,
            rmux_core::KeyCode::from(b'q'),
        )
        .await
        .expect("stale help close is safely ignored"));
    let active_attach = handler.active_attach.lock().await;
    assert_eq!(
        active_attach.by_pid[&attach_pid]
            .overlay
            .as_ref()
            .expect("replacement overlay remains")
            .id(),
        replacement_id
    );
}

#[tokio::test]
async fn attached_help_rename_rekeys_and_switch_closes_instead_of_rerouting() {
    let handler = RequestHandler::new();
    let attach_pid = std::process::id();
    let alpha = session_name("help-alpha");
    let beta = session_name("help-beta");
    let gamma = session_name("help-gamma");
    let mut control_rx = create_attached_session(&handler, attach_pid, &alpha).await;
    assert!(matches!(
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: beta.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
        Response::NewSession(_)
    ));
    drain_attach_controls(&mut control_rx);

    let _ = open_help(&handler, attach_pid, &mut control_rx).await;
    let _ = navigate_help(
        &handler,
        attach_pid,
        &mut control_rx,
        b"\x1b[F",
        "help End before rename",
    )
    .await;
    let offset_before = help_scroll_state(&handler, attach_pid).await.0;
    assert!(matches!(
        handler
            .handle(Request::RenameSession(RenameSessionRequest {
                target: alpha,
                new_name: gamma.clone(),
            }))
            .await,
        Response::RenameSession(_)
    ));
    {
        let active_attach = handler.active_attach.lock().await;
        let active = &active_attach.by_pid[&attach_pid];
        let Some(ClientOverlayState::Popup(popup)) = active.overlay.as_ref() else {
            panic!("help survives same-identity rename");
        };
        assert_eq!(popup.current_target.session_name(), &gamma);
        assert_eq!(
            popup.scrollable_text.as_ref().expect("help text").offset(),
            offset_before
        );
    }

    let switched = handler
        .dispatch(
            attach_pid,
            Request::SwitchClient(SwitchClientRequest {
                target: beta.clone(),
            }),
        )
        .await
        .response;
    assert_eq!(
        switched,
        Response::SwitchClient(rmux_proto::SwitchClientResponse {
            session_name: beta.clone(),
        })
    );
    let active_attach = handler.active_attach.lock().await;
    let active = &active_attach.by_pid[&attach_pid];
    assert_eq!(active.session_name, beta);
    assert!(
        active.overlay.is_none(),
        "session switch must close stale help"
    );
}

#[tokio::test]
async fn attached_non_help_output_commands_do_not_open_the_help_surface() {
    let handler = RequestHandler::new();
    let attach_pid = std::process::id();
    let alpha = session_name("help-narrow-scope");
    let _control_rx = create_attached_session(&handler, attach_pid, &alpha).await;
    let bindings = handler
        .parse_command_string_one_group(
            "bind-key X { display-message -p ordinary-output } ; bind-key Y { list-keys }",
        )
        .await
        .expect("test bindings parse");
    handler
        .execute_parsed_commands_for_test(attach_pid, bindings)
        .await
        .expect("test bindings install");

    for input in [b"\x02X".as_slice(), b"\x02Y".as_slice()] {
        handler
            .handle_attached_live_input_for_test(attach_pid, input)
            .await
            .expect("non-help binding executes");
        let active_attach = handler.active_attach.lock().await;
        assert!(
            active_attach.by_pid[&attach_pid].overlay.is_none(),
            "only list-keys -N may open the attached help surface"
        );
    }
}
