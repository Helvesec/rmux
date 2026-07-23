use super::*;
use crate::handler::QueuedLifecycleEvent;
use rmux_core::LifecycleEvent;

const LONG_PREFIX_CHAIN_REPETITIONS: usize = 8_192;
const LARGE_FOCUS_CHAIN_REPETITIONS: usize = 4_096;
const PROMPT_CANCEL_CHAIN_REPETITIONS: usize = 512;
// These stress cases execute one real binding command per repetition in a
// debug test binary. Keep the 8K recursion regression load and give parallel
// nextest runs headroom without removing the finite completion bound.
const LONG_PREFIX_CHAIN_TIMEOUT: Duration = if cfg!(windows) {
    Duration::from_secs(120)
} else {
    Duration::from_secs(60)
};
const ITERATIVE_INPUT_CHAIN_TIMEOUT: Duration = if cfg!(windows) {
    Duration::from_secs(60)
} else {
    Duration::from_secs(30)
};
const BOUNDED_REROUTE_CHAIN_TIMEOUT: Duration = Duration::from_secs(30);
const BACKGROUND_RUN_SHELL_TIMEOUT: Duration = if cfg!(windows) {
    // A cold PowerShell process can exceed ten seconds on hosted Windows CI.
    Duration::from_secs(30)
} else {
    Duration::from_secs(10)
};

async fn set_global_hook(handler: &RequestHandler, hook: HookName, command: &str) {
    let response = handler
        .handle(Request::SetHookMutation(SetHookMutationRequest {
            scope: ScopeSelector::Global,
            hook,
            command: Some(command.to_owned()),
            lifecycle: HookLifecycle::Persistent,
            append: false,
            unset: false,
            run_immediately: false,
            index: None,
        }))
        .await;
    assert!(matches!(response, Response::SetHook(_)), "{response:?}");
}

async fn wait_for_buffer(handler: &RequestHandler, name: &str, expected: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        {
            let state = handler.state.lock().await;
            if let Ok((_, content)) = state.buffers.show(Some(name)) {
                if String::from_utf8_lossy(content) == expected {
                    return;
                }
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "buffer {name} did not reach {expected:?}"
        );
        sleep(Duration::from_millis(10)).await;
    }
}

async fn drain_lifecycle_hooks(
    handler: &RequestHandler,
    events: &mut tokio::sync::broadcast::Receiver<QueuedLifecycleEvent>,
) {
    loop {
        match events.try_recv() {
            Ok(event) => handler.dispatch_lifecycle_hook(event).await,
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
            | Err(tokio::sync::broadcast::error::TryRecvError::Closed) => break,
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(skipped)) => {
                panic!("lifecycle events lagged during test: {skipped}");
            }
        }
    }
}

async fn enable_mouse(handler: &RequestHandler) {
    let response = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::Mouse,
            value: "on".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(response, Response::SetOption(_)));
}

#[tokio::test]
async fn send_keys_uses_runtime_extended_key_format_for_mode_two() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");

    create_send_keys_test_session(&handler, &alpha).await;

    let set_format = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::ExtendedKeysFormat,
            value: "csi-u".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(set_format, Response::SetOption(_)));

    {
        let mut state = handler.state.lock().await;
        state
            .append_bytes_to_pane_transcript_for_test(&alpha, 0, 0, b"\x1b[>4;2m")
            .expect("mode 2 transcript update");
    }

    let expected = encode_key(
        mode::MODE_KEYS_EXTENDED_2,
        ExtendedKeyFormat::CsiU,
        key_string_lookup_string("M-C-a").expect("key parses"),
    )
    .expect("extended key encodes");
    let capture = RawPaneInputProbe::start(&handler, &alpha, "extended-key", expected.len()).await;

    let response = handler
        .handle(Request::SendKeysExt(SendKeysExtRequest {
            target: Some(PaneTarget::new(alpha.clone(), 0)),
            keys: vec!["M-C-a".to_owned()],
            expand_formats: false,
            hex: false,
            literal: false,
            dispatch_key_table: false,
            copy_mode_command: false,
            forward_mouse_event: false,
            reset_terminal: false,
            repeat_count: None,
        }))
        .await;
    assert_eq!(
        response,
        Response::SendKeys(SendKeysResponse { key_count: 1 })
    );

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, &expected).await;
}

#[tokio::test]
async fn send_keys_sends_modified_cursor_keys_without_extended_mode() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");

    create_send_keys_test_session(&handler, &alpha).await;

    let expected = b"\x1b[1;5A";
    let capture =
        RawPaneInputProbe::start(&handler, &alpha, "send-keys-c-up", expected.len()).await;
    let response = handler
        .handle(Request::SendKeys(SendKeysRequest {
            target: PaneTarget::new(alpha.clone(), 0),
            keys: vec!["C-Up".to_owned()],
        }))
        .await;
    assert_eq!(
        response,
        Response::SendKeys(SendKeysResponse { key_count: 1 })
    );

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, expected).await;
}

#[cfg(windows)]
#[tokio::test]
async fn live_attach_ctrl_a_emulates_cmd_select_all() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    {
        let mut state = handler.state.lock().await;
        state
            .options
            .set(
                ScopeSelector::Global,
                OptionName::DefaultShell,
                "cmd.exe".to_owned(),
                SetOptionMode::Replace,
            )
            .expect("test default-shell is valid");
    }
    create_send_keys_test_session(&handler, &alpha).await;

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let mut expected = encode_key(
        0,
        ExtendedKeyFormat::Xterm,
        key_string_lookup_string("C-Home").expect("C-Home parses"),
    )
    .expect("C-Home encodes");
    expected.extend_from_slice(
        &encode_key(
            0,
            ExtendedKeyFormat::Xterm,
            key_string_lookup_string("S-End").expect("S-End parses"),
        )
        .expect("S-End encodes"),
    );
    let capture =
        RawPaneInputProbe::start(&handler, &alpha, "live-attach-cmd-c-a", expected.len()).await;

    let mut pending_input = Vec::new();
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"\x01")
        .await
        .expect("Ctrl+A attached input succeeds");

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, &expected).await;
}

#[cfg(windows)]
#[tokio::test]
async fn live_attach_ctrl_d_uses_windows_console_key_path() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    let capture = RawPaneInputProbe::start(&handler, &alpha, "live-attach-c-d", 1).await;

    let mut pending_input = Vec::new();
    let keystroke = rmux_proto::AttachedKeystroke::new(vec![0x04]).with_windows_console_key(
        rmux_proto::AttachedWindowsConsoleKey::new(0x44, 0x20, 0x04, 0x0008, 1),
    );
    let forwarded = handler
        .handle_attached_keystroke_input(requester_pid, &mut pending_input, &keystroke)
        .await
        .expect("Ctrl+D attached input succeeds");

    assert!(forwarded);
    assert!(pending_input.is_empty());
    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, &[0x04]).await;
}

#[cfg(windows)]
#[tokio::test]
async fn live_attach_unbound_ctrl_p_uses_windows_console_key_path() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    let capture = RawPaneInputProbe::start(&handler, &alpha, "live-attach-c-p", 1).await;

    let mut pending_input = Vec::new();
    let keystroke = rmux_proto::AttachedKeystroke::new(vec![0x10]).with_windows_console_key(
        rmux_proto::AttachedWindowsConsoleKey::new(0x50, 0x19, 0x10, 0x0008, 1),
    );
    let forwarded = handler
        .handle_attached_keystroke_input(requester_pid, &mut pending_input, &keystroke)
        .await
        .expect("Ctrl+P attached input succeeds");

    assert!(forwarded);
    assert!(pending_input.is_empty());
    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, &[0x10]).await;
}

#[cfg(windows)]
#[tokio::test]
async fn live_attach_prefix_ctrl_b_is_not_forwarded_as_windows_console_key() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    let capture = RawPaneInputProbe::start(&handler, &alpha, "live-attach-c-b", 0).await;

    let mut pending_input = Vec::new();
    let keystroke = rmux_proto::AttachedKeystroke::new(vec![0x02]).with_windows_console_key(
        rmux_proto::AttachedWindowsConsoleKey::new(0x42, 0x30, 0x02, 0x0008, 1),
    );
    let forwarded = handler
        .handle_attached_keystroke_input(requester_pid, &mut pending_input, &keystroke)
        .await
        .expect("Ctrl+B attached input succeeds");

    assert!(!forwarded);
    assert!(pending_input.is_empty());
    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, &[]).await;
}

#[cfg(windows)]
#[tokio::test]
async fn live_attach_windows_console_ctrl_semicolon_dispatches_root_binding() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    let rebound = handler
        .handle(Request::BindKey(Box::new(BindKeyRequest {
            table_name: "root".to_owned(),
            key: "C-;".to_owned(),
            note: Some("live-attach-ctrl-semicolon".to_owned()),
            repeat: false,
            command: Some(vec![
                "send-keys".to_owned(),
                "-l".to_owned(),
                "R".to_owned(),
            ]),
        })))
        .await;
    assert!(matches!(rebound, Response::BindKey(_)));

    let capture =
        RawPaneInputProbe::start(&handler, &alpha, "live-attach-c-semicolon-root", 1).await;
    let mut pending_input = Vec::new();
    let keystroke = rmux_proto::AttachedKeystroke::new(b";".to_vec()).with_windows_console_key(
        rmux_proto::AttachedWindowsConsoleKey::new(0xba, 0x27, b';' as u16, 0x0008, 1),
    );
    let forwarded = handler
        .handle_attached_keystroke_input(requester_pid, &mut pending_input, &keystroke)
        .await
        .expect("Ctrl+; attached input succeeds");

    assert!(!forwarded);
    assert!(pending_input.is_empty());
    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, b"R").await;
}

#[cfg(windows)]
#[tokio::test]
async fn live_attach_windows_console_ctrl_semicolon_enters_prefix_table() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;

    let set_prefix = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::Prefix,
            value: "C-;".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(set_prefix, Response::SetOption(_)));

    let rebound = handler
        .handle(Request::BindKey(Box::new(BindKeyRequest {
            table_name: "prefix".to_owned(),
            key: "X".to_owned(),
            note: Some("live-attach-ctrl-semicolon-prefix".to_owned()),
            repeat: false,
            command: Some(vec![
                "send-keys".to_owned(),
                "-l".to_owned(),
                "P".to_owned(),
            ]),
        })))
        .await;
    assert!(matches!(rebound, Response::BindKey(_)));

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    let capture =
        RawPaneInputProbe::start(&handler, &alpha, "live-attach-c-semicolon-prefix", 1).await;

    let mut pending_input = Vec::new();
    let keystroke = rmux_proto::AttachedKeystroke::new(b";".to_vec()).with_windows_console_key(
        rmux_proto::AttachedWindowsConsoleKey::new(0xba, 0x27, b';' as u16, 0x0008, 1),
    );
    let forwarded = handler
        .handle_attached_keystroke_input(requester_pid, &mut pending_input, &keystroke)
        .await
        .expect("Ctrl+; attached input succeeds");
    assert!(!forwarded);

    handler
        .handle_attached_live_input_for_test(requester_pid, b"X")
        .await
        .expect("prefix X dispatches after Ctrl+;");

    assert!(pending_input.is_empty());
    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, b"P").await;
}

#[tokio::test]
async fn live_attach_csi_u_ctrl_semicolon_enters_prefix_table() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;

    let set_prefix = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::Prefix,
            value: "C-;".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(set_prefix, Response::SetOption(_)));

    let rebound = handler
        .handle(Request::BindKey(Box::new(BindKeyRequest {
            table_name: "prefix".to_owned(),
            key: "X".to_owned(),
            note: Some("live-attach-csi-u-ctrl-semicolon".to_owned()),
            repeat: false,
            command: Some(vec![
                "send-keys".to_owned(),
                "-l".to_owned(),
                "U".to_owned(),
            ]),
        })))
        .await;
    assert!(matches!(rebound, Response::BindKey(_)));

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    let capture =
        RawPaneInputProbe::start(&handler, &alpha, "live-attach-csi-u-c-semicolon-prefix", 1).await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x1b[59;5uX")
        .await
        .expect("CSI-u Ctrl+; prefix dispatches");

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, b"U").await;
}

async fn assert_prefix_chunks(
    label: &str,
    prefix: &str,
    binding_key: &str,
    chunks: &[&[u8]],
    expected_pane_input: &[u8],
) {
    assert_prefix_option_chunks(
        label,
        OptionName::Prefix,
        prefix,
        binding_key,
        chunks,
        expected_pane_input,
    )
    .await;
}

async fn assert_prefix_option_chunks(
    label: &str,
    option: OptionName,
    prefix: &str,
    binding_key: &str,
    chunks: &[&[u8]],
    expected_pane_input: &[u8],
) {
    let handler = RequestHandler::new();
    let alpha = session_name(label);
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    let set_prefix = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Session(alpha.clone()),
            option,
            value: prefix.to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(set_prefix, Response::SetOption(_)));
    let rebound = handler
        .handle(Request::BindKey(Box::new(BindKeyRequest {
            table_name: "prefix".to_owned(),
            key: binding_key.to_owned(),
            note: Some("printable prefix chunking".to_owned()),
            repeat: false,
            command: Some(vec![
                "set-buffer".to_owned(),
                "-b".to_owned(),
                "printable-prefix-hit".to_owned(),
                "yes".to_owned(),
            ]),
        })))
        .await;
    assert!(matches!(rebound, Response::BindKey(_)));

    let (control_tx, control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    let control_drain =
        spawn_accounted_attach_control_drain(&handler, requester_pid, control_rx).await;
    let capture =
        RawPaneInputProbe::start(&handler, &alpha, label, expected_pane_input.len()).await;
    let mut pending_input = Vec::new();
    for chunk in chunks {
        handler
            .handle_attached_live_input(requester_pid, &mut pending_input, chunk)
            .await
            .expect("printable prefix input succeeds");
    }
    assert!(pending_input.is_empty());

    wait_for_buffer(&handler, "printable-prefix-hit", "yes").await;
    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, expected_pane_input).await;
    control_drain.abort();
}

#[tokio::test]
async fn live_attach_printable_prefix_dispatch_is_chunk_boundary_independent() {
    assert_prefix_chunks(
        "printable-prefix-coalesced",
        "x",
        "y",
        &[b"axy".as_slice()],
        b"a",
    )
    .await;
    assert_prefix_chunks(
        "printable-prefix-segmented",
        "x",
        "y",
        &[b"a".as_slice(), b"xy".as_slice()],
        b"a",
    )
    .await;
}

