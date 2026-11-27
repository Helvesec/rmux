use super::*;
use rmux_proto::SplitDirection;

fn rect(x: i32, y: i32, cols: i32, rows: i32) -> FloatingPaneRect {
    FloatingPaneRect::new(x, y, cols, rows).expect("valid floating rect")
}

fn stack_ids(window: &Window) -> Vec<PaneId> {
    window
        .floating_stack()
        .placements()
        .iter()
        .map(|placement| placement.pane_id())
        .collect()
}

fn three_pane_window() -> Window {
    let mut window = Window::new(TerminalSize {
        cols: 120,
        rows: 40,
    });
    window.split_after_position_with_id(0, PaneId::new(10));
    window.split_after_position_with_id(1, PaneId::new(20));
    window
}

#[test]
fn float_pane_removes_only_its_tiled_leaf_and_preserves_sparse_identity_index() {
    let mut window = three_pane_window();
    let floated_id = window.pane(1).expect("pane 1 exists").id();

    window.float_pane(1, rect(-3, 4, 30, 10)).unwrap();

    let stack = window.floating_stack();
    assert_eq!(
        stack.placement(floated_id),
        Some(FloatingPanePlacement::new(floated_id, rect(-3, 4, 30, 10)))
    );
    assert_eq!(window.pane(1).expect("pane 1 exists").id(), floated_id);
    assert_eq!(window.pane(1).expect("pane 1 exists").index(), 1);
    assert!(!window
        .current_layout_tree()
        .expect("remaining tiled panes")
        .contains_leaf(floated_id));
    assert_eq!(
        window.pane(1).expect("pane 1 exists").geometry(),
        PaneGeometry::new(0, 4, 30, 10)
    );
}

#[test]
fn float_pane_at_tiled_geometry_uses_saved_layout_and_preserves_selection() {
    let mut window = three_pane_window();
    let source_id = window.pane(1).expect("source pane exists").id();
    let existing_float = window.pane(2).expect("float pane exists").id();
    window.float_pane(2, rect(70, 5, 20, 7)).unwrap();
    assert!(window.select_pane(1));
    let active = window.active_pane_index();
    let last = window.last_pane_index();
    let canonical = window
        .current_layout_tree()
        .and_then(|tree| tree.pane_geometry(source_id))
        .expect("source remains in the saved tiled layout");
    assert!(window.toggle_zoom(1));
    assert_ne!(
        window.pane(1).expect("source pane exists").geometry(),
        canonical
    );

    window.float_pane_at_tiled_geometry(1).unwrap();

    assert!(!window.is_zoomed());
    assert_eq!(window.active_pane_index(), active);
    assert_eq!(window.last_pane_index(), last);
    assert_eq!(stack_ids(&window), vec![existing_float, source_id]);
    assert_eq!(
        window
            .floating_stack()
            .placement(source_id)
            .expect("source is frontmost floating pane")
            .rect(),
        rect(
            i32::from(canonical.x()),
            i32::from(canonical.y()),
            i32::from(canonical.cols()),
            i32::from(canonical.rows()),
        )
    );
}

#[test]
fn named_layout_preserves_floats_and_float_resize_is_local() {
    let mut window = three_pane_window();
    window.float_pane(1, rect(5, 6, 20, 7)).unwrap();
    let float_before = window.floating_stack();

    window.set_layout(LayoutName::EvenVertical);

    assert_eq!(window.floating_stack(), float_before);
    assert_eq!(
        window.pane(1).expect("pane 1 exists").geometry(),
        PaneGeometry::new(5, 6, 20, 7)
    );
    let tile_zero = window.pane(0).expect("pane 0 exists").geometry();
    let tile_two = window.pane(2).expect("pane 2 exists").geometry();

    window.resize_floating_pane(1, 44, 8).unwrap();

    assert_eq!(window.pane(0).expect("pane 0 exists").geometry(), tile_zero);
    assert_eq!(window.pane(2).expect("pane 2 exists").geometry(), tile_two);
    assert_eq!(
        window
            .floating_stack()
            .placement(PaneId::new(10))
            .expect("floating pane")
            .rect(),
        rect(5, 6, 44, 8)
    );
}

