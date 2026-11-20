//! Repeating `set-option history-limit 0` retrims the row that collection at
//! zero retains, and a transfer between sessions keeps each pane's own limit.
//!
//! Both transcript paths are covered: with a pane output, where the mutation is
//! published, and without one, where the final state must be the same.

use rmux_core::PaneId;
use rmux_proto::{
    OptionName, OptionScopeSelector, ScopeSelector, SessionName, SetOptionMode, TerminalSize,
};

use super::HandlerState;
use crate::pane_io::{pane_output_channel, PaneInvalidationReason, PaneObservationItem};
use crate::pane_transcript::PaneTranscript;

const SIZE: TerminalSize = TerminalSize { cols: 20, rows: 3 };
const PANE: PaneId = PaneId::new(1);
/// Home, then erase-in-display 2, which transfers the used rows.
const CLEAR: &[u8] = b"\x1b[H\x1b[2J";

fn session() -> SessionName {
    SessionName::new("history-limit-zero").expect("valid session name")
}

fn set_session_history_limit(state: &mut HandlerState, session_name: &SessionName, value: &str) {
    state
        .options
        .set(
            ScopeSelector::Session(session_name.clone()),
            OptionName::HistoryLimit,
            value.to_owned(),
            SetOptionMode::Replace,
        )
        .expect("history-limit accepts the value");
}

/// Installs a transcript born with a positive limit, with an optional output.
fn install(state: &mut HandlerState, session_name: &SessionName, with_output: bool) -> Installed {
    install_with_limit(state, session_name, with_output, 3)
}

fn install_with_limit(
    state: &mut HandlerState,
    session_name: &SessionName,
    with_output: bool,
    limit: usize,
) -> Installed {
    install_pane(state, session_name, PANE, with_output, limit)
}

fn install_pane(
    state: &mut HandlerState,
    session_name: &SessionName,
    pane_id: PaneId,
    with_output: bool,
    limit: usize,
) -> Installed {
    let transcript = PaneTranscript::shared(limit, SIZE);
    state
        .transcripts
        .entry(session_name.clone())
        .or_default()
        .insert(pane_id, transcript.clone());
    let output = pane_output_channel();
    let (_, (), observer) = output.capture_with_observer(|| ());
    if with_output {
        state
            .pane_outputs
            .entry(session_name.clone())
            .or_default()
            .insert(pane_id, output.clone());
    }
    Installed {
        transcript,
        output,
        observer,
    }
}

struct Installed {
    transcript: crate::pane_transcript::SharedPaneTranscript,
    output: crate::pane_io::PaneOutputSender,
    observer: crate::pane_io::PaneOutputReceiver,
}

impl Installed {
    fn history_size(&self) -> usize {
        self.transcript.lock().history_size()
    }

    fn history_limit(&self) -> usize {
        self.transcript.lock().history_limit()
    }

    /// The active grid's history and the presented one, which the synchronized
    /// output snapshot can hold apart.
    fn history_pair(&self) -> (usize, usize) {
        let guard = self.transcript.lock();
        (guard.history_size(), guard.presentation_history_size())
    }

    fn boundary(&self) -> crate::pane_io::PaneBoundary {
        let (boundary, (), _) = self.output.capture_with_observer(|| ());
        boundary
    }

    fn boundary_still_subscribable(&self, boundary: crate::pane_io::PaneBoundary) -> bool {
        self.output.subscribe_at_boundary(boundary).is_some()
    }

    fn feed(&self, bytes: &[u8]) {
        self.transcript.lock().append_bytes(bytes);
    }

    fn take_invalidation(&mut self) -> Option<PaneInvalidationReason> {
        match self.observer.try_recv_observed() {
            Some(PaneObservationItem::Invalidated(invalidation)) => Some(invalidation.reason),
            _ => None,
        }
    }

    fn drain_observations(&mut self) {
        while self.observer.try_recv_observed().is_some() {}
    }
}

