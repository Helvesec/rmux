//! End-to-end coverage of the passthrough input fast path.
//!
//! These tests exercise [`handle_attached_live_input_inner`] for sessions
//! created with `passthrough: true`. They cover:
//!   * no recursion across the dispatch gate
//!   * prefix bindings (Ctrl-B + n / 1 / w) still dispatch despite the
//!     bypass for raw bytes
//!   * mixed raw + prefix in a single batch
//!
//! Anything the harness catches here is something the user otherwise
//! has to find by hand — see the manual reports of "Ctrl-B doesn't do
//! anything" that motivated this file.

use std::time::Duration;

use super::*;
use rmux_proto::SelectWindowRequest;

/// Create a session with `passthrough: true` and register an attach.
/// Mirrors the existing `create_attached_session` helper but flips the
/// passthrough flag and pins a quiet test shell so the pane doesn't
/// race against this test by emitting prompt bytes.
async fn create_passthrough_attached_session(
    handler: &RequestHandler,
    requester_pid: u32,
    session: &SessionName,
) -> mpsc::UnboundedReceiver<AttachControl> {
    let response = handler
        .handle(Request::NewSessionExt(NewSessionExtRequest {
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
            command: Some(vec![
                "/bin/sh".to_owned(),
                "-c".to_owned(),
                "sleep 60".to_owned(),
            ]),
            process_command: None,
            passthrough: true,
            client_environment: None,
        }))
        .await;
    assert!(
        matches!(response, Response::NewSession(_)),
        "passthrough session should be created, got {response:?}"
    );
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, session.clone(), control_tx)
        .await;
    control_rx
}

/// Regression for the SIGABRT on first Ctrl-B in a passthrough attach
/// (mutual recursion: passthrough handler → inner → passthrough
/// handler …). Times out at 5s if the gate gets removed in future.
#[tokio::test]
async fn passthrough_prefix_byte_does_not_recurse_into_passthrough_handler() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let _control_rx = create_passthrough_attached_session(&handler, requester_pid, &alpha).await;

    let mut pending_input = Vec::new();
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        handler.handle_attached_live_input_inner(requester_pid, &mut pending_input, b"\x02"),
    )
    .await;

    match result {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => panic!("input pipeline returned an error: {error}"),
        Err(_) => panic!(
            "input pipeline did not return within 5s — the passthrough \
             handler likely recursed back through \
             handle_attached_live_input_inner. Make sure the dispatch \
             gate (`allow_passthrough_dispatch=false`) is set when the \
             passthrough handler defers prefix bytes to the legacy \
             decoder."
        ),
    }
}

/// Same recursion check, with raw bytes preceding the prefix in the
/// same batch — the realistic case (user typed `hello` then `Ctrl-B`).
#[tokio::test]
async fn passthrough_mixed_raw_then_prefix_does_not_recurse() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let _control_rx = create_passthrough_attached_session(&handler, requester_pid, &alpha).await;

    let mut pending_input = Vec::new();
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        handler.handle_attached_live_input_inner(
            requester_pid,
            &mut pending_input,
            b"hello\x02",
        ),
    )
    .await;
    assert!(
        matches!(result, Ok(Ok(_))),
        "mixed raw+prefix input should complete without recursion: {result:?}"
    );
}

/// Prefix bindings must still dispatch even when the session is
/// passthrough — that's the whole point of intercepting the prefix
/// byte rather than passing it through. This test asserts that
/// `Ctrl-B n` (next-window) actually moves the active window in
/// passthrough mode.
#[tokio::test]
async fn passthrough_prefix_next_window_dispatches() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let _control_rx = create_passthrough_attached_session(&handler, requester_pid, &alpha).await;

    // Add a second window so next-window has somewhere to go.
    assert!(matches!(
        handler
            .handle(Request::NewWindow(NewWindowRequest {
                target: alpha.clone(),
                name: None,
                detached: true,
                start_directory: None,
                environment: None,
                command: None,
                target_window_index: None,
                insert_at_target: false,
            }))
            .await,
        Response::NewWindow(_)
    ));

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02n")
        .await
        .expect("prefix n input must dispatch");

    assert_eq!(
        active_windows(&handler, &alpha).await,
        "0:0\n1:1\n",
        "in passthrough mode, Ctrl-B n must still trigger the \
         next-window binding"
    );
}

