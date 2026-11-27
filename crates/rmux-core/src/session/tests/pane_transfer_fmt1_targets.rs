//! FMT1 R2 — the destination, the pane-list position and the split side are
//! three independent facts, and a destination with no room refuses atomically.
//!
//! Expectations come from tmux 3.7b on this host, receipts
//! `cross-window-{before,after}-nonzero-target`,
//! `cross-window-before-size-nonzero-target`, `full-size-middle-target`,
//! `refuse-cross-window-no-room` and `refuse-full-size-no-room` under
//! `fmt1/oracle-linux/`.

use super::*;
use crate::{FloatingPaneRect, Pane, WindowId};

fn rect(x: i32, y: i32, cols: i32, rows: i32) -> FloatingPaneRect {
    FloatingPaneRect::new(x, y, cols, rows).expect("valid floating rect")
}

fn ids(session: &Session, window_index: u32) -> Vec<PaneId> {
    session
        .window_at(window_index)
        .expect("window exists")
        .panes()
        .iter()
        .map(Pane::id)
        .collect()
}

fn geometry_of(session: &Session, window_index: u32, pane_id: PaneId) -> PaneGeometry {
    session
        .window_at(window_index)
        .expect("window exists")
        .panes()
        .iter()
        .find(|pane| pane.id() == pane_id)
        .expect("pane survives")
        .geometry()
}

fn index_of(session: &Session, window_index: u32, pane_id: PaneId) -> u32 {
    session
        .window_at(window_index)
        .expect("window exists")
        .panes()
        .iter()
        .find(|pane| pane.id() == pane_id)
        .expect("pane survives")
        .index()
}

// `move-pane -h` is a left/right share, spelled `Vertical` at this layer.
const SIDE_BY_SIDE: SplitDirection = SplitDirection::Vertical;
const STACKED: SplitDirection = SplitDirection::Horizontal;

fn join(direction: SplitDirection, detached: bool, before: bool) -> PaneJoinOptions {
    PaneJoinOptions::new(direction, detached, before, false, None)
}

/// Destination window 0 as `[tile anchor, float witness, tile destination]`,
/// plus a floating source alone in window 1. Returns
/// `(session, anchor_id, witness_id, destination_id, source_id, source_window)`.
fn destination_with_floating_predecessor() -> (Session, PaneId, PaneId, PaneId, PaneId, u32) {
    let size = TerminalSize::new(80, 24);
    let mut session = Session::new(session_name("qual"), size);
    let middle = session.split_pane_in_window(0, 0).expect("middle split");
    let destination = session
        .split_pane_in_window(0, middle)
        .expect("destination split");
    let anchor_id = session.window_at(0).unwrap().pane(0).unwrap().id();
    let witness_id = session.window_at(0).unwrap().pane(middle).unwrap().id();
    let destination_id = session
        .window_at(0)
        .unwrap()
        .pane(destination)
        .unwrap()
        .id();
    session
        .window_at_mut(0)
        .unwrap()
        .float_pane(middle, rect(3, 2, 20, 6))
        .expect("float the middle pane");
    let (source_window, _) = session.create_window(size).expect("source window");
    let source_index = session
        .window_at(source_window)
        .unwrap()
        .active_pane_index();
    let source_id = session
        .window_at(source_window)
        .unwrap()
        .pane(source_index)
        .unwrap()
        .id();
    session
        .window_at_mut(source_window)
        .unwrap()
        .float_pane(source_index, rect(5, 4, 18, 5))
        .expect("float the source");
    session.select_window(0).expect("start on the destination");
    assert_eq!(
        ids(&session, 0),
        vec![anchor_id, witness_id, destination_id],
        "the floating witness sits between the two tiles in the pane list"
    );
    (
        session,
        anchor_id,
        witness_id,
        destination_id,
        source_id,
        source_window,
    )
}