#[tokio::test]
async fn live_attach_unicode_prefix_dispatch_is_chunk_boundary_independent() {
    assert_prefix_chunks(
        "unicode-prefix-coalesced",
        "é",
        "x",
        &[b"a\xc3\xa9x".as_slice()],
        b"a",
    )
    .await;
    assert_prefix_chunks(
        "unicode-prefix-segmented",
        "é",
        "x",
        &[b"a\xc3".as_slice(), b"\xa9x".as_slice()],
        b"a",
    )
    .await;
}

async fn assert_unbound_meta_unicode_does_not_activate_plain_prefix(label: &str, chunks: &[&[u8]]) {
    let handler = RequestHandler::new();
    let alpha = session_name(label);
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    let set_prefix = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Session(alpha.clone()),
            option: OptionName::Prefix,
            value: "é".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(set_prefix, Response::SetOption(_)));

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    let expected = b"\x1b\xc3\xa9";
    let capture = RawPaneInputProbe::start(&handler, &alpha, label, expected.len()).await;
    let mut pending_input = Vec::new();
    for chunk in chunks {
        handler
            .handle_attached_live_input(requester_pid, &mut pending_input, chunk)
            .await
            .expect("unbound Meta-Unicode input succeeds");
    }
    assert!(pending_input.is_empty());
    {
        let active_attach = handler.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get(&requester_pid)
            .expect("attached client remains active");
        assert_eq!(active.key_table_name, None, "{label}");
    }

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, expected).await;
}

#[tokio::test]
async fn live_attach_meta_unicode_does_not_alias_plain_unicode_prefix() {
    assert_unbound_meta_unicode_does_not_activate_plain_prefix(
        "meta-unicode-vs-plain-coalesced",
        &[b"\x1b\xc3\xa9".as_slice()],
    )
    .await;
    assert_unbound_meta_unicode_does_not_activate_plain_prefix(
        "meta-unicode-vs-plain-fragmented",
        &[b"\x1b".as_slice(), b"\xc3".as_slice(), b"\xa9".as_slice()],
    )
    .await;
}

#[tokio::test]
async fn live_attach_meta_unicode_prefix_dispatch_is_chunk_boundary_independent() {
    assert_prefix_chunks(
        "meta-unicode-prefix-coalesced",
        "M-é",
        "x",
        &[b"\x1b\xc3\xa9x".as_slice()],
        b"",
    )
    .await;
    assert_prefix_chunks(
        "meta-unicode-prefix-fragmented",
        "M-é",
        "x",
        &[
            b"\x1b".as_slice(),
            b"\xc3".as_slice(),
            b"\xa9".as_slice(),
            b"x".as_slice(),
        ],
        b"",
    )
    .await;
}

#[tokio::test]
async fn live_attach_meta_unicode_prefix2_dispatches() {
    assert_prefix_option_chunks(
        "meta-unicode-prefix2",
        OptionName::Prefix2,
        "M-é",
        "x",
        &[b"\x1b\xc3\xa9x".as_slice()],
        b"",
    )
    .await;
}

#[tokio::test]
async fn live_attach_active_prefix_table_keeps_large_reroute_chain_bounded() {
    let input = b"yx".repeat(LONG_PREFIX_CHAIN_REPETITIONS);
    tokio::time::timeout(
        LONG_PREFIX_CHAIN_TIMEOUT,
        assert_prefix_chunks(
            "printable-prefix-active-large",
            "x",
            "y",
            &[b"x".as_slice(), input.as_slice()],
            b"",
        ),
    )
    .await
    .expect("active prefix table must keep large reroute work bounded");
}

#[tokio::test]
async fn live_attach_long_prefix_chain_is_processed_iteratively() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;

    let rebound = handler
        .handle(Request::BindKey(Box::new(BindKeyRequest {
            table_name: "prefix".to_owned(),
            key: "X".to_owned(),
            note: Some("live-attach-long-prefix-chain".to_owned()),
            repeat: false,
            command: Some(vec![
                "set-buffer".to_owned(),
                "-b".to_owned(),
                "long-prefix-chain".to_owned(),
                "ok".to_owned(),
            ]),
        })))
        .await;
    assert!(matches!(rebound, Response::BindKey(_)));

    let (control_tx, control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    let control_drain =
        spawn_accounted_attach_control_drain(&handler, requester_pid, control_rx).await;
    let input = b"\x02X".repeat(LONG_PREFIX_CHAIN_REPETITIONS);

    tokio::time::timeout(
        LONG_PREFIX_CHAIN_TIMEOUT,
        handler.handle_attached_live_input_for_test(requester_pid, &input),
    )
    .await
    .expect("long prefix chain must finish without recursive growth")
    .expect("long prefix chain dispatch succeeds");

    control_drain.abort();
    let state = handler.state.lock().await;
    assert_eq!(
        state.buffers.get("long-prefix-chain"),
        Some(b"ok".as_slice())
    );
}

#[tokio::test]
async fn live_attach_prompt_cancel_chain_is_processed_iteratively() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;

    let rebound = handler
        .handle(Request::BindKey(Box::new(BindKeyRequest {
            table_name: "prefix".to_owned(),
            key: "X".to_owned(),
            note: Some("live-attach-prompt-cancel-chain".to_owned()),
            repeat: false,
            command: Some(vec![
                "set-buffer".to_owned(),
                "-b".to_owned(),
                "prompt-cancel-chain".to_owned(),
                "ok".to_owned(),
            ]),
        })))
        .await;
    assert!(matches!(rebound, Response::BindKey(_)));

    let (control_tx, control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    let control_drain =
        spawn_accounted_attach_control_drain(&handler, requester_pid, control_rx).await;
    let mut input = b"\x02:\x1b".repeat(PROMPT_CANCEL_CHAIN_REPETITIONS);
    input.extend_from_slice(b"\x02X");

    tokio::time::timeout(
        ITERATIVE_INPUT_CHAIN_TIMEOUT,
        handler.handle_attached_live_input_for_test(requester_pid, &input),
    )
    .await
    .expect("prompt cancel chain must finish without recursive growth")
    .expect("prompt cancel chain dispatch succeeds");

    assert!(!handler.prompt_active(requester_pid).await);
    control_drain.abort();
    let state = handler.state.lock().await;
    assert_eq!(
        state.buffers.get("prompt-cancel-chain"),
        Some(b"ok".as_slice())
    );
}

#[tokio::test]
async fn live_attach_menu_close_reroutes_same_chunk_tail_to_pane() {
    let handler = RequestHandler::new();
    let alpha = session_name("menu-tail");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    let parsed = handler
        .parse_control_commands(r#"display-menu -T Menu "Item" "i" "display-message selected""#)
        .await
        .expect("display-menu parses");
    handler
        .execute_parsed_commands_for_test(requester_pid, parsed)
        .await
        .expect("display-menu executes");

    let expected = b"TAIL\n";
    let capture = RawPaneInputProbe::start(&handler, &alpha, "menu-tail", expected.len()).await;
    let mut pending_input = Vec::new();
    let forwarded = handler
        .handle_attached_live_input_inner(requester_pid, &mut pending_input, b"qTAIL\n")
        .await
        .expect("menu close and pane input succeed");

    assert!(forwarded);
    assert!(pending_input.is_empty());
    assert!(!handler.overlay_active(requester_pid).await);
    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, expected).await;
}

#[tokio::test]
async fn live_attach_menu_waits_for_fragmented_arrow_key() {
    let handler = RequestHandler::new();
    let alpha = session_name("menu-arrow");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    let parsed = handler
        .parse_control_commands(r#"display-menu -T Menu "Item" "i" "display-message selected""#)
        .await
        .expect("display-menu parses");
    handler
        .execute_parsed_commands_for_test(requester_pid, parsed)
        .await
        .expect("display-menu executes");

    let capture = RawPaneInputProbe::start(&handler, &alpha, "menu-arrow", 0).await;
    let mut pending_input = Vec::new();
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"\x1b[")
        .await
        .expect("fragmented menu arrow prefix succeeds");
    assert_eq!(pending_input, b"\x1b[");
    sleep(Duration::from_millis(50)).await;
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"A")
        .await
        .expect("fragmented menu arrow suffix succeeds");

    assert!(pending_input.is_empty());
    assert!(handler.overlay_active(requester_pid).await);
    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, b"").await;
}

#[tokio::test]
async fn live_attach_popup_preserves_complete_meta_input() {
    let handler = RequestHandler::new();
    let alpha = session_name("popup-meta");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    let parsed = handler
        .parse_control_commands("display-popup -N -T Popup -w 20 -h 6")
        .await
        .expect("display-popup parses");
    handler
        .execute_parsed_commands_for_test(requester_pid, parsed)
        .await
        .expect("display-popup executes");

    let capture = RawPaneInputProbe::start(&handler, &alpha, "popup-meta", 0).await;
    let mut pending_input = Vec::new();
    let forwarded = handler
        .handle_attached_live_input_inner(requester_pid, &mut pending_input, b"\x1baPOPUP_TAIL")
        .await
        .expect("popup Meta input succeeds");

    assert!(!forwarded);
    assert!(pending_input.is_empty());
    assert!(handler.overlay_active(requester_pid).await);
    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, b"").await;
}

#[tokio::test]
async fn live_attach_popup_escape_closes_only_after_timeout() {
    let handler = RequestHandler::new();
    let alpha = session_name("popup-escape-timeout");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    let parsed = handler
        .parse_control_commands("display-popup -N -T Popup -w 20 -h 6")
        .await
        .expect("display-popup parses");
    handler
        .execute_parsed_commands_for_test(requester_pid, parsed)
        .await
        .expect("display-popup executes");

    let capture = RawPaneInputProbe::start(&handler, &alpha, "popup-escape-timeout", 0).await;
    let mut pending_input = Vec::new();
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"\x1b")
        .await
        .expect("popup Escape prefix succeeds");
    assert_eq!(pending_input, b"\x1b");
    assert!(handler.overlay_active(requester_pid).await);
    let forwarded = handler
        .flush_attached_pending_escape_input(requester_pid, &mut pending_input)
        .await
        .expect("popup Escape timeout succeeds");

    assert!(!forwarded);
    assert!(pending_input.is_empty());
    assert!(!handler.overlay_active(requester_pid).await);
    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, b"").await;
}

#[tokio::test]
async fn live_attach_copy_mode_entry_reroutes_same_chunk_paste_to_copy_mode() {
    let handler = RequestHandler::new();
    let alpha = session_name("copy-entry-tail");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    let capture = RawPaneInputProbe::start(&handler, &alpha, "copy-entry-tail", 0).await;
    let mut pending_input = Vec::new();
    let forwarded = handler
        .handle_attached_live_input_inner(
            requester_pid,
            &mut pending_input,
            b"\x02[\x1b[200~MARKER\n\x1b[201~",
        )
        .await
        .expect("copy-mode entry and paste input succeed");

    assert!(!forwarded);
    assert!(pending_input.is_empty());
    assert!(handler
        .target_is_in_copy_mode(&PaneTarget::new(alpha.clone(), 0))
        .await
        .expect("copy-mode state resolves"));
    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, b"").await;
}

#[tokio::test]
async fn live_attach_copy_mode_exit_reroutes_same_chunk_tail_to_pane() {
    let handler = RequestHandler::new();
    let alpha = session_name("copy-exit-tail");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02[")
        .await
        .expect("copy-mode entry succeeds");

    let expected = b"TAIL\n";
    let capture =
        RawPaneInputProbe::start(&handler, &alpha, "copy-exit-tail", expected.len()).await;
    let mut pending_input = Vec::new();
    let forwarded = handler
        .handle_attached_live_input_inner(requester_pid, &mut pending_input, b"qTAIL\n")
        .await
        .expect("copy-mode exit and pane input succeed");

    assert!(forwarded);
    assert!(pending_input.is_empty());
    assert!(!handler
        .target_is_in_copy_mode(&PaneTarget::new(alpha.clone(), 0))
        .await
        .expect("copy-mode state resolves"));
    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, expected).await;
}

#[tokio::test]
async fn live_attach_clock_mode_exit_reroutes_same_chunk_tail_to_pane() {
    let handler = RequestHandler::new();
    let alpha = session_name("clock-exit-tail");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    let expected = b"TAIL\n";
    let capture =
        RawPaneInputProbe::start(&handler, &alpha, "clock-exit-tail", expected.len()).await;
    let mut pending_input = Vec::new();
    let forwarded = handler
        .handle_attached_live_input_inner(requester_pid, &mut pending_input, b"\x02tqTAIL\n")
        .await
        .expect("clock-mode exit and pane input succeed");

    assert!(forwarded);
    assert!(pending_input.is_empty());
    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, expected).await;
}

#[tokio::test]
async fn live_attach_clock_mode_consumes_complete_meta_key() {
    let handler = RequestHandler::new();
    let alpha = session_name("clock-meta");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02t")
        .await
        .expect("clock-mode entry succeeds");

    let capture = RawPaneInputProbe::start(&handler, &alpha, "clock-meta", 0).await;
    let mut pending_input = Vec::new();
    let forwarded = handler
        .handle_attached_live_input_inner(requester_pid, &mut pending_input, b"\x1ba")
        .await
        .expect("clock-mode Meta key succeeds");

    assert!(!forwarded);
    assert!(pending_input.is_empty());
    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, b"").await;
}

#[tokio::test]
async fn live_attach_clock_mode_consumes_complete_x10_mouse_event() {
    let handler = RequestHandler::new();
    let alpha = session_name("clock-x10-mouse");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02t")
        .await
        .expect("clock-mode entry succeeds");

    let capture = RawPaneInputProbe::start(&handler, &alpha, "clock-x10-mouse", 0).await;
    let mut pending_input = Vec::new();
    let forwarded = handler
        .handle_attached_live_input_inner(requester_pid, &mut pending_input, b"\x1b[M !!")
        .await
        .expect("clock-mode X10 mouse input succeeds");

    assert!(!forwarded);
    assert!(pending_input.is_empty());
    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, b"").await;
}

