use std::sync::mpsc;
use std::time::{Duration, Instant};

use rmux_core::input::mode::MODE_SYNC;
use rmux_proto::{SessionName, TerminalSize};

use super::{
    publish_pane_bytes, publish_pane_eof, run_pane_transcript_timer_worker, PanePublishContext,
    PaneTranscriptTimer, PaneTranscriptTimerJob,
};
use crate::pane_io::{pane_output_channel, PaneInvalidationReason, PaneObservationItem};
use crate::pane_transcript::PaneTranscript;

const SIZE: TerminalSize = TerminalSize { cols: 40, rows: 3 };

#[test]
fn poisoned_timer_jobs_mutate_and_worker_processes_the_next_job() {
    for synchronized in [false, true] {
        let (tx, rx) = mpsc::channel();
        let mut observations = Vec::new();
        // The second job demonstrates continued worker progress after poison.
        for poisoned in [true, false] {
            let transcript = PaneTranscript::shared(64, SIZE);
            let append = transcript
                .lock()
                .append_bytes_with_effects(if synchronized {
                    b"\x1b[?2026hFINAL"
                } else {
                    b"\x1bPunterminated"
                });
            let deadline = Instant::now() - Duration::from_millis(1);
            let timer = if synchronized {
                let mut timer = append.synchronized_output_timer.expect("sync timer");
                timer.deadline = deadline;
                PaneTranscriptTimer::SynchronizedOutput(timer)
            } else {
                let mut timer = append.ground_timer.expect("ground timer");
                timer.deadline = deadline;
                PaneTranscriptTimer::Ground(timer)
            };
            if poisoned {
                transcript.poison_for_test();
            }
            let output = pane_output_channel();
            observations.push((transcript.clone(), output.subscribe(), poisoned));
            tx.send(PaneTranscriptTimerJob {
                transcript,
                pane_output: output,
                timer,
            })
            .unwrap();
        }
        drop(tx);
        run_pane_transcript_timer_worker(rx);
        for (transcript, mut observer, poisoned) in observations {
            let guard = transcript.lock_for_eof();
            assert_eq!(guard.mode() & MODE_SYNC, 0);
            assert!(guard.pending_bytes().is_empty());
            if synchronized {
                assert!(guard
                    .capture_main(Default::default(), Default::default())
                    .starts_with(b"FINAL"));
            }
            drop(guard);
            assert!(matches!(observer.try_recv_observed(),
                Some(PaneObservationItem::Invalidated(i))
                    if i.reason == PaneInvalidationReason::ParserStateExpired));
            assert!(observer.try_recv_observed().is_none());
            assert_eq!(
                std::panic::catch_unwind(|| drop(transcript.lock())).is_err(),
                poisoned
            );
        }
    }
}

#[test]
fn eof_preserves_poison_and_generation_boundaries() {
    for poisoned in [false, true] {
        for synchronized in [false, true] {
            let transcript = PaneTranscript::shared(64, SIZE);
            transcript.lock().append_bytes(if synchronized {
                b"OLD\x1b[?2026h\x1b[HFINAL"
            } else {
                b"FINAL"
            });
            if poisoned {
                transcript.poison_for_test();
            }
            let output = pane_output_channel();
            output.set_generation(7);
            let before = output.capture_with_observer(|| ()).0;
            let mut observer = output.subscribe();
            let mut raw = output.subscribe();
            publish_pane_eof(&transcript, &output, Some(6));
            assert_eq!(output.capture_with_observer(|| ()).0, before);
            assert!(observer.try_recv_observed().is_none());
            assert!(raw.try_recv().is_none());
            assert_eq!(
                transcript.lock_for_eof().mode() & MODE_SYNC != 0,
                synchronized
            );

            publish_pane_eof(&transcript, &output, Some(7));
            let guard = transcript.lock_for_eof();
            assert_eq!(guard.mode() & MODE_SYNC, 0);
            assert!(guard
                .capture_main(Default::default(), Default::default())
                .starts_with(b"FINAL"));
            drop(guard);
            let exit_sequence = if synchronized { None } else { Some(0) };
            assert!(matches!(observer.try_recv_observed(),
                Some(PaneObservationItem::ProcessExited { output_sequence })
                    if output_sequence == exit_sequence));
            if synchronized {
                assert!(matches!(observer.try_recv_observed(),
                    Some(PaneObservationItem::Invalidated(i))
                        if i.reason == PaneInvalidationReason::TranscriptMutation));
            }
            assert!(observer.try_recv_observed().is_none());
            assert!(matches!(raw.try_recv(),
                Some(rmux_core::events::OutputCursorItem::Event(event))
                    if event.is_empty() && event.sequence() == 0));
            assert!(raw.try_recv().is_none());
            assert_eq!(
                std::panic::catch_unwind(|| drop(transcript.lock())).is_err(),
                poisoned
            );
        }
    }
}

#[test]
fn byte_publication_recovers_poison_and_rejects_stale_generation_before_locking() {
    let transcript = PaneTranscript::shared(64, SIZE);
    transcript.poison_for_test();
    let output = pane_output_channel();
    output.set_generation(7);
    let mut observer = output.subscribe();
    let session = SessionName::new("publication-policy").unwrap();
    let publish = |generation, bytes: &[u8]| {
        publish_pane_bytes(
            PanePublishContext {
                session_name: &session,
                pane_id: rmux_core::PaneId::new(1),
                transcript: &transcript,
                pane_output: &output,
                generation: Some(generation),
                pane_alert_callback: None,
                status_terminal: None,
                emit_no_bell_alert: false,
            },
            bytes.to_vec(),
        )
    };
    assert!(publish(6, b"rejected").is_empty());
    assert_eq!(transcript.lock_for_eof().output_sequence(), 0);
    assert!(observer.try_recv_observed().is_none());

    // The reader keeps parsing its own pane's output through the poison, and
    // the output state it publishes under stays usable for the next batch.
    assert!(publish(7, b"first").is_empty());
    assert!(publish(7, b" second").is_empty());
    let guard = transcript.lock_for_eof();
    assert_eq!(guard.output_sequence(), 2);
    assert!(guard
        .capture_main(Default::default(), Default::default())
        .starts_with(b"first second"));
    drop(guard);
    for expected in [b"first".as_slice(), b" second".as_slice()] {
        assert!(matches!(observer.try_recv_observed(),
            Some(PaneObservationItem::Output(rmux_core::events::OutputCursorItem::Event(event)))
                if event.bytes() == expected));
    }
    assert!(observer.try_recv_observed().is_none());
    assert!(std::panic::catch_unwind(|| drop(transcript.lock())).is_err());
}
