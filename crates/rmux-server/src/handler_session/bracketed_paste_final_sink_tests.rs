//! Final-sink proof for attached bracketed paste that arrives while a pane is
//! still starting.
//!
//! On Windows the initial ConPTY is created off the request path, so attached
//! input can reach `prepare_pane_input_write_with_encoding` before the pane has
//! a terminal. The typed `DeferredInitialPaneInput::BracketedPaste` entry and
//! the console branch of `flush_deferred_initial_pane_input` exist so the paste
//! keeps its bracketed intent across that boundary. The queued-variant test in
//! `pane_terminals/deferred_initial/tests.rs` stops at the enum; these run a
//! real child and compare the bytes it read after the flush.
//!
//! Nothing here stamps the transcript. `append_bytes_to_pane_transcript_for_test`
//! would let the pane look bracket-aware before any child existed, which proves
//! only that a synthetic mode selects a route: the test would stay green even if
//! real child output never published the capability during startup. Instead the
//! pane is held at the production boundary where its child is already running
//! and publishing output while the pane is still in `Starting`, and the mode is
//! read from the production transcript inside that window.
//!
//! The aware and unaware panes pin the observation from both sides. If the
//! capability were never observed, the aware proof could not reach bracketed
//! mode; if it were assumed rather than observed, the unaware child would
//! receive delimiters it never advertised.
//!
//! Input goes through [`AttachedFinalSinkClient`], which owns the control
//! receiver under a pid only it knows; dropping it, including through
//! `let _ =`, which the deny below rejects, leaves no attach to send to.
#![deny(let_underscore_drop)]

use std::time::{Duration, Instant};

use rmux_core::input::mode;
use rmux_proto::{PaneTarget, SessionName};
use tokio::time::sleep;

use crate::test_shell::final_sink::{
    create_final_sink_session, describe_missing_bracketed_mode, observe_pane_output,
    AttachedFinalSinkClient, FinalSinkSlot,
};

use super::super::RequestHandler;
use super::deferred_initial_gate::DeferredInitialGate;

const OPEN: &[u8] = b"\x1b[200~";
const CLOSE: &[u8] = b"\x1b[201~";
const OBSERVATION_TIMEOUT: Duration = Duration::from_secs(30);

fn wrapped(body: &[u8]) -> Vec<u8> {
    let mut wrapped = Vec::with_capacity(OPEN.len() + body.len() + CLOSE.len());
    wrapped.extend_from_slice(OPEN);
    wrapped.extend_from_slice(body);
    wrapped.extend_from_slice(CLOSE);
    wrapped
}

/// Two workers: the assertions below block on the child's slot files while the
/// deferred spawn task must keep running.
fn deferred_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build isolated deferred-pane runtime")
}

async fn bracketed_mode_while_starting(handler: &RequestHandler, session: &SessionName) -> bool {
    let state = handler.state.lock().await;
    assert!(
        state.pane_is_starting_in_window(session, 0, 0),
        "the pane left `Starting` before the proof could observe it"
    );
    state
        .sessions
        .session(session)
        .and_then(|session| session.window_at(0))
        .and_then(|window| window.pane(0))
        .map(rmux_core::Pane::id)
        .and_then(|pane_id| state.pane_screen_state(session, pane_id))
        .is_some_and(|screen| screen.mode & mode::MODE_BRACKETPASTE != 0)
}

/// Waits for the real child's own `ESC[?2004h` to reach the production
/// transcript, asserting on every poll that the pane is still starting.
///
/// An expired wait carries what the transcript had observed, so a missing
/// announcement names the boundary it stopped at rather than only the deadline.
async fn await_child_bracketed_announcement(handler: &RequestHandler, session: &SessionName) {
    let deadline = Instant::now() + OBSERVATION_TIMEOUT;
    loop {
        if bracketed_mode_while_starting(handler, session).await {
            return;
        }
        if Instant::now() >= deadline {
            let target = PaneTarget::with_window(session.clone(), 0, 0);
            let observed = {
                let state = handler.state.lock().await;
                observe_pane_output(&state, &target)
            };
            panic!(
                "the real child never announced ESC[?2004h through pane output \
                 while its pane was starting\n{}",
                describe_missing_bracketed_mode(
                    &target.to_string(),
                    true,
                    OBSERVATION_TIMEOUT,
                    &observed,
                )
            );
        }
        sleep(Duration::from_millis(25)).await;
    }
}

