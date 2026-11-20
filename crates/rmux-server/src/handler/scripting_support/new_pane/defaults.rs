//! The size `new-pane` gives an axis whose flag was not supplied.
//!
//! tmux 3.7b's `layout_get_floating_cell` starts the cell at
//! `sx = w->sx / 2, sy = w->sy / 4` and then replaces an axis only when that
//! axis's own flag was given (`runs/fnp2/oracle-source/layout.c:1371-1385`).
//! Two consequences follow, and both are what this module exists to keep:
//! the axes are independent, so omitting one says nothing about the other,
//! and the divisions are integer, so an odd window truncates.
//!
//! Measured on the frozen tmux 3.7b that produced the adopted FNP1 receipt:
//! an 80x24 window gives 40x6, a 79x25 window gives 39x6, `-x 30` on that odd
//! window gives 30x6 and `-y 8` gives 39x8.
//!
//! Nothing here rescues a zero. The same binary gives a 40x**0** float for an
//! 80x3 window and a **0**x6 float for a 1x24 window, both created with exit 0;
//! RMUX refuses a non-positive rectangle before anything is created, which is
//! the divergence FNP1 chose, FNP2 kept for percentages, and this lot keeps for
//! defaults. No clamp is introduced.

/// The width of a float whose `-x` was omitted: half its window.
pub(super) const fn width(window_cols: u16) -> i32 {
    window_cols as i32 / 2
}

/// The height of a float whose `-y` was omitted: a quarter of its window.
pub(super) const fn height(window_rows: u16) -> i32 {
    window_rows as i32 / 4
}
