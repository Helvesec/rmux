use super::*;
use crate::session::WindowLayoutContext;
use rmux_proto::{
    ResizePaneAdjustment as A, ResizePaneRelativeDirection as D, RmuxError, SessionName,
    SplitDirection,
};

fn fixture(mixed: bool) -> Window {
    let mut window = Window::new(TerminalSize {
        cols: 120,
        rows: 40,
    });
    if mixed {
        window.split_after_position_with_id(0, PaneId::new(10));
        window.split_after_position_with_id(1, PaneId::new(20));
    }
    window
        .float_pane(0, FloatingPaneRect::new(3, 4, 30, 10).unwrap())
        .unwrap();
    if mixed {
        assert!(window.toggle_zoom(1));
    }
    window
}

fn context(window: &mut Window) -> WindowLayoutContext<'_> {
    WindowLayoutContext::new(SessionName::new("resize").unwrap(), 0, window)
}

#[test]
fn single_floating_public_absolute_resize_applies_requested_dimensions() {
    for zoomed in [false, true] {
        for (adjustment, cols, rows) in [
            (A::AbsoluteHeight { rows: 8 }, 30, 8),
            (A::AbsoluteWidth { columns: 25 }, 25, 10),
            (A::AbsoluteHeight { rows: 0 }, 30, 1),
            (A::AbsoluteWidth { columns: 0 }, 1, 10),
            (
                A::AbsoluteSize {
                    columns: 25,
                    rows: 8,
                },
                25,
                8,
            ),
            (
                A::Composite {
                    columns: Some(0),
                    rows: Some(0),
                    relative: None,
                    cells: 2,
                },
                1,
                1,
            ),
        ] {
            let mut window = fixture(false);
            let id = window.pane(0).unwrap().id();
            let active = window.active_pane_index();
            if zoomed {
                context(&mut window).resize_pane(0, A::Zoom).unwrap();
            }
            context(&mut window).resize_pane(0, adjustment).unwrap();
            assert_eq!(
                window.floating_stack().placement(id).unwrap().rect(),
                FloatingPaneRect::new(3, 4, cols, rows).unwrap(),
                "zoomed={zoomed}, {adjustment:?}"
            );
            assert_eq!(window.pane(0).unwrap().geometry().cols(), cols as u16);
            assert_eq!(window.pane(0).unwrap().geometry().rows(), rows as u16);
            assert_eq!(window.active_pane_index(), active);
            assert_eq!(window.floating_stack().placements().len(), 1);
            assert_eq!(window.size(), TerminalSize::new(120, 40));
            assert!(!window.is_zoomed());
        }
    }
}

#[test]
fn explicit_local_floating_resize_is_atomic_and_exits_zoom() {
    for mixed in [false, true] {
        for (adjustment, expected) in [
            (
                A::AbsoluteWidth { columns: 25 },
                FloatingPaneRect::new(3, 4, 25, 10).unwrap(),
            ),
            (
                A::AbsoluteHeight { rows: 8 },
                FloatingPaneRect::new(3, 4, 30, 8).unwrap(),
            ),
            (
                A::AbsoluteSize {
                    columns: 25,
                    rows: 8,
                },
                FloatingPaneRect::new(3, 4, 25, 8).unwrap(),
            ),
            (
                A::Composite {
                    columns: Some(25),
                    rows: Some(8),
                    relative: None,
                    cells: 9,
                },
                FloatingPaneRect::new(3, 4, 25, 8).unwrap(),
            ),
        ] {
            let mut window = fixture(mixed);
            let pane_id = window.pane(0).unwrap().id();
            let active = window.active_pane_index();
            let stack_ids = window
                .floating_stack()
                .placements()
                .iter()
                .map(|placement| placement.pane_id())
                .collect::<Vec<_>>();

            let _ = adjustment;
            window
                .resize_floating_pane(0, expected.cols(), expected.rows())
                .unwrap();

            assert_eq!(window.active_pane_index(), active);
            assert!(!window.is_zoomed());
            assert_eq!(
                window.floating_stack().placement(pane_id).unwrap().rect(),
                expected
            );
            assert_eq!(
                window
                    .floating_stack()
                    .placements()
                    .iter()
                    .map(|placement| placement.pane_id())
                    .collect::<Vec<_>>(),
                stack_ids
            );
        }
    }
}

