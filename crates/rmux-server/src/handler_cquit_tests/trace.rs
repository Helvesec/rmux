//! CQUIT1-R6 — where the bytes a pane receives while it enters Copy come from.
//!
//! Windows reads `1|copy-mode` at `handler_cquit_tests.rs:125`, immediately
//! after the consultation is installed and before this guard has injected
//! anything: `CQUIT-NESTED`, the tree and `-q` are all still ahead. Only
//! `append_pty_bytes_with_effects` raises `unseen_changes`
//! (`pane_transcript.rs:1113-1118`), and its single caller is the pane's own
//! reader (`pane_io/reader.rs:603`), so a non-empty chunk really arrived from
//! this pane's terminal while the mode was up. Two origins are possible and
//! this module separates them without presupposing either:
//!
//!   * residue — output of the pane's preparation or of `register_attach`
//!     landing after the fixture's marker was consumed;
//!   * a repaint — bytes the pane's own terminal produces because
//!     `handle_copy_mode` resizes the session when it installs the mode
//!     (`handler_copy_mode.rs:193-201`). A silent producer does not exclude
//!     one: on Windows the console, not the child, is what redraws.
//!
//! An observer is opened on the pane's own `PaneOutputSender` at an atomic
//! frontier (`capture_with_observer`), so every chunk is attributed to the
//! interval it arrived in, with its sequence, its whole bytes, the
//! invalidations and their reasons, and the gaps. Nothing here waits, sleeps,
//! filters bytes, resets a flag or touches what the guard asserts: it reads
//! what the transcript, the layout and the pane runtime already hold, and
//! prints.

use super::RequestHandler;
use crate::pane_io::{PaneObservationItem, PaneOutputReceiver};
use rmux_core::events::OutputCursorItem;
use rmux_proto::PaneTarget;
use std::fmt::Write as _;
use std::io::Write as _;

/// The prefix every record carries, so one run's trace can be read out of the
/// harness's own output without guessing which line belongs to it.
const TAG: &str = "CQUIT1-R6";

/// Writes one record.
///
/// Nothing here may panic: the closing drain of `PaneOutputTrace` runs while a
/// failed assertion is unwinding, and a panic there would abort the process
/// instead of reporting that failure.
fn record(line: &str) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{TAG} {line}");
    let _ = out.flush();
}

/// The chunk exactly as it was published, with nothing dropped.
fn hex(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(text, "{byte:02x}");
    }
    text
}

/// One interval of a pane's own output, observed from an atomic frontier.
pub(super) struct PaneOutputTrace {
    label: &'static str,
    receiver: PaneOutputReceiver,
}

/// Opens an interval on the pane's own output sender and records its frontier.
pub(super) async fn observe(
    handler: &RequestHandler,
    target: &PaneTarget,
    label: &'static str,
) -> PaneOutputTrace {
    let sender = {
        let state = handler.state.lock().await;
        state
            .pane_output_for_target(
                target.session_name(),
                target.window_index(),
                target.pane_index(),
            )
            .expect("the pane has an output sender")
    };
    let (boundary, (), receiver) = sender.capture_with_observer(|| ());
    record(&format!(
        "{label} open generation={} next_sequence={} invalidation_revision={} \
         process_exit_revision={}",
        boundary.generation,
        boundary.next_output_sequence,
        boundary.invalidation_revision,
        boundary.process_exit_revision,
    ));
    PaneOutputTrace { label, receiver }
}

