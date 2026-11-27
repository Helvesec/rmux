//! Render purity and mapping fidelity tests for `ratatui-rmux`.
//!
//! These tests exercise every code path of the public widget against
//! synthetic [`PaneState`] values. They prove three properties:
//!
//! 1. The pure mapping `theme::*` covers every relevant
//!    glyph/color/modifier branch.
//! 2. `Widget::render` is deterministic — two consecutive renders of
//!    the same state into the same buffer rect produce byte-identical
//!    cells (no clock, no I/O, no hidden state).
//! 3. The widget never panics on out-of-range or malformed snapshots.
//!
//! No tokio runtime is required.

use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect;
use ratatui_core::style::{Color, Modifier, Style};
use ratatui_core::widgets::Widget;

use ratatui_rmux::{cell_style, color, glyph_symbol, modifier, PaneState, PaneWidget};
use rmux_sdk::{
    PaneAttributes, PaneCell, PaneColor, PaneCursor, PaneDisconnectReason, PaneEvent,
    PaneExitReason, PaneGlyph, PaneId, PaneSnapshot,
};

// --- color mapping ---------------------------------------------------------

#[test]
fn color_default_collapses_to_reset() {
    assert_eq!(color(PaneColor::Default), Color::Reset);
    assert_eq!(color(PaneColor::None), Color::Reset);
    assert_eq!(color(PaneColor::Terminal), Color::Reset);
}

#[test]
fn color_ansi_index_maps_to_eight_named_colors() {
    let expected = [
        Color::Black,
        Color::Red,
        Color::Green,
        Color::Yellow,
        Color::Blue,
        Color::Magenta,
        Color::Cyan,
        Color::Gray,
    ];
    for (index, want) in expected.iter().enumerate() {
        let got = color(PaneColor::ansi(index as u8));
        assert_eq!(got, *want, "ANSI index {index} maps to {want:?}");
    }
}

#[test]
fn color_bright_ansi_index_maps_to_eight_light_colors() {
    let expected = [
        Color::DarkGray,
        Color::LightRed,
        Color::LightGreen,
        Color::LightYellow,
        Color::LightBlue,
        Color::LightMagenta,
        Color::LightCyan,
        Color::White,
    ];
    for (index, want) in expected.iter().enumerate() {
        let got = color(PaneColor::bright_ansi(index as u8));
        assert_eq!(got, *want, "bright ANSI index {index} maps to {want:?}");
    }
}

#[test]
fn color_indexed_passes_through() {
    for index in [0u8, 16, 42, 231, 255] {
        assert_eq!(color(PaneColor::indexed(index)), Color::Indexed(index));
    }
}

#[test]
fn color_rgb_passes_through() {
    assert_eq!(color(PaneColor::rgb(0, 0, 0)), Color::Rgb(0, 0, 0));
    assert_eq!(
        color(PaneColor::rgb(0xab, 0xcd, 0xef)),
        Color::Rgb(0xab, 0xcd, 0xef)
    );
}

#[test]
fn color_encoded_unknown_collapses_to_reset() {
    let unknown = PaneColor::Encoded { value: -42 };
    assert_eq!(color(unknown), Color::Reset);
}

// --- modifier mapping ------------------------------------------------------

#[test]
fn modifier_empty_yields_empty() {
    assert_eq!(modifier(PaneAttributes::EMPTY), Modifier::empty());
}

#[test]
fn modifier_single_bits_map_one_to_one() {
    let cases: &[(PaneAttributes, Modifier)] = &[
        (PaneAttributes::BOLD, Modifier::BOLD),
        (PaneAttributes::DIM, Modifier::DIM),
        (PaneAttributes::ITALIC, Modifier::ITALIC),
        (PaneAttributes::UNDERLINE, Modifier::UNDERLINED),
        (PaneAttributes::BLINK, Modifier::SLOW_BLINK),
        (PaneAttributes::REVERSE, Modifier::REVERSED),
        (PaneAttributes::HIDDEN, Modifier::HIDDEN),
        (PaneAttributes::STRIKETHROUGH, Modifier::CROSSED_OUT),
    ];
    for (input, want) in cases {
        assert_eq!(modifier(*input), *want, "input bits {:#x}", input.bits());
    }
}

