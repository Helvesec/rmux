//! What the production transcript had observed when a final-sink proof stopped
//! waiting for its child's own bracketed-paste announcement.
//!
//! Both final-sink siblings hold their pane at the same precondition: the child
//! writes `ESC[?2004h` to its own terminal, the server's transcript parses it,
//! and only then is the pane's mode the production answer to whether this
//! destination is bracketed-paste aware. Nothing may stamp that mode; waiting
//! for it *is* the proof that a real child published the capability.
//!
//! When the wait expires the proof is over, but the diagnosis has to start. The
//! wait used to report only that the mode was never reached, which is the same
//! sentence for a pane that received nothing at all, a pane that received output
//! carrying no announcement, and a pane whose console deposited the
//! announcement as screen text instead of interpreting it as a sequence. Two
//! Windows 10 release executions, each behind a thirteen-minute compilation,
//! produced exactly that sentence and could not say which of those had
//! happened.
//!
//! Everything here is read from production state — the transcript's
//! applied-output count, its screen mode, its cursor and its rendered screen —
//! so the failure names the boundary the announcement stopped at instead of only
//! the deadline that expired. It observes; it never establishes.

use std::time::Duration;

use rmux_core::input::mode;
use rmux_proto::PaneTarget;

use crate::pane_terminals::{HandlerState, PaneCaptureRequest};

use super::escape_bytes;

/// Rendered wherever a pane has no transcript to answer with.
const NO_TRANSCRIPT: &str = "no transcript";

/// The announcement's printable tail, as it would appear on a screen that was
/// handed it as text.
///
/// The `ESC` is deliberately not part of the needle: a control character that
/// reached a screen buffer as text is whatever the capture renderer made of it,
/// while the seven bytes beside it are what identify the sequence.
const ANNOUNCEMENT_ON_SCREEN: &[u8] = b"[?2004h";

/// Everything the production transcript can say about a pane that was expected
/// to change bracketed-paste mode.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ObservedPaneOutput {
    /// The pane's screen mode bits.
    pub(crate) mode: Option<u32>,
    /// How many non-empty output chunks the transcript has applied, which is
    /// zero exactly when nothing the child wrote ever reached this pane.
    pub(crate) output_sequence: Option<u64>,
    /// Where the transcript's cursor stands. An announcement that arrived as
    /// text moved it; one that arrived as a mode change did not.
    pub(crate) cursor_position: Option<(u32, u32)>,
    /// The pane's rendered screen, which is where an announcement that was
    /// never interpreted would be legible.
    pub(crate) screen: Option<Vec<u8>>,
    /// Why the screen could not be rendered, when it could not.
    pub(crate) screen_unavailable: Option<String>,
}

impl ObservedPaneOutput {
    fn bracketed(&self) -> bool {
        self.mode
            .is_some_and(|bits| bits & mode::MODE_BRACKETPASTE != 0)
    }

    fn announcement_is_on_screen(&self) -> bool {
        self.screen.as_deref().is_some_and(|screen| {
            screen
                .windows(ANNOUNCEMENT_ON_SCREEN.len())
                .any(|window| window == ANNOUNCEMENT_ON_SCREEN)
        })
    }

    /// The boundary the announcement stopped at, in the transcript's own terms.
    fn boundary(&self) -> &'static str {
        if self.bracketed() {
            return "this pane is in bracketed-paste mode, so the wait was for the opposite state";
        }
        match self.output_sequence {
            None => "this pane has no transcript, so no child output could have been observed",
            Some(0) => {
                "this pane's transcript applied no output at all, \
                 so nothing the child wrote reached it"
            }
            Some(_) if self.announcement_is_on_screen() => {
                "the announcement reached this pane as screen text, \
                 so the child's console never interpreted it as a sequence"
            }
            Some(_) => "this pane applied child output, but never the announcement itself",
        }
    }
}

/// Reads, from production state alone, everything a missing announcement can be
/// attributed to.
///
/// A pane the session no longer resolves, and a capture the server refuses,
/// are both answers rather than failures: the caller is already reporting one
/// failure and must not be given a second.
pub(crate) fn observe_pane_output(state: &HandlerState, target: &PaneTarget) -> ObservedPaneOutput {
    let Some(pane_id) = state
        .sessions
        .session(target.session_name())
        .and_then(|session| session.window_at(target.window_index()))
        .and_then(|window| window.pane(target.pane_index()))
        .map(rmux_core::Pane::id)
    else {
        return ObservedPaneOutput::default();
    };
    let screen_state = state.pane_screen_state(target.session_name(), pane_id);
    let (screen, screen_unavailable) = match state.capture_transcript(target, screen_capture()) {
        Ok(screen) => (Some(screen), None),
        Err(error) => (None, Some(error.to_string())),
    };
    ObservedPaneOutput {
        mode: screen_state.as_ref().map(|observed| observed.mode),
        output_sequence: state.pane_output_sequence(target.session_name(), pane_id),
        cursor_position: screen_state.map(|observed| observed.cursor_position),
        screen,
        screen_unavailable,
    }
}

/// The ordinary visible-screen capture, which is what `capture-pane` renders.
fn screen_capture() -> PaneCaptureRequest {
    PaneCaptureRequest {
        range: rmux_core::ScreenCaptureRange::default(),
        options: rmux_core::GridRenderOptions::default(),
        alternate: false,
        use_mode_screen: false,
        pending_input: false,
        quiet: true,
        escape_pending: false,
        hyperlinks: false,
    }
}

