//! Final-sink proof for attached bracketed paste on the active pane.
//!
//! Unlike the sibling bracketed-paste suites, nothing here installs
//! `RawPaneInputProbe`. That probe returns `PaneInputSink::CapturedForTest`
//! before the starting-pane queue and before the Windows passthrough/legacy
//! sink selection, so it cannot observe whether the real destination received
//! the delimiters. Each test below runs a real child process in the pane and
//! compares the bytes that child actually read from its standard input.
//!
//! The slot/child machinery lives in [`crate::test_shell::final_sink`] because
//! the deferred-start proof in `handler_session` needs the same child and
//! cannot reach into this private test tree.
//!
//! `let _ = attach_to(..)` drops the attach guard, and with it the control
//! receiver, on the spot; `#[must_use]` only rejects the bare statement. The
//! deny below turns every such discard in this tree into a compile error.
#![deny(let_underscore_drop)]

use std::time::Instant;

use super::*;
use crate::test_shell::final_sink::{
    create_final_sink_session, describe_missing_bracketed_mode, observe_pane_output,
    split_final_sink_pane, AttachedFinalSinkClient, FinalSinkSlot,
};
use attached_client::attach_to;

#[path = "bracketed_paste_final_sink/attached_client.rs"]
mod attached_client;

const OPEN: &[u8] = b"\x1b[200~";
const CLOSE: &[u8] = b"\x1b[201~";

/// CR/LF, `β`, `😀` and control-looking bytes a naive VT filter would eat.
const RICH_BODY: &str = "alpha\r\nβ 😀 \u{2} \u{1b}[9;2u \u{1b}[<64;2;2M omega";

fn wrapped(body: &[u8]) -> Vec<u8> {
    let mut wrapped = Vec::with_capacity(OPEN.len() + body.len() + CLOSE.len());
    wrapped.extend_from_slice(OPEN);
    wrapped.extend_from_slice(body);
    wrapped.extend_from_slice(CLOSE);
    wrapped
}

/// How long a real child is given to publish its capability through pane output.
const ANNOUNCEMENT_TIMEOUT: Duration = Duration::from_secs(30);

/// Waits until the server transcript has observed the child's real DECSET 2004
/// announcement, so routing is decided from production state.
///
/// An expired wait reports what that transcript had actually observed. The
/// deadline alone said only that the mode never changed, which reads the same
/// for a pane that received no child output, one that received output carrying
/// no announcement, and one whose console handed it the announcement as screen
/// text — and a Windows 10 release execution costs a thirteen-minute
/// compilation before it can say so.
pub(super) async fn wait_for_bracketed_mode(
    handler: &RequestHandler,
    target: &PaneTarget,
    expected: bool,
) {
    let deadline = Instant::now() + ANNOUNCEMENT_TIMEOUT;
    loop {
        {
            let state = handler.state.lock().await;
            let pane_mode = state
                .sessions
                .session(target.session_name())
                .and_then(|session| session.window_at(target.window_index()))
                .and_then(|window| window.pane(target.pane_index()))
                .map(rmux_core::Pane::id)
                .and_then(|pane_id| state.pane_screen_state(target.session_name(), pane_id))
                .map(|screen_state| screen_state.mode)
                .unwrap_or_default();
            if (pane_mode & mode::MODE_BRACKETPASTE != 0) == expected {
                return;
            }
            // Read under the same lock as the mode above, so the report and the
            // decision it explains describe one state of the transcript.
            if Instant::now() >= deadline {
                panic!(
                    "{}",
                    describe_missing_bracketed_mode(
                        &target.to_string(),
                        expected,
                        ANNOUNCEMENT_TIMEOUT,
                        &observe_pane_output(&state, target),
                    )
                );
            }
        }
        sleep(Duration::from_millis(25)).await;
    }
}

async fn select_pane(handler: &RequestHandler, target: &PaneTarget) {
    let selected = handler
        .handle(Request::SelectPane(Box::new(SelectPaneRequest {
            target: target.clone(),
            title: None,
            style: None,
            input_disabled: None,
            preserve_zoom: false,
        })))
        .await;
    assert!(matches!(selected, Response::SelectPane(_)));
}

async fn set_synchronize_panes(handler: &RequestHandler, session: &rmux_proto::SessionName) {
    let set_sync = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Window(WindowTarget::with_window(session.clone(), 0)),
            option: OptionName::SynchronizePanes,
            value: "on".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(set_sync, Response::SetOption(_)));
}

