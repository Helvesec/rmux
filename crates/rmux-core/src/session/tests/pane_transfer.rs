use super::*;

#[test]
fn last_pane_selects_the_previous_pane_in_the_addressed_window() {
    let mut session = Session::new(
        session_name("alpha"),
        TerminalSize {
            cols: 120,
            rows: 40,
        },
    );
    session.split_active_pane().expect("split succeeds");
    session.select_pane(1).expect("pane 1 exists");
    session.select_pane(0).expect("pane 0 exists");

    let pane_index = session.last_pane_in_window(0).expect("last pane exists");

    assert_eq!(pane_index, 1);
    assert_eq!(session.active_pane_index(), 1);
    assert_eq!(session.window().last_pane_index(), Some(0));
}

#[test]
fn detached_swap_panes_within_window_preserves_active_and_last_pane_tracking() {
    let mut session = Session::new(
        session_name("alpha"),
        TerminalSize {
            cols: 120,
            rows: 40,
        },
    );
    session.split_active_pane().expect("split succeeds");
    session.select_pane(1).expect("pane 1 exists");
    session.select_pane(0).expect("pane 0 exists");
    let pane_zero_geometry = session.window().pane(0).expect("pane 0 exists").geometry();
    let pane_one_geometry = session.window().pane(1).expect("pane 1 exists").geometry();
    let pane_zero_id = session.pane_id_in_window(0, 0).expect("pane 0 id exists");

    session
        .swap_panes(
            SessionPaneTarget::new(0, 0),
            SessionPaneTarget::new(0, 1),
            PaneSwapOptions::new(true, false),
        )
        .expect("detached same-window swap succeeds");

    assert_eq!(session.active_pane_index(), 1);
    assert_eq!(session.window().last_pane_index(), Some(0));
    assert_eq!(session.pane_id_in_window(0, 1), Some(pane_zero_id));
    assert_eq!(
        session
            .window()
            .panes()
            .iter()
            .map(|pane| pane.index())
            .collect::<Vec<_>>(),
        vec![0, 1]
    );
    assert_eq!(
        session.window().pane(0).expect("pane 0 exists").geometry(),
        pane_zero_geometry
    );
    assert_eq!(
        session.window().pane(1).expect("pane 1 exists").geometry(),
        pane_one_geometry
    );
}

#[test]
fn same_window_swap_renumbers_by_window_order_and_selects_target_identity() {
    let mut session = Session::new(
        session_name("alpha"),
        TerminalSize {
            cols: 100,
            rows: 30,
        },
    );
    session.split_active_pane().expect("first split succeeds");
    session.split_pane(1).expect("right split succeeds");
    session.split_pane(0).expect("left split succeeds");
    session.select_layout(LayoutName::Tiled);
    session
        .rotate_window(0, RotateWindowDirection::Down)
        .expect("rotate succeeds");

    let source_id = session.pane_id_in_window(0, 0).expect("source pane exists");
    let target_id = session.pane_id_in_window(0, 2).expect("target pane exists");

    session
        .swap_panes(
            SessionPaneTarget::new(0, 0),
            SessionPaneTarget::new(0, 2),
            PaneSwapOptions::new(false, false),
        )
        .expect("same-window swap succeeds");

    assert_eq!(
        session
            .window()
            .panes()
            .iter()
            .map(|pane| pane.index())
            .collect::<Vec<_>>(),
        vec![0, 1, 2, 3]
    );
    assert_eq!(session.active_pane_index(), 0);
    assert_eq!(session.pane_id_in_window(0, 0), Some(target_id));
    assert_eq!(session.pane_id_in_window(0, 2), Some(source_id));
}