#[tokio::test]
async fn live_attach_clock_mode_waits_for_fragmented_arrow() {
    let handler = RequestHandler::new();
    let alpha = session_name("clock-arrow");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02t")
        .await
        .expect("clock-mode entry succeeds");

    let capture = RawPaneInputProbe::start(&handler, &alpha, "clock-arrow", 0).await;
    let mut pending_input = Vec::new();
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"\x1b[")
        .await
        .expect("fragmented arrow prefix succeeds");
    assert_eq!(pending_input, b"\x1b[");
    sleep(Duration::from_millis(50)).await;
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"A")
        .await
        .expect("fragmented arrow suffix succeeds");

    assert!(pending_input.is_empty());
    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, b"").await;
}

#[tokio::test]
async fn live_attach_clock_mode_reroutes_complete_bracketed_paste() {
    let handler = RequestHandler::new();
    let alpha = session_name("clock-paste");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02t")
        .await
        .expect("clock-mode entry succeeds");

    let expected = b"BODY\n";
    let capture = RawPaneInputProbe::start(&handler, &alpha, "clock-paste", expected.len()).await;
    let mut pending_input = Vec::new();
    let forwarded = handler
        .handle_attached_live_input_inner(
            requester_pid,
            &mut pending_input,
            b"\x1b[200~BODY\n\x1b[201~",
        )
        .await
        .expect("clock-mode bracketed paste succeeds");

    assert!(forwarded);
    assert!(pending_input.is_empty());
    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, expected).await;
}

#[tokio::test]
async fn live_attach_copy_mode_waits_for_fragmented_meta_key() {
    let handler = RequestHandler::new();
    let alpha = session_name("copy-meta");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02[")
        .await
        .expect("copy-mode entry succeeds");

    let capture = RawPaneInputProbe::start(&handler, &alpha, "copy-meta", 0).await;
    let mut pending_input = Vec::new();
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"\x1b")
        .await
        .expect("Meta prefix succeeds");
    assert_eq!(pending_input, b"\x1b");
    sleep(Duration::from_millis(50)).await;
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"a")
        .await
        .expect("Meta suffix succeeds");

    assert!(pending_input.is_empty());
    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, b"").await;
}

#[tokio::test]
async fn live_attach_clock_mode_consumes_escape_when_timeout_expires() {
    let handler = RequestHandler::new();
    let alpha = session_name("clock-escape-timeout");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02t")
        .await
        .expect("clock-mode entry succeeds");

    let capture = RawPaneInputProbe::start(&handler, &alpha, "clock-escape-timeout", 0).await;
    let mut pending_input = Vec::new();
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"\x1b")
        .await
        .expect("standalone escape prefix succeeds");
    assert_eq!(pending_input, b"\x1b");
    let forwarded = handler
        .flush_attached_pending_escape_input(requester_pid, &mut pending_input)
        .await
        .expect("clock-mode escape timeout succeeds");

    assert!(!forwarded);
    assert!(pending_input.is_empty());
    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, b"").await;
}

#[tokio::test]
async fn send_keys_m_forwards_the_current_mouse_event_to_the_pane() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;

    {
        let mut state = handler.state.lock().await;
        state
            .append_bytes_to_pane_transcript_for_test(&alpha, 0, 0, b"\x1b[?1000h")
            .expect("mouse mode transcript update");
    }

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let (window_id, pane_id, pane_target) = {
        let state = handler.state.lock().await;
        let session = state.sessions.session(&alpha).expect("session exists");
        let window = session.window_at(0).expect("window exists");
        let pane = window.pane(0).expect("pane exists");
        (window.id(), pane.id(), PaneTarget::new(alpha.clone(), 0))
    };

    let raw = MouseForwardEvent {
        b: 0,
        lb: 0,
        x: 1,
        y: 1,
        lx: 1,
        ly: 1,
        sgr_b: 0,
        sgr_type: ' ',
        ignore: false,
    };
    {
        let mut active_attach = handler.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get_mut(&requester_pid)
            .expect("attached client exists");
        active.mouse.current_event = Some(AttachedMouseEvent {
            raw,
            session_id: 0,
            window_id: Some(window_id.as_u32()),
            pane_id: Some(pane_id),
            pane_target: Some(pane_target.clone()),
            location: MouseLocation::Pane,
            status_at: None,
            status_lines: 0,
            ignore: false,
        });
    }

    let expected =
        encode_mouse_event(mode::MODE_MOUSE_STANDARD, &raw, raw.x, raw.y).expect("mouse encodes");
    let capture = RawPaneInputProbe::start(&handler, &alpha, "mouse-forward", expected.len()).await;

    let response = handler
        .handle(Request::SendKeysExt(SendKeysExtRequest {
            target: Some(pane_target),
            keys: Vec::new(),
            expand_formats: false,
            hex: false,
            literal: false,
            dispatch_key_table: false,
            copy_mode_command: false,
            forward_mouse_event: true,
            reset_terminal: false,
            repeat_count: None,
        }))
        .await;
    assert_eq!(
        response,
        Response::SendKeys(SendKeysResponse { key_count: 0 })
    );

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, &expected).await;
}

#[tokio::test]
async fn live_attach_extended_keys_are_reencoded_for_the_target_pane() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let expected = b"\x1b[Z";
    let capture =
        RawPaneInputProbe::start(&handler, &alpha, "live-attach-extended-key", expected.len())
            .await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x1b[9;2u")
        .await
        .expect("live attach input");

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, expected).await;
}

#[tokio::test]
async fn read_only_live_attach_drops_decoded_key_input() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    {
        let mut active_attach = handler.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get_mut(&requester_pid)
            .expect("attach is active");
        active
            .flags
            .insert(crate::client_flags::ClientFlags::READONLY);
    }

    let capture = RawPaneInputProbe::start(&handler, &alpha, "read-only-decoded-key", 0).await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x1b[9;2u")
        .await
        .expect("read-only live attach input");

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, b"").await;
}

#[tokio::test]
async fn live_attach_shift_enter_csi_u_survives_extended_key_mode() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;

    let set_format = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::ExtendedKeysFormat,
            value: "csi-u".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(set_format, Response::SetOption(_)));

    {
        let mut state = handler.state.lock().await;
        state
            .append_bytes_to_pane_transcript_for_test(&alpha, 0, 0, b"\x1b[>4;2m")
            .expect("extended key mode transcript update");
    }

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let expected = b"\x1b[13;2u";
    let capture =
        RawPaneInputProbe::start(&handler, &alpha, "live-attach-shift-enter", expected.len()).await;

    handler
        .handle_attached_live_input_for_test(requester_pid, expected)
        .await
        .expect("live attach S-Enter input");

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, expected).await;
}

#[tokio::test]
async fn live_attach_standalone_escape_flushes_when_timeout_expires() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let expected = b"\x1b";
    let capture =
        RawPaneInputProbe::start(&handler, &alpha, "live-attach-escape-time", expected.len()).await;

    let mut pending_input = Vec::new();
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, expected)
        .await
        .expect("standalone escape fragment");
    assert_eq!(pending_input, expected);

    let flushed = handler
        .flush_attached_pending_escape_input(requester_pid, &mut pending_input)
        .await
        .expect("pending escape flush");

    assert!(flushed);
    assert!(pending_input.is_empty());
    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, expected).await;
}

#[tokio::test]
async fn live_attach_fragmented_arrow_consumes_pending_escape_before_timeout() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let expected = encode_key(
        0,
        ExtendedKeyFormat::Xterm,
        key_string_lookup_string("Up").expect("Up parses"),
    )
    .expect("Up encodes");
    let capture = RawPaneInputProbe::start(
        &handler,
        &alpha,
        "live-attach-fragmented-up",
        expected.len(),
    )
    .await;

    let mut pending_input = Vec::new();
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"\x1b")
        .await
        .expect("arrow escape prefix");
    assert_eq!(pending_input, b"\x1b");
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"[A")
        .await
        .expect("arrow suffix");

    assert!(pending_input.is_empty());
    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, &expected).await;
}

#[tokio::test]
async fn live_attach_fragmented_arrow_survives_target_extended_key_mode() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;

    {
        let mut state = handler.state.lock().await;
        state
            .append_bytes_to_pane_transcript_for_test(&alpha, 0, 0, b"\x1b[>4;2m")
            .expect("extended key mode transcript update");
    }

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let expected = b"\x1b[A";
    let capture = RawPaneInputProbe::start(
        &handler,
        &alpha,
        "live-attach-extended-mode-fragmented-up",
        expected.len(),
    )
    .await;

    let mut pending_input = Vec::new();
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"\x1b")
        .await
        .expect("arrow escape prefix");
    assert_eq!(pending_input, b"\x1b");
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"[A")
        .await
        .expect("arrow suffix");

    assert!(pending_input.is_empty());
    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, expected).await;
}

#[tokio::test]
async fn live_attach_invalid_extended_sequence_does_not_repeat_preceding_bytes() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    // Cursor-position reports intentionally pass through to the pane. Their
    // numeric CSI prefix also enters the extended-key decoder before rejection.
    let expected = b"a\x1b[12;40R";
    let capture = RawPaneInputProbe::start(
        &handler,
        &alpha,
        "live-attach-invalid-extended-prefix",
        expected.len(),
    )
    .await;
    let mut pending_input = Vec::new();

    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, expected)
        .await
        .expect("cursor-position response with plain prefix");

    assert!(pending_input.is_empty());
    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, expected).await;
}

#[tokio::test]
async fn live_attach_ambiguous_escape_prefixes_wait_for_suffix() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;

    {
        let mut state = handler.state.lock().await;
        state
            .append_bytes_to_pane_transcript_for_test(&alpha, 0, 0, b"\x1b[>4;2m")
            .expect("extended key mode transcript update");
    }

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    for (label, chunks, expected) in [
        (
            "ss3-up",
            [b"\x1bO".as_slice(), b"A".as_slice()],
            b"\x1b[A".as_slice(),
        ),
        (
            "csi-home",
            [b"\x1b[".as_slice(), b"H".as_slice()],
            b"\x1b[1~".as_slice(),
        ),
        (
            "csi-home-7",
            [b"\x1b[7".as_slice(), b"~".as_slice()],
            b"\x1b[1~".as_slice(),
        ),
        (
            "csi-end-8",
            [b"\x1b[8".as_slice(), b"~".as_slice()],
            b"\x1b[4~".as_slice(),
        ),
        (
            "ss3-f1",
            [b"\x1bO".as_slice(), b"P".as_slice()],
            b"\x1bOP".as_slice(),
        ),
        (
            "csi-f9",
            [b"\x1b[20".as_slice(), b"~".as_slice()],
            b"\x1b[20~".as_slice(),
        ),
    ] {
        let capture = RawPaneInputProbe::start(&handler, &alpha, label, expected.len()).await;
        let mut pending_input = Vec::new();
        for chunk in chunks {
            handler
                .handle_attached_live_input(requester_pid, &mut pending_input, chunk)
                .await
                .expect("fragmented escape sequence");
        }
        assert!(
            pending_input.is_empty(),
            "{label} should not leave pending input"
        );
        capture.finish(&handler, &alpha).await;
        capture.assert_contents(&handler, expected).await;
    }
}

#[tokio::test]
async fn live_attach_fragmented_meta_key_consumes_pending_escape_before_timeout() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let expected = encode_key(
        0,
        ExtendedKeyFormat::Xterm,
        key_string_lookup_string("M-1").expect("M-1 parses"),
    )
    .expect("M-1 encodes");
    let capture = RawPaneInputProbe::start(
        &handler,
        &alpha,
        "live-attach-fragmented-meta",
        expected.len(),
    )
    .await;

    let mut pending_input = Vec::new();
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"\x1b")
        .await
        .expect("meta escape prefix");
    assert_eq!(pending_input, b"\x1b");
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"1")
        .await
        .expect("meta suffix");

    assert!(pending_input.is_empty());
    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, &expected).await;
}

#[tokio::test]
async fn live_attach_control_bytes_dispatch_tmux_distinct_bindings() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    for (key, literal) in [
        ("C-h", "H"),
        ("BSpace", "B"),
        ("C-j", "J"),
        ("Enter", "E"),
        ("C-Space", "S"),
        ("C-\\", "L"),
        ("C-]", "R"),
        ("C-^", "C"),
        ("C-_", "U"),
    ] {
        let rebound = handler
            .handle(Request::BindKey(Box::new(BindKeyRequest {
                table_name: "root".to_owned(),
                key: key.to_owned(),
                note: Some("live-attach-control-byte".to_owned()),
                repeat: false,
                command: Some(vec![
                    "send-keys".to_owned(),
                    "-l".to_owned(),
                    literal.to_owned(),
                ]),
            })))
            .await;
        assert!(matches!(rebound, Response::BindKey(_)), "{key} should bind");
    }

    let input = b"\x08\x7f\x0a\x0d\x00\x1c\x1d\x1e\x1f";
    let expected = b"HBJESLRCU";
    let capture = RawPaneInputProbe::start(
        &handler,
        &alpha,
        "live-attach-control-bindings",
        expected.len(),
    )
    .await;

    handler
        .handle_attached_live_input_for_test(requester_pid, input)
        .await
        .expect("live attach control binding input");

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, expected).await;
}

#[tokio::test]
async fn live_attach_plain_fast_path_dispatches_root_binding() {
    let handler = RequestHandler::new();
    let alpha = session_name("plain-fast-root-binding");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    let rebound = handler
        .handle(Request::BindKey(Box::new(BindKeyRequest {
            table_name: "root".to_owned(),
            key: "x".to_owned(),
            note: Some("plain fast path root binding".to_owned()),
            repeat: false,
            command: Some(vec![
                "send-keys".to_owned(),
                "-l".to_owned(),
                "R".to_owned(),
            ]),
        })))
        .await;
    assert!(matches!(rebound, Response::BindKey(_)), "{rebound:?}");

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    let capture = RawPaneInputProbe::start(&handler, &alpha, "plain-fast-root-binding", 1).await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"x")
        .await
        .expect("plain root binding input");

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, b"R").await;
}

#[tokio::test]
async fn live_attach_plain_fast_path_dispatches_custom_default_table_binding() {
    let handler = RequestHandler::new();
    let alpha = session_name("plain-fast-custom-binding");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    let configured = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Session(alpha.clone()),
            option: OptionName::KeyTable,
            value: "custom-fast".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(
        matches!(configured, Response::SetOption(_)),
        "{configured:?}"
    );
    let rebound = handler
        .handle(Request::BindKey(Box::new(BindKeyRequest {
            table_name: "custom-fast".to_owned(),
            key: "x".to_owned(),
            note: Some("plain fast path custom binding".to_owned()),
            repeat: false,
            command: Some(vec![
                "send-keys".to_owned(),
                "-l".to_owned(),
                "C".to_owned(),
            ]),
        })))
        .await;
    assert!(matches!(rebound, Response::BindKey(_)), "{rebound:?}");

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    let capture = RawPaneInputProbe::start(&handler, &alpha, "plain-fast-custom-binding", 1).await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"x")
        .await
        .expect("plain custom-table binding input");

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, b"C").await;
}

