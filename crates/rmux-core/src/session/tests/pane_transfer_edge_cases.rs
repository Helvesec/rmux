use super::*;

#[test]
fn join_pane_same_pane_same_window_rejects() {
    let mut session = Session::new(
        session_name("alpha"),
        TerminalSize {
            cols: 120,
            rows: 40,
        },
    );
    session.split_active_pane().expect("split succeeds");

    let error = session
        .join_pane(
            SessionPaneTarget::new(0, 0),
            SessionPaneTarget::new(0, 0),
            PaneJoinOptions::new(SplitDirection::Vertical, false, false, false, None),
        )
        .expect_err("joining a pane to itself should fail");

    assert!(
        error.to_string().contains("must be different"),
        "expected source==target error, got: {error}"
    );
}

#[test]
fn break_pane_single_pane_window_moves_the_window() {
    let mut session = Session::new(session_name("alpha"), TerminalSize { cols: 80, rows: 24 });
    session
        .insert_window_with_initial_pane(1, TerminalSize { cols: 80, rows: 24 })
        .expect("insert window 1 succeeds");
    let pane_id = session.pane_id_in_window(0, 0).expect("pane 0 id exists");

    let dest = session
        .break_pane(
            SessionPaneTarget::new(0, 0),
            BreakPaneOptions::new(Some(2), None, false, false, false),
        )
        .expect("break-pane single-pane window uses move");

    assert_eq!(dest, 2);
    assert!(session.window_at(0).is_none());
    assert!(session.window_at(2).is_some());
    assert_eq!(session.pane_id_in_window(2, 0), Some(pane_id));
}

#[test]
fn break_pane_single_pane_window_renumbers_the_destination_window_to_zero() {
    let mut session = Session::new(session_name("alpha"), TerminalSize { cols: 80, rows: 24 });
    session.split_active_pane().expect("split succeeds");
    session
        .insert_window_with_initial_pane(1, TerminalSize { cols: 80, rows: 24 })
        .expect("insert window 1 succeeds");
    let pane_id = session.pane_id_in_window(0, 1).expect("pane 1 id exists");
    session
        .kill_pane_in_window(0, 0)
        .expect("killing pane 0 leaves pane 1 behind");

    let dest = session
        .break_pane(
            SessionPaneTarget::new(0, 0),
            BreakPaneOptions::new(Some(2), None, true, false, false),
        )
        .expect("break-pane single-pane window uses move");

    assert_eq!(dest, 2);
    assert_eq!(session.pane_id_in_window(2, 0), Some(pane_id));
    assert_eq!(
        session
            .window_at(2)
            .expect("window 2 exists")
            .panes()
            .iter()
            .map(|pane| pane.index())
            .collect::<Vec<_>>(),
        vec![0]
    );
}

#[test]
fn break_pane_to_other_session_renumbers_the_detached_window_to_pane_zero() {
    let mut source = Session::new(
        session_name("alpha"),
        TerminalSize {
            cols: 120,
            rows: 40,
        },
    );
    let mut destination = Session::new(
        session_name("beta"),
        TerminalSize {
            cols: 120,
            rows: 40,
        },
    );
    source.split_active_pane().expect("first split succeeds");
    source.split_pane(0).expect("second split succeeds");
    let moved_pane_id = source.pane_id_in_window(0, 2).expect("pane id exists");

    let destination_index = source
        .break_pane_to_session(
            SessionPaneTarget::new(0, 2),
            &mut destination,
            BreakPaneOptions::new(Some(5), None, true, false, false),
        )
        .expect("cross-session break succeeds");

    assert_eq!(destination_index, 5);
    assert_eq!(destination.pane_id_in_window(5, 0), Some(moved_pane_id));
    assert_eq!(
        destination
            .window_at(5)
            .expect("window 5 exists")
            .panes()
            .iter()
            .map(|pane| pane.index())
            .collect::<Vec<_>>(),
        vec![0]
    );
}

#[test]
fn break_pane_to_other_session_moves_last_source_window() {
    let mut source = Session::new(
        session_name("alpha"),
        TerminalSize {
            cols: 120,
            rows: 40,
        },
    );
    let mut destination = Session::new(
        session_name("hidden"),
        TerminalSize {
            cols: 120,
            rows: 40,
        },
    );
    source.split_active_pane().expect("split succeeds");
    source
        .kill_pane_in_window(0, 0)
        .expect("killing pane 0 leaves a distinct pane id behind");
    let moved_pane_id = source.pane_id_in_window(0, 0).expect("pane id exists");

    let destination_index = source
        .break_pane_to_session(
            SessionPaneTarget::new(0, 0),
            &mut destination,
            BreakPaneOptions::new(None, None, true, false, false),
        )
        .expect("cross-session break of last source window succeeds");

    assert_eq!(destination_index, 1);
    assert!(source.windows().is_empty());
    assert_eq!(destination.pane_id_in_window(1, 0), Some(moved_pane_id));
}