#[test]
fn repeated_zero_option_mutation_retrims_the_collected_row() {
    for with_output in [false, true] {
        let session_name = session();
        let mut state = HandlerState::default();
        let mut installed = install(&mut state, &session_name, with_output);

        // A real mutation to zero, through the option branch.
        set_session_history_limit(&mut state, &session_name, "0");
        state.refresh_transcript_limits_for_scope(
            &ScopeSelector::Session(session_name.clone()),
            OptionName::HistoryLimit,
        );
        assert_eq!(installed.history_limit(), 0, "with_output={with_output}");
        installed.drain_observations();

        // Real bytes: collection at zero keeps the last transferred row.
        installed.feed(b"A1\r\nA2\r\nA3\r\n");
        installed.feed(CLEAR);
        assert_eq!(installed.history_size(), 1, "with_output={with_output}");

        // Another option's refresh leaves it alone too.
        state.refresh_transcript_limits_for_scope(
            &ScopeSelector::Global,
            OptionName::InputBufferSize,
        );
        assert_eq!(installed.history_size(), 1, "with_output={with_output}");
        installed.drain_observations();

        // Repeating the same zero through the option branch drops the row.
        state.refresh_transcript_limits_for_scope(
            &ScopeSelector::Session(session_name.clone()),
            OptionName::HistoryLimit,
        );
        assert_eq!(
            installed.history_size(),
            0,
            "a repeated zero option mutation retrims, with_output={with_output}"
        );
        assert_eq!(
            installed.take_invalidation(),
            with_output.then_some(PaneInvalidationReason::TranscriptMutation),
            "the dropped row is published only where an output exists"
        );

        // Collection stays on: the next clear retains one row again.
        installed.feed(b"B1\r\nB2\r\nB3\r\n");
        installed.feed(CLEAR);
        assert_eq!(installed.history_size(), 1, "with_output={with_output}");
    }
}

/// tmux 3.7b gives a pane the limit of the session that spawns it and changes
/// it only on a `history-limit` mutation (`session_update_history`); join-pane,
/// move-pane, swap-pane, move-window and swap-window move the pane as it is.
/// A transfer between sessions therefore keeps the moved pane's limit and rows,
/// leaves the destination's own panes alone and publishes nothing; the next
/// mutation then applies each session's limit to the panes it holds.
#[test]
fn a_transfer_between_sessions_keeps_each_pane_limit_and_rows() {
    const MOVED: PaneId = PaneId::new(2);
    let mut state = HandlerState::default();
    let source = SessionName::new("transfer-source").expect("valid session name");
    let destination = SessionName::new("transfer-destination").expect("valid session name");
    set_session_history_limit(&mut state, &source, "6");
    set_session_history_limit(&mut state, &destination, "1");
    let mut moved = install_pane(&mut state, &source, MOVED, true, 6);
    // A pane born elsewhere, whose limit differs from its session's.
    let mut resident = install_pane(&mut state, &destination, PANE, true, 4);
    moved.feed(b"M0\r\nM1\r\nM2\r\nM3\r\nM4\r\nM5\r\n");
    resident.feed(b"R0\r\nR1\r\nR2\r\nR3\r\nR4\r\n");
    moved.drain_observations();
    resident.drain_observations();
    let before = (
        (moved.history_limit(), moved.history_pair()),
        (resident.history_limit(), resident.history_pair()),
    );
    assert_eq!((before.0 .0, before.1 .0), (6, 4));
    assert!(before.0 .1 .0 > 1 && before.1 .1 .0 > 1, "{before:?}");

    state
        .move_pane_outputs_between_sessions(&source, &destination, &[MOVED])
        .expect("join or move into the destination");
    let after_move = (
        (moved.history_limit(), moved.history_pair()),
        (resident.history_limit(), resident.history_pair()),
    );
    assert_eq!(
        after_move, before,
        "a move keeps every pane's limit and rows"
    );

    state
        .move_pane_outputs_between_sessions(&destination, &source, &[MOVED])
        .expect("move back to the source");
    state
        .swap_pane_outputs_between_sessions(&source, &[MOVED], &destination, &[PANE])
        .expect("swap the two panes");
    let after_swap = (
        (moved.history_limit(), moved.history_pair()),
        (resident.history_limit(), resident.history_pair()),
    );
    assert_eq!(
        after_swap, before,
        "a swap keeps every pane's limit and rows"
    );
    assert_eq!(moved.take_invalidation(), None);
    assert_eq!(resident.take_invalidation(), None);

    // The next mutation applies each session's limit to the panes it now
    // holds: the moved pane takes the destination's, the resident the source's.
    set_session_history_limit(&mut state, &destination, "2");
    state.refresh_transcript_limits_for_scope(
        &ScopeSelector::Session(destination.clone()),
        OptionName::HistoryLimit,
    );
    assert_eq!((moved.history_limit(), resident.history_limit()), (2, 6));
}