#[test]
fn modifier_underline_variants_collapse_to_underlined() {
    for variant in [
        PaneAttributes::UNDERLINE,
        PaneAttributes::DOUBLE_UNDERLINE,
        PaneAttributes::CURLY_UNDERLINE,
        PaneAttributes::DOTTED_UNDERLINE,
        PaneAttributes::DASHED_UNDERLINE,
    ] {
        let got = modifier(variant);
        assert_eq!(
            got,
            Modifier::UNDERLINED,
            "underline variant {:#x} must map only to UNDERLINED",
            variant.bits(),
        );
    }
}

#[test]
fn modifier_combined_bits_compose() {
    let combined = PaneAttributes::BOLD
        | PaneAttributes::ITALIC
        | PaneAttributes::REVERSE
        | PaneAttributes::STRIKETHROUGH;
    let want = Modifier::BOLD | Modifier::ITALIC | Modifier::REVERSED | Modifier::CROSSED_OUT;
    assert_eq!(modifier(combined), want);
}

#[test]
fn modifier_charset_and_no_attributes_are_dropped() {
    // `CHARSET` and `NO_ATTRIBUTES` have no ratatui counterpart and
    // must not silently grow new modifier bits.
    let bits = PaneAttributes::CHARSET | PaneAttributes::NO_ATTRIBUTES;
    assert_eq!(modifier(bits), Modifier::empty());
}

// --- glyph mapping --------------------------------------------------------

#[test]
fn glyph_symbol_returns_text_payload() {
    let glyph = PaneGlyph::new("a", 1);
    assert_eq!(glyph_symbol(&glyph), "a");
    let wide = PaneGlyph::new("漢", 2);
    assert_eq!(glyph_symbol(&wide), "漢");
}

#[test]
fn glyph_symbol_padding_is_empty() {
    assert_eq!(glyph_symbol(&PaneGlyph::padding()), "");
}

#[test]
fn cell_style_combines_fg_bg_and_modifiers() {
    let cell = PaneCell {
        glyph: PaneGlyph::new("x", 1),
        attributes: PaneAttributes::BOLD | PaneAttributes::REVERSE,
        foreground: PaneColor::ansi(1),
        background: PaneColor::rgb(10, 20, 30),
        underline: PaneColor::Default,
    };
    let style = cell_style(&cell);
    assert_eq!(style.fg, Some(Color::Red));
    assert_eq!(style.bg, Some(Color::Rgb(10, 20, 30)));
    assert!(style.add_modifier.contains(Modifier::BOLD));
    assert!(style.add_modifier.contains(Modifier::REVERSED));
}

// --- widget render --------------------------------------------------------

fn glyph_cell(text: &str, fg: PaneColor, bg: PaneColor, attrs: PaneAttributes) -> PaneCell {
    PaneCell {
        glyph: PaneGlyph::new(text, 1),
        attributes: attrs,
        foreground: fg,
        background: bg,
        underline: PaneColor::Default,
    }
}

fn small_state() -> PaneState {
    let cells = vec![
        glyph_cell(
            "h",
            PaneColor::ansi(1),
            PaneColor::Default,
            PaneAttributes::BOLD,
        ),
        glyph_cell(
            "i",
            PaneColor::rgb(0, 255, 0),
            PaneColor::indexed(7),
            PaneAttributes::ITALIC,
        ),
        PaneCell::blank(),
        PaneCell::blank(),
        PaneCell::blank(),
        glyph_cell(
            "!",
            PaneColor::ansi(4),
            PaneColor::Default,
            PaneAttributes::EMPTY,
        ),
    ];
    let snapshot = PaneSnapshot::new(3, 2, cells, PaneCursor::default()).unwrap();
    PaneState::from_snapshot(snapshot)
}