#[test]
fn join_pane_from_other_session_moves_last_source_window() {
    let mut source = Session::new(
        session_name("alpha"),
        TerminalSize {
            cols: 120,
            rows: 40,
        },
    );
    let mut destination = Session::new(
        session_name("hidden"),
        TerminalSize {
            cols: 120,
            rows: 40,
        },
    );
    source.split_active_pane().expect("split succeeds");
    source
        .kill_pane_in_window(0, 0)
        .expect("killing pane 0 leaves a distinct pane id behind");
    let moved_pane_id = source.pane_id_in_window(0, 0).expect("pane id exists");

    destination
        .join_pane_from_session(
            SessionPaneTarget::new(0, 0),
            &mut source,
            SessionPaneTarget::new(0, 0),
            PaneJoinOptions::new(SplitDirection::Vertical, true, false, false, None),
        )
        .expect("cross-session join of last source window succeeds");

    assert!(source.windows().is_empty());
    assert_eq!(
        destination
            .window_at(0)
            .expect("destination window exists")
            .pane_count(),
        2
    );
    assert_eq!(destination.pane_id_in_window(0, 1), Some(moved_pane_id));
}

#[test]
fn resize_pane_in_window_targets_the_requested_pane_in_custom_layouts() {
    let mut session = Session::new(
        session_name("alpha"),
        TerminalSize {
            cols: 100,
            rows: 40,
        },
    );
    session.split_active_pane().expect("first split succeeds");
    session.split_pane(0).expect("second split succeeds");
    session
        .select_custom_layout_in_window(
            0,
            &layout_string("100x40,0,0{60x40,0,0,0,39x40,61,0[39x19,61,0,2,39x20,61,20,1]}"),
        )
        .expect("custom layout applies");

    session
        .resize_pane_in_window(0, 2, ResizePaneAdjustment::AbsoluteWidth { columns: 34 })
        .expect("pane resize succeeds");

    assert_eq!(
        session.window().layout_dump(),
        layout_string("100x40,0,0{65x40,0,0,0,34x40,66,0[34x19,66,0,2,34x20,66,20,1]}")
    );
    assert_eq!(
        session.window().pane(0).expect("pane 0 exists").geometry(),
        PaneGeometry::new(0, 0, 65, 40)
    );
    assert_eq!(
        session.window().pane(1).expect("pane 1 exists").geometry(),
        PaneGeometry::new(66, 0, 34, 19)
    );
    assert_eq!(
        session.window().pane(2).expect("pane 2 exists").geometry(),
        PaneGeometry::new(66, 20, 34, 20)
    );
}

#[test]
fn swap_panes_self_swap_is_a_no_op() {
    let mut session = Session::new(
        session_name("alpha"),
        TerminalSize {
            cols: 120,
            rows: 40,
        },
    );
    session.split_active_pane().expect("split succeeds");
    let active_before = session.active_pane_index();

    session
        .swap_panes(
            SessionPaneTarget::new(0, 0),
            SessionPaneTarget::new(0, 0),
            PaneSwapOptions::new(false, false),
        )
        .expect("self-swap succeeds as no-op");

    assert_eq!(session.active_pane_index(), active_before);
}

#[test]
fn join_pane_cross_window_removes_empty_source_window() {
    let mut session = Session::new(session_name("alpha"), TerminalSize { cols: 80, rows: 24 });
    session.split_active_pane().expect("split succeeds");
    session
        .insert_window_with_initial_pane(1, TerminalSize { cols: 80, rows: 24 })
        .expect("insert window 1 succeeds");

    // Join pane 0:1 from window 0 into window 1. Window 0 keeps pane 0.
    session
        .join_pane(
            SessionPaneTarget::new(0, 1),
            SessionPaneTarget::new(1, 0),
            PaneJoinOptions::new(SplitDirection::Vertical, false, false, false, None),
        )
        .expect("joining pane from window 0 into window 1 succeeds");

    assert!(session.window_at(0).is_some());
    assert_eq!(
        session.window_at(1).expect("window 1 exists").pane_count(),
        2
    );
}

#[test]
fn break_pane_after_flag_shifts_windows_correctly() {
    let mut session = Session::new(session_name("alpha"), TerminalSize { cols: 80, rows: 24 });
    session.split_active_pane().expect("split succeeds");
    session
        .insert_window_with_initial_pane(1, TerminalSize { cols: 80, rows: 24 })
        .expect("insert window 1 succeeds");

    let dest = session
        .break_pane(
            SessionPaneTarget::new(0, 1),
            BreakPaneOptions::new(None, None, true, true, false),
        )
        .expect("break-pane -a from active window succeeds");

    // After the active window (0), so destination is 1. Existing window 1 shifts to 2.
    assert_eq!(dest, 1);
    assert!(session.window_at(0).is_some());
    assert!(session.window_at(1).is_some());
    assert!(session.window_at(2).is_some());
}

#[test]
fn break_pane_before_flag_shifts_windows_correctly() {
    let mut session = Session::new(session_name("alpha"), TerminalSize { cols: 80, rows: 24 });
    session.split_active_pane().expect("split succeeds");
    session
        .insert_window_with_initial_pane(1, TerminalSize { cols: 80, rows: 24 })
        .expect("insert window 1 succeeds");

    let dest = session
        .break_pane(
            SessionPaneTarget::new(1, 0),
            BreakPaneOptions::new(None, None, true, false, true),
        )
        .expect("break-pane -b from active window succeeds");

    // Break pane 1:0 before the active window (0). Active window shifts up.
    assert_eq!(dest, 0);
    assert!(session.window_at(0).is_some());
    assert!(session.window_at(1).is_some());
}