/// Creates the session, waits for the pane child and returns its target.
async fn started_final_sink_pane(
    handler: &RequestHandler,
    session: &rmux_proto::SessionName,
    slot: &FinalSinkSlot,
    bracket_aware: bool,
) -> PaneTarget {
    create_final_sink_session(handler, session, slot).await;
    let target = PaneTarget::with_window(session.clone(), 0, 0);
    handler
        .wait_for_pane_startup_to_finish_for_test(&target)
        .await;
    slot.wait_until_ready();
    wait_for_bracketed_mode(handler, &target, bracket_aware).await;
    target
}

// ---------------------------------------------------------------------------
// Active destination
// ---------------------------------------------------------------------------

#[tokio::test]
async fn active_aware_pane_child_receives_the_bracketed_paste_delimiters() {
    let handler = RequestHandler::new();
    let session = session_name("final-sink-aware");
    let expected = wrapped(RICH_BODY.as_bytes());
    let slot = FinalSinkSlot::new("aware", &expected, true);

    started_final_sink_pane(&handler, &session, &slot, true).await;
    let client = attach_to(&handler, &session).await;
    client
        .send(&handler, &expected)
        .await
        .expect("attached bracketed paste");

    slot.assert_application_bytes(
        "a bracketed-paste-aware child must receive ESC[200~ + body + ESC[201~ verbatim",
    );
}

#[tokio::test]
async fn active_unaware_pane_child_receives_only_the_paste_body() {
    let handler = RequestHandler::new();
    let session = session_name("final-sink-unaware");
    let body = RICH_BODY.as_bytes();
    let slot = FinalSinkSlot::new("unaware", body, false);

    started_final_sink_pane(&handler, &session, &slot, false).await;
    let client = attach_to(&handler, &session).await;
    client
        .send(&handler, &wrapped(body))
        .await
        .expect("attached bracketed paste");

    slot.assert_application_bytes("an unaware child must receive the body with no delimiters");
}

async fn assert_clock_mode_paste(label: &str, bracket_aware: bool, fragmented: bool) {
    let handler = RequestHandler::new();
    let session = session_name(label);
    let paste = wrapped(RICH_BODY.as_bytes());
    let mut expected = if bracket_aware {
        paste.clone()
    } else {
        RICH_BODY.as_bytes().to_vec()
    };
    expected.extend_from_slice(b"TAIL");
    let slot = FinalSinkSlot::new(label, &expected, bracket_aware);
    #[cfg(unix)]
    let slot = slot.until_controlled_eof();

    let target = started_final_sink_pane(&handler, &session, &slot, bracket_aware).await;
    handler.disable_pane_focus_reports_for_test(&target).await;
    // Clock mode publishes overlays; the client keeps their receiver alive.
    let client = AttachedFinalSinkClient::register(&handler, &session).await;
    client
        .send(&handler, b"\x02t")
        .await
        .expect("clock-mode entry");
    assert!(handler.target_is_in_clock_mode(&target).await.unwrap());

    let paste_len = paste.len();
    let mut input = paste;
    input.extend_from_slice(b"TAIL");
    let chunks = if fragmented {
        vec![
            &input[..2],
            &input[2..4],
            &input[4..OPEN.len() + 3],
            &input[OPEN.len() + 3..paste_len - 4],
            &input[paste_len - 4..paste_len - 1],
            &input[paste_len - 1..],
        ]
    } else {
        vec![input.as_slice()]
    };
    let mut pending_input = Vec::new();
    for (index, chunk) in chunks.iter().enumerate() {
        client
            .send_read(&handler, &mut pending_input, chunk)
            .await
            .expect("clock-mode paste and same-chunk tail");
        if index + 1 < chunks.len() {
            assert!(handler.target_is_in_clock_mode(&target).await.unwrap());
        }
    }
    assert!(pending_input.is_empty());
    assert!(!handler.target_is_in_clock_mode(&target).await.unwrap());
    // Unix writes paste, tail and VEOF through the same ordered PTY. VEOF is
    // outside the assertion payload: one flushes the last partial line, the
    // next makes the child's unbounded read return EOF. No quiet-period pass.
    #[cfg(unix)]
    client
        .send(&handler, b"\x04\x04")
        .await
        .expect("controlled end of the application stream");
    slot.assert_application_bytes(
        "clock-mode must reroute the entire paste using the child's mode, followed by the tail",
    );
}

#[tokio::test]
async fn clock_mode_complete_paste_preserves_delimiters_for_an_aware_child() {
    assert_clock_mode_paste("clock-complete-aware", true, false).await;
}

