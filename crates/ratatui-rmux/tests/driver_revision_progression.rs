//! Acceptance test for revision-driven redraw progression.
//!
//! The daemon advances `PaneSnapshot::revision` when a capture observes a
//! state that differs from the pane's previous capture; a change that no
//! capture observed does not advance it. `ratatui-rmux` must treat each changed
//! revision as one redraw opportunity and must never skip a later snapshot in a
//! normal progression.

use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect;
use ratatui_core::widgets::Widget;
use ratatui_rmux::{PaneState, PaneWidget};
use rmux_sdk::{PaneCell, PaneCursor, PaneGlyph, PaneSnapshot};

fn snapshot(text: &str, revision: u64) -> PaneSnapshot {
    PaneSnapshot::new(
        1,
        1,
        vec![PaneCell::new(PaneGlyph::new(text, 1))],
        PaneCursor::default(),
    )
    .expect("valid one-cell snapshot")
    .with_revision(revision)
}

fn render_symbol(state: &PaneState) -> String {
    let area = Rect::new(0, 0, 1, 1);
    let mut buffer = Buffer::empty(area);
    PaneWidget::new(state).render(area, &mut buffer);
    buffer
        .cell((0, 0))
        .expect("cell exists")
        .symbol()
        .to_owned()
}

#[test]
fn monotone_snapshot_revisions_drive_one_redraw_per_visible_transition() {
    let mut state = PaneState::default();

    for (index, text) in ["a", "b", "c"].iter().enumerate() {
        state.set_snapshot(snapshot(text, (index + 1) as u64));
        assert_eq!(
            state.generation,
            (index + 1) as u64,
            "each new revision should advance the redraw generation once",
        );
        assert_eq!(render_symbol(&state), *text);
    }
}