/// The gate is keyed by runtime session name. A freshly created session owns
/// its own runtime, so the two coincide; this fails loudly if that stops
/// holding rather than silently never gating.
async fn assert_runtime_session_is_the_session(handler: &RequestHandler, session: &SessionName) {
    let state = handler.state.lock().await;
    assert_eq!(
        &state.runtime_session_name_for_window(session, 0),
        session,
        "the deferred gate is keyed by runtime session name"
    );
}

#[test]
fn deferred_bracketed_paste_reaches_a_child_that_announced_the_capability_itself() {
    deferred_runtime().block_on(async {
        let handler = RequestHandler::new();
        let session = SessionName::new("final-sink-deferred-aware").expect("valid session name");
        let gate = DeferredInitialGate::install(&session);

        let expected = wrapped("deferred\r\nβ😀 body".as_bytes());
        let slot = FinalSinkSlot::new("deferred-aware", &expected, true);
        create_final_sink_session(&handler, &session, &slot).await;
        assert_runtime_session_is_the_session(&handler, &session).await;

        // The gate holds the pane in `Starting` while its real child runs, so
        // the capability below is published by actual pane output.
        await_child_bracketed_announcement(&handler, &session).await;

        let client = AttachedFinalSinkClient::register(&handler, &session).await;
        client
            .send(&handler, &expected)
            .await
            .expect("bracketed paste is accepted while the pane starts");

        assert!(
            handler
                .state
                .lock()
                .await
                .pane_is_starting_in_window(&session, 0, 0),
            "the paste must have been queued, not written to a live terminal"
        );

        drop(gate);
        let target = PaneTarget::with_window(session.clone(), 0, 0);
        handler
            .wait_for_pane_startup_to_finish_for_test(&target)
            .await;

        slot.assert_application_bytes(
            "a paste queued before startup finished must reach the child with the \
             delimiters its own announcement selected",
        );
    });
}

#[test]
fn deferred_bracketed_paste_to_a_child_that_never_announced_arrives_unwrapped() {
    deferred_runtime().block_on(async {
        let handler = RequestHandler::new();
        let session = SessionName::new("final-sink-deferred-unaware").expect("valid session name");
        let gate = DeferredInitialGate::install(&session);

        let body = "deferred-unaware\r\nβ😀".as_bytes();
        let slot = FinalSinkSlot::new("deferred-unaware", body, false);
        create_final_sink_session(&handler, &session, &slot).await;
        assert_runtime_session_is_the_session(&handler, &session).await;

        // Readiness proves this child reached the same point in its script as
        // the aware one, past where that child announces. Nothing will ever set
        // bracketed mode for it.
        slot.wait_until_ready();
        assert!(
            !bracketed_mode_while_starting(&handler, &session).await,
            "a child that never announced ESC[?2004h must not be treated as aware"
        );

        let client = AttachedFinalSinkClient::register(&handler, &session).await;
        client
            .send(&handler, &wrapped(body))
            .await
            .expect("bracketed paste is accepted while the pane starts");

        assert!(
            handler
                .state
                .lock()
                .await
                .pane_is_starting_in_window(&session, 0, 0),
            "the paste must have been queued, not written to a live terminal"
        );

        drop(gate);
        let target = PaneTarget::with_window(session.clone(), 0, 0);
        handler
            .wait_for_pane_startup_to_finish_for_test(&target)
            .await;

        slot.assert_application_bytes(
            "a queued paste must reach an unannounced child with no delimiters",
        );
    });
}
