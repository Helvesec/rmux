use super::*;
#[cfg(windows)]
use crate::input_keys::{encode_key, ExtendedKeyFormat};
#[cfg(windows)]
use rmux_core::key_string_lookup_string;

#[cfg(windows)]
const ATTACHED_EXIT_INPUT: &[u8] = b"RMUX_EXIT\r\n";
#[cfg(not(windows))]
const ATTACHED_EXIT_INPUT: &[u8] = b"exit\r";

#[cfg(windows)]
const SUBMITTED_EXIT_LINE_NEEDLE: &str = "RMUX_EXIT";
#[cfg(not(windows))]
const SUBMITTED_EXIT_LINE_NEEDLE: &str = "exit";

const REMAIN_ON_EXIT_CAPTURE_SETTLE_TIMEOUT: Duration = Duration::from_secs(20);

#[tokio::test]
async fn attached_remain_on_exit_strips_the_submitted_exit_line_from_dead_pane_capture() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let mut control_rx = create_exit_attached_session(&handler, requester_pid, &alpha).await;
    let target = PaneTarget::new(alpha.clone(), 0);

    assert!(matches!(
        handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Pane(target.clone()),
                option: OptionName::RemainOnExit,
                value: "on".to_owned(),
                mode: SetOptionMode::Replace,
            }))
            .await,
        Response::SetOption(_)
    ));
    prepare_exit_prompt(&handler, &target).await;
    handler
        .active_attach
        .lock()
        .await
        .by_pid
        .get_mut(&requester_pid)
        .expect("attached client remains registered")
        .terminal_context = OuterTerminalContext::from_pairs(&[("TERM", "xterm-256color")]);
    drain_attach_controls(&mut control_rx);
    {
        let mut state = handler.state.lock().await;
        state
            .append_bytes_to_pane_transcript_for_test(&alpha, 0, 0, b"\x1b[?1003h\x1b[?1006h")
            .expect("live pane enables all-motion mouse tracking");
    }
    handler.refresh_attached_session(&alpha).await;
    let tracking_target = recv_switch_target(&mut control_rx, "live pane mouse tracking").await;
    let tracking_start = tracking_target.outer_terminal.attach_start_sequence();
    assert!(
        tracking_start
            .windows(b"\x1b[?1003h".len())
            .any(|window| window == b"\x1b[?1003h"),
        "live pane tracking must reach the outer terminal"
    );

    handler
        .handle_attached_live_input_for_test(requester_pid, ATTACHED_EXIT_INPUT)
        .await
        .expect("attached exit input");
    wait_for_dead_pane(&handler, &alpha, 0, 0).await;

    let mouse_tracking_enables = [
        b"\x1b[?1000h".as_slice(),
        b"\x1b[?1002h".as_slice(),
        b"\x1b[?1003h".as_slice(),
    ];
    let dead_target = take_switch_target(
        recv_matching_attach_control(&mut control_rx, "kept-dead pane refresh", |control| {
            let AttachControl::Switch(target) = control else {
                return false;
            };
            target
                .with_target(|target| {
                    let start = target.outer_terminal.attach_start_sequence();
                    mouse_tracking_enables
                        .iter()
                        .all(|enable| !start.windows(enable.len()).any(|window| window == *enable))
                })
                .unwrap_or(false)
        })
        .await,
    );
    let dead_start = dead_target.outer_terminal.attach_start_sequence();
    for enable in mouse_tracking_enables {
        assert!(
            !dead_start
                .windows(enable.len())
                .any(|window| window == enable),
            "kept-dead pane must not re-enable outer mouse tracking: {dead_start:?}"
        );
    }
    let transition = dead_target
        .outer_terminal
        .transition_sequence_from(&tracking_target.outer_terminal);
    for disable in [
        b"\x1b[?1003l".as_slice(),
        b"\x1b[?1002l",
        b"\x1b[?1000l",
        b"\x1b[?1006l",
    ] {
        assert!(
            transition
                .windows(disable.len())
                .any(|window| window == disable),
            "kept-dead pane refresh must disable outer mouse tracking: {transition:?}"
        );
    }

    let deadline = tokio::time::Instant::now() + REMAIN_ON_EXIT_CAPTURE_SETTLE_TIMEOUT;
    let capture = loop {
        let capture = capture_pane_print(&handler, target.clone()).await;
        if capture.contains("Pane is dead") && !capture.contains(SUBMITTED_EXIT_LINE_NEEDLE) {
            break capture;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "attached remain-on-exit capture did not settle, got {capture:?}"
        );
        sleep(Duration::from_millis(20)).await;
    };
    assert!(
        !capture.contains(SUBMITTED_EXIT_LINE_NEEDLE),
        "attached remain-on-exit capture must not keep the submitted exit line, got {capture:?}"
    );
    assert!(
        capture.contains("Pane is dead"),
        "dead pane capture should include remain-on-exit status, got {capture:?}"
    );
}

