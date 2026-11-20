use std::time::Duration;

use rmux_core::OptionStore;
use rmux_proto::{SessionName, TerminalSize};

use crate::pane_transcript::{
    PaneTranscript, SharedPaneTranscript, PANE_SYNCHRONIZED_OUTPUT_TIMEOUT,
};
use crate::renderer::PaneRenderDelta;

use super::LivePaneRender;

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

fn synchronized_output_renderer(split: bool) -> (Box<LivePaneRender>, SharedPaneTranscript) {
    let size = TerminalSize { cols: 48, rows: 5 };
    let mut session =
        crate::session_fixture::SessionFixture::new(session_name("sync-output"), size);
    if split {
        session
            .split_active_pane_with_direction(rmux_proto::SplitDirection::Vertical)
            .expect("split pane");
    }
    let pane = session.window().active_pane().expect("active pane").clone();
    let transcript = PaneTranscript::shared(
        100,
        TerminalSize {
            cols: pane.geometry().cols(),
            rows: pane.geometry().rows().saturating_sub(1).max(1),
        },
    );
    transcript
        .lock()
        .append_bytes(b"\x1b[2J\x1b[HSTABLE_BEFORE_SYNC");
    let renderer = LivePaneRender::new_from_transcript(
        transcript.clone(),
        session.view(),
        OptionStore::new(),
        pane,
    )
    .expect("initial render snapshot");
    (renderer, transcript)
}

