//! `tiled-layout-max-columns` at the window level.
//!
//! The expected geometries are the cells measured on the frozen tmux 3.7b
//! oracle (`runs/tcol1/oracle/20260913T014246Z-89ac`): a 120x60 window with
//! four tiled panes gives two columns at x=0,60 and two rows at y=0,30 without
//! a bound, and one column at x=0 with four rows at y=0,15,30,45 under a bound
//! of 1. A bound of 2 or of 65535 leaves the two-by-two grid alone.

use super::Window;
use crate::{FloatingPaneRect, PaneGeometry};
use rmux_proto::{LayoutName, ResizePaneAdjustment, TerminalSize};

const WINDOW: TerminalSize = TerminalSize {
    cols: 120,
    rows: 60,
};

fn window_with_tiled_panes(count: usize) -> Window {
    let mut window = Window::new(WINDOW);
    for _ in 1..count {
        window.split_after_position(0);
    }
    window
}

fn geometries(window: &Window) -> Vec<PaneGeometry> {
    (0..window.pane_count() as u32)
        .map(|index| window.pane(index).expect("pane exists").geometry())
        .collect()
}

/// Two columns at x=0,60 and two rows at y=0,30.
fn oracle_two_by_two() -> Vec<PaneGeometry> {
    vec![
        PaneGeometry::new(0, 0, 59, 29),
        PaneGeometry::new(60, 0, 60, 29),
        PaneGeometry::new(0, 30, 59, 30),
        PaneGeometry::new(60, 30, 60, 30),
    ]
}

/// One column at x=0 and four rows at y=0,15,30,45.
fn oracle_one_by_four() -> Vec<PaneGeometry> {
    vec![
        PaneGeometry::new(0, 0, 120, 14),
        PaneGeometry::new(0, 15, 120, 14),
        PaneGeometry::new(0, 30, 120, 14),
        PaneGeometry::new(0, 45, 120, 15),
    ]
}

#[test]
fn four_tiled_panes_follow_the_oracle_cell_for_each_bound() {
    for (bound, expected) in [
        (None, oracle_two_by_two()),
        (Some(1), oracle_one_by_four()),
        (Some(2), oracle_two_by_two()),
        (Some(65535), oracle_two_by_two()),
    ] {
        let mut window = window_with_tiled_panes(4);
        window.set_layout_with_tiled_max_columns(LayoutName::Tiled, None, None, bound);

        assert_eq!(geometries(&window), expected, "bound {bound:?}");
    }
}

/// The bound is kept with the window's other rebuild parameters, so a rebuild
/// that the user did not ask a layout for still honours it.
#[test]
fn the_bound_survives_a_later_rebuild_of_the_same_window() {
    let mut window = window_with_tiled_panes(4);
    window.set_layout_with_tiled_max_columns(LayoutName::Tiled, None, None, Some(1));
    assert_eq!(geometries(&window), oracle_one_by_four());

    window.resize_main_pane(ResizePaneAdjustment::AbsoluteHeight { rows: 20 });

    assert!(
        geometries(&window).iter().all(|geometry| geometry.x() == 0),
        "the rebuild kept a single column: {:?}",
        geometries(&window)
    );
}

#[test]
fn a_checkpoint_restores_the_bound_and_not_only_the_tree() {
    let mut window = window_with_tiled_panes(4);
    window.set_layout_with_tiled_max_columns(LayoutName::Tiled, None, None, Some(1));
    let checkpoint = window.checkpoint();

    window.set_layout_with_tiled_max_columns(LayoutName::Tiled, None, None, None);
    assert_eq!(geometries(&window), oracle_two_by_two());

    window.restore_checkpoint(checkpoint);
    assert_eq!(geometries(&window), oracle_one_by_four());

    // A rebuild after the restore still uses the restored bound, which the
    // restored tree alone could not show.
    window.resize_main_pane(ResizePaneAdjustment::AbsoluteHeight { rows: 20 });
    assert!(
        geometries(&window).iter().all(|geometry| geometry.x() == 0),
        "the restored bound survived the rebuild: {:?}",
        geometries(&window)
    );
}