#[test]
fn last_tiled_pane_can_float_and_tile_back_without_losing_placement() {
    let mut window = Window::new(TerminalSize { cols: 80, rows: 24 });
    let pane_id = window.pane(0).expect("pane 0 exists").id();

    window.float_pane(0, rect(-2, -3, 33, 9)).unwrap();

    assert_eq!(window.layout_dump(), "");
    assert_eq!(stack_ids(&window), vec![pane_id]);
    assert!(window.current_layout_tree().is_none());

    window.set_layout(LayoutName::Tiled);

    assert_eq!(window.layout_dump(), "");
    assert_eq!(stack_ids(&window), vec![pane_id]);

    window
        .tile_floating_pane(0, None, SplitDirection::Vertical, false)
        .unwrap();

    assert!(window.floating_stack().is_empty());
    assert!(window
        .current_layout_tree()
        .expect("retiled pane")
        .contains_leaf(pane_id));
    assert_eq!(
        window.pane(0).expect("pane 0 exists").geometry(),
        PaneGeometry::new(0, 0, 80, 24)
    );
}

#[test]
fn floating_stack_tracks_order_visibility_selection_and_removal() {
    let mut window = three_pane_window();
    let pane_zero = window.pane(0).expect("pane 0 exists").id();
    let pane_two = window.pane(2).expect("pane 2 exists").id();
    window.float_pane(0, rect(1, 1, 20, 5)).unwrap();
    window.float_pane(2, rect(3, 3, 30, 6)).unwrap();

    assert_eq!(stack_ids(&window), vec![pane_zero, pane_two]);

    window.set_floating_pane_visible(0, false).unwrap();
    assert!(!window
        .floating_stack()
        .placement(pane_zero)
        .expect("pane zero is floating")
        .visible());

    window.raise_floating_pane(0).unwrap();
    assert_eq!(stack_ids(&window), vec![pane_two, pane_zero]);

    window.select_floating_pane(2).unwrap();
    assert_eq!(window.active_pane_index(), 2);
    assert_eq!(stack_ids(&window), vec![pane_zero, pane_two]);

    let removed = window.remove_pane(2).expect("floating pane removed");

    assert_eq!(removed.id(), pane_two);
    assert_eq!(stack_ids(&window), vec![pane_zero]);
    assert!(window.floating_stack().placement(pane_two).is_none());
}

#[test]
fn floating_transactions_reject_invalid_inputs_without_mutating() {
    let mut window = three_pane_window();
    let before = window.clone();
    assert!(FloatingPaneRect::new(0, 0, 0, 1).is_err());
    assert!(window
        .tile_floating_pane(0, Some(1), SplitDirection::Vertical, false)
        .is_err());
    assert_eq!(window, before);

    window.float_pane(0, rect(0, 0, 20, 5)).unwrap();
    let before = window.clone();

    assert!(window.float_pane(0, rect(1, 1, 20, 5)).is_err());
    assert!(window
        .tile_floating_pane(0, Some(0), SplitDirection::Vertical, false)
        .is_err());
    assert!(window.resize_floating_pane(0, 0, 5).is_err());
    assert!(window.move_floating_pane(99, 1, 1).is_err());

    assert_eq!(window, before);
}

#[test]
fn tiling_float_rejects_a_two_cell_axis_without_mutation() {
    let mut window = Window::new(TerminalSize { cols: 5, rows: 10 });
    window.split_after_position_with_id_and_direction(0, PaneId::new(10), SplitDirection::Vertical);
    let narrow = window
        .panes()
        .iter()
        .find(|pane| pane.geometry().cols() == 2)
        .unwrap()
        .index();
    let floating = window
        .insert_floating_pane(PaneId::new(20), rect(0, 0, 1, 1).into(), true)
        .unwrap();
    let before = window.clone();

    assert!(window
        .tile_floating_pane(floating, Some(narrow), SplitDirection::Vertical, false)
        .is_err());
    assert_eq!(window, before);
}

#[test]
fn public_pane_replacement_transfers_floating_placement_through_checkpoint_clone() {
    let mut window = three_pane_window();
    window.float_pane(1, rect(7, 8, 30, 9)).unwrap();
    let replacement = Pane::new_with_id(PaneId::new(99), 1, PaneGeometry::new(0, 0, 1, 1));

    *window.pane_mut(1).expect("pane 1 exists") = replacement;

    assert!(window.floating_stack().placement(PaneId::new(10)).is_none());
    assert!(window.floating_stack().placement(PaneId::new(99)).is_some());
    let checkpoint = window.checkpoint();
    let mut cloned = window.clone();

    cloned.set_size(TerminalSize {
        cols: 121,
        rows: 41,
    });
    assert!(cloned.floating_stack().placement(PaneId::new(99)).is_some());

    window.restore_checkpoint(checkpoint);
    assert!(window.floating_stack().placement(PaneId::new(99)).is_some());
}