/// Repeating zero on an already empty history runs the setter but publishes
/// nothing.
#[test]
fn repeated_zero_on_an_empty_history_does_not_invalidate() {
    let session_name = session();
    let mut state = HandlerState::default();
    let mut installed = install(&mut state, &session_name, true);

    set_session_history_limit(&mut state, &session_name, "0");
    state.refresh_transcript_limits_for_scope(
        &ScopeSelector::Session(session_name.clone()),
        OptionName::HistoryLimit,
    );
    installed.drain_observations();
    assert_eq!(installed.history_size(), 0);

    state.refresh_transcript_limits_for_scope(
        &ScopeSelector::Session(session_name.clone()),
        OptionName::HistoryLimit,
    );

    assert_eq!(installed.history_size(), 0);
    assert_eq!(
        installed.take_invalidation(),
        None,
        "repeating zero over an empty history is not a mutation to publish"
    );
}

/// The synchronized-output snapshot can hold a row the active grid no longer
/// has, and the setter clears both: dropping it must still be published.
///
/// The frozen presentation is established with real bytes — `DECSET 2026` then
/// `ED3` — and asserted as an explicit precondition, so nothing here depends on
/// a synchronization timeout. An expiry would collapse the two views and is not
/// a success of this fixture.
#[test]
fn a_repeated_zero_that_clears_only_the_presented_history_is_published() {
    for with_output in [false, true] {
        let session_name = session();
        let mut state = HandlerState::default();
        let mut installed = install(&mut state, &session_name, with_output);

        set_session_history_limit(&mut state, &session_name, "0");
        state.refresh_transcript_limits_for_scope(
            &ScopeSelector::Session(session_name.clone()),
            OptionName::HistoryLimit,
        );
        installed.feed(b"A1\r\nA2\r\nA3\r\n");
        installed.feed(CLEAR);
        assert_eq!(
            installed.history_pair(),
            (1, 1),
            "with_output={with_output}"
        );

        // Freeze the presentation, then clear the active scrollback only.
        installed.feed(b"\x1b[?2026h");
        installed.feed(b"\x1b[3J");
        assert_eq!(
            installed.history_pair(),
            (0, 1),
            "precondition: the presented row outlives the active one, with_output={with_output}"
        );
        installed.drain_observations();
        let before = with_output.then(|| installed.boundary());

        state.refresh_transcript_limits_for_scope(
            &ScopeSelector::Session(session_name.clone()),
            OptionName::HistoryLimit,
        );

        assert_eq!(
            installed.history_pair(),
            (0, 0),
            "the setter clears the snapshot too, with_output={with_output}"
        );
        assert_eq!(
            installed.take_invalidation(),
            with_output.then_some(PaneInvalidationReason::TranscriptMutation),
            "the presented row disappearing is published, with_output={with_output}"
        );
        if let Some(before) = before {
            assert!(
                !installed.boundary_still_subscribable(before),
                "the boundary captured before the mutation must not be reusable"
            );
        }

        // Collection is still on: leaving synchronized output and clearing again
        // retains one row.
        installed.feed(b"\x1b[?2026l");
        installed.feed(b"B1\r\nB2\r\nB3\r\n");
        installed.feed(CLEAR);
        assert_eq!(
            installed.history_pair(),
            (1, 1),
            "with_output={with_output}"
        );
    }
}

fn set_global_limit(state: &mut HandlerState, value: &str) {
    state
        .options
        .set(
            ScopeSelector::Global,
            OptionName::HistoryLimit,
            value.to_owned(),
            SetOptionMode::Replace,
        )
        .unwrap();
    state.refresh_transcript_limits_for_scope(&ScopeSelector::Global, OptionName::HistoryLimit);
}

