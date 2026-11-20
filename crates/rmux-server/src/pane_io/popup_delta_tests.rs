use super::*;
use crate::renderer::{render_popup_overlay, OverlayRect, PopupContent, PopupRenderSpec};
use rmux_core::{input::InputParser, BoxLines, GridRenderOptions, Screen, Style};
use rmux_proto::TerminalSize;

fn popup(rows: &[&str]) -> OverlayFrame {
    OverlayFrame::persistent(
        render_popup_overlay(&PopupRenderSpec {
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
            content: PopupContent::Surface(rows.iter().map(|r| r.as_bytes().to_vec()).collect()),
        }),
        1,
        1,
    )
    .with_row_diff()
}

fn screen(frames: &[&[u8]]) -> Vec<Vec<u8>> {
    let mut parser = InputParser::new();
    let mut screen = Screen::new(TerminalSize { cols: 40, rows: 10 }, 0);
    for frame in frames {
        parser.parse(frame, &mut screen);
    }
    (0..10)
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
        .collect()
}

#[test]
fn popup_delta_preserves_screen_and_erases_shortened_coloured_wide_rows() {
    let before = popup(&["unchanged", "\x1b[31m긴 문자열 abcdef", "last"]);
    let after = popup(&["unchanged", "\x1b[32m짧음", "last"]);
    let delta = popup_frame_delta(Some(&before.frame), true, &after).unwrap();
    assert!(delta.len() < after.frame.len() / 2);
    assert!(!String::from_utf8_lossy(&delta).contains("unchanged"));
    assert_eq!(screen(&[&before.frame, &delta]), screen(&[&after.frame]));
    let cleared = popup(&["unchanged", "", "last"]);
    let delta = popup_frame_delta(Some(&after.frame), true, &cleared).unwrap();
    assert_eq!(screen(&[&after.frame, &delta]), screen(&[&cleared.frame]));
}

#[test]
fn popup_delta_skips_identical_frames_but_caches_full_restorable_frame() {
    let before = popup(&["one", "two", "three"]);
    assert_eq!(
        popup_frame_delta(Some(&before.frame), true, &before),
        Some(vec![])
    );
    let after = popup(&["one", "changed", "three"]);
    let mut cache = None;
    let mut visible = true;
    update_persistent_overlay_cache(&mut cache, &mut visible, &before);
    let delta = popup_frame_delta(cache.as_deref(), visible, &after).unwrap();
    update_persistent_overlay_cache(&mut cache, &mut visible, &after);
    assert_ne!(cache.as_deref(), Some(delta.as_slice()));
    assert_eq!(cache.as_deref(), Some(after.frame.as_slice()));
}

#[test]
fn popup_delta_falls_back_for_restore_resize_move_and_overlapping_menu() {
    let before = popup(&["one", "two", "three"]);
    assert!(popup_frame_delta(None, true, &before).is_none());
    assert!(popup_frame_delta(Some(&before.frame), false, &before).is_none());
    let moved = OverlayFrame::persistent(
        String::from_utf8(before.frame.clone())
            .unwrap()
            .replace("[3;3H", "[2;3H")
            .into_bytes(),
        1,
        2,
    )
    .with_row_diff();
    assert!(popup_frame_delta(Some(&before.frame), true, &moved).is_none());
    let mut resized = popup(&["one", "two", "three"]);
    let last_row_len = popup_rows(&resized.frame).unwrap().last().unwrap().1.len();
    resized.frame.truncate(resized.frame.len() - last_row_len);
    assert!(popup_frame_delta(Some(&before.frame), true, &resized).is_none());
    let mut nested = popup(&["one", "two", "three"]);
    nested.frame.extend_from_slice(&before.frame);
    assert!(popup_frame_delta(Some(&nested.frame), true, &nested).is_none());
    let explicit_refresh = OverlayFrame::persistent(before.frame.clone(), 1, 2);
    assert!(popup_frame_delta(Some(&before.frame), true, &explicit_refresh).is_none());
}
