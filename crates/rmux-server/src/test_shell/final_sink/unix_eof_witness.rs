//! Witness that the controlled-EOF capture is published at the true EOF only.
//!
//! `unix_eof` claims that `out` means "EOF was read": never "a line ended",
//! "the last line ended" or "a VEOF flushed a partial line". In canonical mode
//! a VEOF after pending bytes only delivers them as a short read; a VEOF on an
//! empty line buffer delivers the zero-length read that is EOF.
//!
//! No interval is waited out. Every premature trigger is followed by bytes the
//! capture must contain: the first LF by more lines, the last LF by an
//! unterminated `tail`, and the VEOF that flushes `sec` by the rest of the
//! input. A child that published at any of them can never hold all of
//! `CAPTURE` in `out.part`, so the wait observes its `out` and fails. Only EOF
//! follows the last byte; a child counting exactly this capture's length is
//! beyond any witness of the published bytes.

use std::fs;
use std::time::Instant;

use rmux_proto::{PaneTarget, SessionName};
use tokio::sync::mpsc;

use super::{create_final_sink_session, FinalSinkSlot, TeardownOutcome, OUT_FILE, POLL_INTERVAL};
use crate::handler::RequestHandler;

/// UTF-8, short, free of the reserved VEOF and NUL, and without a final LF.
const CAPTURE: &[u8] = b"first line\nsecond line\ntail";
/// The input, in pane order: each VEOF here flushes a partial line.
const INPUT: [&[u8]; 4] = [b"first line\nsec", b"\x04", b"ond line\ntail", b"\x04"];

impl FinalSinkSlot {
    /// Waits until `out.part` holds exactly the expected bytes, failing as soon
    /// as `out` exists, the child reports an error, or a byte diverges.
    fn assert_received_unpublished(&self, context: &str) {
        let deadline = Instant::now() + self.capture_timeout;
        loop {
            if let Some(error) = self.child_error() {
                panic!(
                    "{context}: {}",
                    self.describe("the child failed before EOF", Some(error))
                );
            }
            if self.directory.join(OUT_FILE).exists() {
                let published = fs::read(self.directory.join(OUT_FILE)).unwrap_or_default();
                panic!(
                    "{context}: {}",
                    self.describe_against("the capture was published before EOF", &published, None)
                );
            }
            let received = self.partial_bytes();
            if received == self.expected {
                return;
            }
            if !self.expected.starts_with(&received) {
                panic!(
                    "{context}: {}",
                    self.describe_against(
                        "the child read bytes off the expected prefix",
                        &received,
                        None
                    )
                );
            }
            if Instant::now() >= deadline {
                panic!(
                    "{context}: {}",
                    self.describe(
                        &format!(
                            "the child never held every expected byte within {:?}",
                            self.capture_timeout
                        ),
                        None,
                    )
                );
            }
            std::thread::sleep(POLL_INTERVAL);
        }
    }
}

#[tokio::test]
async fn controlled_eof_publishes_only_at_true_eof() {
    let handler = RequestHandler::new();
    let session = SessionName::new("eof-unpublished").expect("valid session name");
    let slot = FinalSinkSlot::new("eof-unpublished", CAPTURE, false).until_controlled_eof();
    create_final_sink_session(&handler, &session, &slot).await;
    let target = PaneTarget::with_window(session.clone(), 0, 0);
    handler
        .wait_for_pane_startup_to_finish_for_test(&target)
        .await;
    slot.wait_until_ready();
    handler.disable_pane_focus_reports_for_test(&target).await;
    let requester_pid = std::process::id();
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, session.clone(), control_tx)
        .await;

    let delivered: Vec<u8> = INPUT.concat().into_iter().filter(|&b| b != 4).collect();
    assert_eq!(
        delivered, CAPTURE,
        "the input must deliver exactly the capture"
    );
    for bytes in INPUT {
        handler
            .handle_attached_live_input_for_test(requester_pid, bytes)
            .await
            .expect("pane input before EOF");
    }
    slot.assert_received_unpublished("LFs and flushing VEOFs must not publish");

    // The line buffer is empty after the last flush, so this VEOF is EOF.
    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x04")
        .await
        .expect("controlled end of the application stream");
    slot.assert_application_bytes("EOF must publish exactly the bytes read before it");
    let teardown = slot.teardown();
    assert!(
        matches!(teardown, TeardownOutcome::Acknowledged),
        "the published child must leave its park: {teardown}"
    );
}