#[test]
fn widget_renders_glyphs_and_styles() {
    let state = small_state();
    let area = Rect::new(0, 0, 3, 2);
    let mut buf = Buffer::empty(area);
    PaneWidget::new(&state).render(area, &mut buf);

    assert_eq!(buf.cell((0, 0)).unwrap().symbol(), "h");
    assert_eq!(buf.cell((0, 0)).unwrap().fg, Color::Red);
    assert!(buf.cell((0, 0)).unwrap().modifier.contains(Modifier::BOLD));

    assert_eq!(buf.cell((1, 0)).unwrap().symbol(), "i");
    assert_eq!(buf.cell((1, 0)).unwrap().fg, Color::Rgb(0, 255, 0));
    assert_eq!(buf.cell((1, 0)).unwrap().bg, Color::Indexed(7));
    assert!(buf
        .cell((1, 0))
        .unwrap()
        .modifier
        .contains(Modifier::ITALIC));

    assert_eq!(buf.cell((0, 1)).unwrap().symbol(), " ");
    assert_eq!(buf.cell((2, 1)).unwrap().symbol(), "!");
    assert_eq!(buf.cell((2, 1)).unwrap().fg, Color::Blue);
}

#[test]
fn widget_render_is_deterministic() {
    let state = small_state();
    let area = Rect::new(0, 0, 4, 2);

    let mut first = Buffer::empty(area);
    PaneWidget::new(&state).render(area, &mut first);

    // Render twice into a fresh buffer and compare byte-for-byte.
    let mut second = Buffer::empty(area);
    PaneWidget::new(&state).render(area, &mut second);

    assert_eq!(first, second, "widget renders must be deterministic");

    // Re-rendering into the same buffer must also be a no-op once the
    // first render has already painted the cells.
    let mut third = Buffer::empty(area);
    let widget = PaneWidget::new(&state);
    widget.render(area, &mut third);
    let after_first = third.clone();
    let widget = PaneWidget::new(&state);
    widget.render(area, &mut third);
    assert_eq!(
        after_first, third,
        "re-rendering same state must be idempotent"
    );
}

#[test]
fn widget_handles_wide_glyph_padding() {
    let cells = vec![
        PaneCell::new(PaneGlyph::new("漢", 2)),
        PaneCell::padding(),
        PaneCell::new(PaneGlyph::new("z", 1)),
    ];
    let snapshot = PaneSnapshot::new(3, 1, cells, PaneCursor::default()).unwrap();
    let state = PaneState::from_snapshot(snapshot);
    let area = Rect::new(0, 0, 3, 1);
    let mut buf = Buffer::empty(area);
    buf.cell_mut((1, 0)).unwrap().set_symbol("x");
    PaneWidget::new(&state).render(area, &mut buf);
    assert_eq!(buf.cell((0, 0)).unwrap().symbol(), "漢");
    // Padding cells must not draw a second glyph, but they must clear
    // stale host-buffer content from prior renders.
    assert_eq!(buf.cell((1, 0)).unwrap().symbol(), " ");
    assert_eq!(buf.cell((2, 0)).unwrap().symbol(), "z");
}

#[test]
fn widget_clips_oversized_snapshot_to_render_area() {
    let cells = vec![
        glyph_cell(
            "a",
            PaneColor::Default,
            PaneColor::Default,
            PaneAttributes::EMPTY,
        ),
        glyph_cell(
            "b",
            PaneColor::Default,
            PaneColor::Default,
            PaneAttributes::EMPTY,
        ),
        glyph_cell(
            "c",
            PaneColor::Default,
            PaneColor::Default,
            PaneAttributes::EMPTY,
        ),
        glyph_cell(
            "d",
            PaneColor::Default,
            PaneColor::Default,
            PaneAttributes::EMPTY,
        ),
    ];
    let snapshot = PaneSnapshot::new(2, 2, cells, PaneCursor::default()).unwrap();
    let state = PaneState::from_snapshot(snapshot);
    let area = Rect::new(0, 0, 1, 1);
    let mut buf = Buffer::empty(area);
    PaneWidget::new(&state).render(area, &mut buf);
    assert_eq!(buf.cell((0, 0)).unwrap().symbol(), "a");
}