#[tokio::test]
async fn live_attach_plain_fast_path_forwards_unbound_input_unchanged() {
    let handler = RequestHandler::new();
    let alpha = session_name("plain-fast-unbound");
    let requester_pid = std::process::id();
    let input = b"plain text\r";

    create_send_keys_test_session(&handler, &alpha).await;
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    let capture =
        RawPaneInputProbe::start(&handler, &alpha, "plain-fast-unbound", input.len()).await;

    handler
        .handle_attached_live_input_for_test(requester_pid, input)
        .await
        .expect("unbound plain input");

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, input).await;
}

#[tokio::test]
async fn live_attach_utf8_dispatches_bound_key_and_forwards_unbound_tail() {
    let handler = RequestHandler::new();
    let alpha = session_name("utf8-root-binding");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    let rebound = handler
        .handle(Request::BindKey(Box::new(BindKeyRequest {
            table_name: "root".to_owned(),
            key: "é".to_owned(),
            note: Some("utf8 root binding".to_owned()),
            repeat: false,
            command: Some(vec![
                "send-keys".to_owned(),
                "-l".to_owned(),
                "R".to_owned(),
            ]),
        })))
        .await;
    assert!(matches!(rebound, Response::BindKey(_)), "{rebound:?}");

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    let expected = "Rλ".as_bytes();
    let capture =
        RawPaneInputProbe::start(&handler, &alpha, "utf8-root-binding", expected.len()).await;

    handler
        .handle_attached_live_input_for_test(requester_pid, "éλ".as_bytes())
        .await
        .expect("utf8 bound and unbound input");

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, expected).await;
}

#[tokio::test]
async fn live_attach_named_key_table_dispatches_before_rerouted_plain_tail() {
    let handler = RequestHandler::new();
    let alpha = session_name("named-table-live-input");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    let bound = handler
        .handle(Request::BindKey(Box::new(BindKeyRequest {
            table_name: "named-live".to_owned(),
            key: "x".to_owned(),
            note: Some("named live table".to_owned()),
            repeat: false,
            command: Some(vec![
                "set-buffer".to_owned(),
                "-b".to_owned(),
                "named-live-hit".to_owned(),
                "yes".to_owned(),
            ]),
        })))
        .await;
    assert!(matches!(bound, Response::BindKey(_)), "{bound:?}");
    let switched = handler
        .handle(Request::SwitchClientExt(SwitchClientExtRequest {
            target: None,
            key_table: Some("named-live".to_owned()),
        }))
        .await;
    assert!(
        matches!(switched, Response::SwitchClient(_)),
        "{switched:?}"
    );

    let capture = RawPaneInputProbe::start(&handler, &alpha, "named-live-tail", 4).await;
    let mut pending_input = Vec::new();
    let forwarded = handler
        .handle_attached_live_input_inner(requester_pid, &mut pending_input, b"xTAIL")
        .await
        .expect("named table key and tail route");

    assert!(forwarded);
    assert!(pending_input.is_empty());
    wait_for_buffer(&handler, "named-live-hit", "yes").await;
    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, b"TAIL").await;
}

#[tokio::test]
async fn live_attach_prefix_precedes_a_transient_key_table() {
    let handler = RequestHandler::new();
    let alpha = session_name("prefix-precedes-transient-live");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let bound = handler
        .handle(Request::BindKey(Box::new(BindKeyRequest {
            table_name: "named-live".to_owned(),
            key: "C-b".to_owned(),
            note: Some("must lose to live prefix input".to_owned()),
            repeat: false,
            command: Some(vec![
                "set-buffer".to_owned(),
                "-b".to_owned(),
                "wrong-live-table-hit".to_owned(),
                "yes".to_owned(),
            ]),
        })))
        .await;
    assert!(matches!(bound, Response::BindKey(_)), "{bound:?}");
    let switched = handler
        .handle(Request::SwitchClientExt(SwitchClientExtRequest {
            target: None,
            key_table: Some("named-live".to_owned()),
        }))
        .await;
    assert!(
        matches!(switched, Response::SwitchClient(_)),
        "{switched:?}"
    );

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02")
        .await
        .expect("live prefix input from a transient table");

    let active_attach = handler.active_attach.lock().await;
    let active = active_attach
        .by_pid
        .get(&requester_pid)
        .expect("attached client should remain registered");
    assert_eq!(active.key_table_name.as_deref(), Some("prefix"));
    drop(active_attach);

    let wrong_table_hit = handler
        .handle(Request::ShowBuffer(ShowBufferRequest {
            name: Some("wrong-live-table-hit".to_owned()),
        }))
        .await;
    assert!(
        matches!(wrong_table_hit, Response::Error(_)),
        "the live transient table's prefix-key binding must not execute"
    );
}

#[tokio::test]
async fn live_attach_key_table_off_keeps_prefix_disabled() {
    let handler = RequestHandler::new();
    let alpha = session_name("key-table-off-live");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    for (option, value) in [(OptionName::Prefix, "None"), (OptionName::KeyTable, "off")] {
        let configured = handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Session(alpha.clone()),
                option,
                value: value.to_owned(),
                mode: SetOptionMode::Replace,
            }))
            .await;
        assert!(
            matches!(configured, Response::SetOption(_)),
            "{configured:?}"
        );
    }

    let bound = handler
        .handle(Request::BindKey(Box::new(BindKeyRequest {
            table_name: "off".to_owned(),
            key: "C-b".to_owned(),
            note: Some("disabled-prefix table binding".to_owned()),
            repeat: false,
            command: Some(vec![
                "set-buffer".to_owned(),
                "-b".to_owned(),
                "off-table-hit".to_owned(),
                "yes".to_owned(),
            ]),
        })))
        .await;
    assert!(matches!(bound, Response::BindKey(_)), "{bound:?}");

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02")
        .await
        .expect("disabled-prefix table input");

    let shown = handler
        .handle(Request::ShowBuffer(ShowBufferRequest {
            name: Some("off-table-hit".to_owned()),
        }))
        .await;
    let Response::ShowBuffer(shown) = shown else {
        panic!("expected off-table binding to execute, got {shown:?}");
    };
    assert_eq!(shown.command_output().stdout(), b"yes");

    let active_attach = handler.active_attach.lock().await;
    let active = active_attach
        .by_pid
        .get(&requester_pid)
        .expect("attached client should remain registered");
    assert_eq!(active.key_table_name, None);
}

#[tokio::test]
async fn live_attach_nul_dispatches_c_at_alias_binding() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let rebound = handler
        .handle(Request::BindKey(Box::new(BindKeyRequest {
            table_name: "root".to_owned(),
            key: "C-@".to_owned(),
            note: Some("live-attach-control-byte-alias".to_owned()),
            repeat: false,
            command: Some(vec![
                "send-keys".to_owned(),
                "-l".to_owned(),
                "A".to_owned(),
            ]),
        })))
        .await;
    assert!(matches!(rebound, Response::BindKey(_)));

    let capture = RawPaneInputProbe::start(&handler, &alpha, "live-attach-c-at-alias", 1).await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x00")
        .await
        .expect("live attach C-@ alias input");

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, b"A").await;
}

#[tokio::test]
async fn live_attach_meta_control_bytes_do_not_wait_for_following_input() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let expected = b"\x1b\x01\x1b\x7f";
    let capture =
        RawPaneInputProbe::start(&handler, &alpha, "live-attach-meta-control", expected.len())
            .await;

    let mut pending_input = Vec::new();
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"\x1b\x01")
        .await
        .expect("meta control input");
    assert!(pending_input.is_empty());
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"\x1b\x7f")
        .await
        .expect("meta backspace input");
    assert!(pending_input.is_empty());

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, expected).await;
}

#[tokio::test]
async fn live_attach_committed_utf8_text_preserves_latin_and_ime_payload_chunks() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let expected = "Latin ABC 123 | 日本語かな | 한글 | cafe\u{0301}".as_bytes();
    let capture = RawPaneInputProbe::start(
        &handler,
        &alpha,
        "live-attach-committed-utf8-text",
        expected.len(),
    )
    .await;

    let mut pending_input = Vec::new();
    for chunk in [&expected[..17], &expected[17..35], &expected[35..]] {
        handler
            .handle_attached_live_input(requester_pid, &mut pending_input, chunk)
            .await
            .expect("committed utf8 text chunk");
    }
    assert!(pending_input.is_empty());

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, expected).await;
}

#[tokio::test]
async fn live_attach_preserves_c1_and_malformed_utf8_bytes() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let input = b"\x9bA\xc3(\xe2(\xa1";
    let expected = input;
    let capture = RawPaneInputProbe::start(
        &handler,
        &alpha,
        "live-attach-invalid-bytes",
        expected.len(),
    )
    .await;

    handler
        .handle_attached_live_input_for_test(requester_pid, input)
        .await
        .expect("invalid byte input");

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, expected).await;
}

#[tokio::test]
async fn live_attach_focus_sequences_are_consumed_at_attach_boundary() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    {
        let mut state = handler.state.lock().await;
        state
            .append_bytes_to_pane_transcript_for_test(&alpha, 0, 0, b"\x1b[?1004l")
            .expect("focus mode reset transcript update");
    }

    let capture = RawPaneInputProbe::start(&handler, &alpha, "live-attach-focus", 0).await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x1b[I\x1b[O")
        .await
        .expect("live attach focus input");

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, b"").await;
}

#[tokio::test]
async fn live_attach_large_focus_chain_has_bounded_reroute_work() {
    let handler = RequestHandler::new();
    let alpha = session_name("large-focus-chain");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    {
        let mut state = handler.state.lock().await;
        state
            .append_bytes_to_pane_transcript_for_test(&alpha, 0, 0, b"\x1b[?1004l")
            .expect("focus mode reset transcript update");
    }

    let input = b"\x1b[I\x1b[O".repeat(LARGE_FOCUS_CHAIN_REPETITIONS);
    let capture = RawPaneInputProbe::start(&handler, &alpha, "large-focus-chain", 0).await;
    tokio::time::timeout(
        BOUNDED_REROUTE_CHAIN_TIMEOUT,
        handler.handle_attached_live_input_for_test(requester_pid, &input),
    )
    .await
    .expect("large focus chain must not grow reroute work with the whole frame")
    .expect("large focus chain succeeds");

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, b"").await;
}

#[tokio::test]
async fn live_attach_focus_sequences_emit_client_and_pane_hooks_in_order() {
    let handler = RequestHandler::new();
    let alpha = session_name("live-attach-focus-hooks");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    set_global_hook(
        &handler,
        HookName::ClientFocusIn,
        "set-buffer -a -b focus-hooks CFI,",
    )
    .await;
    set_global_hook(
        &handler,
        HookName::PaneFocusIn,
        "set-buffer -a -b focus-hooks PFI,",
    )
    .await;
    set_global_hook(
        &handler,
        HookName::PaneFocusOut,
        "set-buffer -a -b focus-hooks PFO,",
    )
    .await;
    set_global_hook(
        &handler,
        HookName::ClientFocusOut,
        "set-buffer -a -b focus-hooks CFO,",
    )
    .await;
    let mut lifecycle = handler.subscribe_lifecycle_events();
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x1b[I\x1b[O")
        .await
        .expect("live attach focus input");

    drain_lifecycle_hooks(&handler, &mut lifecycle).await;
    wait_for_buffer(&handler, "focus-hooks", "CFI,PFI,PFO,CFO,").await;
}

#[tokio::test]
async fn live_attach_focus_hook_mode_transition_reroutes_same_chunk_paste() {
    let handler = RequestHandler::new();
    let alpha = session_name("focus-copy-tail");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    set_global_hook(&handler, HookName::ClientFocusIn, "copy-mode").await;
    let lifecycle_events = handler
        .take_lifecycle_dispatch_receiver()
        .expect("lifecycle dispatch receiver activates once");
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let lifecycle_handler = handler.clone();
    let lifecycle_task = tokio::spawn(async move {
        lifecycle_handler
            .consume_lifecycle_hooks(lifecycle_events, shutdown_rx)
            .await;
    });
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    let capture = RawPaneInputProbe::start(&handler, &alpha, "focus-copy-tail", 0).await;
    {
        let mut state = handler.state.lock().await;
        state
            .append_bytes_to_pane_transcript_for_test(&alpha, 0, 0, b"\x1b[?1004l")
            .expect("focus mode reset transcript update");
    }

    let mut pending_input = Vec::new();
    let forwarded = handler
        .handle_attached_live_input_inner(
            requester_pid,
            &mut pending_input,
            b"\x1b[I\x1b[200~MARKER\n\x1b[201~",
        )
        .await
        .expect("focus hook mode transition succeeds");

    assert!(!forwarded);
    assert!(pending_input.is_empty());
    assert!(handler
        .target_is_in_copy_mode(&PaneTarget::new(alpha.clone(), 0))
        .await
        .expect("copy-mode state resolves"));
    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, b"").await;
    let _ = shutdown_tx.send(());
    lifecycle_task.await.expect("lifecycle task joins");
}

#[tokio::test]
async fn live_attach_focus_hook_pane_transition_reloads_bracketed_paste_mode_for_tail() {
    let handler = RequestHandler::new();
    let alpha = session_name("focus-pane-paste-tail");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    let split = handler
        .handle(Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Session(alpha.clone()),
            direction: SplitDirection::Horizontal,
            before: false,
            environment: None,
        }))
        .await;
    assert!(matches!(split, Response::SplitWindow(_)), "{split:?}");

    let first = PaneTarget::with_window(alpha.clone(), 0, 0);
    let second = PaneTarget::with_window(alpha.clone(), 0, 1);
    let selected = handler
        .handle(Request::SelectPane(Box::new(SelectPaneRequest {
            target: first.clone(),
            title: None,
            style: None,
            input_disabled: None,
            preserve_zoom: false,
        })))
        .await;
    assert!(matches!(selected, Response::SelectPane(_)), "{selected:?}");
    {
        let mut state = handler.state.lock().await;
        state
            .append_bytes_to_pane_transcript_for_test(&alpha, 0, 1, b"\x1b[?2004h")
            .expect("second pane enables bracketed paste mode");
        state.start_pane_input_capture_for_test(&first);
        state.start_pane_input_capture_for_test(&second);
    }

    let select_second = format!("select-pane -t {}:0.1", alpha.as_str());
    set_global_hook(&handler, HookName::ClientFocusIn, &select_second).await;
    let lifecycle_events = handler
        .take_lifecycle_dispatch_receiver()
        .expect("lifecycle dispatch receiver activates once");
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let lifecycle_handler = handler.clone();
    let lifecycle_task = tokio::spawn(async move {
        lifecycle_handler
            .consume_lifecycle_hooks(lifecycle_events, shutdown_rx)
            .await;
    });
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    {
        let mut state = handler.state.lock().await;
        state
            .append_bytes_to_pane_transcript_for_test(&alpha, 0, 0, b"\x1b[?1004l")
            .expect("first pane focus mode reset transcript update");
    }

    let paste = b"\x1b[200~MARKER\n\x1b[201~";
    let mut input = b"\x1b[I".to_vec();
    input.extend_from_slice(paste);
    let mut pending_input = Vec::new();
    let forwarded = handler
        .handle_attached_live_input_inner(requester_pid, &mut pending_input, &input)
        .await
        .expect("focus hook pane transition succeeds");

    assert!(forwarded);
    assert!(pending_input.is_empty());
    {
        let state = handler.state.lock().await;
        assert_eq!(
            state
                .sessions
                .session(&alpha)
                .expect("session exists")
                .window_at(0)
                .expect("window exists")
                .active_pane_index(),
            1
        );
        assert_eq!(state.pane_input_capture_for_test(&first), Some(Vec::new()));
        assert_eq!(
            state.pane_input_capture_for_test(&second),
            Some(paste.to_vec())
        );
    }

    let _ = shutdown_tx.send(());
    lifecycle_task.await.expect("lifecycle task joins");
}

