use super::*;
use crate::renderer::{
    render_popup_cursor, render_popup_overlay, OverlayRect, PopupContent, PopupCursor,
    PopupRenderSpec,
};
use rmux_core::{input::InputParser, BoxLines, GridRenderOptions, Screen, Style};

// Match PopupOverlayState::render: complete self-contained rows, then the
// projected cursor (or the hidden cursor of a noninteractive popup).
fn frame(text: &str, cursor: PopupCursor) -> OverlayFrame {
    let mut bytes = render_popup_overlay(&PopupRenderSpec {
        rect: OverlayRect {
            x: 2,
            y: 2,
            width: 20,
            height: 3,
        },
        title: String::new(),
        style: Style::default(),
        border_style: Style::default(),
        border_lines: BoxLines::None,
        content: PopupContent::Surface(vec![
            b"unchanged".to_vec(),
            text.as_bytes().to_vec(),
            b"last".to_vec(),
        ]),
    });
    bytes.extend_from_slice(&render_popup_cursor(cursor));
    OverlayFrame::persistent(bytes, 0, 1).with_row_diff()
}

fn repaint(overlay: &OverlayFrame) -> OverlayFrame {
    let mut repaint = OverlayFrame::persistent(overlay.frame.clone(), 0, 1).with_row_diff();
    repaint.defer_pane_updates = overlay.defer_pane_updates;
    repaint
}

fn snapshot(frames: &[&[u8]]) -> (Vec<Vec<u8>>, (u32, u32), bool) {
    let mut parser = InputParser::new();
    let mut screen = Screen::new(TerminalSize { cols: 40, rows: 10 }, 0);
    for bytes in frames {
        parser.parse(bytes, &mut screen);
    }
    let rows = (0..10)
        .map(|row| {
            screen
                .render_visible_line_independent_with_default_style(
                    row,
                    GridRenderOptions {
                        with_sequences: true,
                        include_empty_cells: true,
                        trim_spaces: false,
                        ..GridRenderOptions::default()
                    },
                    &Style::default(),
                )
                .unwrap()
        })
        .collect();
    (
        rows,
        screen.cursor_position(),
        screen.mode() & rmux_core::input::mode::MODE_CURSOR != 0,
    )
}

fn assert_delta(before: &[u8], after: &[u8], emitted: &[u8]) {
    assert!(
        !String::from_utf8_lossy(emitted).contains("unchanged"),
        "an unchanged popup row must not be emitted: {emitted:?}"
    );
    assert_eq!(
        snapshot(&[before, emitted]),
        snapshot(&[before, after]),
        "incremental output must preserve cells, cursor position and visibility"
    );
}

async fn through_pending_controls(frames: &[OverlayFrame]) {
    let name = SessionName::new("popup-cursor-pending").unwrap();
    let (stream, mut peer) = tokio::net::UnixStream::pair().unwrap();
    let stream = AttachTransport::from(stream);
    let mut target = open_attach_target(test_attach_target(&name, b"BASE", None), false).unwrap();
    let mut controls = VecDeque::new();
    let (_control_tx, mut control_rx) = mpsc::unbounded_channel();
    let backlog = AtomicUsize::new(0);
    let mut passthroughs = DeferredPassthroughQueue::default();
    let mut refresh = AttachRefreshScheduler::default();
    let mut render_generation = 0;
    let mut overlay_generation = 0;
    let mut cache = None;
    let mut visible = false;
    let mut state = None;
    let mut locked = false;
    let mut before = Vec::new();
    for (i, overlay) in frames.iter().enumerate() {
        controls.push_back(AttachControl::Overlay(repaint(overlay)));
        controls.push_back(AttachControl::Write(b"ACK-CURSOR".to_vec()));
        apply_pending_attach_controls(
            &mut controls,
            Some(&mut control_rx),
            &backlog,
            &mut target,
            &stream,
            &mut passthroughs,
            &mut refresh,
            &mut render_generation,
            &mut overlay_generation,
            &mut cache,
            &mut visible,
            &mut state,
            &mut locked,
            None,
        )
        .await
        .unwrap();
        let emitted = read_attach_data_until(&mut peer, b"ACK-CURSOR").await;
        let emitted = emitted.strip_suffix(b"ACK-CURSOR").unwrap();
        if i > 0 {
            assert_delta(&before, &overlay.frame, emitted);
        }
        assert_eq!(cache.as_deref(), Some(overlay.frame.as_slice()));
        assert!(visible);
        assert_eq!(
            cache.as_ref().unwrap().defer_pane_updates,
            overlay.defer_pane_updates
        );
        before = overlay.frame.clone();
    }
    // Explicit refresh restores the whole cached frame including the cursor.
    let full = frames.last().unwrap();
    let mut restore = repaint(full);
    restore.row_diff = false;
    controls.push_back(AttachControl::Overlay(restore));
    controls.push_back(AttachControl::Write(b"ACK-RESTORE".to_vec()));
    apply_pending_attach_controls(
        &mut controls,
        Some(&mut control_rx),
        &backlog,
        &mut target,
        &stream,
        &mut passthroughs,
        &mut refresh,
        &mut render_generation,
        &mut overlay_generation,
        &mut cache,
        &mut visible,
        &mut state,
        &mut locked,
        None,
    )
    .await
    .unwrap();
    let restored = read_attach_data_until(&mut peer, b"ACK-RESTORE").await;
    assert!(restored
        .windows(full.frame.len())
        .any(|bytes| bytes == full.frame));
}

