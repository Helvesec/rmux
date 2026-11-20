use super::LivePaneRender;
use crate::pane_io::deferred_passthrough::{DeferredPassthroughQueue, PaneOutputGeneration};
use crate::pane_transcript::{presentation_test_clock::CaptureExpiry, PaneTranscript};
use crate::renderer::PaneRenderDelta;
use rmux_core::{FloatingPaneRect, OptionStore, TerminalPassthrough};
use rmux_proto::{
    OptionName, ScopeSelector, SessionName, SetOptionMode, TerminalSize, WindowTarget,
};

#[test]
fn selected_presentation_keeps_rows_cursor_modes_and_projection_coherent_across_expiry() {
    for layout in 0..3 {
        for fallback in [false, true] {
            for replaceable in [false, true] {
                let mut session = crate::session_fixture::SessionFixture::new(
                    SessionName::new("capture-provenance").unwrap(),
                    TerminalSize { cols: 64, rows: 8 },
                );
                if layout != 0 {
                    session.split_active_pane().unwrap();
                }
                if layout == 2 {
                    session
                        .store
                        .window_layout_context_mut(&WindowTarget::with_window(
                            session.name().clone(),
                            0,
                        ))
                        .unwrap()
                        .float_pane(1, FloatingPaneRect::new(-3, 0, 48, 5).unwrap())
                        .unwrap();
                }
                let pane = session.window().active_pane().unwrap().clone();
                let transcript = PaneTranscript::shared(
                    100,
                    TerminalSize {
                        cols: pane.geometry().cols(),
                        rows: pane.geometry().rows(),
                    },
                );
                let mut options = OptionStore::new();
                if fallback {
                    options
                        .set(
                            ScopeSelector::Global,
                            OptionName::PaneColours,
                            "red".into(),
                            SetOptionMode::Replace,
                        )
                        .unwrap();
                }
                transcript.lock().append_bytes(b"---MATRIX_OLD");
                let mut renderer = LivePaneRender::new_from_transcript(
                    transcript.clone(),
                    session.view(),
                    options,
                    pane,
                )
                .unwrap();
                transcript.lock().append_bytes(
                    b"\x1b[?2026h\x1b[2J\x1b[H---MATRIX_FINAL\x1b[3;7H\x1b[?25l\x1b[5 q",
                );
                let _ = renderer.render_frame_from_transcript(true);
                assert!(renderer.delivered_frame().is_frozen());
                let old = renderer.snapshot.clone();
                let generation = PaneOutputGeneration::subscribed(7);
                let images = vec![
                    TerminalPassthrough::kitty_graphics(0, 0, b"Gf=100;SU1H".to_vec()),
                    TerminalPassthrough::sixel(0, 0, b"q~".to_vec()),
                    TerminalPassthrough::raw(0, 0, b"raw-image".to_vec()),
                ];
                let mut queue = DeferredPassthroughQueue::default();
                queue.defer(generation, images.clone());
                let _clock = CaptureExpiry::arm_on_selection();
                let PaneRenderDelta::Incremental(_) =
                    renderer.render_frame_from_transcript(replaceable)
                else {
                    panic!("matrix must remain renderable");
                };
                assert_eq!(
                    renderer.snapshot, old,
                    "layout={layout}, fallback={fallback}, replaceable={replaceable}"
                );
                assert!(queue
                    .take_renderable(
                        generation,
                        generation,
                        Vec::new(),
                        renderer.delivered_frame().is_frozen()
                    )
                    .is_empty());
                let PaneRenderDelta::Incremental(frame) =
                    renderer.render_frame_from_transcript(replaceable)
                else {
                    panic!("convergence must publish a frame");
                };
                assert!(frame
                    .frame()
                    .windows(b"MATRIX_FINAL".len())
                    .any(|w| w == b"MATRIX_FINAL"));
                assert!(!renderer.delivered_frame().is_frozen());
                assert_eq!(
                    queue.take_renderable(generation, generation, Vec::new(), false),
                    images
                );
                assert!(queue
                    .take_renderable(generation, generation, Vec::new(), false)
                    .is_empty());
            }
        }
    }
}