#[tokio::test]
async fn attached_display_message_print_reports_client_size_and_cursor_position() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let _control_rx = create_attached_session(&handler, requester_pid, &alpha).await;
    let target = PaneTarget::new(alpha.clone(), 0);

    replace_transcript_contents(
        &handler,
        &target,
        TerminalSize { cols: 80, rows: 23 },
        b"PROMPT> ",
    )
    .await;

    let response = handler
        .handle(Request::DisplayMessage(rmux_proto::DisplayMessageRequest {
            target: None,
            print: true,
            message: Some(
                "#{client_width}x#{client_height}|#{cursor_x}|#{cursor_y}|#{session_width}x#{session_height}|#{pane_width}x#{pane_height}"
                    .to_owned(),
            ),
            empty_target_context: false,
            }))
        .await;
    let Response::DisplayMessage(response) = response else {
        panic!("expected display-message response");
    };
    let output = response
        .command_output()
        .expect("display-message -p returns output");
    assert_eq!(output.stdout(), b"80x24|8|0|80x23|80x23\n");
}

#[tokio::test]
async fn attached_exit_on_last_pane_closes_the_session_and_client() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let mut control_rx = create_exit_attached_session(&handler, requester_pid, &alpha).await;
    let target = PaneTarget::new(alpha.clone(), 0);

    prepare_exit_prompt(&handler, &target).await;
    drain_attach_controls(&mut control_rx);

    handler
        .handle_attached_live_input_for_test(requester_pid, ATTACHED_EXIT_INPUT)
        .await
        .expect("attached exit input");

    tokio::time::timeout(ATTACH_LIFECYCLE_TIMEOUT, async {
        loop {
            match control_rx.recv().await {
                Some(AttachControl::Exited) => break,
                Some(_) => {}
                None => panic!("attach control channel closed before exit notification"),
            }
        }
    })
    .await
    .expect("timed out waiting for attach exit notification");
    wait_for_session_removed(&handler, &alpha).await;
}

#[tokio::test]
async fn attached_last_pane_exit_honors_detach_on_destroy_off() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let beta = session_name("pane-exit-destroy-beta");
    let alpha = session_name("pane-exit-destroy-alpha");
    create_quiet_session(&handler, &beta).await;
    let mut control_rx = create_exit_attached_session(&handler, requester_pid, &alpha).await;
    let target = PaneTarget::new(alpha.clone(), 0);
    let response = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Session(alpha.clone()),
            option: OptionName::DetachOnDestroy,
            value: "off".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(response, Response::SetOption(_)), "{response:?}");
    prepare_exit_prompt(&handler, &target).await;
    drain_attach_controls(&mut control_rx);

    handler
        .handle_attached_live_input_for_test(requester_pid, ATTACHED_EXIT_INPUT)
        .await
        .expect("attached exit input");

    let switched = recv_matching_attach_control(
        &mut control_rx,
        "last-pane detach-on-destroy switch",
        |control| matches!(control, AttachControl::Switch(_)),
    )
    .await;
    assert_eq!(take_switch_target(switched).session_name, beta);
    wait_for_session_removed(&handler, &alpha).await;
    let active_attach = handler.active_attach.lock().await;
    let active = active_attach
        .by_pid
        .get(&requester_pid)
        .expect("pane-exit switch preserves attached client");
    assert_eq!(active.session_name, beta);
    assert!(!active.closing.load(Ordering::SeqCst));
}