// Exchange complete public values with matching indices and distinct identities.
// Assertions observe only the final valid state, never an intermediate duplicate.
fn exchange_public_pane(window: &mut Window, index: u32) -> PaneId {
    let mut donor =
        Window::new_with_initial_pane(window.size(), PaneId::new(99), WindowId::new(99));
    donor.pane_mut(0).unwrap().set_index(index);
    std::mem::swap(
        window.pane_mut(index).unwrap(),
        donor.pane_mut(index).unwrap(),
    );
    PaneId::new(99)
}

#[test]
fn floating_pane_mut_predicates_resolve_both_owners_without_reconciliation() {
    for floated in [false, true] {
        let mut window = three_pane_window();
        if floated {
            window.float_pane(1, rect(7, 8, 30, 9)).unwrap();
        }
        let old = window.pane(1).unwrap().id();
        let new = exchange_public_pane(&mut window, 1);
        let checkpoint = window.checkpoint();
        for candidate in [window.clone(), {
            window.restore_checkpoint(checkpoint);
            window
        }] {
            assert_eq!(candidate.is_floating_pane_id(new), floated);
            assert_eq!(candidate.is_tiled_pane_id(new), !floated);
            assert!(!candidate.is_floating_pane_id(old));
            assert!(!candidate.is_tiled_pane_id(old));
            assert_eq!(candidate.tiled_pane_ids().contains(&new), !floated);
            for direction in [SplitDirection::Vertical, SplitDirection::Horizontal] {
                assert_eq!(candidate.can_split_pane(1, direction), !floated);
            }
        }
    }
}

fn replaced_floating_session() -> crate::Session {
    let mut session = crate::Session::new(
        rmux_proto::SessionName::new("source").unwrap(),
        TerminalSize::new(120, 40),
    );
    session.split_pane(0).unwrap();
    session
        .window_mut()
        .float_pane(1, rect(7, 8, 30, 9))
        .unwrap();
    exchange_public_pane(session.window_mut(), 1);
    session
}

#[test]
fn floating_pane_mut_split_refuses_before_mutation() {
    for direction in [SplitDirection::Vertical, SplitDirection::Horizontal] {
        for before in [false, true] {
            let mut session = replaced_floating_session();
            let snapshot = format!("{session:?}");
            assert!(session
                .split_pane_in_window_with_id_and_direction_before(
                    0,
                    1,
                    PaneId::new(100),
                    direction,
                    before
                )
                .is_err());
            assert_eq!(format!("{session:?}"), snapshot);
        }
    }
}

// FMT1 moves a floating source into a tiled target through the same transfer.
// Only a floating *destination* keeps the refusal, and the source-side arms of
// the acquired matrix below now assert that transfer instead of a rejection.
#[test]
fn floating_pane_mut_join_siblings_refuse_before_mutation() {
    use crate::{PaneJoinOptions, SessionPaneTarget};
    for full in [false, true] {
        for floating_source in [false, true] {
            for route in ["within", "between", "cross"] {
                let mut session = replaced_floating_session();
                let mut other = crate::Session::new(
                    rmux_proto::SessionName::new("other").unwrap(),
                    TerminalSize::new(120, 40),
                );
                // Keep identities unique even across standalone sessions.
                *other.window_mut().pane_mut(0).unwrap() =
                    Pane::new_with_id(PaneId::new(200), 0, PaneGeometry::new(0, 0, 120, 40));
                let other_window = if route == "between" {
                    session.create_window(TerminalSize::new(120, 40)).unwrap().0
                } else {
                    0
                };
                let floating = SessionPaneTarget::new(0, 1);
                let tiled = SessionPaneTarget::new(other_window, 0);
                let options =
                    PaneJoinOptions::new(SplitDirection::Vertical, false, false, full, None);
                let snapshot = format!("{session:?}");
                let other_snapshot = format!("{other:?}");
                let result = if route == "cross" {
                    if floating_source {
                        other.join_pane_from_session(floating, &mut session, tiled, options)
                    } else {
                        session.join_pane_from_session(tiled, &mut other, floating, options)
                    }
                } else if floating_source {
                    session.join_pane(floating, tiled, options)
                } else {
                    session.join_pane(tiled, floating, options)
                };
                // The "cross" arms address a destination pane index that the
                // standalone session never had, so they stay refusals for a
                // target reason and are not a floating-source witness.
                let transfers = floating_source && route != "cross";
                assert_eq!(
                    result.is_err(),
                    !transfers,
                    "{route} source={floating_source} full={full}"
                );
                assert_eq!(format!("{other:?}"), other_snapshot);
                if !transfers {
                    assert_eq!(format!("{session:?}"), snapshot, "{route}");
                    continue;
                }
                let moved = session
                    .window_at(other_window)
                    .expect("destination window survives")
                    .panes()
                    .iter()
                    .find(|pane| pane.id() == PaneId::new(99))
                    .expect("the replaced floating identity reaches the destination");
                assert!(
                    !session
                        .window_at(other_window)
                        .expect("destination window survives")
                        .is_floating_pane_id(moved.id()),
                    "{route} full={full}"
                );
                assert!(session
                    .window_at(other_window)
                    .expect("destination window survives")
                    .floating_stack()
                    .is_empty());
            }
        }
    }
}

