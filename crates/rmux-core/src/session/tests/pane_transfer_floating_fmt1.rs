//! FMT1 — a floating source reaching a tiled target through `move-pane`/`join-pane`.
//!
//! Expectations come from two authorities. The same-window cells reuse the five
//! acquired tmux 3.7b receipts of `floating-conversion-prep` (Mac). The new cells
//! — cross-window, cross-session, `-f`, `-l`/`-p`, and a zoom held by another
//! pane — were measured on this host's qualified tmux 3.7b
//! (`/usr/local/bin/tmux`, sha256 03219809…18a7) and are recorded under
//! `fmt1/oracle-linux/`. Every divergence asserted here is an
//! inherited rule shared with the tiled sibling, never one FMT1 introduces; the
//! cases that carry one name it explicitly.

use super::*;
use crate::{FloatingPaneRect, WindowId};

fn rect(x: i32, y: i32, cols: i32, rows: i32) -> FloatingPaneRect {
    FloatingPaneRect::new(x, y, cols, rows).expect("valid floating rect")
}

fn geometry(session: &Session, window_index: u32, pane_index: u32) -> PaneGeometry {
    session
        .window_at(window_index)
        .expect("window exists")
        .pane(pane_index)
        .expect("pane exists")
        .geometry()
}

fn pane_ids(session: &Session, window_index: u32) -> Vec<(u32, PaneId)> {
    session
        .window_at(window_index)
        .expect("window exists")
        .panes()
        .iter()
        .map(|pane| (pane.index(), pane.id()))
        .collect()
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

fn stack_ids(session: &Session, window_index: u32) -> Vec<PaneId> {
    session
        .window_at(window_index)
        .expect("window exists")
        .floating_stack()
        .placements()
        .iter()
        .map(|placement| placement.pane_id())
        .collect()
}

/// tmux fixture of the acquired receipts: one 80x24 tile, a 30x8 floating
/// source at (3,2) and a 12x5 floating witness at (50,4), source inactive.
/// Returns `(source_index, witness_index)`; the tile keeps index 0.
fn oracle_fixture() -> (Session, u32, u32) {
    let mut session = Session::new(session_name("qual"), TerminalSize::new(80, 24));
    let source = session.split_pane_in_window(0, 0).expect("source split");
    let witness = session
        .split_pane_in_window(0, source)
        .expect("witness split");
    let window = session.window_at_mut(0).expect("window exists");
    window
        .float_pane(source, rect(3, 2, 30, 8))
        .expect("float source");
    window
        .float_pane(witness, rect(50, 4, 12, 5))
        .expect("float witness");
    session
        .select_pane_in_window(0, 0)
        .expect("select the tile");
    assert_eq!(geometry(&session, 0, 0), PaneGeometry::new(0, 0, 80, 24));
    assert_eq!(
        geometry(&session, 0, source),
        PaneGeometry::new(3, 2, 30, 8)
    );
    assert_eq!(
        geometry(&session, 0, witness),
        PaneGeometry::new(50, 4, 12, 5)
    );
    (session, source, witness)
}

fn join(direction: SplitDirection, detached: bool, before: bool) -> PaneJoinOptions {
    PaneJoinOptions::new(direction, detached, before, false, None)
}

// `move-pane -h` is a left/right share, which rmux spells `SplitDirection::Vertical`
// once `join_pane_internal_direction` has inverted the request flag.
const SIDE_BY_SIDE: SplitDirection = SplitDirection::Vertical;
const STACKED: SplitDirection = SplitDirection::Horizontal;

#[test]
fn floating_source_shares_the_tiled_target_like_the_default_horizontal_receipt() {
    let (mut session, source, witness) = oracle_fixture();
    let source_id = session.window_at(0).unwrap().pane(source).unwrap().id();
    let witness_id = session.window_at(0).unwrap().pane(witness).unwrap().id();

    session
        .join_pane(
            SessionPaneTarget::new(0, source),
            SessionPaneTarget::new(0, 0),
            join(SIDE_BY_SIDE, false, false),
        )
        .expect("a floating source shares a tiled target");

    // Receipt default-horizontal: %0 -> 40x24 at (0,0), %1 -> 39x24 at (41,0),
    // %1 selected, %2 untouched, indices 0/1/2 preserved.
    assert_eq!(geometry(&session, 0, 0), PaneGeometry::new(0, 0, 40, 24));
    assert_eq!(geometry(&session, 0, 1), PaneGeometry::new(41, 0, 39, 24));
    assert_eq!(geometry(&session, 0, 2), PaneGeometry::new(50, 4, 12, 5));
    assert_eq!(
        pane_ids(&session, 0),
        vec![
            (0, session.window_at(0).unwrap().pane(0).unwrap().id()),
            (1, source_id),
            (2, witness_id)
        ]
    );
    assert_eq!(stack_ids(&session, 0), vec![witness_id]);
    assert_eq!(session.window_at(0).unwrap().active_pane_index(), 1);
    assert!(!session.window_at(0).unwrap().is_zoomed());
}

#[test]
fn detached_before_stacks_the_source_first_and_keeps_the_target_selected() {
    let (mut session, source, witness) = oracle_fixture();
    let source_id = session.window_at(0).unwrap().pane(source).unwrap().id();
    let witness_id = session.window_at(0).unwrap().pane(witness).unwrap().id();
    let tile_id = session.window_at(0).unwrap().pane(0).unwrap().id();

    session
        .join_pane(
            SessionPaneTarget::new(0, source),
            SessionPaneTarget::new(0, 0),
            join(STACKED, true, true),
        )
        .expect("detached before-share succeeds");

    // Receipt detached-before: the source takes the first 80x12 band, the tile
    // keeps 80x11 at (0,13), the tile stays selected and the witness is untouched.
    let window = session.window_at(0).unwrap();
    let source_index = window
        .panes()
        .iter()
        .find(|pane| pane.id() == source_id)
        .expect("moved source survives")
        .index();
    let tile_index = window
        .panes()
        .iter()
        .find(|pane| pane.id() == tile_id)
        .expect("tile survives")
        .index();
    assert_eq!(
        geometry(&session, 0, source_index),
        PaneGeometry::new(0, 0, 80, 12)
    );
    assert_eq!(
        geometry(&session, 0, tile_index),
        PaneGeometry::new(0, 13, 80, 11)
    );
    assert_eq!(
        session.window_at(0).unwrap().active_pane_index(),
        tile_index,
        "-d keeps the previously active pane selected"
    );
    assert_eq!(stack_ids(&session, 0), vec![witness_id]);
    assert_eq!(
        geometry(
            &session,
            0,
            session
                .window_at(0)
                .unwrap()
                .panes()
                .iter()
                .find(|pane| pane.id() == witness_id)
                .unwrap()
                .index()
        ),
        PaneGeometry::new(50, 4, 12, 5),
        "an unchanged witness float keeps its rectangle"
    );
    // Acquired receipt `detached-before`: tmux renumbers the moved pane to the
    // index right after its destination whatever `-b` says, because `-b` only
    // picks the side of the split. The tile therefore keeps index 0 and the
    // moved float takes index 1.
    assert_eq!((tile_index, source_index), (0, 1));
}

#[test]
fn no_direction_flag_stacks_the_share() {
    let (mut session, source, _) = oracle_fixture();
    session
        .join_pane(
            SessionPaneTarget::new(0, source),
            SessionPaneTarget::new(0, 0),
            join(STACKED, false, false),
        )
        .expect("stacked share succeeds");
    assert_eq!(geometry(&session, 0, 0), PaneGeometry::new(0, 0, 80, 12));
    assert_eq!(geometry(&session, 0, 1), PaneGeometry::new(0, 13, 80, 11));
}

#[test]
fn full_size_floating_source_shares_the_window_root() {
    // Linux receipt full-size-two-tiles: the moved float takes the whole right
    // half of the window, not half of the named target's cell.
    let (mut session, source, witness) = oracle_fixture();
    let source_id = session.window_at(0).unwrap().pane(source).unwrap().id();
    let witness_id = session.window_at(0).unwrap().pane(witness).unwrap().id();
    let tile_id = session.window_at(0).unwrap().pane(0).unwrap().id();
    // A second tile makes `-f` observable: a full-size share must ignore the
    // named target's cell and split the window root instead.
    let neighbour = session
        .split_pane_in_window_with_direction(0, 0, STACKED)
        .expect("second tile");
    let neighbour_id = session.window_at(0).unwrap().pane(neighbour).unwrap().id();
    assert_eq!(
        geometry(&session, 0, index_of(&session, 0, tile_id)),
        PaneGeometry::new(0, 0, 80, 12)
    );

    session
        .join_pane(
            SessionPaneTarget::new(0, index_of(&session, 0, source_id)),
            SessionPaneTarget::new(0, index_of(&session, 0, neighbour_id)),
            PaneJoinOptions::new(SIDE_BY_SIDE, false, false, true, None),
        )
        .expect("full-size share succeeds");

    assert_eq!(
        geometry(&session, 0, index_of(&session, 0, source_id)),
        PaneGeometry::new(41, 0, 39, 24),
        "-f shares the window root, not the target cell"
    );
    assert_eq!(
        geometry(&session, 0, index_of(&session, 0, tile_id)),
        PaneGeometry::new(0, 0, 40, 12)
    );
    assert_eq!(
        geometry(&session, 0, index_of(&session, 0, neighbour_id)),
        PaneGeometry::new(0, 13, 40, 11)
    );
    assert_eq!(
        geometry(&session, 0, index_of(&session, 0, witness_id)),
        PaneGeometry::new(50, 4, 12, 5)
    );
    assert_eq!(stack_ids(&session, 0), vec![witness_id]);
}

#[test]
fn size_flags_size_the_moved_floating_pane() {
    // Linux receipts size-absolute and size-percentage: `-l 20` and `-l 30%`
    // both size the moved pane against the target's axis, never the float rect.
    for (size, expected_cols) in [
        (PaneSplitSize::Absolute(20), 20),
        (PaneSplitSize::Percentage(30), 24),
    ] {
        let (mut session, source, _) = oracle_fixture();
        session
            .join_pane(
                SessionPaneTarget::new(0, source),
                SessionPaneTarget::new(0, 0),
                PaneJoinOptions::new(SIDE_BY_SIDE, false, false, false, Some(size)),
            )
            .expect("sized share succeeds");
        assert_eq!(geometry(&session, 0, 1).cols(), expected_cols);
        assert_eq!(
            geometry(&session, 0, 0).cols(),
            80 - 1 - expected_cols,
            "the target keeps the rest of the axis"
        );
    }
}

#[test]
fn a_floating_source_that_is_zoomed_shares_the_saved_tiling() {
    // Receipt zoom-conversion: zoom drops to 0, the share uses the saved layout
    // and the source stays active with the same identity.
    let (mut session, source, witness) = oracle_fixture();
    session
        .window_layout_context_mut(0)
        .unwrap()
        .toggle_zoom(source)
        .unwrap();
    assert!(session.window_at(0).unwrap().is_zoomed());
    let witness_id = session.window_at(0).unwrap().pane(witness).unwrap().id();

    session
        .join_pane(
            SessionPaneTarget::new(0, source),
            SessionPaneTarget::new(0, 0),
            join(SIDE_BY_SIDE, false, false),
        )
        .expect("a zoomed floating source shares the tile");

    assert!(!session.window_at(0).unwrap().is_zoomed());
    assert_eq!(geometry(&session, 0, 0), PaneGeometry::new(0, 0, 40, 24));
    assert_eq!(geometry(&session, 0, 1), PaneGeometry::new(41, 0, 39, 24));
    assert_eq!(stack_ids(&session, 0), vec![witness_id]);
}

#[test]
fn a_zoom_held_by_another_pane_is_dropped_and_the_saved_cell_is_shared() {
    // Linux receipt zoom-other-pane: the share happens inside the target's
    // canonical 80x12 cell, not inside the zoomed full-window projection, and
    // the pane that held the zoom returns to its saved band.
    let (mut session, source, witness) = oracle_fixture();
    let source_id = session.window_at(0).unwrap().pane(source).unwrap().id();
    let witness_id = session.window_at(0).unwrap().pane(witness).unwrap().id();
    let tile_id = session.window_at(0).unwrap().pane(0).unwrap().id();
    let neighbour = session
        .split_pane_in_window_with_direction(0, 0, STACKED)
        .expect("second tile");
    let neighbour_id = session.window_at(0).unwrap().pane(neighbour).unwrap().id();
    session
        .window_layout_context_mut(0)
        .unwrap()
        .toggle_zoom(neighbour)
        .unwrap();
    assert!(session.window_at(0).unwrap().is_zoomed());

    session
        .join_pane(
            SessionPaneTarget::new(0, index_of(&session, 0, source_id)),
            SessionPaneTarget::new(0, index_of(&session, 0, tile_id)),
            join(SIDE_BY_SIDE, false, false),
        )
        .expect("share succeeds while another pane is zoomed");

    assert!(!session.window_at(0).unwrap().is_zoomed());
    assert_eq!(
        geometry(&session, 0, index_of(&session, 0, tile_id)),
        PaneGeometry::new(0, 0, 40, 12)
    );
    assert_eq!(
        geometry(&session, 0, index_of(&session, 0, source_id)),
        PaneGeometry::new(41, 0, 39, 12)
    );
    assert_eq!(
        geometry(&session, 0, index_of(&session, 0, neighbour_id)),
        PaneGeometry::new(0, 13, 80, 11)
    );
    assert_eq!(
        geometry(&session, 0, index_of(&session, 0, witness_id)),
        PaneGeometry::new(50, 4, 12, 5)
    );
    assert_eq!(stack_ids(&session, 0), vec![witness_id]);
    assert_eq!(
        session.window_at(0).unwrap().active_pane_index(),
        index_of(&session, 0, source_id),
        "the moved pane takes the selection away from the zoomed pane"
    );
}

#[test]
fn floating_source_crosses_windows_and_keeps_its_identity() {
    // Linux receipt cross-window-default.
    for detached in [false, true] {
        let (mut session, source, witness) = oracle_fixture();
        let (far, _) = session
            .create_window(TerminalSize::new(80, 24))
            .expect("second window");
        let source_id = session.window_at(0).unwrap().pane(source).unwrap().id();
        let witness_id = session.window_at(0).unwrap().pane(witness).unwrap().id();
        let far_id = session.window_at(far).unwrap().pane(0).unwrap().id();
        session
            .select_window(0)
            .expect("start on the source window");

        session
            .join_pane(
                SessionPaneTarget::new(0, source),
                SessionPaneTarget::new(far, 0),
                join(SIDE_BY_SIDE, detached, false),
            )
            .expect("a floating source crosses windows");

        assert_eq!(
            pane_ids(&session, far),
            vec![(0, far_id), (1, source_id)],
            "the moved pane follows the destination order"
        );
        assert_eq!(
            geometry(&session, far, 0),
            PaneGeometry::new(0, 0, 40, 24),
            "detached={detached}"
        );
        assert_eq!(geometry(&session, far, 1), PaneGeometry::new(41, 0, 39, 24));
        assert!(stack_ids(&session, far).is_empty());
        // The source window keeps its tile, renumbers the surviving witness and
        // leaves its rectangle alone.
        assert_eq!(geometry(&session, 0, 0), PaneGeometry::new(0, 0, 80, 24));
        assert_eq!(geometry(&session, 0, 1), PaneGeometry::new(50, 4, 12, 5));
        assert_eq!(stack_ids(&session, 0), vec![witness_id]);
        assert_eq!(
            session.active_window_index(),
            if detached { 0 } else { far },
            "-d keeps the current window"
        );
        assert_eq!(
            session.window_at(far).unwrap().active_pane_index(),
            if detached { 0 } else { 1 }
        );
    }
}

#[test]
fn floating_source_crosses_sessions_and_keeps_its_identity() {
    // Linux receipt cross-session-default.
    let (mut source_session, source, witness) = oracle_fixture();
    let mut destination = Session::new_with_initial_window(
        session_name("other"),
        TerminalSize::new(80, 24),
        0,
        PaneId::new(100),
        WindowId::new(100),
    );
    let source_id = source_session
        .window_at(0)
        .unwrap()
        .pane(source)
        .unwrap()
        .id();
    let witness_id = source_session
        .window_at(0)
        .unwrap()
        .pane(witness)
        .unwrap()
        .id();

    destination
        .join_pane_from_session(
            SessionPaneTarget::new(0, 0),
            &mut source_session,
            SessionPaneTarget::new(0, source),
            join(SIDE_BY_SIDE, false, false),
        )
        .expect("a floating source crosses sessions");

    assert_eq!(
        pane_ids(&destination, 0),
        vec![(0, PaneId::new(100)), (1, source_id)]
    );
    assert_eq!(
        geometry(&destination, 0, 0),
        PaneGeometry::new(0, 0, 40, 24)
    );
    assert_eq!(
        geometry(&destination, 0, 1),
        PaneGeometry::new(41, 0, 39, 24)
    );
    assert!(stack_ids(&destination, 0).is_empty());
    assert_eq!(stack_ids(&source_session, 0), vec![witness_id]);
    assert_eq!(
        geometry(&source_session, 0, 0),
        PaneGeometry::new(0, 0, 80, 24)
    );
}

#[test]
fn the_last_floating_pane_leaves_and_tears_down_its_source_window() {
    // No tmux cell can produce this: tmux cannot float a window's last pane.
    // rmux can, so the transfer must reach the acquired empty-window teardown
    // without killing the moved runtime identity.
    let mut session = Session::new(session_name("qual"), TerminalSize::new(80, 24));
    let (far, far_pane_id) = session
        .create_window(TerminalSize::new(80, 24))
        .expect("second window");
    session
        .window_at_mut(0)
        .unwrap()
        .float_pane(0, rect(3, 2, 30, 8))
        .expect("float the only pane");
    let source_id = session.window_at(0).unwrap().pane(0).unwrap().id();
    assert!(session.window_at(0).unwrap().layout_dump().is_empty());

    session
        .join_pane(
            SessionPaneTarget::new(0, 0),
            SessionPaneTarget::new(far, 0),
            join(SIDE_BY_SIDE, false, false),
        )
        .expect("the last floating pane may leave its window");

    assert!(
        session.window_at(0).is_none(),
        "the emptied source window is torn down"
    );
    assert_eq!(
        pane_ids(&session, far),
        vec![(0, far_pane_id), (1, source_id)]
    );
    assert!(stack_ids(&session, far).is_empty());
    assert_eq!(session.active_window_index(), far);
}

#[test]
fn a_floating_target_stays_refused_without_touching_state_or_zoom() {
    for (detached, before, full, zoom) in [
        (false, false, false, false),
        (true, true, false, true),
        (false, false, true, true),
    ] {
        let (mut session, source, witness) = oracle_fixture();
        if zoom {
            session
                .window_layout_context_mut(0)
                .unwrap()
                .toggle_zoom(source)
                .unwrap();
        }
        let before_state = session.clone();
        for (from, to) in [(source, witness), (0, witness)] {
            let error = session
                .join_pane(
                    SessionPaneTarget::new(0, from),
                    SessionPaneTarget::new(0, to),
                    PaneJoinOptions::new(SIDE_BY_SIDE, detached, before, full, None),
                )
                .expect_err("a floating target is refused");
            assert!(
                error.to_string().contains("floating placement"),
                "unexpected refusal: {error}"
            );
            assert_eq!(
                session, before_state,
                "a refusal must not change the window, its stack or its zoom"
            );
            assert_eq!(session.window_at(0).unwrap().is_zoomed(), zoom);
        }
    }
}

#[test]
fn a_share_with_no_room_refuses_atomically_and_keeps_the_zoom() {
    // Receipt refuse-insufficient-space: exit 1 with no state change. rmux keeps
    // the zoom that tmux drops; that corrective divergence is already decided.
    let mut session = Session::new(session_name("qual"), TerminalSize::new(80, 2));
    let source = session.split_pane_in_window(0, 0).expect("source split");
    session
        .window_at_mut(0)
        .unwrap()
        .float_pane(source, rect(3, 2, 30, 8))
        .expect("float the source");
    session.select_pane_in_window(0, source).unwrap();
    session
        .window_layout_context_mut(0)
        .unwrap()
        .toggle_zoom(source)
        .unwrap();
    assert!(session.window_at(0).unwrap().is_zoomed());
    let before_state = session.clone();

    let error = session
        .join_pane(
            SessionPaneTarget::new(0, source),
            SessionPaneTarget::new(0, 0),
            join(STACKED, false, false),
        )
        .expect_err("two rows cannot be shared");

    assert!(
        error.to_string().contains("cannot split target pane"),
        "unexpected refusal: {error}"
    );
    assert_eq!(session, before_state);
    assert!(session.window_at(0).unwrap().is_zoomed());
}

#[test]
fn a_self_target_refuses_before_any_effect() {
    let (mut session, source, _) = oracle_fixture();
    let before_state = session.clone();
    let error = session
        .join_pane(
            SessionPaneTarget::new(0, source),
            SessionPaneTarget::new(0, source),
            join(SIDE_BY_SIDE, false, false),
        )
        .expect_err("source and target must differ");
    assert!(error
        .to_string()
        .contains("source and target panes must be different"));
    assert_eq!(session, before_state);
}