#[test]
fn widget_is_safe_for_zero_dimensions() {
    let state = PaneState::default();
    let area = Rect::new(0, 0, 0, 0);
    let mut buf = Buffer::empty(Rect::new(0, 0, 4, 4));
    PaneWidget::new(&state).render(area, &mut buf);
    // The empty area means no cell was touched; spot-check origin.
    assert_eq!(buf.cell((0, 0)).unwrap().symbol(), " ");
}

#[test]
fn widget_does_not_panic_on_malformed_snapshot() {
    // Build a snapshot whose cell vector is too short for its claimed
    // dimensions. `PaneSnapshot::new` would refuse this, so we build
    // it via deserialization-style direct construction.
    let mut state = PaneState::default();
    state.snapshot.cols = 3;
    state.snapshot.rows = 2;
    state.snapshot.cells = vec![PaneCell::blank()];
    let area = Rect::new(0, 0, 3, 2);
    let mut buf = Buffer::empty(area);
    PaneWidget::new(&state).render(area, &mut buf);
    // Render fell back to base style; assert no crash and that every
    // cell is the default empty ratatui cell.
    for y in 0..area.height {
        for x in 0..area.width {
            assert_eq!(buf.cell((x, y)).unwrap().symbol(), " ");
        }
    }
}

#[test]
fn widget_base_style_paints_full_area_even_when_snapshot_is_smaller() {
    // A 1x1 captured grid drawn into a 3x2 area: cells beyond the
    // snapshot rows/cols must still carry the supplied base style so the
    // host gets a uniform backdrop while the widget remains pure.
    let snapshot = PaneSnapshot::new(
        1,
        1,
        vec![PaneCell::new(PaneGlyph::new("a", 1))],
        PaneCursor::default(),
    )
    .unwrap();
    let state = PaneState::from_snapshot(snapshot);
    let base = Style::new().bg(Color::Blue);
    let area = Rect::new(0, 0, 3, 2);
    let mut buf = Buffer::empty(area);
    for y in 0..area.height {
        for x in 0..area.width {
            buf.cell_mut((x, y)).unwrap().set_symbol("x");
        }
    }
    PaneWidget::new(&state)
        .base_style(base)
        .render(area, &mut buf);
    assert_eq!(buf.cell((0, 0)).unwrap().symbol(), "a");
    for (x, y) in [(1, 0), (2, 0), (0, 1), (1, 1), (2, 1)] {
        assert_eq!(buf.cell((x, y)).unwrap().symbol(), " ", "cell ({x},{y})");
        assert_eq!(buf.cell((x, y)).unwrap().bg, Color::Blue, "cell ({x},{y})");
    }
}

#[test]
fn widget_clears_symbols_when_rerendering_smaller_snapshot() {
    let large = small_state();
    let small_snapshot = PaneSnapshot::new(
        1,
        1,
        vec![PaneCell::new(PaneGlyph::new("a", 1))],
        PaneCursor::default(),
    )
    .unwrap();
    let small = PaneState::from_snapshot(small_snapshot);
    let area = Rect::new(0, 0, 3, 2);
    let mut buf = Buffer::empty(area);

    PaneWidget::new(&large).render(area, &mut buf);
    PaneWidget::new(&small).render(area, &mut buf);

    assert_eq!(buf.cell((0, 0)).unwrap().symbol(), "a");
    for (x, y) in [(1, 0), (2, 0), (0, 1), (1, 1), (2, 1)] {
        assert_eq!(buf.cell((x, y)).unwrap().symbol(), " ", "cell ({x},{y})");
    }
}