/// `Ctrl-B 1` is the default binding for select-window 1. This was
/// the specific dispatch the user reported as broken — assert it.
#[tokio::test]
async fn passthrough_prefix_one_selects_window_one() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let _control_rx = create_passthrough_attached_session(&handler, requester_pid, &alpha).await;

    // Create window 1 so we have something to select.
    assert!(matches!(
        handler
            .handle(Request::NewWindow(NewWindowRequest {
                target: alpha.clone(),
                name: None,
                detached: true,
                start_directory: None,
                environment: None,
                command: None,
                target_window_index: None,
                insert_at_target: false,
            }))
            .await,
        Response::NewWindow(_)
    ));

    // NewWindow auto-selects the new window; flip back to 0 so the
    // assertion below proves the dispatch *moved* the active window.
    assert!(matches!(
        handler
            .handle(Request::SelectWindow(SelectWindowRequest {
                target: WindowTarget::with_window(alpha.clone(), 0),
            }))
            .await,
        Response::SelectWindow(_)
    ));
    assert_eq!(
        active_windows(&handler, &alpha).await,
        "0:1\n1:0\n",
        "test precondition: window 0 must be active before dispatch"
    );

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x021")
        .await
        .expect("prefix 1 input must dispatch");

    assert_eq!(
        active_windows(&handler, &alpha).await,
        "0:0\n1:1\n",
        "in passthrough mode, Ctrl-B 1 must dispatch select-window 1 — \
         this is the binding the user found broken manually",
    );
}

/// Prefix split across two batches must still dispatch. Users often
/// release Ctrl-B before pressing the follow-on key, so the two bytes
/// arrive in separate Data frames.
#[tokio::test]
async fn passthrough_prefix_split_across_batches_dispatches() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let _control_rx = create_passthrough_attached_session(&handler, requester_pid, &alpha).await;

    assert!(matches!(
        handler
            .handle(Request::NewWindow(NewWindowRequest {
                target: alpha.clone(),
                name: None,
                detached: true,
                start_directory: None,
                environment: None,
                command: None,
                target_window_index: None,
                insert_at_target: false,
            }))
            .await,
        Response::NewWindow(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::SelectWindow(SelectWindowRequest {
                target: WindowTarget::with_window(alpha.clone(), 0),
            }))
            .await,
        Response::SelectWindow(_)
    ));

    // Send the prefix in one batch.
    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02")
        .await
        .expect("prefix byte alone must enter prefix table");
    // Then the follow-on in a separate batch.
    handler
        .handle_attached_live_input_for_test(requester_pid, b"1")
        .await
        .expect("follow-on key must dispatch the prefix binding");

    assert_eq!(
        active_windows(&handler, &alpha).await,
        "0:0\n1:1\n",
        "prefix + follow-on across batches must still dispatch the binding"
    );
}

/// Ctrl-B c is the default binding for `new-window`. Tests a dispatch
/// that has an observable side-effect distinct from window selection
/// (window count goes 1 → 2). Complements `prefix_one_selects_window_one`
/// by exercising a different binding family.
#[tokio::test]
async fn passthrough_prefix_c_creates_new_window() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let _control_rx = create_passthrough_attached_session(&handler, requester_pid, &alpha).await;

    assert_eq!(
        active_windows(&handler, &alpha).await,
        "0:1\n",
        "test precondition: session starts with a single window 0"
    );

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02c")
        .await
        .expect("prefix c input must dispatch new-window");

    let after = active_windows(&handler, &alpha).await;
    assert!(
        after.contains("1:1"),
        "Ctrl-B c in passthrough mode must create a new window and \
         activate it. Got: {after:?}"
    );
}

/// Plain typed bytes (no prefix in the batch) must be forwarded to the
/// pane verbatim. The `forwarded_to_pane` boolean returned by the input
/// pipeline confirms a write happened — if a future regression silently
/// drops or buffers raw input, this assertion fails.
#[tokio::test]
async fn passthrough_typed_bytes_are_forwarded_to_pane() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let _control_rx = create_passthrough_attached_session(&handler, requester_pid, &alpha).await;

    let mut pending_input = Vec::new();
    let forwarded = handler
        .handle_attached_live_input_inner(requester_pid, &mut pending_input, b"hello world")
        .await
        .expect("typed bytes input");
    assert!(
        forwarded,
        "plain typed bytes in passthrough mode must reach the pane — \
         the whole point of the bypass is to forward them verbatim"
    );
    assert!(
        pending_input.is_empty(),
        "passthrough handler should not leave bytes pending — it writes \
         immediately. Pending was: {pending_input:?}"
    );
}