#[test]
fn floating_relative_resize_rejects_atomically() {
    let mut adjustments = Vec::new();
    for direction in [D::Up, D::Down, D::Left, D::Right] {
        for cells in [0, 2] {
            adjustments.push(direction.to_adjustment(cells));
            adjustments.push(A::Composite {
                columns: Some(25),
                rows: Some(8),
                relative: Some(direction),
                cells,
            });
            adjustments.push(A::Composite {
                columns: None,
                rows: None,
                relative: Some(direction),
                cells,
            });
        }
    }
    for mixed in [false, true] {
        for adjustment in &adjustments {
            let mut window = fixture(mixed);
            let before = window.clone();
            let result = context(&mut window).resize_pane(0, *adjustment);
            assert_eq!(window, before, "mixed={mixed}, {adjustment:?}");
            assert!(
                matches!(result, Err(RmuxError::Server(_))),
                "{adjustment:?}: {result:?}"
            );
        }
    }
}

#[test]
fn floating_generic_resize_preserves_valid_noops_and_zoom() {
    for mixed in [false, true] {
        for adjustment in [
            A::NoOp,
            A::TrimBelow,
            A::Composite {
                columns: None,
                rows: None,
                relative: None,
                cells: 2,
            },
        ] {
            let mut window = fixture(mixed);
            let before = window.clone();
            context(&mut window).resize_pane(0, adjustment).unwrap();
            assert_eq!(window, before);
        }
        let mut window = fixture(mixed);
        let mut expected = window.clone();
        expected.toggle_zoom(0);
        context(&mut window).resize_pane(0, A::Zoom).unwrap();
        assert_eq!(window, expected);
    }
}

#[test]
fn floating_resize_primitives_do_not_mutate() {
    for mixed in [false, true] {
        for operation in 0..7 {
            let mut window = fixture(mixed);
            let before = window.clone();
            let resized = match operation {
                0 => window.resize_pane_width(0, 25),
                1 => window.resize_pane_height(0, 8),
                2 => window.resize_pane_to(0, SplitDirection::Vertical, 25),
                3 => window.resize_pane_by(0, A::Right { cells: 2 }),
                4 => window.resize_new_split_pane_to(0, SplitDirection::Vertical, 25, false),
                5 => window.resize_new_split_pane_to(0, SplitDirection::Horizontal, 8, true),
                _ => {
                    window.resize_main_pane(A::AbsoluteSize {
                        columns: 25,
                        rows: 8,
                    });
                    false
                }
            };
            assert_eq!(window, before, "mixed={mixed}, operation={operation}");
            assert!(!resized);
        }
    }
}

#[test]
fn tiled_generic_resize_preserves_membership() {
    for mixed in [false, true] {
        let mut window = Window::new(TerminalSize {
            cols: 120,
            rows: 40,
        });
        window.split_after_position_with_id(0, PaneId::new(10));
        window.split_after_position_with_id(1, PaneId::new(20));
        if mixed {
            window
                .float_pane(2, FloatingPaneRect::new(3, 4, 30, 10).unwrap())
                .unwrap();
        }
        let floating = window.floating_stack();
        for adjustment in [
            A::AbsoluteWidth { columns: 40 },
            A::AbsoluteHeight { rows: 20 },
            A::AbsoluteSize {
                columns: 45,
                rows: 22,
            },
            A::Right { cells: 2 },
            A::Composite {
                columns: Some(40),
                rows: Some(20),
                relative: Some(D::Left),
                cells: 2,
            },
        ] {
            window.toggle_zoom(0);
            context(&mut window).resize_pane(0, adjustment).unwrap();
            assert!(!window.is_zoomed());
            assert_eq!(
                window
                    .floating_stack()
                    .placements()
                    .iter()
                    .map(|p| p.pane_id())
                    .collect::<Vec<_>>(),
                floating
                    .placements()
                    .iter()
                    .map(|p| p.pane_id())
                    .collect::<Vec<_>>()
            );
        }
        let before = window.clone();
        assert!(context(&mut window)
            .resize_pane(99, A::AbsoluteWidth { columns: 30 })
            .is_err());
        assert_eq!(window, before);
    }
}

fn canonical_fixture(floats: u32, horizontal: bool) -> Window {
    let mut window = Window::new(TerminalSize::new(80, 12));
    if horizontal {
        window.split_after_position_with_id(0, PaneId::new(50));
        window.set_layout(LayoutName::EvenHorizontal);
    }
    // Insert floating panes directly through the product owner: no tiled split
    // can change the root axis or its canonical content in this fixture.
    for n in 0..floats {
        let id = PaneId::new(n + 1);
        let index = window.panes.len() as u32;
        window.panes.push(crate::Pane::new_with_id(
            id,
            index,
            crate::PaneGeometry::new(0, 0, 12, 4),
        ));
        window
            .floating_stack
            .push_frontmost(FloatingPanePlacement::new(
                id,
                FloatingPaneRect::new(-3, 1, 12, 4).unwrap(),
            ))
            .unwrap();
        window.apply_layout_tree();
    }
    window
}