#[test]
fn widget_renders_at_non_origin_rect_with_clipping() {
    // The widget must respect the supplied area's (x, y) offset and never
    // touch buffer cells outside it.
    let cells = vec![
        glyph_cell(
            "p",
            PaneColor::Default,
            PaneColor::Default,
            PaneAttributes::EMPTY,
        ),
        glyph_cell(
            "q",
            PaneColor::Default,
            PaneColor::Default,
            PaneAttributes::EMPTY,
        ),
    ];
    let snapshot = PaneSnapshot::new(2, 1, cells, PaneCursor::default()).unwrap();
    let state = PaneState::from_snapshot(snapshot);
    let buffer_area = Rect::new(0, 0, 4, 3);
    let mut buf = Buffer::empty(buffer_area);
    let area = Rect::new(1, 1, 2, 1);
    PaneWidget::new(&state).render(area, &mut buf);
    assert_eq!(buf.cell((1, 1)).unwrap().symbol(), "p");
    assert_eq!(buf.cell((2, 1)).unwrap().symbol(), "q");
    // Cells outside the render area remain the default empty " ".
    for (x, y) in [
        (0, 0),
        (1, 0),
        (2, 0),
        (3, 0),
        (0, 1),
        (3, 1),
        (0, 2),
        (3, 2),
    ] {
        assert_eq!(buf.cell((x, y)).unwrap().symbol(), " ", "cell ({x},{y})");
    }
}

#[test]
fn widget_render_ignores_lifecycle_pause_and_lag_state() {
    // The widget projects the grid only — pause/lag/lifecycle markers are
    // host concerns. Render output must be byte-identical regardless of
    // these state fields so that decorating widgets (status bars, etc.)
    // own the visual representation.
    let mut state_a = small_state();
    let mut state_b = small_state();
    state_b.apply_event(&PaneEvent::Pause {
        pane_id: PaneId::new(1),
    });
    state_b.apply_event(&PaneEvent::Lag {
        pane_id: PaneId::new(1),
    });
    state_b.apply_event(&PaneEvent::Disconnect {
        pane_id: None,
        reason: PaneDisconnectReason::TransportClosed,
    });
    state_a.apply_event(&PaneEvent::Exit {
        reason: PaneExitReason::Bare,
    });

    let area = Rect::new(0, 0, 3, 2);
    let mut buf_a = Buffer::empty(area);
    let mut buf_b = Buffer::empty(area);
    PaneWidget::new(&state_a).render(area, &mut buf_a);
    PaneWidget::new(&state_b).render(area, &mut buf_b);
    assert_eq!(
        buf_a, buf_b,
        "render output must depend only on the captured grid",
    );
}

#[test]
fn widget_render_does_not_require_async_runtime() {
    // The widget renders inside a synchronous block with no executor
    // context. If anything in the render path required an async runtime,
    // the call would panic with `there is no reactor running`. The fact
    // that this test is not annotated with `#[tokio::test]` and does not
    // pull in a runtime is the structural part of the check; the
    // assertions confirm the call completed without panicking.
    let state = small_state();
    let result = {
        let area = Rect::new(0, 0, 4, 2);
        let mut buf = Buffer::empty(area);
        PaneWidget::new(&state).render(area, &mut buf);
        buf
    };
    assert_eq!(result.area, Rect::new(0, 0, 4, 2));
}

#[test]
fn widget_render_clips_wide_glyph_at_right_edge() {
    // A wide glyph clipped to a one-column widget must not claim the first
    // cell of an adjacent widget or suppress that cell from Buffer::diff.
    let cells = vec![PaneCell::new(PaneGlyph::new("漢", 2)), PaneCell::padding()];
    let snapshot = PaneSnapshot::new(2, 1, cells, PaneCursor::default()).unwrap();
    let state = PaneState::from_snapshot(snapshot);
    let buffer_area = Rect::new(0, 0, 2, 1);
    let previous = Buffer::empty(buffer_area);
    let mut current = Buffer::empty(buffer_area);
    PaneWidget::new(&state).render(Rect::new(0, 0, 1, 1), &mut current);
    current.cell_mut((1, 0)).unwrap().set_symbol("X");

    assert_eq!(current.cell((0, 0)).unwrap().symbol(), " ");
    assert_eq!(current.cell((1, 0)).unwrap().symbol(), "X");
    assert!(
        previous
            .diff(&current)
            .iter()
            .any(|(x, y, cell)| (*x, *y, cell.symbol()) == (1, 0, "X")),
        "clipped wide glyph must not suppress the adjacent widget update"
    );
}