/// Empty input must not error or panic — defensive against callers that
/// might forward an empty Data frame (some control flows can produce
/// zero-length writes during reattach).
#[tokio::test]
async fn passthrough_empty_input_is_a_noop() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let _control_rx = create_passthrough_attached_session(&handler, requester_pid, &alpha).await;

    let mut pending_input = Vec::new();
    let forwarded = handler
        .handle_attached_live_input_inner(requester_pid, &mut pending_input, b"")
        .await
        .expect("empty input must not error");
    assert!(
        !forwarded,
        "empty input must not report a forward (nothing was written)"
    );
}

/// Multibyte UTF-8 input flows through verbatim. A previous regression
/// (the arrow-key bug this whole file was created for) was the encoder
/// re-shaping bytes; this test pins down that no such reshaping happens
/// for non-ASCII characters either.
#[tokio::test]
async fn passthrough_utf8_input_is_forwarded() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let _control_rx = create_passthrough_attached_session(&handler, requester_pid, &alpha).await;

    let mut pending_input = Vec::new();
    // Three-byte UTF-8 sequences (em dash, Greek pi, snowman).
    let forwarded = handler
        .handle_attached_live_input_inner(
            requester_pid,
            &mut pending_input,
            "— π ☃".as_bytes(),
        )
        .await
        .expect("utf-8 input must not error");
    assert!(
        forwarded,
        "multibyte UTF-8 must reach the pane verbatim in passthrough mode"
    );
}

/// Arrow-key bytes (the bug that motivated the whole passthrough input
/// rework) must be forwarded as-is — `forwarded_to_pane=true` proves a
/// write happened. The companion `lossy_arrow_round_trip_motivates_passthrough_bypass`
/// test in `input_keys/tests.rs` documents *why* the bypass exists; this
/// one proves the bypass is wired in for passthrough sessions.
#[tokio::test]
async fn passthrough_arrow_key_bytes_are_forwarded() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let _control_rx = create_passthrough_attached_session(&handler, requester_pid, &alpha).await;

    // \x1bOA is up-arrow in DECCKM application-cursor mode (what vim
    // sets via \e[?1h, which in passthrough flows verbatim to the host).
    let mut pending_input = Vec::new();
    let forwarded = handler
        .handle_attached_live_input_inner(requester_pid, &mut pending_input, b"\x1bOA")
        .await
        .expect("arrow key input must not error");
    assert!(
        forwarded,
        "arrow-key bytes (\\x1bOA, up arrow in app cursor mode) must \
         reach the pane verbatim in passthrough mode — otherwise vim's \
         arrow keys appear broken"
    );
}

/// Raw bytes, prefix, follow-on, more raw bytes — all in one batch. The
/// passthrough handler must flush the pre-prefix raw bytes BEFORE
/// dispatching the binding (otherwise typing-order is lost). Then the
/// post-binding bytes go through whatever the binding left the dispatch
/// state at (root table after a successful select-window).
#[tokio::test]
async fn passthrough_prefix_in_middle_of_raw_bytes_dispatches_and_continues() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let _control_rx = create_passthrough_attached_session(&handler, requester_pid, &alpha).await;

    assert!(matches!(
        handler
            .handle(Request::NewWindow(NewWindowRequest {
                target: alpha.clone(),
                name: None,
                detached: true,
                start_directory: None,
                environment: None,
                command: None,
                target_window_index: None,
                insert_at_target: false,
            }))
            .await,
        Response::NewWindow(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::SelectWindow(SelectWindowRequest {
                target: WindowTarget::with_window(alpha.clone(), 0),
            }))
            .await,
        Response::SelectWindow(_)
    ));

    // "ab" → raw, "\x021" → prefix + select-window 1, "cd" → raw again
    handler
        .handle_attached_live_input_for_test(requester_pid, b"ab\x021cd")
        .await
        .expect("mixed raw+prefix+raw input must not error");

    assert_eq!(
        active_windows(&handler, &alpha).await,
        "0:0\n1:1\n",
        "the prefix binding inside a mixed batch must still dispatch"
    );
}