#[test]
fn before_splits_the_named_destination_past_a_floating_predecessor() {
    for before in [false, true] {
        let (mut session, anchor_id, witness_id, destination_id, source_id, source_window) =
            destination_with_floating_predecessor();
        let anchor_before = geometry_of(&session, 0, anchor_id);
        let destination_before = geometry_of(&session, 0, destination_id);
        let active_before = session.window_at(0).unwrap().active_pane_index();

        session
            .join_pane(
                SessionPaneTarget::new(source_window, index_of(&session, source_window, source_id)),
                SessionPaneTarget::new(0, index_of(&session, 0, destination_id)),
                join(SIDE_BY_SIDE, true, before),
            )
            .expect("a floating predecessor never decides the split");

        // The named destination is the pane that was split; the anchor and the
        // floating witness are untouched.
        assert_eq!(geometry_of(&session, 0, anchor_id), anchor_before);
        assert_eq!(
            geometry_of(&session, 0, witness_id),
            PaneGeometry::new(3, 2, 20, 6)
        );
        assert!(session
            .window_at(0)
            .unwrap()
            .floating_stack()
            .contains(witness_id));
        let destination_after = geometry_of(&session, 0, destination_id);
        let moved_after = geometry_of(&session, 0, source_id);
        assert_eq!(destination_after.rows(), destination_before.rows());
        assert_eq!(
            u32::from(destination_after.cols()) + 1 + u32::from(moved_after.cols()),
            u32::from(destination_before.cols()),
            "the two share exactly the destination's own cell"
        );
        assert_eq!(
            moved_after.x() < destination_after.x(),
            before,
            "`-b` picks the side and only the side"
        );
        // The pane list keeps the moved pane immediately after its destination.
        assert_eq!(
            ids(&session, 0),
            vec![anchor_id, witness_id, destination_id, source_id]
        );
        assert_eq!(
            session.window_at(0).unwrap().active_pane_index(),
            active_before,
            "`-d` keeps the selection"
        );
    }
}

#[test]
fn full_size_shares_the_window_and_still_renumbers_next_to_its_destination() {
    // Receipt `full-size-middle-target`: a destination that is neither first
    // nor last keeps every rectangle proportional and the moved pane lands at
    // the destination's index plus one.
    for source_before_destination in [false, true] {
        let size = TerminalSize::new(80, 24);
        let mut session = Session::new(session_name("qual"), size);
        // Three stacked tiles, like the oracle cell, so `-f` sharing sideways
        // leaves every survivor on the other half of the window.
        let second = session
            .split_pane_in_window_with_direction(0, 0, STACKED)
            .expect("second tile");
        let third = session
            .split_pane_in_window_with_direction(0, second, STACKED)
            .expect("third tile");
        let first_id = session.window_at(0).unwrap().pane(0).unwrap().id();
        let second_id = session.window_at(0).unwrap().pane(second).unwrap().id();
        let third_id = session.window_at(0).unwrap().pane(third).unwrap().id();
        let (source_id, destination_id) = if source_before_destination {
            (first_id, second_id)
        } else {
            (third_id, second_id)
        };
        let source_index = index_of(&session, 0, source_id);
        session
            .window_at_mut(0)
            .unwrap()
            .float_pane(source_index, rect(3, 2, 20, 6))
            .expect("float the source");

        session
            .join_pane(
                SessionPaneTarget::new(0, index_of(&session, 0, source_id)),
                SessionPaneTarget::new(0, index_of(&session, 0, destination_id)),
                PaneJoinOptions::new(SIDE_BY_SIDE, true, false, true, None),
            )
            .expect("full-size share succeeds");

        let moved = geometry_of(&session, 0, source_id);
        assert_eq!(
            (moved.x(), moved.y(), moved.cols(), moved.rows()),
            (41, 0, 39, 24),
            "`-f` shares the whole window, not the destination's cell"
        );
        for survivor in [first_id, second_id, third_id]
            .into_iter()
            .filter(|pane_id| *pane_id != source_id)
        {
            assert_eq!(
                geometry_of(&session, 0, survivor).cols(),
                40,
                "every surviving tile keeps the other half"
            );
        }
        assert_eq!(
            index_of(&session, 0, source_id),
            index_of(&session, 0, destination_id) + 1,
            "`-f` picks the root over the named leaf, never the list position"
        );
        assert!(session.window_at(0).unwrap().floating_stack().is_empty());
    }
}