mod scene_projection {
    use ratatui_core::{buffer::Buffer, layout::Rect, widgets::Widget};
    use ratatui_rmux::{PaneState, SceneWidget};
    use rmux_sdk::*;
    #[test]
    fn resolved_spans_clip_wide_cells_and_hide_occluded_cursor() {
        let rect = SceneRect {
            x: -1,
            y: 0,
            cols: 3,
            rows: 1,
        };
        let pane = ScenePane {
            id: PaneId::new(4),
            index: 9,
            floating: true,
            layer: 1,
            focused: true,
            visible: true,
            content: rect.clone(),
            frame: rect.clone(),
            content_clip: rect.clone(),
            frame_clip: rect,
            hit_regions: vec![],
            spans: vec![SceneSpan {
                x: 0,
                y: 0,
                source_row: 0,
                source_col: 1,
                cols: 1,
            }],
        };
        let scene = SceneSnapshot {
            schema: 1,
            session_id: SessionId::new(1),
            window_id: WindowId::new(2),
            window_index: 0,
            revision: "test".into(),
            cols: 3,
            rows: 1,
            content_y_offset: 0,
            active_pane_id: Some(pane.id),
            panes: vec![pane],
            borders: vec![],
        };
        let cell = |text: &str, width| PaneCell {
            glyph: PaneGlyph::new(text, width),
            attributes: PaneAttributes::EMPTY,
            foreground: PaneColor::Default,
            background: PaneColor::Default,
            underline: PaneColor::Default,
        };
        let snapshot = PaneSnapshot::new(
            3,
            1,
            vec![cell("A", 1), cell("界", 2), cell("", 0)],
            PaneCursor::new(0, 0, true, 0),
        )
        .unwrap();
        let mut panes = vec![(PaneId::new(4), PaneState::from_snapshot(snapshot))];
        let area = Rect::new(1, 1, 3, 1);
        let mut buffer = Buffer::empty(Rect::new(0, 0, 5, 3));
        buffer.cell_mut((4, 1)).unwrap().set_symbol("Z");
        SceneWidget::new(&scene, &panes).render(area, &mut buffer);
        assert_eq!(buffer.cell((1, 1)).unwrap().symbol(), " ");
        assert_eq!(buffer.cell((4, 1)).unwrap().symbol(), "Z");
        assert_eq!(SceneWidget::new(&scene, &panes).cursor(area), None);
        panes[0].1.snapshot.cursor.col = 1;
        assert_eq!(SceneWidget::new(&scene, &panes).cursor(area), Some((1, 1)));
        // Same slot with wrong stable identity cannot contribute pixels or cursor.
        panes[0].0 = PaneId::new(5);
        assert_eq!(SceneWidget::new(&scene, &panes).cursor(area), None);
    }
}

// R3632 M1: the incoming Buffer must not contribute text attributes.
mod buffer_modifiers {
    use super::*;
    use ratatui_rmux::SceneWidget;
    use rmux_sdk::{
        SceneBorder, ScenePane, SceneRect, SceneSnapshot, SceneSpan, SessionId, WindowId,
    };

    fn state(attributes: PaneAttributes) -> PaneState {
        let mut cell = PaneCell::new(PaneGlyph::new("X", 1));
        cell.attributes = attributes;
        PaneState::from_snapshot(
            PaneSnapshot::new(1, 1, vec![cell], PaneCursor::default()).unwrap(),
        )
    }

    fn scene(cols: u16, rows: u16) -> SceneSnapshot {
        let rect = SceneRect {
            x: 0,
            y: 0,
            cols: i32::from(cols),
            rows: i32::from(rows),
        };
        SceneSnapshot {
            schema: 1,
            session_id: SessionId::new(0),
            window_id: WindowId::new(0),
            window_index: 0,
            revision: String::new(),
            cols,
            rows,
            content_y_offset: 0,
            active_pane_id: Some(PaneId::new(0)),
            borders: vec![],
            panes: vec![ScenePane {
                id: PaneId::new(0),
                index: 0,
                floating: false,
                layer: 0,
                focused: true,
                visible: true,
                content: rect.clone(),
                frame: rect.clone(),
                content_clip: rect.clone(),
                frame_clip: rect,
                hit_regions: vec![],
                spans: (0..rows)
                    .map(|y| SceneSpan {
                        x: 0,
                        y,
                        source_row: usize::from(y),
                        source_col: 0,
                        cols,
                    })
                    .collect(),
            }],
        }
    }