/// Two prefix sequences back-to-back in one batch: `Ctrl-B n` then
/// `Ctrl-B n` again — should move forward twice.
#[tokio::test]
async fn passthrough_multiple_prefix_sequences_in_one_batch_each_dispatches() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let _control_rx = create_passthrough_attached_session(&handler, requester_pid, &alpha).await;

    // Build three windows so next-window has somewhere to go twice.
    for _ in 0..2 {
        assert!(matches!(
            handler
                .handle(Request::NewWindow(NewWindowRequest {
                    target: alpha.clone(),
                    name: None,
                    detached: true,
                    start_directory: None,
                    environment: None,
                    command: None,
                    target_window_index: None,
                    insert_at_target: false,
                }))
                .await,
            Response::NewWindow(_)
        ));
    }
    assert!(matches!(
        handler
            .handle(Request::SelectWindow(SelectWindowRequest {
                target: WindowTarget::with_window(alpha.clone(), 0),
            }))
            .await,
        Response::SelectWindow(_)
    ));

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02n\x02n")
        .await
        .expect("double prefix-n batch must dispatch both bindings");

    assert_eq!(
        active_windows(&handler, &alpha).await,
        "0:0\n1:0\n2:1\n",
        "two prefix+n in one batch must advance the active window twice"
    );
}

/// After a prefix binding finishes, the dispatch state must return to
/// the root table so the next byte is treated as new input. Verifies by
/// sending `Ctrl-B n` then a plain typed byte and asserting the plain
/// byte is forwarded as raw input (not interpreted as a prefix follow-on).
#[tokio::test]
async fn passthrough_after_prefix_binding_returns_to_root_table() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let _control_rx = create_passthrough_attached_session(&handler, requester_pid, &alpha).await;

    assert!(matches!(
        handler
            .handle(Request::NewWindow(NewWindowRequest {
                target: alpha.clone(),
                name: None,
                detached: true,
                start_directory: None,
                environment: None,
                command: None,
                target_window_index: None,
                insert_at_target: false,
            }))
            .await,
        Response::NewWindow(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::SelectWindow(SelectWindowRequest {
                target: WindowTarget::with_window(alpha.clone(), 0),
            }))
            .await,
        Response::SelectWindow(_)
    ));

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02n")
        .await
        .expect("prefix n");
    // After the binding fires, we should be back at the root table.
    let mut pending_input = Vec::new();
    let forwarded = handler
        .handle_attached_live_input_inner(requester_pid, &mut pending_input, b"x")
        .await
        .expect("post-binding raw byte");
    assert!(
        forwarded,
        "plain typed byte after a prefix binding must be forwarded as \
         raw input (the binding shouldn't leave us stuck in the prefix \
         table)"
    );
}

/// FAILING — user-reported on macOS WezTerm over SSH: after running
/// vim in a passthrough session, `Ctrl-B w` stopped working.  Root
/// cause confirmed (`cat -v` showed `^[[27;5;98~`): vim opted the
/// terminal into modifyOtherKeys, and WezTerm now sends Ctrl-B as the
/// xterm extended form `\x1b[27;5;98~` rather than the literal `\x02`
/// byte.  The passthrough fast path does a naïve byte search for
/// `0x02` and misses the CSI sequence entirely, so the whole thing
/// gets forwarded raw to the inner shell and the binding never
/// dispatches.
///
/// Fix shape: scan with `decode_extended_key` (or
/// `is_extended_key_prefix` + decode) when we see `\x1b[`, and treat
/// any sequence that decodes to the configured prefix key as a
/// prefix match — defer it (and what follows) to the legacy decoder
/// the same way we already defer the literal byte.
#[tokio::test]
async fn passthrough_prefix_modifyotherkeys_xterm_form_dispatches() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let _control_rx = create_passthrough_attached_session(&handler, requester_pid, &alpha).await;

    // Set up a second window so next-window is a visible side-effect.
    assert!(matches!(
        handler
            .handle(Request::NewWindow(NewWindowRequest {
                target: alpha.clone(),
                name: None,
                detached: true,
                start_directory: None,
                environment: None,
                command: None,
                target_window_index: None,
                insert_at_target: false,
            }))
            .await,
        Response::NewWindow(_)
    ));

    // `\x1b[27;5;98~` = xterm modifyOtherKeys encoding of Ctrl-B
    // (modifier 5 = Ctrl, code 98 = 'b').  Followed by literal 'n'.
    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x1b[27;5;98~n")
        .await
        .expect("xterm modifyOtherKeys prefix + n must dispatch");

    assert_eq!(
        active_windows(&handler, &alpha).await,
        "0:0\n1:1\n",
        "Ctrl-B in xterm modifyOtherKeys form (\\x1b[27;5;98~) must \
         activate the prefix table the same as the literal \\x02 byte. \
         Otherwise vim-exit leaves real users with a dead Ctrl-B."
    );
}

