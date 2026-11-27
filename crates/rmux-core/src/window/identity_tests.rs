use super::*;
use rmux_proto::{ResizePaneAdjustment, SplitDirection};

const LAYOUTS: [LayoutName; 7] = [
    LayoutName::EvenHorizontal,
    LayoutName::EvenVertical,
    LayoutName::MainHorizontal,
    LayoutName::MainHorizontalMirrored,
    LayoutName::MainVertical,
    LayoutName::MainVerticalMirrored,
    LayoutName::Tiled,
];

fn window(layout: LayoutName) -> Window {
    let mut window = Window::new_with_initial_pane(
        TerminalSize {
            cols: 120,
            rows: 40,
        },
        PaneId::new(41),
        WindowId::new(12),
    );
    window.split_after_position_with_id(0, PaneId::new(900));
    window.split_after_position_with_id(1, PaneId::new(7));
    window.set_layout(layout);
    window
}

fn position(window: &Window, id: PaneId) -> usize {
    window
        .panes
        .iter()
        .position(|pane| pane.id() == id)
        .unwrap()
}

fn geometry(window: &Window, id: PaneId) -> PaneGeometry {
    window.panes[position(window, id)].geometry()
}

fn same_cells(expected: &Window, actual: &Window) {
    assert_eq!(expected.layout_dump(), actual.layout_dump());
    assert_eq!(expected.pane_count(), actual.pane_count());
    for pane in expected.panes() {
        assert_eq!(
            pane.geometry(),
            geometry(actual, pane.id()),
            "pane {}",
            pane.id()
        );
    }
}

#[test]
fn sparse_leaves_survive_storage_reordering_resize_and_zoom() {
    for layout in LAYOUTS {
        let mut expected = window(layout);
        let mut reordered = expected.clone();
        reordered.panes.reverse();
        reordered.apply_layout_tree();
        same_cells(&expected, &reordered);
        for window in [&mut expected, &mut reordered] {
            window.set_size(TerminalSize {
                cols: 135,
                rows: 47,
            });
            window.resize_pane_width(0, 70);
            window.resize_pane_height(0, 25);
            window.resize_pane_by(0, ResizePaneAdjustment::Right { cells: 2 });
            window.resize_pane_by(0, ResizePaneAdjustment::Down { cells: 2 });
            window.spread_layout(0);
        }
        same_cells(&expected, &reordered);
        let saved = reordered.layout_dump();
        assert!(reordered.toggle_zoom(0));
        assert_eq!(reordered.layout_dump(), saved);
        assert!(reordered.visible_layout_dump().ends_with(",41"));
        reordered.auto_unzoom();
        same_cells(&expected, &reordered);
    }
}

#[test]
fn sparse_split_remove_and_full_size_insert_use_identity_in_every_named_layout() {
    for layout in LAYOUTS {
        for direction in [SplitDirection::Vertical, SplitDirection::Horizontal] {
            for before in [false, true] {
                let mut expected = window(layout);
                let mut reordered = expected.clone();
                reordered.panes.reverse();
                for window in [&mut expected, &mut reordered] {
                    let target = position(window, PaneId::new(41));
                    window.split_at_position_with_id_and_direction(
                        target,
                        PaneId::new(301),
                        direction,
                        before,
                    );
                }
                same_cells(&expected, &reordered);
                for window in [&mut expected, &mut reordered] {
                    let index = window.pane_index_for_id(PaneId::new(41)).unwrap();
                    assert_eq!(window.extract_pane(index).unwrap().id(), PaneId::new(41));
                    window
                        .insert_pane_full_size(
                            Pane::new_with_id(PaneId::new(8000), 20, PaneGeometry::new(0, 0, 1, 1)),
                            direction,
                            before,
                        )
                        .unwrap();
                }
                same_cells(&expected, &reordered);
            }
        }
    }
}

#[test]
fn same_window_move_preserves_identity_across_sibling_and_nested_paths() {
    for layout in LAYOUTS {
        for direction in [SplitDirection::Vertical, SplitDirection::Horizontal] {
            for before in [false, true] {
                for (source, target) in [(41, 900), (900, 41), (900, 7), (7, 900)] {
                    let mut expected = window(layout);
                    let mut reordered = expected.clone();
                    reordered.panes.reverse();
                    for window in [&mut expected, &mut reordered] {
                        let source = position(window, PaneId::new(source));
                        let target = position(window, PaneId::new(target));
                        let insertion =
                            target + usize::from(!before) - usize::from(source < target);
                        window
                            .move_pane_by_splitting_target(
                                source, target, insertion, direction, before,
                            )
                            .unwrap();
                    }
                    same_cells(&expected, &reordered);
                }
            }
        }
    }
}

#[test]
fn explicit_swap_rotation_and_cross_window_replacement_reassign_occupied_cells() {
    for layout in LAYOUTS {
        let mut window = window(layout);
        let original: Vec<_> = window.panes.iter().map(Pane::geometry).collect();
        assert!(window.swap_panes(0, 1));
        assert_eq!(geometry(&window, PaneId::new(900)), original[0]);
        assert_eq!(geometry(&window, PaneId::new(41)), original[1]);
        for direction in [RotateWindowDirection::Up, RotateWindowDirection::Down] {
            window.rotate_panes(direction);
            for (pane, cell) in window.panes.iter().zip(&original) {
                assert_eq!(pane.geometry(), *cell);
            }
        }
        window
            .replace_pane_for_swap(
                0,
                Pane::new_with_id(PaneId::new(9999), 0, PaneGeometry::new(0, 0, 1, 1)),
            )
            .unwrap();
        assert_eq!(geometry(&window, PaneId::new(9999)), original[0]);
        let saved = window.layout_dump();
        window.panes.reverse();
        window.apply_layout_tree();
        assert_eq!(window.layout_dump(), saved);
        assert_eq!(geometry(&window, PaneId::new(9999)), original[0]);
    }
}