fn incremental_bytes(delta: PaneRenderDelta) -> Vec<u8> {
    let PaneRenderDelta::Incremental(frame) = delta else {
        panic!("synchronized output fixture must stay incrementally renderable");
    };
    frame.frame().to_vec()
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn append(transcript: &SharedPaneTranscript, bytes: &[u8]) {
    transcript.lock().append_bytes(bytes);
}

#[test]
fn presentation_frozen_tracks_the_frame_the_client_received() {
    let (mut renderer, transcript) = synchronized_output_renderer(false);
    assert!(
        !renderer.delivered_frame().is_frozen(),
        "a stable pane renders the live screen"
    );

    append(&transcript, b"\x1b[?2026h\x1b[2J\x1b[HPAUSED_PARTIAL_ONLY");
    let paused = incremental_bytes(renderer.render_frame_from_transcript(true));
    assert!(
        !contains_bytes(&paused, b"PAUSED_PARTIAL_ONLY"),
        "{paused:?}"
    );
    assert!(
        renderer.delivered_frame().is_frozen(),
        "a frame withheld by mode 2026 must report itself as stale, so a positioned passthrough is held"
    );

    append(&transcript, b"\x1b[2J\x1b[HPAUSED_FINAL\x1b[?2026l");
    let released = incremental_bytes(renderer.render_frame_from_transcript(true));
    assert!(contains_bytes(&released, b"PAUSED_FINAL"), "{released:?}");
    assert!(
        !renderer.delivered_frame().is_frozen(),
        "the converged frame must release what the interval was holding"
    );
}

#[test]
fn presentation_frozen_follows_the_fail_open_deadline() {
    let (mut renderer, transcript) = synchronized_output_renderer(false);
    append(&transcript, b"\x1b[?2026h\x1b[2J\x1b[HPAUSED_PARTIAL_ONLY");
    let _ = renderer.render_frame_from_transcript(true);
    assert!(renderer.delivered_frame().is_frozen());

    // The child never sends `?2026l`. Progress comes from the bounded
    // fail-open read, and a held passthrough must not outlive it.
    std::thread::sleep(PANE_SYNCHRONIZED_OUTPUT_TIMEOUT + Duration::from_millis(100));

    let expired = incremental_bytes(renderer.render_frame_from_transcript(true));
    assert!(
        contains_bytes(&expired, b"PAUSED_PARTIAL_ONLY"),
        "the deadline must publish the pending screen: {expired:?}"
    );
    assert!(
        !renderer.delivered_frame().is_frozen(),
        "an expired interval renders the live screen, so nothing stays held"
    );
}

#[test]
fn synchronized_output_withholds_replaceable_full_frame_until_fragmented_release() {
    let (mut renderer, transcript) = synchronized_output_renderer(false);

    transcript.lock().append_bytes(b"\x1b[?20");
    let _ = renderer.render_frame_from_transcript(true);
    transcript
        .lock()
        .append_bytes(b"26h\x1b[2J\x1b[HPAUSED_PARTIAL_ONLY");

    let paused = incremental_bytes(renderer.render_frame_from_transcript(true));
    assert!(
        !contains_bytes(&paused, b"PAUSED_PARTIAL_ONLY"),
        "replaceable full frames must not publish child state inside mode 2026: {paused:?}"
    );

    transcript
        .lock()
        .append_bytes(b"\x1b[2J\x1b[HPAUSED_FINAL\x1b[?20");
    let still_paused = incremental_bytes(renderer.render_frame_from_transcript(true));
    assert!(
        !contains_bytes(&still_paused, b"PAUSED_FINAL"),
        "a fragmented release must remain withheld until its final byte"
    );
    transcript.lock().append_bytes(b"26l");

    let released = incremental_bytes(renderer.render_frame_from_transcript(true));
    assert!(contains_bytes(&released, b"PAUSED_FINAL"), "{released:?}");
    assert!(
        !contains_bytes(&released, b"PAUSED_PARTIAL_ONLY"),
        "release must publish only the converged screen: {released:?}"
    );
}

#[test]
fn synchronized_output_withholds_split_incremental_frame_until_release() {
    let (mut renderer, transcript) = synchronized_output_renderer(true);

    transcript.lock().append_bytes(b"\x1b[?20");
    let _ = renderer.render_interactive_frame_from_transcript();
    transcript
        .lock()
        .append_bytes(b"26h\x1b[2J\x1b[HPAUSED_PARTIAL_ONLY");

    let paused = incremental_bytes(renderer.render_interactive_frame_from_transcript());
    assert!(
        !contains_bytes(&paused, b"PAUSED_PARTIAL_ONLY"),
        "strict split-pane deltas must not publish child state inside mode 2026: {paused:?}"
    );

    transcript
        .lock()
        .append_bytes(b"\x1b[2J\x1b[HPAUSED_FINAL\x1b[?2026l");
    let released = incremental_bytes(renderer.render_interactive_frame_from_transcript());
    assert!(contains_bytes(&released, b"PAUSED_FINAL"), "{released:?}");
    assert!(
        !contains_bytes(&released, b"PAUSED_PARTIAL_ONLY"),
        "incremental release must converge without replaying the partial row: {released:?}"
    );
}

#[test]
fn optimized_capture_expiry_keeps_positioned_image_until_real_convergence() {
    use crate::pane_io::deferred_passthrough::{DeferredPassthroughQueue, PaneOutputGeneration};
    use crate::pane_transcript::presentation_test_clock::CaptureExpiry;
    use rmux_core::TerminalPassthrough;

    for split in [false, true] {
        for replaceable in [false, true] {
            let (mut renderer, transcript) = synchronized_output_renderer(split);
            append(
                &transcript,
                b"\x1b[?2026h\x1b[2J\x1b[HCONVERGED_IMAGE_ANCHOR",
            );
            let _ = renderer.render_frame_from_transcript(true);
            assert!(renderer.delivered_frame().is_frozen());
            let generation = PaneOutputGeneration::subscribed(1);
            let image = TerminalPassthrough::kitty_graphics(0, 0, b"Gf=100;SU1H".to_vec());
            let mut queue = DeferredPassthroughQueue::default();
            queue.defer(generation, vec![image.clone()]);
            let _clock = CaptureExpiry::arm();
            let frozen = incremental_bytes(renderer.render_frame_from_transcript(replaceable));
            assert!(!contains_bytes(&frozen, b"CONVERGED_IMAGE_ANCHOR"));
            let premature = queue.take_renderable(
                generation,
                generation,
                Vec::new(),
                renderer.delivered_frame().is_frozen(),
            );
            assert!(
                premature.is_empty(),
                "expiry relabeled frozen rows and released the image"
            );
            assert!(!queue.is_empty());
            let converged = incremental_bytes(renderer.render_frame_from_transcript(replaceable));
            assert!(contains_bytes(&converged, b"CONVERGED_IMAGE_ANCHOR"));
            assert!(!renderer.delivered_frame().is_frozen());
            assert_eq!(
                queue.take_renderable(generation, generation, Vec::new(), false),
                vec![image]
            );
            assert!(queue
                .take_renderable(generation, generation, Vec::new(), false)
                .is_empty());
        }
    }
}