#[tokio::test]
async fn live_attach_focus_hook_reindexed_pane_identity_reloads_tail_capabilities() {
    let handler = RequestHandler::new();
    let alpha = session_name("focus-reindexed-pane-tail");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    let split = handler
        .handle(Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Session(alpha.clone()),
            direction: SplitDirection::Horizontal,
            before: false,
            environment: None,
        }))
        .await;
    assert!(matches!(split, Response::SplitWindow(_)), "{split:?}");

    let target = PaneTarget::with_window(alpha.clone(), 0, 0);
    let selected = handler
        .handle(Request::SelectPane(Box::new(SelectPaneRequest {
            target: target.clone(),
            title: None,
            style: None,
            input_disabled: None,
            preserve_zoom: false,
        })))
        .await;
    assert!(matches!(selected, Response::SelectPane(_)), "{selected:?}");
    let (removed_pane_id, surviving_pane_id) = {
        let mut state = handler.state.lock().await;
        let session = state.sessions.session(&alpha).expect("session exists");
        let removed_pane_id = session
            .pane_id_in_window(0, 0)
            .expect("first pane identity exists");
        let surviving_pane_id = session
            .pane_id_in_window(0, 1)
            .expect("second pane identity exists");
        state
            .append_bytes_to_pane_transcript_for_test(&alpha, 0, 1, b"\x1b[?2004h")
            .expect("surviving pane enables bracketed paste mode");
        state.start_pane_input_capture_for_test(&target);
        (removed_pane_id, surviving_pane_id)
    };

    let kill_first = format!("kill-pane -t {}:0.0", alpha.as_str());
    set_global_hook(&handler, HookName::ClientFocusIn, &kill_first).await;
    let lifecycle_events = handler
        .take_lifecycle_dispatch_receiver()
        .expect("lifecycle dispatch receiver activates once");
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let lifecycle_handler = handler.clone();
    let lifecycle_task = tokio::spawn(async move {
        lifecycle_handler
            .consume_lifecycle_hooks(lifecycle_events, shutdown_rx)
            .await;
    });
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    {
        let mut state = handler.state.lock().await;
        state
            .append_bytes_to_pane_transcript_for_test(&alpha, 0, 0, b"\x1b[?1004l")
            .expect("first pane focus mode reset transcript update");
    }

    let paste = b"\x1b[200~MARKER\n\x1b[201~";
    let mut input = b"\x1b[I".to_vec();
    input.extend_from_slice(paste);
    let mut pending_input = Vec::new();
    let forwarded = handler
        .handle_attached_live_input_inner(requester_pid, &mut pending_input, &input)
        .await
        .expect("focus hook reindexed pane transition succeeds");

    assert!(forwarded);
    assert!(pending_input.is_empty());
    assert_eq!(
        handler
            .attached_input_target(requester_pid)
            .await
            .expect("attached target remains valid"),
        target
    );
    {
        let state = handler.state.lock().await;
        let session = state.sessions.session(&alpha).expect("session exists");
        assert_eq!(session.pane_id_in_window(0, 0), Some(surviving_pane_id));
        assert_ne!(session.pane_id_in_window(0, 0), Some(removed_pane_id));
        assert_eq!(
            state.pane_input_capture_for_test(&target),
            Some(paste.to_vec())
        );
    }

    let _ = shutdown_tx.send(());
    lifecycle_task.await.expect("lifecycle task joins");
}

#[tokio::test]
async fn live_attach_focus_hook_respawn_reloads_same_pane_tail_capabilities() {
    let handler = RequestHandler::new();
    let alpha = session_name("focus-respawn-pane-tail");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    let target = PaneTarget::with_window(alpha.clone(), 0, 0);
    {
        let mut state = handler.state.lock().await;
        state
            .append_bytes_to_pane_transcript_for_test(&alpha, 0, 0, b"\x1b[?2004h")
            .expect("pane enables bracketed paste before respawn");
        state.start_pane_input_capture_for_test(&target);
    }

    let respawn = format!("respawn-pane -k -t {}:0.0", alpha.as_str());
    set_global_hook(&handler, HookName::ClientFocusIn, &respawn).await;
    let lifecycle_events = handler
        .take_lifecycle_dispatch_receiver()
        .expect("lifecycle dispatch receiver activates once");
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let lifecycle_handler = handler.clone();
    let lifecycle_task = tokio::spawn(async move {
        lifecycle_handler
            .consume_lifecycle_hooks(lifecycle_events, shutdown_rx)
            .await;
    });
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    {
        let mut state = handler.state.lock().await;
        state
            .append_bytes_to_pane_transcript_for_test(&alpha, 0, 0, b"\x1b[?1004l")
            .expect("pane focus mode reset transcript update");
    }

    let body = b"MARKER\n";
    let mut input = b"\x1b[I\x1b[200~".to_vec();
    input.extend_from_slice(body);
    input.extend_from_slice(b"\x1b[201~");
    let mut pending_input = Vec::new();
    let forwarded = handler
        .handle_attached_live_input_inner(requester_pid, &mut pending_input, &input)
        .await
        .expect("focus hook respawn and paste tail succeed");

    assert!(forwarded);
    assert!(pending_input.is_empty());
    {
        let state = handler.state.lock().await;
        assert_eq!(
            state.pane_input_capture_for_test(&target),
            Some(body.to_vec())
        );
        assert_eq!(
            state
                .transcript_handle(&target)
                .expect("respawned transcript exists")
                .lock()
                .expect("transcript mutex")
                .mode()
                & mode::MODE_BRACKETPASTE,
            0
        );
    }

    let _ = shutdown_tx.send(());
    lifecycle_task.await.expect("lifecycle task joins");
}

#[tokio::test]
async fn live_attach_first_typist_after_second_attach_marks_changed_client_active() {
    let handler = RequestHandler::new();
    let alpha = session_name("live-attach-client-active");
    let first_pid = std::process::id();
    let second_pid = first_pid.saturating_add(1);

    create_send_keys_test_session(&handler, &alpha).await;
    set_global_hook(
        &handler,
        HookName::ClientActive,
        "set-buffer -a -b client-active active,",
    )
    .await;
    let mut lifecycle = handler.subscribe_lifecycle_events();
    let (first_tx, _first_rx) = mpsc::unbounded_channel();
    let _first_attach = handler
        .register_attach(first_pid, alpha.clone(), first_tx)
        .await;
    let (second_tx, _second_rx) = mpsc::unbounded_channel();
    let _second_attach = handler
        .register_attach(second_pid, alpha.clone(), second_tx)
        .await;

    handler
        .handle_attached_live_input_for_test(first_pid, b"a")
        .await
        .expect("first client input");

    drain_lifecycle_hooks(&handler, &mut lifecycle).await;
    wait_for_buffer(&handler, "client-active", "active,").await;
}

#[tokio::test]
async fn live_attach_theme_reports_emit_client_theme_hooks() {
    let handler = RequestHandler::new();
    let alpha = session_name("live-attach-theme-hooks");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    set_global_hook(
        &handler,
        HookName::ClientDarkTheme,
        "set-buffer -a -b theme-hooks dark,",
    )
    .await;
    set_global_hook(
        &handler,
        HookName::ClientLightTheme,
        "set-buffer -a -b theme-hooks light,",
    )
    .await;
    let mut lifecycle = handler.subscribe_lifecycle_events();
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x1b[?997;1n\x1b[?997;2n")
        .await
        .expect("live attach theme reports");

    drain_lifecycle_hooks(&handler, &mut lifecycle).await;
    wait_for_buffer(&handler, "theme-hooks", "dark,light,").await;
}

#[tokio::test]
async fn live_attach_focus_sequences_forward_when_pane_focus_mode_is_enabled() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    {
        let mut state = handler.state.lock().await;
        state
            .append_bytes_to_pane_transcript_for_test(&alpha, 0, 0, b"\x1b[?1004h")
            .expect("focus mode transcript update");
    }

    let expected = b"\x1b[I\x1b[O";
    let capture =
        RawPaneInputProbe::start(&handler, &alpha, "live-attach-focus-mode", expected.len()).await;

    handler
        .handle_attached_live_input_for_test(requester_pid, expected)
        .await
        .expect("live attach focus mode input");

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, expected).await;
}

#[tokio::test]
async fn live_attach_fragmented_mouse_does_not_repeat_preceding_bytes() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let capture =
        RawPaneInputProbe::start(&handler, &alpha, "live-attach-fragmented-mouse-prefix", 1).await;
    let mut pending_input = Vec::new();
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"a\x1b[<0;12")
        .await
        .expect("partial SGR mouse with plain prefix");
    assert_eq!(pending_input, b"\x1b[<0;12");

    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b";34M")
        .await
        .expect("SGR mouse suffix");

    assert!(pending_input.is_empty());
    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, b"a").await;
}

#[tokio::test]
async fn live_attach_mouse_sequences_dispatch_default_mouse_bindings() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    enable_mouse(&handler).await;

    {
        let mut state = handler.state.lock().await;
        state
            .append_bytes_to_pane_transcript_for_test(&alpha, 0, 0, b"\x1b[?1002h")
            .expect("mouse motion mode transcript update");
    }

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let rebound = handler
        .handle(Request::BindKey(Box::new(BindKeyRequest {
            table_name: "root".to_owned(),
            key: "MouseDrag1Pane".to_owned(),
            note: Some("live-attach-mouse".to_owned()),
            repeat: false,
            command: Some(vec!["send-keys".to_owned(), "-M".to_owned()]),
        })))
        .await;
    assert!(matches!(rebound, Response::BindKey(_)));

    let expected = encode_mouse_event(
        mode::MODE_MOUSE_BUTTON,
        &MouseForwardEvent {
            b: 32,
            lb: 0,
            x: 1,
            y: 1,
            lx: 0,
            ly: 0,
            sgr_b: 32,
            sgr_type: 'M',
            ignore: false,
        },
        1,
        1,
    )
    .expect("mouse encodes");
    let capture =
        RawPaneInputProbe::start(&handler, &alpha, "live-attach-mouse", expected.len()).await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x1b[<32;2;2M")
        .await
        .expect("live attach mouse input");

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, &expected).await;

    let active_attach = handler.active_attach.lock().await;
    let event = active_attach
        .by_pid
        .get(&requester_pid)
        .and_then(|active| active.mouse.current_event.as_ref())
        .expect("current mouse event");
    assert_eq!(event.location, MouseLocation::Pane);
}

#[tokio::test]
async fn live_attach_forwards_pane_requested_mouse_when_mouse_option_is_off() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;

    {
        let mut state = handler.state.lock().await;
        state
            .append_bytes_to_pane_transcript_for_test(&alpha, 0, 0, b"\x1b[?1002h\x1b[?1006h")
            .expect("mouse motion mode transcript update");
    }

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let expected = b"\x1b[<32;2;2M";
    let capture = RawPaneInputProbe::start(
        &handler,
        &alpha,
        "live-attach-pane-mouse-off",
        expected.len(),
    )
    .await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x1b[<32;2;2M")
        .await
        .expect("live attach mouse input");

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, expected).await;

    let active_attach = handler.active_attach.lock().await;
    let event = active_attach
        .by_pid
        .get(&requester_pid)
        .and_then(|active| active.mouse.current_event.as_ref());
    assert!(event.is_none());
}