#[tokio::test]
async fn clock_mode_complete_paste_strips_delimiters_for_an_unaware_child() {
    assert_clock_mode_paste("clock-complete-unaware", false, false).await;
}

#[tokio::test]
async fn clock_mode_fragmented_paste_preserves_delimiters_for_an_aware_child() {
    assert_clock_mode_paste("clock-fragmented-aware", true, true).await;
}

#[tokio::test]
async fn clock_mode_fragmented_paste_strips_delimiters_for_an_unaware_child() {
    assert_clock_mode_paste("clock-fragmented-unaware", false, true).await;
}

/// More UTF-16 code units than one `write_windows_console_utf8` record batch,
/// with a surrogate pair and CR/LF inside, so a truncating, splitting or
/// re-ordering writer becomes visible in the child's bytes.
fn batch_crossing_body() -> String {
    let mut body = String::from("β😀\r\n");
    while body.encode_utf16().count() < 2_100 {
        body.push_str("0123456789");
    }
    body.push_str("\r\nEND😀");
    body
}

#[tokio::test]
async fn bracketed_paste_child_receives_a_body_crossing_the_record_batch_boundary() {
    let handler = RequestHandler::new();
    let session = session_name("final-sink-batch");
    let expected = wrapped(batch_crossing_body().as_bytes());
    let slot = FinalSinkSlot::new("batch", &expected, true);

    started_final_sink_pane(&handler, &session, &slot, true).await;
    let client = attach_to(&handler, &session).await;
    client
        .send(&handler, &expected)
        .await
        .expect("attached bracketed paste");

    slot.assert_application_bytes(
        "a payload longer than one console record batch must arrive whole",
    );
}

#[tokio::test]
async fn split_bracketed_paste_delimiters_reach_the_child_unchanged() {
    let handler = RequestHandler::new();
    let session = session_name("final-sink-split");
    let expected = wrapped(b"split-markers");
    let slot = FinalSinkSlot::new("split", &expected, true);

    started_final_sink_pane(&handler, &session, &slot, true).await;
    let client = attach_to(&handler, &session).await;

    // Representative interior splits of both delimiters, which is what a real
    // client produces when a paste straddles read boundaries.
    let mut pending_input = Vec::new();
    for chunk in [
        &expected[..2],
        &expected[2..4],
        &expected[4..OPEN.len() + 3],
        &expected[OPEN.len() + 3..expected.len() - 4],
        &expected[expected.len() - 4..expected.len() - 1],
        &expected[expected.len() - 1..],
    ] {
        client
            .send_read(&handler, &mut pending_input, chunk)
            .await
            .expect("split bracketed paste chunk");
    }
    assert!(pending_input.is_empty());

    slot.assert_application_bytes(
        "delimiters split across reads must be reassembled for the child",
    );
}

// ---------------------------------------------------------------------------
// Mixed synchronize-panes
// ---------------------------------------------------------------------------

async fn assert_mixed_synchronized_final_sink(label: &str, active_pane: u32) {
    let handler = RequestHandler::new();
    let session = session_name(label);
    let body = b"mixed-destinations";
    let aware_expected = wrapped(body);

    let aware = FinalSinkSlot::new("mixed-aware", &aware_expected, true);
    let unaware = FinalSinkSlot::new("mixed-unaware", body, false);

    let aware_target = started_final_sink_pane(&handler, &session, &aware, true).await;
    let unaware_target = split_final_sink_pane(&handler, &session, &unaware).await;
    handler
        .wait_for_pane_startup_to_finish_for_test(&unaware_target)
        .await;
    unaware.wait_until_ready();
    wait_for_bracketed_mode(&handler, &unaware_target, false).await;
    assert_eq!(aware_target, PaneTarget::with_window(session.clone(), 0, 0));

    select_pane(
        &handler,
        &PaneTarget::with_window(session.clone(), 0, active_pane),
    )
    .await;
    set_synchronize_panes(&handler, &session).await;

    let client = attach_to(&handler, &session).await;
    // The client always sends the wrapped form; the server must recompute the
    // mode for each destination instead of reusing the source pane's.
    client
        .send(&handler, &aware_expected)
        .await
        .expect("synchronized bracketed paste");

    aware.assert_application_bytes(&format!(
        "the aware destination must keep its delimiters (active pane {active_pane})"
    ));
    unaware.assert_application_bytes(&format!(
        "the unaware destination must receive the body only (active pane {active_pane})"
    ));
}

#[tokio::test]
async fn synchronized_mixed_panes_recompute_mode_per_child_when_the_active_pane_is_aware() {
    assert_mixed_synchronized_final_sink("final-sink-mixed-aware", 0).await;
}