#[test]
fn canonical_root_axis_transfer_matches_native_tmux032_and033() {
    for (horizontal, columns, rows, expected_cols, expected_rows, content_cols, content_rows) in [
        (false, Some(10), Some(3), 12, 3, 80, 13),
        (false, Some(0), None, 12, 4, 80, 12),
        (false, None, Some(0), 12, 1, 80, 15),
        (true, Some(10), Some(3), 10, 4, 82, 12),
    ] {
        let mut w = canonical_fixture(1, horizontal);
        let index = w.pane_index_for_id(PaneId::new(1)).unwrap();
        w.resize_canonical_absolute(index, columns, rows).unwrap();
        let rect = w.floating_stack().placement(PaneId::new(1)).unwrap().rect();
        assert_eq!(
            (rect.x(), rect.y(), rect.cols(), rect.rows()),
            (-3, 1, expected_cols, expected_rows)
        );
        assert_eq!(w.size(), TerminalSize::new(80, 12));
        assert_eq!(
            w.current_layout_tree().unwrap().size(),
            TerminalSize::new(content_cols, content_rows)
        );
    }
}

#[test]
fn canonical_three_float_order_survives_focus_checkpoint_and_remap() {
    for (target, expected) in [(1, [3, 5, 4]), (2, [4, 3, 5]), (3, [4, 5, 3])] {
        let mut w = canonical_fixture(3, false);
        w.raise_floating_pane(1).unwrap();
        let checkpoint = w.checkpoint();
        w.resize_canonical_absolute(target, None, Some(2)).unwrap();
        w.restore_checkpoint(checkpoint);
        w.resize_canonical_absolute(target, None, Some(3)).unwrap();
        for (id, rows) in (1..=3).zip(expected) {
            assert_eq!(
                w.floating_stack()
                    .placement(PaneId::new(id))
                    .unwrap()
                    .rect()
                    .rows(),
                rows
            );
        }
        assert_eq!(w.pane(0).unwrap().geometry().rows(), 12);
    }
    let mut w = canonical_fixture(3, false);
    let geometry = w.pane(1).unwrap().geometry();
    *w.pane_mut(1).unwrap() = crate::Pane::new_with_id(PaneId::new(99), 1, geometry);
    w.resize_canonical_absolute(1, None, Some(3)).unwrap();
    assert_eq!(
        w.resize_topology.floating_attachment_order,
        vec![PaneId::new(99), PaneId::new(2), PaneId::new(3)]
    );
    assert_eq!(
        w.floating_stack()
            .placement(PaneId::new(2))
            .unwrap()
            .rect()
            .rows(),
        5
    );
}

#[test]
fn canonical_exhaustion_is_bounded_and_failed_plan_rolls_back() {
    let mut w = canonical_fixture(3, true);
    let index = w.pane_index_for_id(PaneId::new(1)).unwrap();
    w.resize_canonical_absolute(index, Some(1000), None)
        .unwrap();
    assert_eq!(
        w.floating_stack()
            .placement(PaneId::new(1))
            .unwrap()
            .rect()
            .cols(),
        111
    );
    assert_eq!(w.pane(0).unwrap().geometry().cols(), 1);
    assert_eq!(w.pane(1).unwrap().geometry().cols(), 1);
    let mut w = canonical_fixture(1, false);
    w.resize_floating_pane(1, 65536, 4).unwrap();
    let before = w.clone();
    assert!(w.resize_canonical_absolute(1, None, Some(3)).is_err());
    assert_eq!(w, before);
}