#[tokio::test]
async fn live_attach_focus_follows_active_pane_motion_when_mouse_option_is_off() {
    let handler = RequestHandler::new();
    let alpha = session_name("pane-requested-focus-follows-mouse");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    let split = handler
        .handle(Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Session(alpha.clone()),
            direction: SplitDirection::Horizontal,
            before: false,
            environment: None,
        }))
        .await;
    assert!(matches!(split, Response::SplitWindow(_)), "{split:?}");
    let selected = handler
        .handle(Request::SelectPane(Box::new(SelectPaneRequest {
            target: PaneTarget::new(alpha.clone(), 0),
            title: None,
            style: None,
            input_disabled: None,
            preserve_zoom: false,
        })))
        .await;
    assert!(matches!(selected, Response::SelectPane(_)), "{selected:?}");
    {
        let mut state = handler.state.lock().await;
        state
            .append_bytes_to_pane_transcript_for_test(&alpha, 0, 0, b"\x1b[?1003h\x1b[?1006h")
            .expect("pane all-motion mode transcript update");
    }
    let enabled = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::FocusFollowsMouse,
            value: "on".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(enabled, Response::SetOption(_)), "{enabled:?}");

    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach_with_terminal_context(
            requester_pid,
            alpha.clone(),
            control_tx,
            crate::outer_terminal::OuterTerminalContext::from_pairs(&[("TERM", "xterm-256color")]),
        )
        .await;
    handler
        .handle_attached_resize(
            requester_pid,
            TerminalSize {
                cols: 100,
                rows: 30,
            },
        )
        .await
        .expect("attached resize succeeds");
    while control_rx.try_recv().is_ok() {}
    handler.refresh_attached_session(&alpha).await;
    let tracking = tokio::time::timeout(Duration::from_secs(2), control_rx.recv())
        .await
        .expect("tracking refresh is bounded")
        .expect("attach control channel remains open");
    let crate::pane_io::AttachControl::Switch(tracking) = tracking else {
        panic!("expected tracking switch refresh, got {tracking:?}");
    };
    let tracking = tracking.into_target();
    let tracking_start = tracking.outer_terminal.attach_start_sequence();
    assert!(
        tracking_start
            .windows(b"\x1b[?1003h".len())
            .any(|window| window == b"\x1b[?1003h"),
        "the active pane's all-motion request must reach the outer terminal"
    );

    let mouse_move = {
        let state = handler.state.lock().await;
        let session = state.sessions.session(&alpha).expect("session exists");
        assert_eq!(session.window().active_pane_index(), 0);
        let pane = session.window().pane(1).expect("pane 1 exists");
        let x = pane.geometry().x().saturating_add(1);
        let y = pane.geometry().y().saturating_add(1);
        format!("\x1b[<35;{};{}M", x + 1, y + 1)
    };

    handler
        .handle_attached_live_input_for_test(requester_pid, mouse_move.as_bytes())
        .await
        .expect("pane-requested mouse motion succeeds");

    {
        let state = handler.state.lock().await;
        assert_eq!(
            state
                .sessions
                .session(&alpha)
                .expect("session exists")
                .window()
                .active_pane_index(),
            1
        );
    }
    let switched = tokio::time::timeout(Duration::from_secs(2), control_rx.recv())
        .await
        .expect("focus refresh is bounded")
        .expect("attach control channel remains open");
    let crate::pane_io::AttachControl::Switch(switched) = switched else {
        panic!("expected focus switch refresh, got {switched:?}");
    };
    let switched = switched.into_target();
    let transition = switched
        .outer_terminal
        .transition_sequence_from(&tracking.outer_terminal);
    assert!(
        transition
            .windows(b"\x1b[?1003l".len())
            .any(|window| window == b"\x1b[?1003l"),
        "moving to a pane without all-motion tracking must disable outer 1003"
    );
}

#[tokio::test]
async fn live_attach_ignores_mouse_when_option_and_pane_tracking_are_off() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    let capture = RawPaneInputProbe::start(&handler, &alpha, "live-attach-no-mouse", 0).await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x1b[<32;2;2M")
        .await
        .expect("live attach mouse input");

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, b"").await;
    let active_attach = handler.active_attach.lock().await;
    assert!(active_attach
        .by_pid
        .get(&requester_pid)
        .and_then(|active| active.mouse.current_event.as_ref())
        .is_none());
}

#[tokio::test]
async fn live_attach_mouse_down_selects_the_clicked_pane() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;

    let split = handler
        .handle(Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Session(alpha.clone()),
            direction: SplitDirection::Horizontal,
            before: false,
            environment: None,
        }))
        .await;
    assert!(matches!(split, Response::SplitWindow(_)));

    let selected = handler
        .handle(Request::SelectPane(Box::new(SelectPaneRequest {
            target: PaneTarget::new(alpha.clone(), 0),
            title: None,
            style: None,
            input_disabled: None,
            preserve_zoom: false,
        })))
        .await;
    assert!(matches!(selected, Response::SelectPane(_)));

    enable_mouse(&handler).await;

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let (click_x, click_y) = {
        let state = handler.state.lock().await;
        let session = state.sessions.session(&alpha).expect("session exists");
        let window = session.window();
        assert_eq!(window.active_pane_index(), 0);
        let pane = window.pane(1).expect("pane 1 exists");
        (
            pane.geometry().x().saturating_add(1),
            pane.geometry().y().saturating_add(1),
        )
    };
    let mouse_down = format!("\x1b[<0;{};{}M", click_x + 1, click_y + 1);

    handler
        .handle_attached_live_input_for_test(requester_pid, mouse_down.as_bytes())
        .await
        .expect("live attach mouse down input");

    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("session exists");
    assert_eq!(session.window().active_pane_index(), 1);
}

#[tokio::test]
async fn live_attach_focus_follows_mouse_selects_the_resized_hovered_pane_when_enabled() {
    let handler = RequestHandler::new();
    let alpha = session_name("focus-follows-mouse");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    let split = handler
        .handle(Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Session(alpha.clone()),
            direction: SplitDirection::Horizontal,
            before: false,
            environment: None,
        }))
        .await;
    assert!(matches!(split, Response::SplitWindow(_)), "{split:?}");
    let selected = handler
        .handle(Request::SelectPane(Box::new(SelectPaneRequest {
            target: PaneTarget::new(alpha.clone(), 0),
            title: None,
            style: None,
            input_disabled: None,
            preserve_zoom: false,
        })))
        .await;
    assert!(matches!(selected, Response::SelectPane(_)), "{selected:?}");
    enable_mouse(&handler).await;

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    handler
        .handle_attached_resize(
            requester_pid,
            TerminalSize {
                cols: 100,
                rows: 30,
            },
        )
        .await
        .expect("attached resize succeeds");

    let mouse_move = {
        let state = handler.state.lock().await;
        let session = state.sessions.session(&alpha).expect("session exists");
        let pane = session.window().pane(1).expect("pane 1 exists");
        let x = pane.geometry().x().saturating_add(1);
        let y = pane.geometry().y().saturating_add(1);
        format!("\x1b[<35;{};{}M", x + 1, y + 1)
    };

    handler
        .handle_attached_live_input_for_test(requester_pid, mouse_move.as_bytes())
        .await
        .expect("mouse move with focus disabled");
    {
        let state = handler.state.lock().await;
        assert_eq!(
            state
                .sessions
                .session(&alpha)
                .expect("session exists")
                .window()
                .active_pane_index(),
            0
        );
    }

    let enabled = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::FocusFollowsMouse,
            value: "on".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(enabled, Response::SetOption(_)), "{enabled:?}");
    handler
        .handle_attached_live_input_for_test(requester_pid, mouse_move.as_bytes())
        .await
        .expect("mouse move with focus enabled");

    let state = handler.state.lock().await;
    assert_eq!(
        state
            .sessions
            .session(&alpha)
            .expect("session exists")
            .window()
            .active_pane_index(),
        1
    );
}

#[tokio::test]
async fn stale_attached_mouse_focus_cannot_select_after_client_session_replacement() {
    let handler = RequestHandler::new();
    let alpha = session_name("stale-mouse-focus-alpha");
    let beta = session_name("stale-mouse-focus-beta");
    let requester_pid = u32::MAX - 91;

    create_send_keys_test_session(&handler, &alpha).await;
    create_send_keys_test_session(&handler, &beta).await;
    let split = handler
        .handle(Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Session(alpha.clone()),
            direction: SplitDirection::Horizontal,
            before: false,
            environment: None,
        }))
        .await;
    assert!(matches!(split, Response::SplitWindow(_)), "{split:?}");
    let selected = handler
        .handle(Request::SelectPane(Box::new(SelectPaneRequest {
            target: PaneTarget::new(alpha.clone(), 0),
            title: None,
            style: None,
            input_disabled: None,
            preserve_zoom: false,
        })))
        .await;
    assert!(matches!(selected, Response::SelectPane(_)), "{selected:?}");

    let (alpha_tx, _alpha_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, alpha.clone(), alpha_tx)
        .await;
    let stale_identity = handler.active_attach_identity_for_test(requester_pid).await;
    let (session_id, window_id, pane_id) = {
        let state = handler.state.lock().await;
        let session = state.sessions.session(&alpha).expect("alpha exists");
        (
            session.id(),
            session.window().id().as_u32(),
            session.window().pane(1).expect("pane 1 exists").id(),
        )
    };

    let (beta_tx, _beta_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, beta.clone(), beta_tx)
        .await;
    handler
        .select_attached_mouse_focus(stale_identity, &alpha, session_id, window_id, pane_id)
        .await
        .expect("stale focus is ignored");

    let state = handler.state.lock().await;
    assert_eq!(
        state
            .sessions
            .session(&alpha)
            .expect("alpha exists")
            .window()
            .active_pane_index(),
        0
    );
    let active_attach = handler.active_attach.lock().await;
    assert_eq!(
        active_attach
            .by_pid
            .get(&requester_pid)
            .expect("replacement attach exists")
            .session_name,
        beta
    );
}

/// Splits the window, keeps pane 0 active, and returns SGR mouse-down bytes
/// aimed at pane 1 so a custom MouseDown1Pane binding can be exercised.
async fn setup_two_pane_mouse_click(
    handler: &RequestHandler,
    alpha: &rmux_proto::SessionName,
    requester_pid: u32,
) -> (
    String,
    tokio::sync::mpsc::UnboundedReceiver<crate::pane_io::AttachControl>,
) {
    let split = handler
        .handle(Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Session(alpha.clone()),
            direction: SplitDirection::Horizontal,
            before: false,
            environment: None,
        }))
        .await;
    assert!(matches!(split, Response::SplitWindow(_)), "{split:?}");
    let selected = handler
        .handle(Request::SelectPane(Box::new(SelectPaneRequest {
            target: PaneTarget::new(alpha.clone(), 0),
            title: None,
            style: None,
            input_disabled: None,
            preserve_zoom: false,
        })))
        .await;
    assert!(matches!(selected, Response::SelectPane(_)), "{selected:?}");
    enable_mouse(handler).await;

    let (control_tx, control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let (click_x, click_y) = {
        let state = handler.state.lock().await;
        let session = state.sessions.session(alpha).expect("session exists");
        let window = session.window();
        assert_eq!(window.active_pane_index(), 0);
        let pane = window.pane(1).expect("pane 1 exists");
        (
            pane.geometry().x().saturating_add(1),
            pane.geometry().y().saturating_add(1),
        )
    };
    (
        format!("\x1b[<0;{};{}M", click_x + 1, click_y + 1),
        control_rx,
    )
}

#[tokio::test]
async fn live_attach_mouse_binding_executes_every_command_in_the_sequence() {
    // Issue #96 reports that a root mouse binding of the shape
    // `select-pane -t = \; <command>` runs select-pane but skips the tail on
    // a live Windows attach. Bind the exact shape and assert BOTH effects.
    let handler = RequestHandler::new();
    let alpha = session_name("mouse-binding-sequence");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    let rebound = handler
        .handle(Request::BindKey(Box::new(BindKeyRequest {
            table_name: "root".to_owned(),
            key: "MouseDown1Pane".to_owned(),
            note: Some("issue-96-sequence".to_owned()),
            repeat: false,
            command: Some(vec![
                "select-pane".to_owned(),
                "-t".to_owned(),
                "=".to_owned(),
                ";".to_owned(),
                "set-buffer".to_owned(),
                "-b".to_owned(),
                "mouse-hit".to_owned(),
                "clicked".to_owned(),
            ]),
        })))
        .await;
    assert!(matches!(rebound, Response::BindKey(_)), "{rebound:?}");

    let (mouse_down, control_rx) =
        setup_two_pane_mouse_click(&handler, &alpha, requester_pid).await;
    handler
        .handle_attached_live_input_for_test(requester_pid, mouse_down.as_bytes())
        .await
        .expect("live attach mouse down input");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let (active_pane, buffer) = {
            let state = handler.state.lock().await;
            let session = state.sessions.session(&alpha).expect("session exists");
            (
                session.window().active_pane_index(),
                state
                    .buffers
                    .show(Some("mouse-hit"))
                    .ok()
                    .map(|(_, contents)| contents.to_vec()),
            )
        };
        if active_pane == 1 && buffer.as_deref() == Some(b"clicked".as_slice()) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "mouse binding sequence incomplete: active_pane={active_pane}, buffer={buffer:?}"
        );
        sleep(Duration::from_millis(10)).await;
    }
    drop(control_rx);
}

#[tokio::test]
async fn live_attach_mouse_binding_switch_client_rebases_its_command_queue() {
    let handler = RequestHandler::new();
    let alpha = session_name("mouse-switch-alpha");
    let beta = session_name("mouse-switch-beta");
    let requester_pid = u32::MAX - 77;

    create_send_keys_test_session(&handler, &alpha).await;
    create_send_keys_test_session(&handler, &beta).await;
    let rebound = handler
        .handle(Request::BindKey(Box::new(BindKeyRequest {
            table_name: "root".to_owned(),
            key: "MouseDown1Pane".to_owned(),
            note: Some("mouse-switch-client-queue".to_owned()),
            repeat: false,
            command: Some(vec![
                "switch-client".to_owned(),
                "-t".to_owned(),
                beta.to_string(),
                ";".to_owned(),
                "set-buffer".to_owned(),
                "-b".to_owned(),
                "mouse-switch-tail".to_owned(),
                "done".to_owned(),
            ]),
        })))
        .await;
    assert!(matches!(rebound, Response::BindKey(_)), "{rebound:?}");

    let (mouse_down, control_rx) =
        setup_two_pane_mouse_click(&handler, &alpha, requester_pid).await;
    handler
        .handle_attached_live_input_for_test(requester_pid, mouse_down.as_bytes())
        .await
        .expect("live attach mouse switch binding");

    wait_for_buffer(&handler, "mouse-switch-tail", "done").await;
    let active_attach = handler.active_attach.lock().await;
    let active = active_attach
        .by_pid
        .get(&requester_pid)
        .expect("attached client remains registered");
    assert_eq!(active.session_name, beta);
    drop(control_rx);
}

