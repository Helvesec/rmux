use super::PaneTranscript;
use rmux_core::{GridRenderOptions, ScreenCaptureRange};
use rmux_proto::TerminalSize;

fn visible_capture(transcript: &PaneTranscript) -> String {
    String::from_utf8(
        transcript.capture_main(ScreenCaptureRange::default(), GridRenderOptions::default()),
    )
    .expect("capture is utf8")
}

/// tmux 3.7b keeps a BEL inside a DCS as payload. Without the outer ST, the
/// `tmux;` wrapper swallows the following text and emits no passthrough; only
/// the input ground timeout discards it, after which output prints again.
#[test]
fn append_bytes_recovers_after_tmux_wrapped_osc_bel_without_outer_st() {
    let mut transcript = PaneTranscript::new(10, TerminalSize { cols: 40, rows: 4 });

    let swallowed = transcript.append_bytes_with_effects(b"\x1bPtmux;\x1b\x1b]4;0;?\x07OpenCode");

    assert!(swallowed.passthroughs.is_empty());
    assert!(swallowed.ground_timer.is_some());
    let capture = visible_capture(&transcript);
    assert!(!capture.contains("OpenCode"));
    assert!(!capture.contains("4;0;?"));

    transcript.make_ground_timer_due_for_test();
    let recovered = transcript.append_bytes_with_effects(b"AFTER");

    assert!(recovered.recovery_rebase_required);
    assert!(recovered.passthroughs.is_empty());
    assert_eq!(visible_capture(&transcript), "AFTER\n\n\n\n");
}
