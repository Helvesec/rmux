use std::sync::{Arc, Mutex};

use rmux_core::PaneId;

use super::{PaneTranscript, PaneTranscriptUnavailable, SharedPaneTranscript, TerminalSize};

static_assertions::assert_impl_all!(SharedPaneTranscript: Clone, Send, Sync);
static_assertions::assert_not_impl_any!(SharedPaneTranscript:
    std::ops::Deref,
    AsRef<Mutex<PaneTranscript>>,
    AsRef<Arc<Mutex<PaneTranscript>>>,
    Into<Arc<Mutex<PaneTranscript>>>
);

const SIZE: TerminalSize = TerminalSize { cols: 20, rows: 3 };

#[test]
fn clone_preserves_identity_and_shared_mutations() {
    let transcript = PaneTranscript::shared(64, SIZE);
    let clone = transcript.clone();
    let other = PaneTranscript::shared(64, SIZE);
    assert!(transcript.same_instance(&clone));
    assert!(!transcript.same_instance(&other));
    transcript.lock().append_bytes(b"ordinary output");
    clone.lock().set_title("shared title");
    assert_eq!(clone.lock().output_sequence(), 1);
    assert_eq!(transcript.lock().title(), "shared title");
    assert_eq!(other.lock().output_sequence(), 0);
    assert_eq!(other.lock().title(), "");
    drop(transcript);
    assert_eq!(clone.lock().title(), "shared title");
}

#[test]
fn guard_keeps_the_lock_until_the_caller_drops_it() {
    let transcript = PaneTranscript::shared(64, SIZE);
    let clone = transcript.clone();
    let guard = transcript.lock();
    assert!(clone.lock_unavailable_for_test());
    drop(guard);
    assert!(!clone.lock_unavailable_for_test());
}

#[test]
fn ordinary_acquisition_fails_fast_naming_its_pane() {
    let transcript = PaneTranscript::shared_for_pane(PaneId::new(7), 64, SIZE);
    transcript.poison_for_test();
    let payload = std::panic::catch_unwind(|| drop(transcript.lock()))
        .expect_err("a poisoned transcript fails fast");
    let unavailable = payload
        .downcast::<PaneTranscriptUnavailable>()
        .expect("the abort names the pane");
    assert_eq!(unavailable.pane_id(), PaneId::new(7));
    assert_eq!(unavailable.to_string(), "pane %7 unavailable");
}

#[test]
fn exceptional_acquisitions_recover_without_clearing_poison() {
    for acquire in [
        SharedPaneTranscript::lock_for_output_mutation,
        SharedPaneTranscript::lock_for_eof,
        SharedPaneTranscript::lock_for_exit,
        SharedPaneTranscript::lock_for_recovery_capture,
    ] {
        let transcript = PaneTranscript::shared(64, SIZE);
        acquire(&transcript).append_bytes(b"healthy");
        transcript.poison_for_test();
        acquire(&transcript).append_bytes(b" recovered");
        assert_eq!(acquire(&transcript).output_sequence(), 2);
        assert!(std::panic::catch_unwind(|| drop(transcript.lock())).is_err());
    }
}

#[test]
fn resize_precheck_recovers_but_does_not_mutate() {
    let transcript = PaneTranscript::shared(64, SIZE);
    let changed = TerminalSize { cols: 40, ..SIZE };
    for poisoned in [false, true] {
        if poisoned {
            transcript.poison_for_test();
        }
        assert!(!transcript.needs_resize(SIZE));
        assert!(transcript.needs_resize(changed));
        assert_eq!(transcript.lock_for_eof().screen().size(), SIZE);
        assert!(!transcript.lock_unavailable_for_test() || poisoned);
    }
}

#[test]
fn cross_pane_sweep_skips_poison_without_clearing_it() {
    let transcript = PaneTranscript::shared(64, SIZE);
    transcript
        .lock_for_cross_pane_sweep()
        .expect("healthy sweep")
        .append_bytes(b"healthy");
    transcript.poison_for_test();
    assert!(transcript.lock_for_cross_pane_sweep().is_none());
    assert!(std::panic::catch_unwind(|| drop(transcript.lock())).is_err());
}

#[test]
fn preview_reads_frozen_presentation_and_discards_poison() {
    let transcript = PaneTranscript::shared(64, SIZE);
    transcript.lock().append_bytes(b"OLD");
    transcript.lock().append_bytes(b"\x1b[?2026h\x1b[HNEW");
    let preview = transcript.preview_screen().expect("healthy preview");
    let capture = preview.capture_transcript(Default::default(), Default::default());
    assert!(capture.starts_with(b"OLD"));
    transcript.poison_for_test();
    assert!(transcript.preview_screen().is_none());
    assert!(std::panic::catch_unwind(|| drop(transcript.lock())).is_err());
}
