//! The stored width of a line follows every re-width.
//!
//! [`GridLine::width`] is not observable from outside the grid module, and the
//! arithmetic that consumes it — `line_width` and `reflow_logical_width`, which
//! feed the wrapped-cursor offset — only misbehaves once the stored width and
//! the real one disagree. That makes this an internal invariant, so it is
//! asserted here rather than through a screen-level consequence.
//!
//! The shape that breaks it is a compacted painted line: a history line keeps
//! the width it was written at while `compact_for_history` truncates its cell
//! vector to the painted end, so a later re-width to exactly that count finds
//! the vector already the right length. Before this correction the stored width
//! was only updated when the vector length changed, and such a line kept the
//! old value.

use super::{GridCell, GridLine};
use crate::input::COLOUR_DEFAULT;

const ERASE_BG: i32 = 1;

/// A twelve-column line whose first eight columns are erase-painted and whose
/// tail is untouched, then compacted the way a line entering scrollback is.
fn compacted_painted_line() -> GridLine {
    let mut line = GridLine::new(12);
    line.materialize_for_cell_mutation();
    for x in 0..8 {
        *line.cell_mut(x).expect("column inside the line") = GridCell::blank_with_bg(ERASE_BG);
    }
    assert_eq!(line.width(), 12, "fixture: written at twelve columns");
    assert_eq!(
        line.painted_end(),
        8,
        "fixture: the paint reaches exactly eight columns"
    );
    assert_eq!(
        line.used_end(),
        0,
        "fixture: paint alone is not written content"
    );
    line.compact_for_history();
    assert_eq!(
        line.width(),
        12,
        "fixture: compaction keeps the width it was written at"
    );
    line
}

#[test]
fn re_widthing_a_compacted_painted_line_updates_its_stored_width() {
    let mut line = compacted_painted_line();
    let cells_before = line.cells().len();

    // The target equals the number of cells the compaction left behind, so the
    // vector needs no resize at all. The stored width must still follow.
    line.resize_width_preserving_wrap(8, COLOUR_DEFAULT);

    assert_eq!(
        line.width(),
        8,
        "the stored width follows a re-width even when the vector is already \
         the right length"
    );
    assert_eq!(
        line.cells().len(),
        cells_before,
        "no cell was added or removed by this re-width"
    );
    for x in 0..8 {
        assert_eq!(
            line.cell(x).unwrap_or_default().bg(),
            ERASE_BG,
            "column {x} keeps its painted background"
        );
    }
}

#[test]
fn re_widthing_a_compacted_painted_line_bumps_its_revision() {
    let mut line = compacted_painted_line();
    let revision_before = line.revision();

    line.resize_width_preserving_wrap(8, COLOUR_DEFAULT);

    assert_ne!(
        line.revision(),
        revision_before,
        "a width change is a change, even with the vector length unchanged"
    );
}

#[test]
fn re_widthing_a_line_that_is_already_the_target_leaves_it_alone() {
    // Not the compacted fixture: re-widthing that one back to twelve really
    // does grow its vector from eight cells, so it legitimately touches. The
    // no-op this pins is a line whose stored width AND cell vector already
    // match the target.
    let mut line = GridLine::new(12);
    line.materialize_for_cell_mutation();
    *line.cell_mut(0).expect("column inside the line") = GridCell::blank_with_bg(ERASE_BG);
    assert_eq!(line.width(), 12);
    assert_eq!(line.cells().len(), 12);
    let revision_before = line.revision();

    line.resize_width_preserving_wrap(12, COLOUR_DEFAULT);

    assert_eq!(line.width(), 12, "the width was already twelve");
    assert_eq!(line.cells().len(), 12, "the vector was already twelve");
    assert_eq!(
        line.revision(),
        revision_before,
        "an unchanged width and an unchanged vector touch nothing"
    );
}
