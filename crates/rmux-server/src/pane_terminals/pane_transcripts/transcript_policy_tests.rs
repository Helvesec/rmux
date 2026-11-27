use std::panic::{catch_unwind, AssertUnwindSafe};

use rmux_core::{PaneGeometry, PaneId};
use rmux_proto::{SessionName, TerminalSize};

use super::HandlerState;
use crate::pane_io::{pane_output_channel, PaneInvalidationReason, PaneObservationItem};
use crate::pane_terminal_lookup::SessionPane;
use crate::pane_transcript::PaneTranscript;

const SIZE: TerminalSize = TerminalSize { cols: 20, rows: 3 };

fn geometry(size: TerminalSize) -> SessionPane {
    SessionPane {
        id: PaneId::new(1),
        window_index: 0,
        index: 0,
        geometry: PaneGeometry::new(0, 0, size.cols, size.rows),
        window_size: size,
    }
}

#[test]
fn resize_policy_depends_on_output_and_whether_geometry_changes() {
    for with_output in [false, true] {
        for poisoned in [false, true] {
            let session = SessionName::new("resize-policy").unwrap();
            let mut state = HandlerState::default();
            let transcript = PaneTranscript::shared(64, SIZE);
            transcript.lock().append_bytes(b"preserved");
            state
                .transcripts
                .entry(session.clone())
                .or_default()
                .insert(PaneId::new(1), transcript.clone());
            let output = pane_output_channel();
            let (initial, (), mut observer) = output.capture_with_observer(|| ());
            if with_output {
                state
                    .pane_outputs
                    .entry(session.clone())
                    .or_default()
                    .insert(PaneId::new(1), output.clone());
            }
            if poisoned {
                transcript.poison_for_test();
            }
            state.resize_transcripts(&session, &[geometry(SIZE)], None);
            assert!(observer.try_recv_observed().is_none());
            let changed = TerminalSize { cols: 30, ..SIZE };
            let result = catch_unwind(AssertUnwindSafe(|| {
                state.resize_transcripts(&session, &[geometry(changed)], None);
            }));
            let strict_failure = poisoned && !with_output;
            assert_eq!(result.is_err(), strict_failure);
            let guard = transcript.lock_for_eof();
            assert_eq!(
                guard.screen().size(),
                if strict_failure { SIZE } else { changed }
            );
            assert!(guard
                .capture_main(Default::default(), Default::default())
                .starts_with(b"preserved"));
            drop(guard);
            if with_output {
                let Some(PaneObservationItem::Invalidated(invalidation)) =
                    observer.try_recv_observed()
                else {
                    panic!("changed geometry must invalidate once");
                };
                assert_eq!(invalidation.reason, PaneInvalidationReason::Resize);
                assert_eq!(
                    invalidation.boundary.invalidation_revision,
                    initial.invalidation_revision + 1
                );
                assert_eq!(
                    invalidation.boundary.next_output_sequence,
                    initial.next_output_sequence
                );
            }
            assert!(observer.try_recv_observed().is_none());
            if !strict_failure {
                state.resize_transcripts(&session, &[geometry(changed)], None);
                assert!(observer.try_recv_observed().is_none());
            }
            assert_eq!(catch_unwind(|| drop(transcript.lock())).is_err(), poisoned);
        }
    }
}

#[test]
fn repeated_resize_skips_output_lock_but_changed_resize_keeps_it_strict() {
    let session = SessionName::new("resize-precheck").unwrap();
    let mut state = HandlerState::default();
    let transcript = PaneTranscript::shared(64, SIZE);
    transcript.poison_for_test();
    state
        .transcripts
        .entry(session.clone())
        .or_default()
        .insert(PaneId::new(1), transcript.clone());
    let output = pane_output_channel();
    output.poison_state_for_test();
    state
        .pane_outputs
        .entry(session.clone())
        .or_default()
        .insert(PaneId::new(1), output);
    state.resize_transcripts(&session, &[geometry(SIZE)], None);
    assert!(catch_unwind(AssertUnwindSafe(|| {
        state.resize_transcripts(
            &session,
            &[geometry(TerminalSize { cols: 30, ..SIZE })],
            None,
        );
    }))
    .is_err());
    assert_eq!(transcript.lock_for_eof().screen().size(), SIZE);
}

#[test]
fn runtime_append_recovers_only_with_output() {
    for with_output in [false, true] {
        let session = SessionName::new("mutation-policy").unwrap();
        let mut state = HandlerState::default();
        let transcript = PaneTranscript::shared(64, SIZE);
        state
            .transcripts
            .entry(session.clone())
            .or_default()
            .insert(PaneId::new(1), transcript.clone());
        let output = pane_output_channel();
        if with_output {
            state
                .pane_outputs
                .entry(session.clone())
                .or_default()
                .insert(PaneId::new(1), output.clone());
        }
        state
            .append_bytes_to_runtime_pane_transcript(&session, PaneId::new(1), b"healthy")
            .unwrap();
        transcript.poison_for_test();
        let (initial, (), mut observer) = output.capture_with_observer(|| ());
        let result = catch_unwind(AssertUnwindSafe(|| {
            state
                .append_bytes_to_runtime_pane_transcript(&session, PaneId::new(1), b" recovered")
                .unwrap();
        }));
        assert_eq!(result.is_err(), !with_output);
        assert_eq!(
            transcript.lock_for_eof().output_sequence(),
            if with_output { 2 } else { 1 }
        );
        if with_output {
            let Some(PaneObservationItem::Invalidated(invalidation)) = observer.try_recv_observed()
            else {
                panic!("append must invalidate recovery");
            };
            assert_eq!(
                invalidation.reason,
                PaneInvalidationReason::TranscriptMutation
            );
            assert_eq!(
                invalidation.boundary.invalidation_revision,
                initial.invalidation_revision + 1
            );
            state
                .append_bytes_to_runtime_pane_transcript(&session, PaneId::new(1), b"")
                .unwrap();
        }
        assert!(observer.try_recv_observed().is_none());
        assert_eq!(transcript.lock_for_eof().history_limit(), 64);
    }
}

#[test]
fn pixel_only_resize_changes_winops_replies_for_split_pane() {
    let session = SessionName::new("cell-size").unwrap();
    let mut state = HandlerState::default();
    let transcript = PaneTranscript::shared(64, SIZE);
    state
        .transcripts
        .entry(session.clone())
        .or_default()
        .insert(PaneId::new(1), transcript.clone());
    let client = rmux_proto::TerminalGeometry::new(80, 24)
        .with_pixels(rmux_proto::TerminalPixels::new(800, 480));
    for (geometry_update, expected) in [
        (Some(client), &b"\x1b[6;20;10t\x1b[4;60;200t"[..]),
        (None, &b"\x1b[6;32;16t\x1b[4;96;320t"[..]),
    ] {
        state.resize_transcripts(&session, &[geometry(SIZE)], geometry_update);
        let result = transcript
            .lock()
            .append_bytes_and_take_replies(b"\x1b[16t\x1b[14t");
        assert_eq!(result.replies, expected);
    }
}
