//! The property this crate exists for: a host on `ratatui` paints with it.
//!
//! Every other test here builds its `Buffer` from `ratatui_core` — the crate
//! this one depends on — so all of them would keep passing if the dependency
//! were re-pinned to a `ratatui` aliased under the `ratatui-core` name. That is
//! what it used to be, and under it a host on ratatui 0.30 could not call
//! `render` at all: its `Buffer` was a different type with the same path, and
//! cargo reports that as a trait bound that "is not satisfied" against a type
//! whose name is identical to the one required.
//!
//! So these go through `ratatui` itself, the way a host does. They assert
//! nothing about the pixels — `render.rs` covers the projection — only that the
//! types unify, which is a compile-time claim the test body cannot fake.

use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use ratatui::widgets::Widget;
use ratatui::Terminal;

use ratatui_rmux::{PaneState, PaneWidget};
use rmux_sdk::{PaneAttributes, PaneCell, PaneColor, PaneCursor, PaneGlyph, PaneSnapshot};

fn cell(text: &str) -> PaneCell {
    PaneCell {
        glyph: PaneGlyph::new(text, 1),
        attributes: PaneAttributes::EMPTY,
        foreground: PaneColor::Default,
        background: PaneColor::Default,
        underline: PaneColor::Default,
    }
}

fn two_by_one() -> PaneState {
    let snapshot = PaneSnapshot::new(2, 1, vec![cell("h"), cell("i")], PaneCursor::default())
        .expect("a 2x1 snapshot of two cells");
    PaneState::from_snapshot(snapshot)
}

/// `ratatui::buffer::Buffer` is `ratatui_core::buffer::Buffer`, so a host can
/// hand this widget the buffer it already has.
#[test]
fn a_ratatui_buffer_is_the_buffer_this_widget_paints() {
    let state = two_by_one();
    let area = Rect::new(0, 0, 2, 1);
    let mut buffer = ratatui::buffer::Buffer::empty(area);

    PaneWidget::new(&state).render(area, &mut buffer);

    assert_eq!(buffer.cell((0, 0)).expect("a cell").symbol(), "h");
    assert_eq!(buffer.cell((1, 0)).expect("a cell").symbol(), "i");
}

/// And the path a host actually takes: `Frame::render_widget` inside a draw
/// closure, which is where the trait bound has to hold for this crate to be
/// usable at all.
#[test]
fn a_frame_renders_the_widget_the_way_a_host_does() {
    let state = two_by_one();
    let mut terminal = Terminal::new(TestBackend::new(2, 1)).expect("a test terminal");

    terminal
        .draw(|frame| frame.render_widget(PaneWidget::new(&state), frame.area()))
        .expect("the draw closure accepted the widget");

    let buffer = terminal.backend().buffer();
    assert_eq!(buffer.cell((0, 0)).expect("a cell").symbol(), "h");
    assert_eq!(buffer.cell((1, 0)).expect("a cell").symbol(), "i");
}