#[test]
fn imported_ids_are_remapped_in_window_order_and_excess_leaves_are_trimmed() {
    let mut window = window(LayoutName::EvenHorizontal);
    // IDs are foreign and intentionally duplicated. They never become owners.
    let body = "120x40,0,0{30x40,0,0,888,30x40,31,0,888,28x40,62,0,42,29x40,91,0,99}";
    let layout = format!("{:04x},{body}", crate::layout::layout_checksum(body));
    window.panes.reverse();
    window.apply_custom_layout(&layout).unwrap();
    assert_eq!(
        geometry(&window, PaneId::new(7)),
        PaneGeometry::new(0, 0, 30, 40)
    );
    assert_eq!(
        geometry(&window, PaneId::new(900)),
        PaneGeometry::new(31, 0, 30, 40)
    );
    assert_eq!(
        geometry(&window, PaneId::new(41)),
        PaneGeometry::new(62, 0, 58, 40)
    );
    let dump = window.layout_dump();
    assert!(!dump.contains("888"));
    window.save_old_layout();
    window.set_layout(LayoutName::MainVerticalMirrored);
    assert!(window.reapply_old_layout().unwrap());
    assert_eq!(window.layout_dump(), dump);
    window.panes.reverse();
    window.apply_layout_tree();
    assert_eq!(window.layout_dump(), dump);
    assert!(window.resize_pane_width(0, 50));
    assert_eq!(geometry(&window, PaneId::new(41)).cols(), 50);
}

#[test]
fn mirrored_mutations_follow_measured_tmux_37b_cells() {
    // Pinned tmux 3.7b: 120x40, three panes, main pane 0, defaults 80x24.
    for (layout, direction, split_main, split_new, spread_main) in [
        (
            LayoutName::MainVerticalMirrored,
            SplitDirection::Horizontal,
            PaneGeometry::new(40, 0, 80, 20),
            PaneGeometry::new(40, 21, 80, 19),
            PaneGeometry::new(61, 0, 59, 40),
        ),
        (
            LayoutName::MainHorizontalMirrored,
            SplitDirection::Vertical,
            PaneGeometry::new(0, 16, 60, 24),
            PaneGeometry::new(61, 16, 59, 24),
            PaneGeometry::new(0, 21, 120, 19),
        ),
    ] {
        let mut split = window(layout);
        let secondary = geometry(&split, PaneId::new(900));
        split.split_after_position_with_id_and_direction(0, PaneId::new(301), direction);
        assert_eq!(geometry(&split, PaneId::new(41)), split_main);
        assert_eq!(geometry(&split, PaneId::new(301)), split_new);
        assert_eq!(geometry(&split, PaneId::new(900)), secondary);
        let mut spread = window(layout);
        assert!(spread.spread_layout(0));
        assert_eq!(geometry(&spread, PaneId::new(41)), spread_main);
        let mut removed = window(layout);
        removed.extract_pane(0).unwrap();
        let expected = match direction {
            SplitDirection::Horizontal => PaneGeometry::new(0, 0, 120, 20),
            SplitDirection::Vertical => PaneGeometry::new(0, 0, 60, 40),
        };
        assert_eq!(geometry(&removed, PaneId::new(900)), expected);
    }
}

#[test]
fn insertion_and_single_pane_lifecycle_bind_actual_ids() {
    for insertion in 0_usize..=3 {
        let mut window = window(LayoutName::MainVerticalMirrored);
        let target = window.panes[insertion.saturating_sub(1)].id();
        let mut expected = window.clone();
        expected.split_at_position_with_id_and_direction(
            insertion.saturating_sub(1),
            PaneId::new(600),
            SplitDirection::Horizontal,
            insertion == 0,
        );
        // The transfer contract names the split target, the list position and
        // the side separately; a positional insert is the case where all three
        // happen to agree with the equivalent split.
        let incoming = Pane::new_with_id(PaneId::new(600), 22, PaneGeometry::new(0, 0, 1, 1));
        let plan = window
            .plan_pane_insertion(
                incoming.id(),
                crate::window::PaneInsertionTarget::Leaf(target),
                SplitDirection::Horizontal,
                insertion == 0,
            )
            .unwrap();
        window
            .commit_pane_insertion(plan, insertion, incoming)
            .unwrap();
        same_cells(&expected, &window);
        assert!(geometry(&window, target).rows() < 40);
    }
    let mut window = window(LayoutName::Tiled);
    window.respawn(PaneId::new(4321));
    assert!(window.layout_dump().ends_with(",4321"));
    window.extract_pane(0).unwrap();
    assert!(window.layout_dump().is_empty());
    let incoming = Pane::new_with_id(PaneId::new(8765), 4, PaneGeometry::new(0, 0, 1, 1));
    let plan = window
        .plan_pane_insertion(
            incoming.id(),
            crate::window::PaneInsertionTarget::SoleLeaf,
            SplitDirection::Vertical,
            false,
        )
        .unwrap();
    window.commit_pane_insertion(plan, 0, incoming).unwrap();
    assert!(window.layout_dump().ends_with(",8765"));
    window.split_after_position_with_id(0, PaneId::new(333));
    window.extract_pane(0).unwrap();
    assert!(window.layout_dump().ends_with(",333"));
    assert_eq!(window.panes[0].index(), 0);
}