async fn through_forward_attach(frames: &[OverlayFrame]) {
    let name = SessionName::new("popup-cursor-direct").unwrap();
    let handler = Arc::new(RequestHandler::new());
    let (stream, mut peer) = tokio::net::UnixStream::pair().unwrap();
    let (shutdown_tx, shutdown_rx) = watch::channel(());
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    let attach = tokio::spawn(forward_attach(
        stream,
        test_attach_target(&name, b"BASE-CURSOR", None),
        Vec::new(),
        shutdown_rx,
        control_rx,
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicU64::new(0)),
        LiveAttachInputContext::unregistered_for_test(handler, std::process::id()),
        false,
    ));
    read_attach_data_until(&mut peer, b"BASE-CURSOR").await;
    let mut before = Vec::new();
    for (i, overlay) in frames.iter().enumerate() {
        control_tx
            .send(AttachControl::Overlay(repaint(overlay)))
            .unwrap();
        control_tx
            .send(AttachControl::Write(b"ACK-CURSOR".to_vec()))
            .unwrap();
        let emitted = read_attach_data_until(&mut peer, b"ACK-CURSOR").await;
        let emitted = emitted.strip_suffix(b"ACK-CURSOR").unwrap();
        if i > 0 {
            assert_delta(&before, &overlay.frame, emitted);
        }
        before = overlay.frame.clone();
    }
    shutdown_tx.send(()).unwrap();
    attach.await.unwrap().unwrap();
}

#[tokio::test]
async fn popup_real_frame_delta_preserves_cursor_on_both_attach_paths() {
    let projected = |x, y, visible| PopupCursor::Projected { x, y, visible };
    let frames = [
        frame("\x1b[31m긴 문자열 abcdef", projected(8, 3, true)),
        frame("\x1b[32m짧음", projected(4, 3, true)),
        frame("\x1b[32m짧음", projected(7, 4, true)),
        frame("\x1b[32m짧음", projected(7, 4, false)),
        frame("", projected(4, 3, false)),
        frame("", projected(4, 3, false)),
        frame("", PopupCursor::Hidden),
        frame("", projected(2, 2, true)),
    ];
    through_pending_controls(&frames).await;
    through_forward_attach(&frames).await;
}

#[tokio::test]
async fn popup_delta_preserves_deferred_pane_policy_on_both_attach_paths() {
    let frames = [
        frame("before", PopupCursor::Hidden).with_deferred_pane_updates(),
        frame("after", PopupCursor::Hidden).with_deferred_pane_updates(),
        frame("after", PopupCursor::Hidden).with_deferred_pane_updates(),
    ];
    through_pending_controls(&frames).await;
    through_forward_attach(&frames).await;
}