#[cfg(any(unix, windows))]
async fn create_exit_attached_session(
    handler: &RequestHandler,
    requester_pid: u32,
    session: &SessionName,
) -> mpsc::UnboundedReceiver<AttachControl> {
    create_line_exiting_attached_session(handler, requester_pid, session).await
}

#[cfg(not(any(unix, windows)))]
async fn create_exit_attached_session(
    handler: &RequestHandler,
    requester_pid: u32,
    session: &SessionName,
) -> mpsc::UnboundedReceiver<AttachControl> {
    create_attached_session(handler, requester_pid, session).await
}

#[cfg(any(unix, windows))]
async fn prepare_exit_prompt(_handler: &RequestHandler, _target: &PaneTarget) {}

#[cfg(not(any(unix, windows)))]
async fn prepare_exit_prompt(handler: &RequestHandler, target: &PaneTarget) {
    prepare_attached_shell_prompt(handler, target).await;
}

#[tokio::test]
async fn attached_keystroke_stub_returns_key_dispatched_ack() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let _control_rx = create_attached_session(&handler, requester_pid, &alpha).await;

    let response = handler
        .handle_attached_keystroke(
            requester_pid,
            &AttachedKeystroke::new(b"\x1b[A".to_vec()),
            true,
        )
        .await
        .expect("typed keystroke should reach test handler");

    assert_eq!(response, KeyDispatched::new(3));
}

#[tokio::test]
async fn attached_keystroke_forwarded_ack_reports_not_consumed() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let _control_rx = create_attached_session(&handler, requester_pid, &alpha).await;

    let response = handler
        .handle_attached_keystroke(requester_pid, &AttachedKeystroke::new(b"a".to_vec()), false)
        .await
        .expect("forwarded keystroke should acknowledge");

    assert_eq!(response, KeyDispatched::forwarded(1));
    assert!(!response.consumed());
}

#[tokio::test]
async fn attached_prefix_key_activates_prefix_table() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let _control_rx = create_attached_session(&handler, requester_pid, &alpha).await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02")
        .await
        .expect("prefix key input");

    let active_attach = handler.active_attach.lock().await;
    assert_eq!(
        active_attach
            .by_pid
            .get(&requester_pid)
            .and_then(|active| active.key_table_name.as_deref()),
        Some("prefix")
    );
}

#[tokio::test]
async fn attached_control_space_prefix_activates_prefix_table() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let _control_rx = create_attached_session(&handler, requester_pid, &alpha).await;
    assert!(matches!(
        handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Session(alpha.clone()),
                option: OptionName::Prefix,
                value: "C-Space".to_owned(),
                mode: SetOptionMode::Replace,
            }))
            .await,
        Response::SetOption(_)
    ));

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x00")
        .await
        .expect("C-Space prefix input");

    let active_attach = handler.active_attach.lock().await;
    assert_eq!(
        active_attach
            .by_pid
            .get(&requester_pid)
            .and_then(|active| active.key_table_name.as_deref()),
        Some("prefix")
    );
}

#[tokio::test]
async fn attached_control_space_prefix_c_creates_window() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let _control_rx = create_attached_session(&handler, requester_pid, &alpha).await;
    assert!(matches!(
        handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Session(alpha.clone()),
                option: OptionName::Prefix,
                value: "C-Space".to_owned(),
                mode: SetOptionMode::Replace,
            }))
            .await,
        Response::SetOption(_)
    ));

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x00c")
        .await
        .expect("C-Space c prefix input");

    assert_eq!(
        active_windows(&handler, &alpha).await,
        "0:0\n1:1\n",
        "C-Space c must dispatch the prefix table's new-window binding"
    );
}

#[tokio::test]
async fn attached_printable_space_prefix_c_creates_window() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let _control_rx = create_attached_session(&handler, requester_pid, &alpha).await;
    assert!(matches!(
        handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Session(alpha.clone()),
                option: OptionName::Prefix,
                value: "Space".to_owned(),
                mode: SetOptionMode::Replace,
            }))
            .await,
        Response::SetOption(_)
    ));

    handler
        .handle_attached_live_input_for_test(requester_pid, b" c")
        .await
        .expect("Space c prefix input");

    assert_eq!(
        active_windows(&handler, &alpha).await,
        "0:0\n1:1\n",
        "Space c must dispatch the prefix table's new-window binding"
    );
}