#[test]
fn floating_pane_mut_break_siblings_refuse_before_mutation() {
    for cross in [false, true] {
        let mut session = replaced_floating_session();
        let mut other = crate::Session::new(
            rmux_proto::SessionName::new("other").unwrap(),
            TerminalSize::new(120, 40),
        );
        let snapshot = format!("{session:?}");
        let other_snapshot = format!("{other:?}");
        let source = crate::SessionPaneTarget::new(0, 1);
        let result = if cross {
            session.break_pane_to_session(
                source,
                &mut other,
                crate::BreakPaneOptions::new(None, None, false, false, false),
            )
        } else {
            session.break_pane(
                source,
                crate::BreakPaneOptions::new(None, None, false, false, false),
            )
        };
        assert!(result.is_err());
        assert_eq!(format!("{session:?}"), snapshot);
        assert_eq!(format!("{other:?}"), other_snapshot);
    }
}

#[test]
fn floating_pane_mut_exchange_resolves_simultaneously() {
    let mut window = three_pane_window();
    window.float_pane(1, rect(7, 8, 30, 9)).unwrap();
    let mut tiled = window.pane(0).unwrap().clone();
    let mut floating = window.pane(1).unwrap().clone();
    let tiled_id = tiled.id();
    let floating_id = floating.id();
    tiled.set_index(1);
    floating.set_index(0);
    *window.pane_mut(0).unwrap() = floating;
    *window.pane_mut(1).unwrap() = tiled;
    assert!(window.is_floating_pane_id(tiled_id));
    assert!(!window.is_tiled_pane_id(tiled_id));
    assert!(!window.is_floating_pane_id(floating_id));
    assert!(window.is_tiled_pane_id(floating_id));
    assert_eq!(stack_ids(&window), vec![tiled_id]);
    assert!(!window.can_split_pane(1, SplitDirection::Vertical));
    assert!(window.can_split_pane(0, SplitDirection::Vertical));
}

/// Where the window puts a float whose position was omitted, and what an undo
/// of the window does to the memory it walked (`layout.c:1399-1417`).
#[test]
fn an_omitted_position_walks_the_cascade_and_an_undo_leaves_it_where_it_walked() {
    let mut window = Window::new(TerminalSize {
        cols: 120,
        rows: 40,
    });
    let asked = || FloatingPaneRequest::new(None, None, 10, 4).expect("a positive size");
    let placed = |window: &Window, id: u32| {
        window
            .floating_stack()
            .placement(PaneId::new(id))
            .expect("the float was placed")
            .rect()
    };

    window
        .insert_floating_pane(PaneId::new(10), asked(), true)
        .unwrap();
    assert_eq!(placed(&window, 10), rect(4, 2, 10, 4));
    let checkpoint = window.checkpoint();
    window
        .insert_floating_pane(PaneId::new(20), asked(), true)
        .unwrap();
    assert_eq!(placed(&window, 20), rect(8, 4, 10, 4));

    // Undoing the window takes that pane back with the rest of the layout, and
    // leaves the memory where the attempt walked it: the source assigns it in
    // the omitted branch before anything is placed and has no undo of its own,
    // and a window that proposed a step it had already taken back would keep
    // offering a position it has just refused.
    window.restore_checkpoint(checkpoint);
    assert_eq!(stack_ids(&window), vec![PaneId::new(10)]);
    window
        .insert_floating_pane(PaneId::new(30), asked(), true)
        .unwrap();
    assert_eq!(placed(&window, 30), rect(12, 6, 10, 4));
}
