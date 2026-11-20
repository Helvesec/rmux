use super::*;
use crate::copy_mode::{CopyModeRenderOverlays, CopyModeRenderSnapshot};
use crate::pane_transcript::PaneTranscript;
use crate::renderer::pane_delta::{PaneRenderDelta, PaneRenderSnapshot};

fn options(position: &str, pad: u16) -> OptionStore {
    let mut options = OptionStore::new();
    for (name, value) in [
        (OptionName::Status, "off".to_owned()),
        (OptionName::PaneBorderStatus, "off".to_owned()),
        (OptionName::PaneScrollbars, "on".to_owned()),
        (OptionName::PaneScrollbarsPosition, position.to_owned()),
        (
            OptionName::PaneScrollbarsStyle,
            format!("fg=white,bg=blue,width=1,pad={pad}"),
        ),
    ] {
        options
            .set(ScopeSelector::Global, name, value, SetOptionMode::Replace)
            .unwrap();
    }
    options
}

fn assert_decoration(frame: &[u8], x: usize, pad_only: bool, prompt: bool) {
    assert!(!frame.is_empty(), "visible decoration must produce a frame");
    let mut terminal = screen_with(
        b"XXXXXXXXXX\r\nXXXXXXXXXX\r\nXXXXXXXXXX\r\nXXXXXXXXXX",
        TerminalSize { cols: 10, rows: 4 },
    );
    InputParser::new().parse(frame, &mut terminal);
    for y in 0..4 {
        let mut cells = Vec::new();
        terminal.visit_visible_line_cells(y, 10, |cell| cells.push(cell.text().to_owned()));
        assert_eq!(cells[x], " ", "decoration row {y}: {frame:?}");
        for (col, cell) in cells.iter().enumerate() {
            if col != x {
                assert_eq!(cell, "X", "hidden body/neighbor ({col},{y}): {frame:?}");
            }
        }
    }
    if !pad_only {
        assert!(
            frame
                .windows(3)
                .any(|bytes| bytes == b";44" || bytes == b";47"),
            "track style: {frame:?}"
        );
    }
    assert_eq!(
        terminal.mode() & rmux_core::input::mode::MODE_CURSOR != 0,
        prompt,
        "only the prompt may restore a cursor: {frame:?}"
    );
}

#[test]
fn scrollbar_only_survives_occlusion_and_clipping_across_consumers() {
    // Before correction: body [1,10) is occluded by frame [1,10),
    // while track [0,1) must still paint. Mirror and clip at each edge.
    for (position, clipped, pad_only) in [
        ("left", false, false),
        ("right", false, false),
        ("left", true, false),
        ("right", true, false),
        ("left", false, true),
        ("right", false, true),
    ] {
        let mut session = crate::session_fixture::SessionFixture::new(
            session_name("scrollbar-only"),
            TerminalSize { cols: 10, rows: 4 },
        );
        let options = options(position, u16::from(pad_only));
        let left = position == "left";
        let x = if left { 0 } else { 9 };
        if clipped {
            float_pane(
                &mut session,
                0,
                FloatingPaneRect::new(if left { 9 } else { -9 }, 0, 10, 4).unwrap(),
            );
        } else {
            session.split_active_pane().unwrap();
            if pad_only {
                float_pane(
                    &mut session,
                    0,
                    FloatingPaneRect::new(if left { -1 } else { 0 }, 0, 11, 4).unwrap(),
                );
            }
            float_pane(
                &mut session,
                1,
                FloatingPaneRect::new(if left { 2 } else { -1 }, 0, if left { 8 } else { 9 }, 4)
                    .unwrap(),
            );
        }
        let x = if clipped { 9 - x } else { x };
        let pane = session.window().pane(0).unwrap();
        let mut transcript = PaneTranscript::new(4096, TerminalSize { cols: 10, rows: 4 });
        transcript.append_bytes(b"BODY");
        let screen = transcript.screen();
        let copy = CopyModeRenderSnapshot {
            screen: screen.clone(),
            overlays: CopyModeRenderOverlays::default(),
            history_size: 0,
            scroll_position: 0,
            alternate_on: false,
            line_numbers_enabled: false,
        };
        for (frame, prompt) in [
            (
                super::super::render_pane_screen(session.view(), &options, pane, screen),
                false,
            ),
            (
                super::super::render_pane_screen_preserving_prompt_cursor(
                    session.view(),
                    &options,
                    pane,
                    screen,
                ),
                true,
            ),
            (
                super::super::render_copy_mode_pane_screen(session.view(), &options, pane, &copy),
                false,
            ),
            (
                super::super::render_copy_mode_pane_screen_preserving_prompt_cursor(
                    session.view(),
                    &options,
                    pane,
                    &copy,
                ),
                true,
            ),
        ] {
            assert_decoration(&frame, x, pad_only, prompt);
        }
        let before = PaneRenderSnapshot::capture(session.view(), &options, pane, screen)
            .expect("screen snapshot must retain a visible decoration");
        let before_transcript = PaneRenderSnapshot::capture_unstyled_transcript_reusing(
            session.view(),
            &options,
            pane,
            &transcript,
            None,
        )
        .expect("transcript snapshot must retain a visible decoration");
        assert_decoration(&before.full_frame(), x, pad_only, false);
        assert_eq!(before.full_frame(), before_transcript.full_frame());
        assert!(!before.can_forward_plain_bytes(b"leak"));
        let overlay = super::super::render_clock_overlay(
            session.view(),
            &options,
            &[super::super::ClockPaneRenderData {
                pane_index: 0,
                history_size: 0,
                alternate_on: false,
            }],
            chrono::Local::now(),
        );
        // Check the overlay itself, including cursor state after position restore.
        assert_decoration(&overlay, x, pad_only, false);
        let restore = super::super::render_clock_restore_frame(
            session.view(),
            &options,
            &[super::super::ClockPaneRestoreData {
                pane_index: 0,
                lines: vec!["BODY".into(); 4],
                history_size: 0,
                alternate_on: false,
            }],
            false,
        );
        assert_decoration(&restore, x, pad_only, false);

        transcript.append_bytes(b"\r\n1\r\n2\r\n3\r\n4\r\n5\r\n6\r\n7");
        let after =
            PaneRenderSnapshot::capture(session.view(), &options, pane, transcript.screen())
                .unwrap();
        let after_transcript = PaneRenderSnapshot::capture_unstyled_transcript_reusing(
            session.view(),
            &options,
            pane,
            &transcript,
            Some(&before_transcript),
        )
        .unwrap();
        for (previous, next) in [(&before, &after), (&before_transcript, &after_transcript)] {
            let PaneRenderDelta::Incremental(delta) = previous.diff_to(next) else {
                panic!("scrollbar-only updates must remain incremental");
            };
            assert_decoration(&next.full_frame(), x, pad_only, false);
            if pad_only {
                // Padding does not change when only the slider moves.
                assert!(!delta.frame().windows(4).any(|bytes| bytes == b"BODY"));
            } else {
                assert_decoration(delta.frame(), x, false, false);
            }
        }
    }
}