/// Companion to the xterm-form test: kitty keyboard protocol u-mode
/// encoding of Ctrl-B is `\x1b[98;5u` (code 98 = 'b', modifier 5 =
/// Ctrl).  Terminals supporting CSI-u send this when an app opts in.
#[tokio::test]
async fn passthrough_prefix_kitty_csi_u_form_dispatches() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let _control_rx = create_passthrough_attached_session(&handler, requester_pid, &alpha).await;

    assert!(matches!(
        handler
            .handle(Request::NewWindow(NewWindowRequest {
                target: alpha.clone(),
                name: None,
                detached: true,
                start_directory: None,
                environment: None,
                command: None,
                target_window_index: None,
                insert_at_target: false,
            }))
            .await,
        Response::NewWindow(_)
    ));

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x1b[98;5un")
        .await
        .expect("kitty CSI-u prefix + n must dispatch");

    assert_eq!(
        active_windows(&handler, &alpha).await,
        "0:0\n1:1\n",
        "Ctrl-B in kitty CSI-u form (\\x1b[98;5u) must activate the \
         prefix table the same as the literal \\x02 byte."
    );
}

/// FAILING test for the post-vim-exit DA1 leak. In passthrough mode an
/// unsolicited DA1 reply from the host (`\x1b[?65;4;6;18;22c`) reaches
/// the input handler with NO preceding query from the pane. Today the
/// passthrough handler writes those bytes verbatim to the pty, and zsh
/// (running after vim exited) sees them as keystrokes — visible as
/// `?65;4;6;18;22c` text after the prompt and broken arrow-key state
/// until Enter resets ZLE.
///
/// Fix shape: track outstanding terminal queries seen on the pane→host
/// output stream, and drop responses on the host→pane input stream when
/// no query is outstanding. Until that's wired, this test fails.
#[tokio::test]
async fn passthrough_drops_unsolicited_da1_reply_from_host() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let _control_rx = create_passthrough_attached_session(&handler, requester_pid, &alpha).await;

    let mut pending_input = Vec::new();
    let forwarded = handler
        .handle_attached_live_input_inner(
            requester_pid,
            &mut pending_input,
            b"\x1b[?65;4;6;18;22c",
        )
        .await
        .expect("DA1 reply input");
    assert!(
        !forwarded,
        "an unsolicited DA1 device-attributes reply must NOT be written \
         to the pane in passthrough mode. The receiver is the shell \
         (vim has exited), which would echo the bytes as visible \
         garbage and poison ZLE keymap state."
    );
}

/// Subtler case the simple "drop only when counter==0" filter doesn't
/// catch: vim *did* query DA1 (so the counter is positive), then vim
/// exited (alt-screen-exit `\x1b[?1049l` on pane→client), then the
/// reply arrived destined for vim — but the shell is now the listener.
/// The fix: treat alt-screen-exit as "any in-flight queries are now
/// orphaned" and wipe the counter, so the late reply gets dropped.
///
/// Drives the user-reported regression after running `vi
/// ~/.config/gh/hosts.yml` on macOS WezTerm-over-SSH: the literal text
/// `?65;4;6;18;22c` showed up at the shell prompt.
#[tokio::test]
async fn passthrough_drops_da1_reply_after_curses_program_exits() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let _control_rx = create_passthrough_attached_session(&handler, requester_pid, &alpha).await;

    // Bump the counter to simulate the forwarder observing vim's
    // startup DA1 query on the pane→client direction.
    handler
        .bump_outstanding_terminal_queries(requester_pid, 1)
        .await;

    // Simulate the alt-screen-exit reaching the forwarder.  In
    // production, `forward_attach_passthrough` would call
    // `reset_outstanding_terminal_queries` when it spots
    // `\x1b[?1049l` in pane→client bytes.
    handler
        .reset_outstanding_terminal_queries(requester_pid)
        .await;

    // Now the late DA1 reply arrives on client→pane.  Even though
    // a query *was* outstanding moments ago, the curses-app exit
    // signal means the original asker is gone.  The reply must be
    // dropped, not delivered to whatever's reading the pane PTY now.
    let mut pending_input = Vec::new();
    let forwarded = handler
        .handle_attached_live_input_inner(
            requester_pid,
            &mut pending_input,
            b"\x1b[?65;4;6;18;22c",
        )
        .await
        .expect("post-curses-exit DA1 reply input");
    assert!(
        !forwarded,
        "a DA1 reply arriving after the pane's curses program has \
         exited (alt-screen left, counter reset) must NOT be \
         forwarded — the shell that has taken over the pane never \
         asked for it."
    );
}