    fn assert_clean_render(area: Rect, render: impl Fn(&mut Buffer)) -> Buffer {
        // Offset buffer, neighboring host cells, and an area allowed to extend
        // beyond the buffer exercise the same clearing boundary as painting.
        let mut host = Buffer::empty(Rect::new(1, 1, 6, 3));
        for cell in &mut host.content {
            cell.set_symbol("@")
                .set_style(Style::new().add_modifier(Modifier::all()));
        }
        let mut fresh = host.clone();
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                if let Some(cell) = fresh.cell_mut((x, y)) {
                    cell.reset();
                }
            }
        }
        let mut expected = fresh.clone();
        render(&mut expected);
        for prior in Modifier::all().iter().chain([Modifier::all()]) {
            let mut reused = fresh.clone();
            for y in area.top()..area.bottom() {
                for x in area.left()..area.right() {
                    if let Some(cell) = reused.cell_mut((x, y)) {
                        cell.set_style(Style::new().add_modifier(prior));
                    }
                }
            }
            render(&mut reused);
            assert_eq!(reused, expected, "prior modifiers: {prior:?}");
            render(&mut reused);
            assert_eq!(reused, expected, "repeat with prior modifiers: {prior:?}");
        }
        for y in host.area.top()..host.area.bottom() {
            for x in host.area.left()..host.area.right() {
                if !area.contains((x, y).into()) {
                    assert_eq!(expected[(x, y)], host[(x, y)], "outside render area");
                }
            }
        }
        expected
    }

    #[test]
    fn pane_repaint_removes_previous_snapshot_attributes() {
        let area = Rect::new(0, 0, 1, 1);
        let plain = state(PaneAttributes::EMPTY);
        let mut fresh = Buffer::empty(area);
        PaneWidget::new(&plain).render(area, &mut fresh);
        let mut reused = Buffer::empty(area);
        PaneWidget::new(&state(PaneAttributes::HIDDEN | PaneAttributes::BOLD))
            .render(area, &mut reused);
        PaneWidget::new(&plain).render(area, &mut reused);
        assert_eq!(fresh[(0, 0)].modifier, Modifier::empty());
        assert_eq!(reused, fresh);
    }

    #[test]
    fn scene_repaint_removes_previous_snapshot_attributes() {
        let area = Rect::new(0, 0, 1, 1);
        let scene = scene(1, 1);
        let plain = [(PaneId::new(0), state(PaneAttributes::EMPTY))];
        let hidden = [(
            PaneId::new(0),
            state(PaneAttributes::HIDDEN | PaneAttributes::BOLD),
        )];
        let mut fresh = Buffer::empty(area);
        SceneWidget::new(&scene, &plain).render(area, &mut fresh);
        let mut reused = Buffer::empty(area);
        SceneWidget::new(&scene, &hidden).render(area, &mut reused);
        SceneWidget::new(&scene, &plain).render(area, &mut reused);
        assert_eq!(fresh[(0, 0)].modifier, Modifier::empty());
        assert_eq!(reused, fresh);
    }

    #[test]
    fn pane_host_attributes_are_replaced_with_base_and_snapshot_attributes() {
        let area = Rect::new(2, 2, 3, 2);
        for attrs in [
            PaneAttributes::EMPTY,
            PaneAttributes::HIDDEN | PaneAttributes::BOLD,
        ] {
            let state = state(attrs);
            for base in [
                Style::new(),
                Style::new()
                    .add_modifier(Modifier::ITALIC)
                    .remove_modifier(Modifier::BOLD | Modifier::HIDDEN),
            ] {
                let actual = assert_clean_render(area, |buf| {
                    PaneWidget::new(&state).base_style(base).render(area, buf);
                });
                assert_eq!(actual[(2, 2)].modifier, base.add_modifier | modifier(attrs));
                assert_eq!(actual[(3, 2)].modifier, base.add_modifier);
                assert_eq!(actual[(2, 3)].modifier, base.add_modifier);
            }
        }
    }

    #[test]
    fn scene_host_attributes_are_replaced_in_content_background_and_borders() {
        let area = Rect::new(2, 2, 3, 2);
        let mut scene = scene(3, 2);
        scene.borders.push(SceneBorder {
            x: 2,
            y: 0,
            glyph: '│',
            pane_id: PaneId::new(0),
            active: true,
        });
        for attrs in [
            PaneAttributes::EMPTY,
            PaneAttributes::HIDDEN | PaneAttributes::BOLD,
        ] {
            let panes = [(PaneId::new(0), state(attrs))];
            let actual = assert_clean_render(area, |buf| {
                SceneWidget::new(&scene, &panes).render(area, buf)
            });
            assert_eq!(actual[(2, 2)].modifier, modifier(attrs));
            assert_eq!(actual[(3, 2)].modifier, Modifier::empty());
            assert_eq!(actual[(4, 2)].symbol(), "│");
            assert_eq!(actual[(4, 2)].modifier, Modifier::empty());
        }
    }

    #[test]
    fn pane_empty_and_malformed_snapshots_clear_host_attributes() {
        let area = Rect::new(2, 2, 3, 2);
        let mut malformed = state(PaneAttributes::EMPTY);
        malformed.snapshot.cols = 2;
        for state in [PaneState::default(), malformed] {
            let actual = assert_clean_render(area, |buf| PaneWidget::new(&state).render(area, buf));
            assert_eq!(actual[(2, 2)].modifier, Modifier::empty());
        }
    }

    #[test]
    fn scene_empty_missing_and_malformed_snapshots_clear_host_attributes() {
        let area = Rect::new(2, 2, 3, 2);
        let scene = scene(3, 2);
        let mut malformed = state(PaneAttributes::EMPTY);
        malformed.snapshot.cols = 2;
        for panes in [
            vec![],
            vec![(PaneId::new(0), PaneState::default())],
            vec![(PaneId::new(0), malformed)],
        ] {
            let actual = assert_clean_render(area, |buf| {
                SceneWidget::new(&scene, &panes).render(area, buf)
            });
            assert_eq!(actual[(2, 2)].modifier, Modifier::empty());
        }
    }

    fn wide_state() -> PaneState {
        PaneState::from_snapshot(
            PaneSnapshot::new(
                4,
                1,
                vec![
                    PaneCell::new(PaneGlyph::new("界", 2)),
                    PaneCell::padding(),
                    PaneCell::new(PaneGlyph::new("界", 2)),
                    PaneCell::padding(),
                ],
                PaneCursor::default(),
            )
            .unwrap(),
        )
    }

    #[test]
    fn pane_padding_and_clipped_wide_cells_clear_host_attributes() {
        let state = wide_state();
        for area in [Rect::new(2, 2, 3, 1), Rect::new(4, 2, 4, 1)] {
            let actual = assert_clean_render(area, |buf| PaneWidget::new(&state).render(area, buf));
            assert_eq!(actual[(area.x, 2)].symbol(), "界");
            assert_eq!(actual[(area.x + 1, 2)].symbol(), " ");
            assert_eq!(actual[(area.x + 2, 2)].symbol(), " ");
        }
    }

    #[test]
    fn scene_padding_and_clipped_wide_cells_clear_host_attributes() {
        let scene = scene(4, 1);
        let panes = [(PaneId::new(0), wide_state())];
        for area in [Rect::new(2, 2, 3, 1), Rect::new(4, 2, 4, 1)] {
            let actual = assert_clean_render(area, |buf| {
                SceneWidget::new(&scene, &panes).render(area, buf)
            });
            assert_eq!(actual[(area.x, 2)].symbol(), "界");
            assert_eq!(actual[(area.x + 1, 2)].symbol(), " ");
            assert_eq!(actual[(area.x + 2, 2)].symbol(), " ");
        }
    }
}