#[test]
fn canonical_overhang_scene_clips_without_changing_content_or_signed_origin() {
    let mut w = canonical_fixture(1, false);
    w.resize_canonical_absolute(1, Some(10), Some(3)).unwrap();
    let scene = w.resolved_scene(crate::ResolvedSceneContext::new(
        w.size(),
        crate::ResolvedPaneBorderStatus::Off,
        crate::ResolvedZoomVisibility::Rendered,
    ));
    let tiled = scene.pane_by_id(PaneId::new(0)).unwrap();
    assert_eq!((tiled.content().cols(), tiled.content().rows()), (80, 13));
    assert_eq!(
        (tiled.content_clip().cols(), tiled.content_clip().rows()),
        (80, 12)
    );
    let floating = scene.pane_by_id(PaneId::new(1)).unwrap();
    assert_eq!((floating.content().x(), floating.content().y()), (-3, 1));
    assert_eq!(floating.content_clip().x(), 0);
    let viewport = scene.into_viewport(0, 0, TerminalSize::new(80, 10));
    assert_eq!(
        viewport
            .pane_by_id(PaneId::new(0))
            .unwrap()
            .content()
            .rows(),
        13
    );
    assert_eq!(
        viewport
            .pane_by_id(PaneId::new(0))
            .unwrap()
            .content_clip()
            .rows(),
        10
    );
}

#[test]
fn canonical_new_pane_attaches_before_focus_or_tiled_insertion() {
    let mut w = Window::new(TerminalSize::new(80, 12));
    let f = w
        .insert_floating_pane(
            PaneId::new(1),
            FloatingPaneRect::new(-3, 1, 12, 4).unwrap().into(),
            true,
        )
        .unwrap();
    let snapshot = w
        .export_rmux_window_snapshot_v2(
            rmux_proto::SessionId::new(1),
            rmux_proto::WindowTarget::with_window(SessionName::new("resize").unwrap(), 0),
        )
        .unwrap();
    assert_eq!(
        snapshot.resize_topology.floating_attachment_order,
        vec![PaneId::new(1)]
    );
    w.split_after_position_with_id(0, PaneId::new(50));
    let f = w.pane_index_for_id(PaneId::new(1)).unwrap_or(f);
    w.resize_canonical_absolute(f, Some(10), Some(3)).unwrap();
    assert_eq!(
        w.floating_stack()
            .placement(PaneId::new(1))
            .unwrap()
            .rect()
            .cols(),
        12
    );
    assert_eq!(w.pane(0).unwrap().geometry().rows(), 13);
}

#[test]
fn l1_034_canonical_signed_endpoints_validate_before_zoom_or_owner_mutation() {
    use rmux_proto::{SessionId, WindowTarget};
    for horizontal in [false, true] {
        let mut w = canonical_fixture(2, horizontal);
        let id = PaneId::new(1);
        let index = w.pane_index_for_id(id).unwrap();
        let rect = if horizontal {
            FloatingPaneRect::new(2147483643, 1, 4, 4)
        } else {
            FloatingPaneRect::new(-3, 2147483643, 12, 4)
        }
        .unwrap();
        assert!(w.floating_stack.set_rect(id, rect));
        w.apply_layout_tree();
        w.select_pane(index);
        let (cols, rows) = if horizontal {
            (Some(4), None)
        } else {
            (None, Some(4))
        };
        w.resize_canonical_absolute(index, cols, rows).unwrap();
        let target = WindowTarget::with_window(SessionName::new("bounds").unwrap(), 0);
        let saved = w
            .export_rmux_window_snapshot_v2(SessionId::new(1), target.clone())
            .unwrap();
        let mut expected = w.clone();
        let (cols, rows) = if horizontal {
            (Some(3), None)
        } else {
            (None, Some(3))
        };
        expected
            .resize_canonical_absolute(index, cols, rows)
            .unwrap();
        // First invalid dimension: exact rollback includes selected pane, IDs,
        // root/attachment order and existing zoom checkpoint.
        assert!(w.toggle_zoom(0));
        let before = w.clone();
        let (badcols, badrows) = if horizontal {
            (Some(5), None)
        } else {
            (None, Some(5))
        };
        assert!(w
            .resize_canonical_absolute(index, badcols, badrows)
            .is_err());
        assert_eq!(w, before);
        assert!(!w.toggle_zoom(0)); // return value is the resulting zoom state
        w.move_floating_pane(index, 0, 0).unwrap();
        w.restore_rmux_window_snapshot_v2(&saved).unwrap();
        assert_eq!(
            w.export_rmux_window_snapshot_v2(SessionId::new(1), target.clone())
                .unwrap(),
            saved
        );
        w.resize_canonical_absolute(index, cols, rows).unwrap();
        assert_eq!(
            w.export_rmux_window_snapshot_v2(SessionId::new(1), target.clone())
                .unwrap(),
            expected
                .export_rmux_window_snapshot_v2(SessionId::new(1), target)
                .unwrap()
        );
    }
}