#[test]
fn a_destination_with_no_room_refuses_atomically_across_windows() {
    // Receipt `refuse-cross-window-no-room`: exit 1, no state change, and the
    // next transfer that does fit still works.
    for full_size in [false, true] {
        let mut session = Session::new(session_name("qual"), TerminalSize::new(80, 2));
        let (destination_window, _) = session
            .create_window(TerminalSize::new(80, 2))
            .expect("destination window");
        let destination_id = session
            .window_at(destination_window)
            .unwrap()
            .pane(0)
            .unwrap()
            .id();
        let source_id = session.window_at(0).unwrap().pane(0).unwrap().id();
        session
            .window_at_mut(0)
            .unwrap()
            .float_pane(0, rect(3, 2, 20, 6))
            .expect("float the source");
        session
            .window_layout_context_mut(destination_window)
            .unwrap()
            .toggle_zoom(0)
            .ok();
        let before = session.clone();

        let error = session
            .join_pane(
                SessionPaneTarget::new(0, 0),
                SessionPaneTarget::new(destination_window, 0),
                PaneJoinOptions::new(STACKED, true, false, full_size, None),
            )
            .expect_err("two rows cannot be shared");
        assert!(
            error.to_string().contains("cannot split"),
            "unexpected refusal: {error}"
        );
        assert_eq!(
            session, before,
            "a refusal leaves both windows, their zoom and their placements alone"
        );

        // The same source still transfers on the axis that does fit.
        session
            .join_pane(
                SessionPaneTarget::new(0, 0),
                SessionPaneTarget::new(destination_window, 0),
                PaneJoinOptions::new(SIDE_BY_SIDE, true, false, full_size, None),
            )
            .expect("the transfer that fits still works after the refusal");
        assert_eq!(
            ids(&session, destination_window),
            vec![destination_id, source_id]
        );
    }
}

#[test]
fn a_same_window_full_size_share_with_no_room_refuses_atomically() {
    let mut session = Session::new(session_name("qual"), TerminalSize::new(80, 2));
    let source = session.split_pane_in_window(0, 0).expect("source split");
    let tile_id = session.window_at(0).unwrap().pane(0).unwrap().id();
    let source_id = session.window_at(0).unwrap().pane(source).unwrap().id();
    session
        .window_at_mut(0)
        .unwrap()
        .float_pane(source, rect(3, 2, 20, 6))
        .expect("float the source");
    let before = session.clone();

    let error = session
        .join_pane(
            SessionPaneTarget::new(0, index_of(&session, 0, source_id)),
            SessionPaneTarget::new(0, index_of(&session, 0, tile_id)),
            PaneJoinOptions::new(STACKED, true, false, true, None),
        )
        .expect_err("two rows cannot be shared");
    assert!(
        error.to_string().contains("cannot split"),
        "unexpected refusal: {error}"
    );
    assert_eq!(session, before);
    assert!(session
        .window_at(0)
        .unwrap()
        .floating_stack()
        .contains(source_id));

    session
        .join_pane(
            SessionPaneTarget::new(0, index_of(&session, 0, source_id)),
            SessionPaneTarget::new(0, index_of(&session, 0, tile_id)),
            PaneJoinOptions::new(SIDE_BY_SIDE, true, false, true, None),
        )
        .expect("the transfer that fits still works after the refusal");
    assert_eq!(ids(&session, 0), vec![tile_id, source_id]);
}

#[test]
fn a_cross_session_transfer_keeps_the_same_destination_and_list_rules() {
    let size = TerminalSize::new(80, 24);
    let (mut destination_session, _, witness_id, destination_id, _, source_window) =
        destination_with_floating_predecessor();
    // Drop the extra window: this route carries the source from another Session.
    destination_session
        .remove_window_allowing_empty(source_window)
        .expect("the spare window goes away");
    let mut source_session = Session::new_with_initial_window(
        session_name("other"),
        size,
        0,
        PaneId::new(500),
        WindowId::new(500),
    );
    source_session
        .window_at_mut(0)
        .unwrap()
        .float_pane(0, rect(5, 4, 18, 5))
        .expect("float the source");

    destination_session
        .join_pane_from_session(
            SessionPaneTarget::new(0, index_of(&destination_session, 0, destination_id)),
            &mut source_session,
            SessionPaneTarget::new(0, 0),
            join(SIDE_BY_SIDE, true, true),
        )
        .expect("cross-session transfer into the named destination");

    assert_eq!(
        index_of(&destination_session, 0, PaneId::new(500)),
        index_of(&destination_session, 0, destination_id) + 1,
        "the list rule does not depend on the route"
    );
    assert!(destination_session
        .window_at(0)
        .unwrap()
        .floating_stack()
        .contains(witness_id));
    assert!(!destination_session
        .window_at(0)
        .unwrap()
        .floating_stack()
        .contains(PaneId::new(500)));
}