/// The pre-existing unbounded entry points keep their exact previous behaviour.
#[test]
fn the_unbounded_wrappers_select_without_a_bound() {
    let mut window = window_with_tiled_panes(4);
    window.set_layout_with_tiled_max_columns(LayoutName::Tiled, None, None, Some(1));
    assert_eq!(geometries(&window), oracle_one_by_four());

    window.set_layout_with_main_pane_size(LayoutName::Tiled, None, None);

    assert_eq!(geometries(&window), oracle_two_by_two());
}

/// Oracle cell: `main-vertical-mirrored` then `next-layout` reaches tiled and
/// uses the bound 1.
#[test]
fn next_layout_reaching_tiled_uses_the_bound() {
    let mut window = window_with_tiled_panes(4);
    window.set_layout_with_main_pane_size(LayoutName::MainVerticalMirrored, None, None);

    let layout = window.next_layout_with_tiled_max_columns(None, None, Some(1));

    assert_eq!(layout, LayoutName::Tiled);
    assert_eq!(geometries(&window), oracle_one_by_four());
}

/// Oracle cell: `even-horizontal` then `previous-layout` reaches tiled and uses
/// the bound 2.
#[test]
fn previous_layout_reaching_tiled_uses_the_bound() {
    let mut window = window_with_tiled_panes(4);
    window.set_layout_with_main_pane_size(LayoutName::EvenHorizontal, None, None);

    let layout = window.previous_layout_with_tiled_max_columns(None, None, Some(2));

    assert_eq!(layout, LayoutName::Tiled);
    assert_eq!(geometries(&window), oracle_two_by_two());

    // The oracle cell is a bound of 2, which is also the unbounded grid; repeat
    // the same cycle under 1 so this witness can fail.
    window.set_layout_with_main_pane_size(LayoutName::EvenHorizontal, None, None);
    let layout = window.previous_layout_with_tiled_max_columns(None, None, Some(1));

    assert_eq!(layout, LayoutName::Tiled);
    assert_eq!(geometries(&window), oracle_one_by_four());
}

/// A floating pane that is already there keeps its geometry and its identity
/// and is not one of the bounded grid's cells.
#[test]
fn a_floating_pane_stays_out_of_the_bounded_grid() {
    let mut window = window_with_tiled_panes(5);
    let floating_id = window.pane(4).expect("pane 4 exists").id();
    window
        .float_pane(4, FloatingPaneRect::new(10, 6, 30, 10).expect("valid rect"))
        .expect("pane 4 floats");
    let floating_geometry = window.pane(4).expect("pane 4 exists").geometry();

    window.set_layout_with_tiled_max_columns(LayoutName::Tiled, None, None, Some(1));

    let tiled = (0..4)
        .map(|index| window.pane(index).expect("tiled pane exists").geometry())
        .collect::<Vec<_>>();
    assert_eq!(tiled, oracle_one_by_four());
    assert_eq!(window.pane(4).expect("pane 4 exists").id(), floating_id);
    assert_eq!(
        window.pane(4).expect("pane 4 exists").geometry(),
        floating_geometry
    );
}

#[test]
fn layouts_other_than_tiled_ignore_the_bound() {
    for layout in [
        LayoutName::EvenHorizontal,
        LayoutName::EvenVertical,
        LayoutName::MainHorizontal,
        LayoutName::MainVertical,
    ] {
        let mut unbounded = window_with_tiled_panes(4);
        unbounded.set_layout_with_tiled_max_columns(layout, None, None, None);

        let mut bounded = window_with_tiled_panes(4);
        bounded.set_layout_with_tiled_max_columns(layout, None, None, Some(1));

        assert_eq!(
            geometries(&bounded),
            geometries(&unbounded),
            "layout {layout:?}"
        );
    }
}

#[test]
fn a_custom_layout_and_the_old_layout_are_unchanged_by_the_bound() {
    let mut window = window_with_tiled_panes(4);
    window.set_layout_with_tiled_max_columns(LayoutName::Tiled, None, None, None);
    let two_by_two = geometries(&window);
    window.save_old_layout();

    window.set_layout_with_tiled_max_columns(LayoutName::Tiled, None, None, Some(1));
    assert_eq!(geometries(&window), oracle_one_by_four());

    assert!(window.reapply_old_layout().expect("old layout reapplies"));

    assert_eq!(geometries(&window), two_by_two);
}