#[test]
fn masked_global_mutation_collects_alpha_and_unset_also_collects_sibling() {
    for with_output in [false, true] {
        let mut state = HandlerState::default();
        let alpha = SessionName::new("small-alpha").unwrap();
        let beta = SessionName::new("small-beta").unwrap();
        set_global_limit(&mut state, "0");
        set_session_history_limit(&mut state, &alpha, "1");
        let mut a = install_with_limit(&mut state, &alpha, with_output, 1);
        let b_cold = install_with_limit(&mut state, &beta, with_output, 0);
        a.feed(b"A0\r\nA1\r\nA2\r\nA3\r\nA4\r\nA5\r\n");
        b_cold.feed(b"B0\r\nB1\r\nB2\r\nB3\r\nB4\r\nB5\r\n");
        assert_eq!((a.history_size(), b_cold.history_size()), (1, 0));
        assert_eq!(
            state
                .options
                .resolve(Some(&alpha), OptionName::HistoryLimit),
            Some("1")
        );
        state.refresh_transcript_limits_for_scope(
            &ScopeSelector::Global,
            OptionName::InputBufferSize,
        );
        assert_eq!(
            a.history_size(),
            1,
            "reads and unrelated options do not collect"
        );
        assert_eq!(a.take_invalidation(), None);
        set_global_limit(&mut state, "2");
        assert_eq!((a.history_limit(), b_cold.history_limit()), (1, 2));
        assert_eq!((a.history_size(), b_cold.history_size()), (0, 0));
        assert_eq!(
            a.take_invalidation(),
            with_output.then_some(PaneInvalidationReason::TranscriptMutation)
        );
        b_cold.feed(b"B6\r\nB7\r\nB8\r\n");
        assert_eq!(
            b_cold.history_size(),
            0,
            "raising cold zero keeps collection disabled"
        );

        // A fresh beta pane inherits2; old alpha still collects at local1.
        let mut b = install_with_limit(&mut state, &beta, with_output, 2);
        b.feed(b"C0\r\nC1\r\nC2\r\nC3\r\nC4\r\nC5\r\n");
        a.feed(b"A6\r\nA7\r\nA8\r\n");
        assert_eq!((a.history_size(), b.history_size()), (1, 2));
        state
            .options
            .set_by_name(
                OptionScopeSelector::Session(alpha.clone()),
                "history-limit",
                None,
                SetOptionMode::Replace,
                false,
                true,
                false,
            )
            .unwrap();
        state.refresh_transcript_limits_for_scope(
            &ScopeSelector::Session(alpha.clone()),
            OptionName::HistoryLimit,
        );
        assert_eq!((a.history_limit(), b.history_limit()), (2, 2));
        assert_eq!(
            (a.history_size(), b.history_size()),
            (1, 1),
            "unset raises alpha but collects saturated sibling at equality"
        );
        for pane in [&mut a, &mut b] {
            assert_eq!(
                pane.take_invalidation(),
                with_output.then_some(PaneInvalidationReason::TranscriptMutation)
            );
        }
        // A local reduction to the current size collects one; the now-underfull
        // sibling is unchanged and must not publish an invalidation.
        set_session_history_limit(&mut state, &alpha, "1");
        state.refresh_transcript_limits_for_scope(
            &ScopeSelector::Session(alpha.clone()),
            OptionName::HistoryLimit,
        );
        assert_eq!((a.history_size(), b.history_size()), (0, 1));
        assert_eq!(
            a.take_invalidation(),
            with_output.then_some(PaneInvalidationReason::TranscriptMutation)
        );
        assert_eq!(b.take_invalidation(), None);
    }
}

#[test]
fn repeated_positive_limit_collects_at_equality_but_not_below_it() {
    for with_output in [false, true] {
        for limit in [1, 2] {
            let mut state = HandlerState::default();
            let name = session();
            set_session_history_limit(&mut state, &name, &limit.to_string());
            let mut pane = install_with_limit(&mut state, &name, with_output, limit);
            pane.feed(b"L0\r\nL1\r\nL2\r\nL3\r\nL4\r\nL5\r\n");
            assert_eq!(pane.history_size(), limit);
            for expected_event in [true, false] {
                set_session_history_limit(&mut state, &name, &limit.to_string());
                state.refresh_transcript_limits_for_scope(
                    &ScopeSelector::Session(name.clone()),
                    OptionName::HistoryLimit,
                );
                assert_eq!(pane.history_size(), limit - 1);
                assert_eq!(
                    pane.take_invalidation(),
                    (with_output && expected_event)
                        .then_some(PaneInvalidationReason::TranscriptMutation)
                );
            }
        }
    }
}

#[test]
fn repeated_positive_limit_publishes_collection_from_frozen_presentation_only() {
    for with_output in [false, true] {
        let mut state = HandlerState::default();
        let name = session();
        set_session_history_limit(&mut state, &name, "1");
        let mut pane = install_with_limit(&mut state, &name, with_output, 1);
        pane.feed(b"L0\r\nL1\r\nL2\r\nL3\r\n");
        assert_eq!(pane.history_pair(), (1, 1));
        pane.feed(b"\x1b[?2026h\x1b[3J");
        assert_eq!(pane.history_pair(), (0, 1));
        state.refresh_transcript_limits_for_scope(
            &ScopeSelector::Session(name),
            OptionName::HistoryLimit,
        );
        assert_eq!(pane.history_pair(), (0, 0));
        assert_eq!(
            pane.take_invalidation(),
            with_output.then_some(PaneInvalidationReason::TranscriptMutation)
        );
    }
}