#[tokio::test]
async fn live_attach_mouse_binding_run_shell_tail_writes_its_file() {
    // Issue #96's reporter bound `select-pane -t = \; run-shell "... > file"`
    // and saw no file. Reproduce that exact shape and require the run-shell
    // side effect to land on disk.
    let handler = RequestHandler::new();
    let alpha = session_name("mouse-binding-run-shell");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    #[cfg(windows)]
    {
        let system_root = std::env::var_os("SystemRoot")
            .unwrap_or_else(|| std::ffi::OsString::from(r"C:\Windows"));
        let powershell = std::path::PathBuf::from(system_root)
            .join("System32")
            .join("WindowsPowerShell")
            .join("v1.0")
            .join("powershell.exe")
            .to_string_lossy()
            .into_owned();
        let mut state = handler.state.lock().await;
        state
            .options
            .set(
                ScopeSelector::Global,
                OptionName::DefaultShell,
                powershell,
                SetOptionMode::Replace,
            )
            .expect("Windows test default-shell is valid");
    }
    let root = std::env::temp_dir().join(format!(
        "rmux-mouse-run-shell-{}-{requester_pid}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("run-shell temp root");
    let output_path = root.join("mouse-hit.txt");
    let _ = std::fs::remove_file(&output_path);
    #[cfg(unix)]
    let shell_command = format!(
        "printf %s hit > {}",
        crate::test_shell::sh_quote_path(&output_path)
    );
    #[cfg(windows)]
    let shell_command = format!(
        "[IO.File]::WriteAllText({}, 'hit', [Text.UTF8Encoding]::new($false))",
        crate::test_shell::powershell_quote_path(&output_path)
    );

    let rebound = handler
        .handle(Request::BindKey(Box::new(BindKeyRequest {
            table_name: "root".to_owned(),
            key: "MouseDown1Pane".to_owned(),
            note: Some("issue-96-run-shell".to_owned()),
            repeat: false,
            command: Some(vec![
                "select-pane".to_owned(),
                "-t".to_owned(),
                "=".to_owned(),
                ";".to_owned(),
                "run-shell".to_owned(),
                "-b".to_owned(),
                shell_command,
            ]),
        })))
        .await;
    assert!(matches!(rebound, Response::BindKey(_)), "{rebound:?}");

    let (mouse_down, control_rx) =
        setup_two_pane_mouse_click(&handler, &alpha, requester_pid).await;
    handler
        .handle_attached_live_input_for_test(requester_pid, mouse_down.as_bytes())
        .await
        .expect("live attach mouse down input");

    let deadline = tokio::time::Instant::now() + BACKGROUND_RUN_SHELL_TIMEOUT;
    loop {
        {
            let state = handler.state.lock().await;
            let session = state.sessions.session(&alpha).expect("session exists");
            if session.window().active_pane_index() == 1 {
                // select-pane ran; keep polling for the run-shell tail below.
            }
        }
        match std::fs::read_to_string(&output_path) {
            Ok(contents) if contents == "hit" => break,
            Ok(contents) => assert!(
                tokio::time::Instant::now() < deadline,
                "run-shell tail wrote unexpected contents {contents:?}"
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => assert!(
                tokio::time::Instant::now() < deadline,
                "run-shell tail never wrote {output_path:?}: the mouse binding skipped it"
            ),
            Err(error) => panic!("reading {output_path:?}: {error}"),
        }
        sleep(Duration::from_millis(25)).await;
    }
    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("session exists");
    assert_eq!(
        session.window().active_pane_index(),
        1,
        "select-pane head of the sequence must also run"
    );
    drop(control_rx);
}

#[tokio::test]
async fn live_attach_mouse_pane_transition_retargets_same_chunk_focus_tail() {
    let handler = RequestHandler::new();
    let alpha = session_name("mouse-pane-focus-tail");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    let split = handler
        .handle(Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Session(alpha.clone()),
            direction: SplitDirection::Horizontal,
            before: false,
            environment: None,
        }))
        .await;
    assert!(matches!(split, Response::SplitWindow(_)), "{split:?}");

    let first = PaneTarget::with_window(alpha.clone(), 0, 0);
    let second = PaneTarget::with_window(alpha.clone(), 0, 1);
    let selected = handler
        .handle(Request::SelectPane(Box::new(SelectPaneRequest {
            target: first.clone(),
            title: None,
            style: None,
            input_disabled: None,
            preserve_zoom: false,
        })))
        .await;
    assert!(matches!(selected, Response::SelectPane(_)), "{selected:?}");
    enable_mouse(&handler).await;

    let (click_x, click_y) = {
        let mut state = handler.state.lock().await;
        state
            .append_bytes_to_pane_transcript_for_test(&alpha, 0, 1, b"\x1b[?1004h")
            .expect("second pane enables focus events");
        state.start_pane_input_capture_for_test(&first);
        state.start_pane_input_capture_for_test(&second);
        let pane = state
            .sessions
            .session(&alpha)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(1))
            .expect("second pane exists");
        (
            pane.geometry().x().saturating_add(1),
            pane.geometry().y().saturating_add(1),
        )
    };
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    let mut lifecycle = handler.subscribe_lifecycle_events();

    let mouse_down = format!("\x1b[<0;{};{}M", click_x + 1, click_y + 1);
    let mut input = mouse_down.into_bytes();
    input.extend_from_slice(b"\x1b[I");
    let mut pending_input = Vec::new();
    let forwarded = handler
        .handle_attached_live_input_inner(requester_pid, &mut pending_input, &input)
        .await
        .expect("mouse pane transition succeeds");

    assert!(forwarded);
    assert!(pending_input.is_empty());
    {
        let state = handler.state.lock().await;
        assert_eq!(
            state
                .sessions
                .session(&alpha)
                .expect("session exists")
                .window_at(0)
                .expect("window exists")
                .active_pane_index(),
            1
        );
        assert_eq!(state.pane_input_capture_for_test(&first), Some(Vec::new()));
        assert_eq!(
            state.pane_input_capture_for_test(&second),
            Some(b"\x1b[I".to_vec())
        );
    }

    let mut pane_focus_in_targets = Vec::new();
    while let Ok(event) = lifecycle.try_recv() {
        if let LifecycleEvent::PaneFocusIn { target } = event.event {
            pane_focus_in_targets.push(target);
        }
    }
    assert_eq!(pane_focus_in_targets, vec![second]);
}

#[tokio::test]
async fn live_attach_mouse_border_drag_pipeline_preserves_mouse_event() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    enable_mouse(&handler).await;

    let split = handler
        .handle(Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Session(alpha.clone()),
            direction: SplitDirection::Horizontal,
            before: false,
            environment: None,
        }))
        .await;
    assert!(matches!(split, Response::SplitWindow(_)));

    let rebound = handler
        .handle(Request::BindKey(Box::new(BindKeyRequest {
            table_name: "root".to_owned(),
            key: "MouseDrag1Border".to_owned(),
            note: Some("live-border-drag-pipeline".to_owned()),
            repeat: false,
            command: Some(vec!["display-message dragged ; resize-pane -M".to_owned()]),
        })))
        .await;
    assert!(matches!(rebound, Response::BindKey(_)));

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let (border_x, y, before_width) = {
        let state = handler.state.lock().await;
        let session = state.sessions.session(&alpha).expect("session exists");
        let pane = session.window().pane(0).expect("left pane exists");
        (
            pane.geometry().x().saturating_add(pane.geometry().cols()),
            pane.geometry().y().saturating_add(1),
            pane.geometry().cols(),
        )
    };
    let drag = format!(
        "\x1b[<0;{};{}M\x1b[<32;{};{}M\x1b[<0;{};{}m",
        border_x.saturating_add(1),
        y.saturating_add(1),
        border_x.saturating_add(6),
        y.saturating_add(1),
        border_x.saturating_add(6),
        y.saturating_add(1),
    );

    handler
        .handle_attached_live_input_for_test(requester_pid, drag.as_bytes())
        .await
        .expect("live attach border drag pipeline input");

    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("session exists");
    let pane = session.window().pane(0).expect("left pane exists");
    assert!(
        pane.geometry().cols() > before_width,
        "MouseDrag1Border pipeline must preserve mouse_event before={before_width} after={}",
        pane.geometry().cols()
    );
}

#[tokio::test]
async fn live_attach_sgr_wheel_forwards_when_pane_mouse_any_is_enabled() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    enable_mouse(&handler).await;

    {
        let mut state = handler.state.lock().await;
        state
            .append_bytes_to_pane_transcript_for_test(&alpha, 0, 0, b"\x1b[?1003h\x1b[?1006h")
            .expect("mouse any and sgr transcript update");
    }

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

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
    let capture =
        RawPaneInputProbe::start(&handler, &alpha, "live-attach-sgr-wheel", expected.len()).await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x1b[<64;2;2M")
        .await
        .expect("live attach wheel input");

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, &expected).await;

    let active_attach = handler.active_attach.lock().await;
    let event = active_attach
        .by_pid
        .get(&requester_pid)
        .and_then(|active| active.mouse.current_event.as_ref())
        .expect("current wheel event");
    assert_eq!(event.location, MouseLocation::Pane);
    assert_eq!(event.raw.b, 64);
    drop(active_attach);

    let state = handler.state.lock().await;
    assert!(
        state
            .pane_copy_mode_summary(&alpha, PaneId::new(0))
            .is_none(),
        "mouse-aware applications should receive the wheel event without entering copy-mode"
    );
}