#[tokio::test]
async fn attached_prefix_prefix_dispatches_send_prefix_once_and_returns_to_root() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let _control_rx = create_attached_session(&handler, requester_pid, &alpha).await;
    let capture = RawPaneInputProbe::start(&handler, &alpha, "attached-prefix-default", 2).await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02\x02x")
        .await
        .expect("prefix send-prefix input");

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, b"\x02x").await;
    let active_attach = handler.active_attach.lock().await;
    assert_eq!(
        active_attach
            .by_pid
            .get(&requester_pid)
            .and_then(|active| active.key_table_name.as_deref()),
        None
    );
}

#[tokio::test]
async fn attached_send_prefix_emits_the_configured_prefix_byte() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let _control_rx = create_attached_session(&handler, requester_pid, &alpha).await;
    assert!(matches!(
        handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Session(alpha.clone()),
                option: OptionName::Prefix,
                value: "C-a".to_owned(),
                mode: SetOptionMode::Replace,
            }))
            .await,
        Response::SetOption(_)
    ));
    let capture = RawPaneInputProbe::start(&handler, &alpha, "attached-prefix-configured", 1).await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x01\x02")
        .await
        .expect("configured prefix send-prefix input");

    capture.finish(&handler, &alpha).await;
    #[cfg(not(windows))]
    let expected = b"\x01".to_vec();
    #[cfg(windows)]
    let expected = windows_cmd_select_all_bytes();
    capture.assert_contents(&handler, &expected).await;
}

#[cfg(windows)]
fn windows_cmd_select_all_bytes() -> Vec<u8> {
    ["C-Home", "S-End"]
        .into_iter()
        .flat_map(|key_name| {
            let key = key_string_lookup_string(key_name).expect("test key must exist");
            encode_key(0, ExtendedKeyFormat::Xterm, key).expect("test key must encode")
        })
        .collect()
}

#[tokio::test]
async fn attached_live_input_preserves_split_utf8_sequences() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    #[cfg(windows)]
    let _control_rx = create_line_echo_attached_session(&handler, requester_pid, &alpha).await;
    #[cfg(not(windows))]
    let _control_rx = create_attached_session(&handler, requester_pid, &alpha).await;
    let target = PaneTarget::new(alpha.clone(), 0);
    #[cfg(not(windows))]
    prepare_attached_shell_prompt(&handler, &target).await;

    let mut pending_input = Vec::new();
    let command = split_utf8_echo_command();
    for (index, chunk) in command.chunks.iter().enumerate() {
        handler
            .handle_attached_live_input(requester_pid, &mut pending_input, chunk)
            .await
            .unwrap_or_else(|error| panic!("utf-8 fragment {index} failed: {error}"));
    }
    let capture = wait_for_capture_containing(
        &handler,
        target,
        command.output_needle,
        "attached input must preserve the split utf-8 output",
    )
    .await;
    if let Some(echoed_command) = command.echoed_command {
        assert!(
            capture.contains(echoed_command),
            "attached input must preserve the split utf-8 command text, got {capture:?}"
        );
    }
}

struct SplitUtf8EchoCommand {
    chunks: Vec<&'static [u8]>,
    output_needle: &'static str,
    echoed_command: Option<&'static str>,
}

#[cfg(unix)]
fn split_utf8_echo_command() -> SplitUtf8EchoCommand {
    SplitUtf8EchoCommand {
        chunks: vec![b"printf 'cafe \xe6", b"\x96", b"\x87\\n'\r"],
        output_needle: "\ncafe 文",
        echoed_command: Some("printf 'cafe 文\\n'"),
    }
}

#[cfg(windows)]
fn split_utf8_echo_command() -> SplitUtf8EchoCommand {
    SplitUtf8EchoCommand {
        chunks: vec![b"cafe \xe6", b"\x96", b"\x87\r\n"],
        output_needle: "cafe 文",
        echoed_command: None,
    }
}