#[tokio::test]
async fn synchronized_mixed_panes_recompute_mode_per_child_when_the_active_pane_is_unaware() {
    assert_mixed_synchronized_final_sink("final-sink-mixed-unaware", 1).await;
}

// ---------------------------------------------------------------------------
// Child-observed ordering
// ---------------------------------------------------------------------------

/// Ordering is what the child read, not the order the server awaited.
///
/// A keystroke and a pasted body do not share a transport. On Windows the
/// keystroke is written to the pane's ConPTY input pipe while the paste is
/// written as console records straight into that pane's input buffer, so the
/// two reach the same console through different Win32 entry points and a
/// correct await order alone does not settle what the child reads first. On
/// Unix both take the pseudoterminal. Either way the child must observe
/// `key -> paste -> key`.
async fn assert_paste_arrives_between_the_keystrokes(label: &str, bracket_aware: bool) {
    let handler = RequestHandler::new();
    let session = session_name(label);
    let body = b"yy";
    let wrapped_body = wrapped(body);
    let mut expected = b"x".to_vec();
    expected.extend_from_slice(if bracket_aware { &wrapped_body } else { body });
    expected.extend_from_slice(b"z");
    let slot = FinalSinkSlot::new(label, &expected, bracket_aware);

    started_final_sink_pane(&handler, &session, &slot, bracket_aware).await;
    let client = attach_to(&handler, &session).await;
    for chunk in [b"x".as_slice(), &wrapped_body, b"z".as_slice()] {
        client.send(&handler, chunk).await.expect("attached input");
    }

    slot.assert_application_bytes(
        "the child must read the paste between the keystrokes that bracket it",
    );
}

#[tokio::test]
async fn a_paste_reaches_an_aware_child_between_the_keystrokes_that_bracket_it() {
    assert_paste_arrives_between_the_keystrokes("final-sink-order-aware", true).await;
}

#[tokio::test]
async fn a_paste_reaches_an_unaware_child_between_the_keystrokes_that_bracket_it() {
    assert_paste_arrives_between_the_keystrokes("final-sink-order-unaware", false).await;
}

// ---------------------------------------------------------------------------
// Negative controls
// ---------------------------------------------------------------------------
//
// These pin the observable contract: no delimiter is ever added around input
// that did not arrive as a bracketed paste. They are byte-exact at the child
// and mode-independent. They do not by themselves identify which internal
// helper ran; the mutation ledger covers that.

#[tokio::test]
async fn send_keys_never_wraps_its_payload_for_an_aware_child() {
    let handler = RequestHandler::new();
    let session = session_name("final-sink-send-keys");
    let literal = "send-keys-plain";
    let slot = FinalSinkSlot::new("send-keys", literal.as_bytes(), true);

    let target = started_final_sink_pane(&handler, &session, &slot, true).await;
    let response = handler
        .handle(Request::SendKeys(SendKeysRequest {
            target: target.clone(),
            keys: vec![literal.to_owned()],
        }))
        .await;
    assert!(matches!(
        response,
        Response::SendKeys(SendKeysResponse { key_count: 1 })
    ));

    slot.assert_application_bytes("send-keys must not enter the bracketed-paste sink");
}

#[tokio::test]
async fn pane_input_and_raw_attached_keystrokes_are_never_wrapped_for_an_aware_child() {
    let handler = RequestHandler::new();
    let session = session_name("final-sink-raw");
    let pane_input = "pane-input";
    let raw = b"raw-keys";
    let mut expected = pane_input.as_bytes().to_vec();
    expected.extend_from_slice(raw);
    let slot = FinalSinkSlot::new("raw", &expected, true);

    started_final_sink_pane(&handler, &session, &slot, true).await;
    let pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&session)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0))
            .map(rmux_core::Pane::id)
            .expect("pane exists")
    };
    let response = handler
        .handle(Request::PaneInput(rmux_proto::PaneInputRequest {
            target: PaneTargetRef::by_id(session.clone(), pane_id),
            keys: vec![pane_input.to_owned()],
            literal: true,
        }))
        .await;
    assert!(
        matches!(response, Response::SendKeys(_)),
        "unexpected pane-input response: {response:?}"
    );

    let client = attach_to(&handler, &session).await;
    client
        .send(&handler, raw)
        .await
        .expect("raw attached keystrokes");

    slot.assert_application_bytes(
        "SDK/raw keystrokes and pane-input must reach the child unwrapped",
    );
}

#[path = "bracketed_paste_final_sink/attach_lifetime.rs"]
mod attach_lifetime;