/// The report a missing bracketed-paste transition fails with.
pub(crate) fn describe_missing_bracketed_mode(
    pane: &str,
    expected: bool,
    waited: Duration,
    observed: &ObservedPaneOutput,
) -> String {
    let mode = match observed.mode {
        Some(bits) => format!("{bits:#06x}"),
        None => NO_TRANSCRIPT.to_owned(),
    };
    let output_sequence = match observed.output_sequence {
        Some(sequence) => sequence.to_string(),
        None => NO_TRANSCRIPT.to_owned(),
    };
    let cursor = match observed.cursor_position {
        Some((column, row)) => format!("({column}, {row})"),
        None => NO_TRANSCRIPT.to_owned(),
    };
    let screen = match (&observed.screen, &observed.screen_unavailable) {
        (Some(screen), _) => escape_bytes(screen),
        (None, Some(reason)) => format!("unavailable: {reason}"),
        (None, None) => "unavailable".to_owned(),
    };
    format!(
        "pane {pane} never reached bracketed-paste mode {expected} within {waited:?}\n  \
         mode: {mode} (bracketed-paste: {})\n  \
         applied output chunks: {output_sequence}\n  \
         cursor: {cursor}\n  \
         screen: {screen}\n  \
         {}",
        observed.bracketed(),
        observed.boundary(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const WAITED: Duration = Duration::from_secs(30);

    fn observed(output_sequence: u64, cursor: (u32, u32), screen: &[u8]) -> ObservedPaneOutput {
        ObservedPaneOutput {
            mode: Some(0),
            output_sequence: Some(output_sequence),
            cursor_position: Some(cursor),
            screen: Some(screen.to_vec()),
            screen_unavailable: None,
        }
    }

    /// Every report carries the pane, the state that was waited for, the bound
    /// that expired and the mode actually observed.
    #[test]
    fn a_report_names_the_pane_the_expected_state_and_the_bound() {
        let report = describe_missing_bracketed_mode(
            "final-sink-aware:0.0",
            true,
            WAITED,
            &observed(4, (0, 0), b""),
        );

        assert!(
            report.starts_with(
                "pane final-sink-aware:0.0 never reached bracketed-paste mode true within 30s"
            ),
            "{report}"
        );
        assert!(
            report.contains("mode: 0x0000 (bracketed-paste: false)"),
            "{report}"
        );
        assert!(report.contains("applied output chunks: 4"), "{report}");
        assert!(report.contains("cursor: (0, 0)"), "{report}");
    }

    /// A pane that applied nothing has a different diagnosis from one that
    /// applied output without the announcement, and the two used to read alike.
    #[test]
    fn a_pane_that_applied_no_output_is_distinguished_from_one_that_applied_some() {
        let silent = describe_missing_bracketed_mode("p", true, WAITED, &observed(0, (0, 0), b""));
        let noisy = describe_missing_bracketed_mode("p", true, WAITED, &observed(7, (0, 0), b"x"));

        assert!(
            silent.contains("applied no output at all, so nothing the child wrote reached it"),
            "{silent}"
        );
        assert!(
            noisy.contains("applied child output, but never the announcement itself"),
            "{noisy}"
        );
    }

    /// The Windows case this correction exists for: a console that never
    /// interpreted the sequence leaves it legible on the screen instead.
    #[test]
    fn an_announcement_that_arrived_as_screen_text_is_named_as_such() {
        let report = describe_missing_bracketed_mode(
            "p",
            true,
            WAITED,
            &observed(2, (7, 0), b"\x1b[?2004h"),
        );

        assert!(
            report.contains(
                "reached this pane as screen text, \
                 so the child's console never interpreted it as a sequence"
            ),
            "{report}"
        );
        assert!(report.contains(r"screen: \x1b[?2004h"), "{report}");
        assert!(
            report.contains("cursor: (7, 0)"),
            "a cursor that moved is part of the same account: {report}"
        );
    }

    /// The unaware proofs wait for the opposite state, so a pane that did become
    /// aware must be reported as that and not as a missing announcement.
    #[test]
    fn a_pane_that_became_aware_when_it_should_not_have_is_reported_as_that() {
        let mut aware = observed(3, (0, 0), b"");
        aware.mode = Some(mode::MODE_BRACKETPASTE);

        let report = describe_missing_bracketed_mode("p", false, WAITED, &aware);

        assert!(report.contains("(bracketed-paste: true)"), "{report}");
        assert!(
            report.contains("is in bracketed-paste mode, so the wait was for the opposite state"),
            "{report}"
        );
    }

    /// A pane with no transcript, and a screen the server would not render, are
    /// reported rather than silently rendered as emptiness.
    #[test]
    fn an_unavailable_transcript_and_screen_are_both_reported() {
        let report = describe_missing_bracketed_mode(
            "p",
            true,
            WAITED,
            &ObservedPaneOutput {
                screen_unavailable: Some("no pane terminal".to_owned()),
                ..ObservedPaneOutput::default()
            },
        );

        assert!(report.contains("mode: no transcript"), "{report}");
        assert!(
            report.contains("applied output chunks: no transcript"),
            "{report}"
        );
        assert!(report.contains("cursor: no transcript"), "{report}");
        assert!(
            report.contains("screen: unavailable: no pane terminal"),
            "{report}"
        );
        assert!(
            report.contains("has no transcript, so no child output could have been observed"),
            "{report}"
        );
    }

    /// A long screen stays readable, and its elision is announced, exactly as
    /// every other byte dump in this harness.
    #[test]
    fn a_long_screen_is_elided_with_its_exact_size() {
        let screen = vec![b'x'; 600];

        let report =
            describe_missing_bracketed_mode("p", true, WAITED, &observed(1, (0, 0), &screen));

        assert!(report.contains("... [88 more byte(s)]"), "{report}");
    }
}