#[test]
fn detached_join_pane_within_window_tracks_active_like_tmux_when_source_was_active() {
    let mut session = Session::new(
        session_name("alpha"),
        TerminalSize {
            cols: 120,
            rows: 40,
        },
    );
    session.split_active_pane().expect("first split succeeds");
    session.split_pane(0).expect("second split succeeds");
    session.select_pane(1).expect("pane 1 exists");
    let pane_one_id = session.pane_id_in_window(0, 1).expect("pane 1 id exists");

    session
        .join_pane(
            SessionPaneTarget::new(0, 1),
            SessionPaneTarget::new(0, 0),
            PaneJoinOptions::new(SplitDirection::Vertical, true, false, false, None),
        )
        .expect("detached in-window join succeeds");

    assert_eq!(session.active_pane_index(), 2);
    assert_eq!(session.window().last_pane_index(), Some(0));
    assert_eq!(session.pane_id_in_window(0, 1), Some(pane_one_id));
    assert_eq!(
        session
            .window()
            .panes()
            .iter()
            .map(|pane| pane.index())
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
}

#[test]
fn detached_move_pane_within_window_resplits_target_before_removing_source_like_tmux() {
    let mut session = Session::new(session_name("alpha"), TerminalSize { cols: 80, rows: 24 });
    session
        .split_active_pane_with_direction(SplitDirection::Vertical)
        .expect("left-right split succeeds");
    session
        .split_pane_with_direction(1, SplitDirection::Horizontal)
        .expect("right top-bottom split succeeds");
    session
        .break_pane(
            SessionPaneTarget::new(0, 2),
            BreakPaneOptions::new(Some(1), Some("broken".to_owned()), true, false, false),
        )
        .expect("break pane succeeds");
    session
        .join_pane(
            SessionPaneTarget::new(1, 0),
            SessionPaneTarget::new(0, 0),
            PaneJoinOptions::new(SplitDirection::Horizontal, true, false, false, None),
        )
        .expect("join pane succeeds");

    session
        .join_pane(
            SessionPaneTarget::new(0, 1),
            SessionPaneTarget::new(0, 0),
            PaneJoinOptions::new(SplitDirection::Horizontal, true, false, false, None),
        )
        .expect("move-pane-style same-window join succeeds");

    assert_eq!(
        session.window().pane(0).expect("pane 0 exists").geometry(),
        PaneGeometry::new(0, 0, 40, 6)
    );
    assert_eq!(
        session.window().pane(1).expect("pane 1 exists").geometry(),
        PaneGeometry::new(0, 7, 40, 17)
    );
    assert_eq!(
        session.window().pane(2).expect("pane 2 exists").geometry(),
        PaneGeometry::new(41, 0, 39, 24)
    );
}

#[test]
fn detached_move_pane_before_target_keeps_tmux_geometry_and_active_fallback() {
    let mut session = Session::new(
        session_name("alpha"),
        TerminalSize {
            cols: 160,
            rows: 48,
        },
    );
    session
        .split_active_pane_with_direction(SplitDirection::Vertical)
        .expect("left-right split succeeds");
    session
        .split_pane_with_direction(1, SplitDirection::Horizontal)
        .expect("right top-bottom split succeeds");
    session
        .split_pane_with_direction(0, SplitDirection::Horizontal)
        .expect("left top-bottom split succeeds");
    session
        .break_pane(
            SessionPaneTarget::new(0, 2),
            BreakPaneOptions::new(Some(1), Some("broken".to_owned()), true, false, false),
        )
        .expect("break pane succeeds");
    session
        .join_pane(
            SessionPaneTarget::new(1, 0),
            SessionPaneTarget::new(0, 0),
            PaneJoinOptions::new(SplitDirection::Horizontal, true, true, false, None),
        )
        .expect("join-pane -d -b succeeds");
    session
        .join_pane(
            SessionPaneTarget::new(0, 2),
            SessionPaneTarget::new(0, 1),
            PaneJoinOptions::new(SplitDirection::Horizontal, true, true, false, None),
        )
        .expect("move-pane -d -b succeeds");

    let geometries = session
        .window()
        .panes()
        .iter()
        .map(|pane| pane.geometry())
        .collect::<Vec<_>>();
    assert_eq!(
        geometries,
        vec![
            PaneGeometry::new(0, 0, 80, 12),
            PaneGeometry::new(0, 13, 80, 5),
            PaneGeometry::new(0, 19, 80, 29),
            PaneGeometry::new(81, 0, 79, 48),
        ]
    );
    assert_eq!(session.active_pane_index(), 3);
    assert_eq!(session.window().last_pane_index(), Some(2));
}

#[test]
fn swap_panes_across_windows_removes_detached_last_pane_without_promoting_incoming() {
    let mut session = Session::new(
        session_name("alpha"),
        TerminalSize {
            cols: 120,
            rows: 40,
        },
    );
    session
        .split_active_pane()
        .expect("window 0 split succeeds");
    session
        .insert_window_with_initial_pane(
            1,
            TerminalSize {
                cols: 120,
                rows: 40,
            },
        )
        .expect("window 1 insert succeeds");
    session
        .split_pane_in_window(1, 0)
        .expect("window 1 split succeeds");
    session
        .select_pane_in_window(0, 1)
        .expect("window 0 pane 1 exists");
    session
        .select_pane_in_window(0, 0)
        .expect("window 0 pane 0 exists");
    session
        .select_pane_in_window(1, 1)
        .expect("window 1 pane 1 exists");
    session
        .select_pane_in_window(1, 0)
        .expect("window 1 pane 0 exists");

    session
        .swap_panes(
            SessionPaneTarget::new(0, 1),
            SessionPaneTarget::new(1, 1),
            PaneSwapOptions::new(true, false),
        )
        .expect("swap succeeds");

    assert_eq!(
        session
            .window_at(0)
            .expect("window 0 exists")
            .last_pane_index(),
        None
    );
    assert_eq!(
        session
            .window_at(1)
            .expect("window 1 exists")
            .last_pane_index(),
        None
    );
    assert_eq!(
        session
            .window_at(0)
            .expect("window 0 exists")
            .panes()
            .iter()
            .map(|pane| pane.index())
            .collect::<Vec<_>>(),
        vec![0, 1]
    );
    assert_eq!(
        session
            .window_at(1)
            .expect("window 1 exists")
            .panes()
            .iter()
            .map(|pane| pane.index())
            .collect::<Vec<_>>(),
        vec![0, 1]
    );
}

#[test]
fn swap_panes_across_windows_preserves_older_last_when_active_pane_leaves() {
    let mut session = Session::new(
        session_name("alpha"),
        TerminalSize {
            cols: 120,
            rows: 40,
        },
    );
    session
        .split_active_pane()
        .expect("window 0 split succeeds");
    session
        .insert_window_with_initial_pane(
            1,
            TerminalSize {
                cols: 120,
                rows: 40,
            },
        )
        .expect("window 1 insert succeeds");
    session
        .split_pane_in_window(1, 0)
        .expect("window 1 split succeeds");
    session
        .select_pane_in_window(0, 1)
        .expect("window 0 pane 1 exists");
    session
        .select_pane_in_window(1, 1)
        .expect("window 1 pane 1 exists");

    session
        .swap_panes(
            SessionPaneTarget::new(0, 1),
            SessionPaneTarget::new(1, 1),
            PaneSwapOptions::new(true, false),
        )
        .expect("swap succeeds");

    assert_eq!(
        session
            .window_at(0)
            .expect("window 0 exists")
            .last_pane_index(),
        Some(0)
    );
    assert_eq!(
        session
            .window_at(1)
            .expect("window 1 exists")
            .last_pane_index(),
        Some(0)
    );
    assert_eq!(
        session
            .last_pane_in_window(0)
            .expect("window 0 last pane survives"),
        0
    );
    assert_eq!(
        session
            .last_pane_in_window(1)
            .expect("window 1 last pane survives"),
        0
    );
}

#[test]
fn swap_panes_across_windows_allows_incoming_index_collision() {
    let mut session = Session::new(
        session_name("alpha"),
        TerminalSize {
            cols: 120,
            rows: 40,
        },
    );
    session
        .split_active_pane()
        .expect("window 0 split succeeds");
    session
        .insert_window_with_initial_pane(
            1,
            TerminalSize {
                cols: 120,
                rows: 40,
            },
        )
        .expect("window 1 insert succeeds");
    session
        .split_pane_in_window(1, 0)
        .expect("window 1 split succeeds");

    let source_id = session.pane_id_in_window(0, 1).expect("source pane exists");
    let target_id = session.pane_id_in_window(1, 0).expect("target pane exists");

    session
        .swap_panes(
            SessionPaneTarget::new(0, 1),
            SessionPaneTarget::new(1, 0),
            PaneSwapOptions::new(false, false),
        )
        .expect("cross-window swap succeeds despite temporary index collision");

    assert_eq!(session.pane_id_in_window(0, 1), Some(target_id));
    assert_eq!(session.pane_id_in_window(1, 0), Some(source_id));
    assert_eq!(
        session
            .window_at(0)
            .expect("window 0 exists")
            .panes()
            .iter()
            .map(|pane| pane.index())
            .collect::<Vec<_>>(),
        vec![0, 1]
    );
    assert_eq!(
        session
            .window_at(1)
            .expect("window 1 exists")
            .panes()
            .iter()
            .map(|pane| pane.index())
            .collect::<Vec<_>>(),
        vec![0, 1]
    );
}

#[test]
fn break_pane_renumbers_the_detached_window_to_pane_zero() {
    let mut session = Session::new(
        session_name("alpha"),
        TerminalSize {
            cols: 120,
            rows: 40,
        },
    );
    session.split_active_pane().expect("first split succeeds");
    session.split_pane(0).expect("second split succeeds");

    let new_window_index = session
        .break_pane(
            SessionPaneTarget::new(0, 2),
            BreakPaneOptions::new(Some(5), None, true, false, false),
        )
        .expect("break succeeds");
    let next_index = session
        .split_pane_in_window(new_window_index, 0)
        .expect("split in new window succeeds");

    assert_eq!(new_window_index, 5);
    assert_eq!(
        session
            .window_at(5)
            .expect("window 5 exists")
            .panes()
            .iter()
            .map(|pane| pane.index())
            .collect::<Vec<_>>(),
        vec![0, 1]
    );
    assert_eq!(next_index, 1);
}

#[test]
fn break_pane_rejects_missing_source_pane_before_moving_single_pane_window() {
    let mut session = Session::new(
        session_name("alpha"),
        TerminalSize {
            cols: 120,
            rows: 40,
        },
    );

    let error = session
        .break_pane(
            SessionPaneTarget::new(0, 5),
            BreakPaneOptions::new(Some(1), None, true, false, false),
        )
        .expect_err("missing source pane should fail");

    assert!(error
        .to_string()
        .contains("pane index does not exist in session"));
    assert!(session.window_at(0).is_some());
    assert!(session.window_at(1).is_none());
}

#[test]
fn join_pane_before_target_honours_requested_size() {
    let mut session = Session::new(
        session_name("alpha"),
        TerminalSize {
            cols: 100,
            rows: 40,
        },
    );
    session
        .split_active_pane()
        .expect("window 0 split succeeds");
    session
        .insert_window_with_initial_pane(
            1,
            TerminalSize {
                cols: 100,
                rows: 40,
            },
        )
        .expect("window 1 insert succeeds");
    session
        .split_pane_in_window(1, 0)
        .expect("window 1 split succeeds");
    session
        .split_pane_in_window(1, 0)
        .expect("window 1 second split succeeds");

    session
        .join_pane(
            SessionPaneTarget::new(1, 2),
            SessionPaneTarget::new(0, 1),
            PaneJoinOptions::new(
                SplitDirection::Vertical,
                true,
                true,
                false,
                Some(PaneSplitSize::Absolute(10)),
            ),
        )
        .expect("cross-window join succeeds");

    let window = session.window_at(0).expect("window 0 exists");
    assert_eq!(
        window
            .panes()
            .iter()
            .map(|pane| pane.index())
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    assert_eq!(
        window
            .pane(1)
            .expect("joined pane exists")
            .geometry()
            .cols(),
        10
    );
}

#[test]
fn join_pane_within_window_horizontal_size_matches_tmux_target_side() {
    let mut session = Session::new(
        session_name("alpha"),
        TerminalSize {
            cols: 100,
            rows: 24,
        },
    );
    session
        .split_active_pane_with_direction(SplitDirection::Horizontal)
        .expect("split succeeds");

    session
        .join_pane(
            SessionPaneTarget::new(0, 1),
            SessionPaneTarget::new(0, 0),
            PaneJoinOptions::new(
                SplitDirection::Horizontal,
                false,
                false,
                false,
                Some(PaneSplitSize::Absolute(5)),
            ),
        )
        .expect("same-window join succeeds");

    let window = session.window_at(0).expect("window 0 exists");
    let first = window.pane(0).expect("pane 0 exists").geometry();
    let second = window.pane(1).expect("pane 1 exists").geometry();
    assert_eq!(first.rows(), 6);
    assert_eq!(second.rows(), 17);
}

#[test]
fn join_pane_within_window_before_size_matches_tmux_moved_side() {
    let mut session = Session::new(
        session_name("alpha"),
        TerminalSize {
            cols: 100,
            rows: 24,
        },
    );
    session
        .split_active_pane_with_direction(SplitDirection::Horizontal)
        .expect("split succeeds");

    session
        .join_pane(
            SessionPaneTarget::new(0, 1),
            SessionPaneTarget::new(0, 0),
            PaneJoinOptions::new(
                SplitDirection::Horizontal,
                true,
                false,
                false,
                Some(PaneSplitSize::Absolute(5)),
            ),
        )
        .expect("same-window before join succeeds");

    let window = session.window_at(0).expect("window 0 exists");
    let first = window.pane(0).expect("pane 0 exists").geometry();
    let second = window.pane(1).expect("pane 1 exists").geometry();
    assert_eq!(first.rows(), 6);
    assert_eq!(second.rows(), 17);
}

#[test]
fn break_pane_moves_a_single_pane_window_without_recreating_it() {
    let mut session = Session::new(
        session_name("alpha"),
        TerminalSize {
            cols: 120,
            rows: 40,
        },
    );
    session
        .insert_window_with_initial_pane(1, TerminalSize { cols: 90, rows: 30 })
        .expect("window 1 insert succeeds");
    let source_window_id = session.window_at(1).expect("window 1 exists").id();

    let destination_index = session
        .break_pane(
            SessionPaneTarget::new(1, 0),
            BreakPaneOptions::new(Some(0), Some("moved".to_owned()), true, true, false),
        )
        .expect("break succeeds");

    assert_eq!(destination_index, 1);
    let moved_window = session.window_at(1).expect("moved window exists");
    assert_eq!(moved_window.id(), source_window_id);
    assert_eq!(moved_window.name(), Some("moved"));
    assert!(session.window_at(2).is_none());
}

#[test]
fn join_pane_clears_alert_flags_for_a_destroyed_source_window() {
    let mut session = Session::new(
        session_name("alpha"),
        TerminalSize {
            cols: 120,
            rows: 40,
        },
    );
    session
        .insert_window_with_initial_pane(1, TerminalSize { cols: 90, rows: 30 })
        .expect("window 1 insert succeeds");
    let _ = session.add_winlink_alert_flags(1, crate::WINLINK_ACTIVITY);

    session
        .join_pane(
            SessionPaneTarget::new(1, 0),
            SessionPaneTarget::new(0, 0),
            PaneJoinOptions::new(SplitDirection::Vertical, true, false, false, None),
        )
        .expect("join succeeds");

    assert!(session.window_at(1).is_none());
    assert!(session.winlink_alert_flags(1).is_empty());
}