#[tokio::test]
async fn live_attach_default_wheel_binding_enters_copy_mode() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    enable_mouse(&handler).await;

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x1b[<64;2;2M")
        .await
        .expect("live attach wheel input");

    let summary = {
        let state = handler.state.lock().await;
        state.pane_copy_mode_summary(&alpha, PaneId::new(0))
    };
    let summary =
        summary.expect("default WheelUpPane binding should enter copy-mode when mouse is on");
    assert!(
        !summary.line_numbers_enabled,
        "mouse-triggered copy-mode entry should hide line numbers like tmux"
    );

    let target = PaneTarget::new(alpha.clone(), 0);
    assert!(matches!(
        handler
            .handle(Request::CopyMode(CopyModeRequest {
                target: Some(target),
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
    let summary = {
        let state = handler.state.lock().await;
        state
            .pane_copy_mode_summary(&alpha, PaneId::new(0))
            .expect("copy mode summary")
    };
    assert!(
        summary.line_numbers_enabled,
        "a later non-mouse copy-mode invocation should restore line numbers"
    );
}

#[tokio::test]
async fn live_attach_wheel_copy_mode_transition_reroutes_same_chunk_paste() {
    let handler = RequestHandler::new();
    let alpha = session_name("wheel-copy-tail");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    enable_mouse(&handler).await;
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let capture = RawPaneInputProbe::start(&handler, &alpha, "wheel-copy-tail", 0).await;
    let mut pending_input = Vec::new();
    let forwarded = handler
        .handle_attached_live_input_inner(
            requester_pid,
            &mut pending_input,
            b"\x1b[<64;2;2M\x1b[200~MARKER\n\x1b[201~",
        )
        .await
        .expect("wheel copy-mode transition succeeds");

    assert!(!forwarded);
    assert!(pending_input.is_empty());
    assert!(handler
        .target_is_in_copy_mode(&PaneTarget::new(alpha.clone(), 0))
        .await
        .expect("copy-mode state resolves"));
    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, b"").await;
}

#[tokio::test]
async fn live_attach_second_click_dispatches_double_click_after_timer() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    enable_mouse(&handler).await;

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let rebound = handler
        .handle(Request::BindKey(Box::new(BindKeyRequest {
            table_name: "root".to_owned(),
            key: "DoubleClick1Pane".to_owned(),
            note: Some("double-click-timer".to_owned()),
            repeat: false,
            command: Some(vec![
                "set-buffer".to_owned(),
                "-b".to_owned(),
                "double-click-timer".to_owned(),
                "ok".to_owned(),
            ]),
        })))
        .await;
    assert!(matches!(rebound, Response::BindKey(_)));

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x1b[<0;2;2M")
        .await
        .expect("first click");
    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x1b[<0;2;2M")
        .await
        .expect("second click");
    tokio::time::sleep(Duration::from_millis(350)).await;

    let contents = {
        let state = handler.state.lock().await;
        state.buffers.get("double-click-timer").map(Vec::from)
    };
    assert_eq!(contents.as_deref(), Some(b"ok".as_slice()));
}

async fn bind_double_click_timer_buffer(handler: &RequestHandler, buffer_name: &str) {
    let rebound = handler
        .handle(Request::BindKey(Box::new(BindKeyRequest {
            table_name: "root".to_owned(),
            key: "DoubleClick1Pane".to_owned(),
            note: Some(buffer_name.to_owned()),
            repeat: false,
            command: Some(vec![
                "set-buffer".to_owned(),
                "-b".to_owned(),
                buffer_name.to_owned(),
                "ok".to_owned(),
            ]),
        })))
        .await;
    assert!(matches!(rebound, Response::BindKey(_)), "{rebound:?}");
}

async fn arm_attached_double_click_timer(handler: &RequestHandler, requester_pid: u32) {
    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x1b[<0;2;2M")
        .await
        .expect("first click");
    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x1b[<0;2;2M")
        .await
        .expect("second click");
}

async fn buffer_contents(handler: &RequestHandler, buffer_name: &str) -> Option<Vec<u8>> {
    let state = handler.state.lock().await;
    state.buffers.get(buffer_name).map(Vec::from)
}

#[tokio::test]
async fn normal_shutdown_cancels_pending_attached_mouse_click_timer() {
    let handler = RequestHandler::new();
    let alpha = session_name("mouse-timer-shutdown");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    enable_mouse(&handler).await;
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, alpha, control_tx)
        .await;
    bind_double_click_timer_buffer(&handler, "mouse-timer-shutdown").await;
    arm_attached_double_click_timer(&handler, requester_pid).await;

    tokio::time::timeout(
        Duration::from_secs(2),
        handler.close_normal_and_drain_lifecycle_producers(),
    )
    .await
    .expect("normal producer drain cancels the pending click timer");
    sleep(Duration::from_millis(350)).await;

    assert_eq!(
        buffer_contents(&handler, "mouse-timer-shutdown").await,
        None,
        "a click timer cannot dispatch after the normal lane closes"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn attached_mouse_click_timer_drains_only_its_local_mutation() {
    let handler = RequestHandler::new();
    let alpha = session_name("mouse-timer-boundary");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    enable_mouse(&handler).await;
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, alpha, control_tx)
        .await;
    bind_double_click_timer_buffer(&handler, "mouse-timer-boundary").await;
    let pause = handler.install_attached_mouse_timer_pause();
    arm_attached_double_click_timer(&handler, requester_pid).await;

    let reached_pause = pause.clone();
    tokio::time::timeout(
        Duration::from_secs(2),
        tokio::task::spawn_blocking(move || {
            reached_pause.mutation_reached.wait();
        }),
    )
    .await
    .expect("click timer reaches its local mutation")
    .expect("mutation waiter joins");

    let close_handler = handler.clone();
    let close = tokio::spawn(async move {
        close_handler
            .close_normal_and_drain_lifecycle_producers()
            .await;
    });
    handler
        .wait_until_normal_lifecycle_producers_closing_for_test()
        .await;
    assert!(
        !close.is_finished(),
        "shutdown must wait while the click timer owns its local mutation"
    );

    let release_pause = pause.clone();
    tokio::time::timeout(
        Duration::from_secs(2),
        tokio::task::spawn_blocking(move || {
            release_pause.mutation_release.wait();
        }),
    )
    .await
    .expect("local click mutation is released")
    .expect("mutation releaser joins");
    tokio::time::timeout(Duration::from_secs(2), pause.dispatch_reached.notified())
        .await
        .expect("expired click reaches the unguarded dispatch boundary");
    tokio::time::timeout(Duration::from_secs(2), close)
        .await
        .expect("shutdown cancels blocked dispatch without draining it")
        .expect("normal producer drain task joins");

    assert_eq!(
        buffer_contents(&handler, "mouse-timer-boundary").await,
        None,
        "the cancelled dispatch cannot execute its binding"
    );
    pause.dispatch_release.notify_one();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_attached_mouse_click_timer_cannot_mutate_same_pid_replacement() {
    let handler = RequestHandler::new();
    let alpha = session_name("mouse-timer-stale-alpha");
    let beta = session_name("mouse-timer-stale-beta");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    create_send_keys_test_session(&handler, &beta).await;
    enable_mouse(&handler).await;
    let (alpha_tx, _alpha_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, alpha.clone(), alpha_tx)
        .await;
    bind_double_click_timer_buffer(&handler, "mouse-timer-stale").await;
    let pause = handler.install_attached_mouse_timer_pause();
    arm_attached_double_click_timer(&handler, requester_pid).await;

    let alpha_target = PaneTarget::new(alpha, 0);
    let clock_mode = handler
        .handle(Request::ClockMode(rmux_proto::ClockModeRequest {
            target: Some(alpha_target.clone()),
        }))
        .await;
    assert!(
        matches!(clock_mode, Response::ClockMode(_)),
        "{clock_mode:?}"
    );

    let reached_pause = pause.clone();
    tokio::time::timeout(
        Duration::from_secs(2),
        tokio::task::spawn_blocking(move || {
            reached_pause.mutation_reached.wait();
        }),
    )
    .await
    .expect("stale click timer reaches its local mutation")
    .expect("mutation waiter joins");
    let release_pause = pause.clone();
    tokio::time::timeout(
        Duration::from_secs(2),
        tokio::task::spawn_blocking(move || {
            release_pause.mutation_release.wait();
        }),
    )
    .await
    .expect("stale click mutation is released")
    .expect("mutation releaser joins");
    tokio::time::timeout(Duration::from_secs(2), pause.dispatch_reached.notified())
        .await
        .expect("stale click reaches dispatch after its local mutation");

    let (beta_tx, _beta_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, beta.clone(), beta_tx)
        .await;
    pause.dispatch_release.notify_one();
    tokio::time::timeout(Duration::from_secs(2), pause.task_completed.notified())
        .await
        .expect("stale click dispatch completes after replacement");

    assert_eq!(
        buffer_contents(&handler, "mouse-timer-stale").await,
        None,
        "the stale timer cannot dispatch against a replacement attach"
    );
    let active_attach = handler.active_attach.lock().await;
    let replacement = active_attach
        .by_pid
        .get(&requester_pid)
        .expect("replacement attach remains active");
    assert_eq!(replacement.session_name, beta);
    assert_eq!(
        replacement.mouse.click_deadline(),
        None,
        "the stale timer cannot change replacement mouse state"
    );
    drop(active_attach);
    assert!(
        handler
            .target_is_in_clock_mode(&alpha_target)
            .await
            .expect("original clock-mode target resolves"),
        "stale mouse dispatch cannot exit clock mode in the original session"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn attached_mouse_click_timer_follows_same_session_identity_rename() {
    let handler = RequestHandler::new();
    let alpha = session_name("mouse-timer-rename-alpha");
    let renamed = session_name("mouse-timer-rename-beta");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    enable_mouse(&handler).await;
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    bind_double_click_timer_buffer(&handler, "mouse-timer-rename").await;
    let pause = handler.install_attached_mouse_timer_pause();
    arm_attached_double_click_timer(&handler, requester_pid).await;

    let response = handler
        .handle(Request::RenameSession(rmux_proto::RenameSessionRequest {
            target: alpha,
            new_name: renamed.clone(),
        }))
        .await;
    assert!(
        matches!(response, Response::RenameSession(_)),
        "{response:?}"
    );

    let reached_pause = pause.clone();
    tokio::time::timeout(
        Duration::from_secs(2),
        tokio::task::spawn_blocking(move || {
            reached_pause.mutation_reached.wait();
        }),
    )
    .await
    .expect("renamed click timer reaches its local mutation")
    .expect("mutation waiter joins");
    let release_pause = pause.clone();
    tokio::time::timeout(
        Duration::from_secs(2),
        tokio::task::spawn_blocking(move || {
            release_pause.mutation_release.wait();
        }),
    )
    .await
    .expect("renamed click mutation is released")
    .expect("mutation releaser joins");
    tokio::time::timeout(Duration::from_secs(2), pause.dispatch_reached.notified())
        .await
        .expect("renamed click reaches dispatch");

    {
        let active_attach = handler.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get(&requester_pid)
            .expect("renamed attach remains active");
        assert_eq!(active.session_name, renamed);
        assert_eq!(active.mouse.click_deadline(), None);
        assert!(!active.mouse.double_click_pending);
        assert!(!active.mouse.triple_click_pending);
        assert!(active.mouse.click_event.is_none());
    }

    pause.dispatch_release.notify_one();
    tokio::time::timeout(Duration::from_secs(2), pause.task_completed.notified())
        .await
        .expect("renamed click dispatch completes");
    assert_eq!(
        buffer_contents(&handler, "mouse-timer-rename").await,
        Some(b"ok".to_vec()),
        "the delayed binding follows the stable session identity through rename"
    );
}

#[tokio::test]
async fn attached_session_switch_resets_click_state_without_clearing_a_new_timer() {
    let handler = RequestHandler::new();
    let alpha = session_name("mouse-timer-switch-alpha");
    let beta = session_name("mouse-timer-switch-beta");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    create_send_keys_test_session(&handler, &beta).await;
    enable_mouse(&handler).await;
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, alpha, control_tx)
        .await;
    let identity = handler.active_attach_identity_for_test(requester_pid).await;
    arm_attached_double_click_timer(&handler, requester_pid).await;
    let old_token = {
        let active_attach = handler.active_attach.lock().await;
        active_attach
            .by_pid
            .get(&requester_pid)
            .and_then(|active| active.mouse.click_timer_token())
            .expect("old session click timer is armed")
    };

    let response = handler
        .handle(Request::SwitchClientExt(SwitchClientExtRequest {
            target: Some(beta.clone()),
            key_table: None,
        }))
        .await;
    assert!(
        matches!(response, Response::SwitchClient(_)),
        "{response:?}"
    );
    {
        let active_attach = handler.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get(&requester_pid)
            .expect("switched attach remains active");
        assert_eq!(active.session_name, beta);
        assert_eq!(active.mouse.click_deadline(), None);
        assert!(!active.mouse.double_click_pending);
        assert!(!active.mouse.triple_click_pending);
        assert!(active.mouse.click_event.is_none());
    }

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x1b[<0;2;2M")
        .await
        .expect("new session click arms its own timer");
    let new_token = {
        let active_attach = handler.active_attach.lock().await;
        active_attach
            .by_pid
            .get(&requester_pid)
            .and_then(|active| active.mouse.click_timer_token())
            .expect("new session click timer is armed")
    };
    assert_ne!(new_token, old_token);

    handler
        .dispatch_expired_attached_mouse_click_for_test(identity, old_token)
        .await
        .expect("obsolete timer invocation is ignored");
    {
        let active_attach = handler.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get(&requester_pid)
            .expect("switched attach remains active");
        assert_eq!(active.mouse.click_timer_token(), Some(new_token));
        assert!(active.mouse.double_click_pending);
        assert!(!active.mouse.triple_click_pending);
        assert!(active.mouse.click_event.is_some());
    }

    handler.close_normal_and_drain_lifecycle_producers().await;
}

#[cfg(unix)]
fn mouse_word_pane_command() -> Vec<String> {
    [
        "/bin/sh",
        "-c",
        "printf 'alpha beta gamma\\n'; exec sleep 60",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

#[cfg(windows)]
fn mouse_word_pane_command() -> Vec<String> {
    let system_root =
        std::env::var_os("SystemRoot").unwrap_or_else(|| std::ffi::OsString::from(r"C:\Windows"));
    let cmd = std::path::PathBuf::from(system_root)
        .join("System32")
        .join("cmd.exe");
    vec![
        cmd.to_string_lossy().into_owned(),
        "/d".to_owned(),
        "/q".to_owned(),
        "/c".to_owned(),
        "echo alpha beta gamma & ping -n 120 127.0.0.1 >NUL".to_owned(),
    ]
}

async fn create_mouse_word_session(handler: &RequestHandler, session: &rmux_proto::SessionName) {
    let created = handler
        .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
            session_name: Some(session.clone()),
            working_directory: None,
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
            group_target: None,
            attach_if_exists: false,
            detach_other_clients: false,
            kill_other_clients: false,
            flags: None,
            window_name: None,
            print_session_info: false,
            print_format: None,
            command: Some(mouse_word_pane_command()),
            process_command: None,
            client_environment: None,
            skip_environment_update: false,
        })))
        .await;
    assert!(matches!(created, Response::NewSession(_)), "{created:?}");
    handler
        .wait_for_pane_startup_to_finish_for_test(&PaneTarget::new(session.clone(), 0))
        .await;
}

async fn wait_for_pane_text(handler: &RequestHandler, target: &PaneTarget, marker: &str) {
    let transcript = {
        let state = handler.state.lock().await;
        state
            .transcript_handle(target)
            .expect("pane transcript exists")
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut last = Vec::new();
    while tokio::time::Instant::now() < deadline {
        last = transcript
            .lock()
            .expect("pane transcript mutex is not poisoned")
            .screen()
            .render_visible_line_independent(0, rmux_core::GridRenderOptions::default())
            .unwrap_or_default();
        if String::from_utf8_lossy(&last).contains(marker) {
            return;
        }
        sleep(Duration::from_millis(10)).await;
    }
    panic!(
        "pane output never contained {marker:?}; last={:?}",
        String::from_utf8_lossy(&last)
    );
}

#[tokio::test]
async fn live_attach_default_double_click_copies_word_from_mouse_pane() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    // Source the selectable text from the real pane output stream. In
    // particular, a Windows ConPTY can publish its initial blank frame after
    // terminal installation, so direct transcript injection would race it.
    create_mouse_word_session(&handler, &alpha).await;
    let target = PaneTarget::new(alpha.clone(), 0);
    wait_for_pane_text(&handler, &target, "alpha beta gamma").await;
    enable_mouse(&handler).await;

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x1b[<0;2;1M")
        .await
        .expect("first click");
    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x1b[<0;2;1M")
        .await
        .expect("second click");

    // The second click arms the double-click timer; the yank lands once it
    // fires. Content is now deterministic (inert pane), so poll for the copy
    // instead of sleeping a fixed interval that jitter can outrun.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    let copied = loop {
        let current = {
            let state = handler.state.lock().await;
            state
                .buffers
                .top_unnamed()
                .and_then(|name| state.buffers.get(name))
                .map(Vec::from)
        };
        if current.as_deref() == Some(b"alpha".as_slice())
            || tokio::time::Instant::now() >= deadline
        {
            break current;
        }
        sleep(Duration::from_millis(10)).await;
    };
    assert_eq!(copied.as_deref(), Some(b"alpha".as_slice()));
}

#[tokio::test]
async fn live_attach_sgr_motion_forwards_without_explicit_binding_when_mouse_all_is_enabled() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    enable_mouse(&handler).await;

    {
        let mut state = handler.state.lock().await;
        state
            .append_bytes_to_pane_transcript_for_test(&alpha, 0, 0, b"\x1b[?1003h\x1b[?1006h")
            .expect("mouse all and sgr transcript update");
    }

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let expected = b"\x1b[<35;2;2M";
    let capture =
        RawPaneInputProbe::start(&handler, &alpha, "live-attach-sgr-motion", expected.len()).await;

    handler
        .handle_attached_live_input_for_test(requester_pid, expected)
        .await
        .expect("live attach motion input");

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, expected).await;
}

#[tokio::test]
async fn read_only_live_attach_drops_mouse_forwarding() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    enable_mouse(&handler).await;

    {
        let mut state = handler.state.lock().await;
        state
            .append_bytes_to_pane_transcript_for_test(&alpha, 0, 0, b"\x1b[?1003h\x1b[?1006h")
            .expect("mouse all and sgr transcript update");
    }

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    {
        let mut active_attach = handler.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get_mut(&requester_pid)
            .expect("attach is active");
        active
            .flags
            .insert(crate::client_flags::ClientFlags::READONLY);
    }

    let capture = RawPaneInputProbe::start(&handler, &alpha, "read-only-mouse-forwarding", 0).await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x1b[<35;2;2M")
        .await
        .expect("read-only live attach mouse input");

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, b"").await;
}

#[tokio::test]
async fn live_attach_sgr_release_forwards_without_explicit_binding_when_mouse_all_is_enabled() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    enable_mouse(&handler).await;

    {
        let mut state = handler.state.lock().await;
        state
            .append_bytes_to_pane_transcript_for_test(&alpha, 0, 0, b"\x1b[?1003h\x1b[?1006h")
            .expect("mouse all and sgr transcript update");
    }

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let expected = b"\x1b[<0;2;2m";
    let capture =
        RawPaneInputProbe::start(&handler, &alpha, "live-attach-sgr-release", expected.len()).await;

    handler
        .handle_attached_live_input_for_test(requester_pid, expected)
        .await
        .expect("live attach release input");

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, expected).await;
}

#[tokio::test]
async fn live_attach_manual_prompt_drag_sequence_does_not_error() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha, control_tx)
        .await;

    let result = handler
        .handle_attached_live_input_for_test(
            requester_pid,
            b"\x1b[<0;7;1M\x1b[<32;9;1M\x1b[<32;10;1M",
        )
        .await;
    assert!(result.is_ok(), "{result:?}");
}