impl PaneOutputTrace {
    /// Records everything published since this interval opened or was last
    /// drained. It never blocks and never waits for anything to arrive: what is
    /// not there yet belongs to the next drain, or to the closing one.
    pub(super) fn drain(&mut self, at: &str) {
        let label = self.label;
        let mut items = 0_usize;
        let mut bytes = 0_usize;
        while let Some(item) = self.receiver.try_recv_observed() {
            items += 1;
            match item {
                PaneObservationItem::Output(OutputCursorItem::Event(event)) => {
                    bytes += event.byte_len();
                    record(&format!(
                        "{label} {at} output sequence={} bytes={} passthroughs={} hex={}",
                        event.sequence(),
                        event.byte_len(),
                        event.passthroughs().len(),
                        hex(event.bytes()),
                    ));
                }
                PaneObservationItem::Output(OutputCursorItem::Gap(gap)) => record(&format!(
                    "{label} {at} gap expected={} resume={} missed={} newest={}",
                    gap.expected_sequence(),
                    gap.resume_sequence(),
                    gap.missed_events(),
                    gap.newest_sequence(),
                )),
                PaneObservationItem::Invalidated(invalidation) => record(&format!(
                    "{label} {at} invalidated reason={:?} generation={} next_sequence={} \
                     invalidation_revision={} process_exit_revision={}",
                    invalidation.reason,
                    invalidation.boundary.generation,
                    invalidation.boundary.next_output_sequence,
                    invalidation.boundary.invalidation_revision,
                    invalidation.boundary.process_exit_revision,
                )),
                PaneObservationItem::ProcessExited { output_sequence } => record(&format!(
                    "{label} {at} process_exited output_sequence={output_sequence:?}"
                )),
            }
        }
        record(&format!("{label} {at} items={items} bytes={bytes}"));
    }
}

impl Drop for PaneOutputTrace {
    /// The last attribution this interval can make: a chunk published between
    /// the drain above and the guard's own reading is still reported here,
    /// including while the assertion that read it is unwinding.
    fn drop(&mut self) {
        self.drain("closing");
    }
}

/// What the pane holds at one point, read where it lives.
///
/// The identity of the tool it is still the same run of - the pane, its pid and
/// the output generation a recreated backend would move - then the sizes, the
/// cursor and the mode state. None of these is waited on, and none of them is
/// what the guard asserts.
pub(super) async fn facts(handler: &RequestHandler, target: &PaneTarget, at: &str) {
    let state = handler.state.lock().await;
    let session_name = target.session_name();
    let window_index = target.window_index();
    let pane_index = target.pane_index();
    let pid = state
        .pane_pid_in_window(session_name, window_index, pane_index)
        .map_or_else(
            |error| format!("unavailable({error})"),
            |pid| pid.to_string(),
        );
    let window = state
        .sessions
        .session(session_name)
        .and_then(|session| session.window_at(window_index));
    let pane = window.and_then(|window| window.pane(pane_index));
    let pane_id = pane.map(|pane| pane.id().to_string());
    let geometry = pane.map(|pane| {
        let geometry = pane.geometry();
        format!(
            "{}x{}+{}+{}",
            geometry.cols(),
            geometry.rows(),
            geometry.x(),
            geometry.y(),
        )
    });
    let window_size = window.map(|window| {
        let size = window.size();
        format!("{}x{}", size.cols, size.rows)
    });
    let generation = state
        .pane_output_for_target(session_name, window_index, pane_index)
        .map(|sender| sender.current_generation())
        .ok();
    let transcript = state
        .transcript_handle(target)
        .expect("the pane has a transcript");
    let transcript = transcript.lock();
    let size = transcript.screen().size();
    let cursor = transcript.screen().cursor_position();
    record(&format!(
        "{at} facts pane={pane_id:?} pid={pid} generation={generation:?} geometry={geometry:?} \
         window_size={window_size:?} screen={}x{} cursor={},{} history={} alternate={} \
         mode={:?} in_mode={} clock_generation={:?} unseen_changes={}",
        size.cols,
        size.rows,
        cursor.0,
        cursor.1,
        transcript.history_size(),
        transcript.is_alternate(),
        transcript.pane_mode_name(),
        transcript.pane_in_mode(),
        transcript.clock_mode_generation(),
        transcript.unseen_changes(),
    ));
}
