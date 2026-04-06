use super::*;

#[test]
fn new_session_starts_with_pane_zero_active() {
    let session = Session::new(
        session_name("alpha"),
        TerminalSize {
            cols: 120,
            rows: 40,
        },
    );

    assert_eq!(session.active_pane_index(), 0);
    assert_eq!(session.active_pane().expect("pane 0 exists").index(), 0);
}

#[test]
fn initial_pane_id_is_always_zero() {
    let session = Session::new(
        session_name("alpha"),
        TerminalSize {
            cols: 120,
            rows: 40,
        },
    );

    assert_eq!(session.active_pane_id(), Some(PaneId::new(0)));
    assert_eq!(session.pane_id(0), Some(PaneId::new(0)));
}

#[test]
fn split_active_pane_returns_new_index_and_selects_the_new_pane() {
    let mut session = Session::new(
        session_name("alpha"),
        TerminalSize {
            cols: 120,
            rows: 40,
        },
    );

    let new_index = session.split_active_pane().expect("split succeeds");

    assert_eq!(new_index, 1);
    assert_eq!(session.active_pane_index(), 1);
    assert_eq!(
        session
            .window()
            .panes()
            .iter()
            .map(|pane| pane.index())
            .collect::<Vec<_>>(),
        vec![0, 1]
    );
}

#[test]
fn split_specific_pane_inserts_after_the_target() {
    let mut session = Session::new(
        session_name("alpha"),
        TerminalSize {
            cols: 120,
            rows: 40,
        },
    );
    session.split_active_pane().expect("first split succeeds");

    let new_index = session.split_pane(0).expect("second split succeeds");

    assert_eq!(new_index, 1);
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
fn split_pane_rejects_tmux_no_space_condition_before_mutating() {
    let mut session = Session::new(session_name("alpha"), TerminalSize { cols: 20, rows: 5 });

    session
        .split_pane_in_window_with_direction(0, 0, SplitDirection::Horizontal)
        .expect("first top-bottom split fits");
    let before = session.window().panes().to_vec();

    let error = session
        .split_pane_in_window_with_direction(0, 1, SplitDirection::Horizontal)
        .expect_err("second top-bottom split does not fit");

    assert_eq!(
        error,
        RmuxError::Message("no space for new pane".to_owned())
    );
    assert_eq!(session.window().panes(), before.as_slice());
}

#[test]
fn select_pane_updates_the_session_active_pane() {
    let mut session = Session::new(
        session_name("alpha"),
        TerminalSize {
            cols: 120,
            rows: 40,
        },
    );
    session.split_active_pane().expect("split succeeds");

    session.select_pane(1).expect("pane exists");

    assert_eq!(session.active_pane_index(), 1);
    assert_eq!(session.active_pane().expect("pane 1 exists").index(), 1);
}

#[test]
fn select_pane_rejects_unknown_pane_indices() {
    let mut session = Session::new(
        session_name("alpha"),
        TerminalSize {
            cols: 120,
            rows: 40,
        },
    );

    let error = session.select_pane(9).expect_err("pane does not exist");

    assert_eq!(
        error,
        RmuxError::invalid_target("alpha:0.9", "pane index does not exist in session")
    );
}

#[test]
fn split_pane_rejects_nonexistent_pane_index() {
    let mut session = Session::new(
        session_name("alpha"),
        TerminalSize {
            cols: 120,
            rows: 40,
        },
    );

    let error = session.split_pane(9).expect_err("pane does not exist");

    assert_eq!(
        error,
        RmuxError::invalid_target("alpha:0.9", "pane index does not exist in session")
    );
}

#[test]
fn split_pane_in_window_rejects_nonexistent_pane_index_in_that_window() {
    let mut session = Session::new(
        session_name("alpha"),
        TerminalSize {
            cols: 120,
            rows: 40,
        },
    );
    session
        .insert_window_with_initial_pane(5, TerminalSize { cols: 90, rows: 30 })
        .expect("window 5 insert succeeds");

    let error = session
        .split_pane_in_window(5, 9)
        .expect_err("pane does not exist");

    assert_eq!(
        error,
        RmuxError::invalid_target("alpha:5.9", "pane index does not exist in session")
    );
}

#[test]
fn split_pane_in_window_rejects_nonexistent_window_index() {
    let mut session = Session::new(
        session_name("alpha"),
        TerminalSize {
            cols: 120,
            rows: 40,
        },
    );

    let error = session
        .split_pane_in_window(5, 0)
        .expect_err("window does not exist");

    assert_eq!(
        error,
        RmuxError::invalid_target("alpha:5", "window index does not exist in session")
    );
}

#[test]
fn horizontal_split_uses_top_bottom_geometry_in_the_addressed_window() {
    let mut session = Session::new(
        session_name("alpha"),
        TerminalSize {
            cols: 100,
            rows: 50,
        },
    );

    let new_index = session
        .split_pane_in_window_with_direction(0, 0, SplitDirection::Horizontal)
        .expect("split succeeds");

    assert_eq!(new_index, 1);
    assert_eq!(session.window().layout(), LayoutName::MainHorizontal);
    assert_eq!(
        session.window().pane(0).expect("pane 0 exists").geometry(),
        PaneGeometry::new(0, 0, 100, 25)
    );
    assert_eq!(
        session.window().pane(1).expect("pane 1 exists").geometry(),
        PaneGeometry::new(0, 26, 100, 24)
    );
}

#[test]
fn resize_pane_recalculates_all_geometry() {
    let mut session = Session::new(
        session_name("alpha"),
        TerminalSize {
            cols: 200,
            rows: 50,
        },
    );
    session.split_active_pane().expect("first split succeeds");
    session.split_pane(0).expect("second split succeeds");

    session
        .resize_pane(2, ResizePaneAdjustment::AbsoluteWidth { columns: 34 })
        .expect("pane exists");

    assert_eq!(
        session.window().pane(0).expect("pane 0 exists").geometry(),
        PaneGeometry::new(0, 0, 50, 50)
    );
    assert_eq!(
        session.window().pane(1).expect("pane 1 exists").geometry(),
        PaneGeometry::new(51, 0, 114, 50)
    );
    assert_eq!(
        session.window().pane(2).expect("pane 2 exists").geometry(),
        PaneGeometry::new(166, 0, 34, 50)
    );
}

#[test]
fn resize_pane_rejects_nonexistent_pane_index() {
    let mut session = Session::new(
        session_name("alpha"),
        TerminalSize {
            cols: 120,
            rows: 40,
        },
    );

    let error = session
        .resize_pane(9, ResizePaneAdjustment::AbsoluteWidth { columns: 34 })
        .expect_err("pane does not exist");

    assert_eq!(
        error,
        RmuxError::invalid_target("alpha:0.9", "pane index does not exist in session")
    );
}

#[test]
fn directional_resize_moves_the_split_border_like_tmux() {
    let mut left = Session::new(session_name("alpha"), TerminalSize { cols: 80, rows: 24 });
    left.split_active_pane().expect("split succeeds");
    left.select_pane(1).expect("right pane exists");
    left.resize_pane(1, ResizePaneAdjustment::Left { cells: 1 })
        .expect("resize succeeds");
    assert_eq!(
        left.window().pane(0).expect("pane 0 exists").geometry(),
        PaneGeometry::new(0, 0, 39, 24)
    );
    assert_eq!(
        left.window().pane(1).expect("pane 1 exists").geometry(),
        PaneGeometry::new(40, 0, 40, 24)
    );

    let mut right = Session::new(session_name("alpha"), TerminalSize { cols: 80, rows: 24 });
    right.split_active_pane().expect("split succeeds");
    right.select_pane(0).expect("left pane exists");
    right
        .resize_pane(0, ResizePaneAdjustment::Right { cells: 1 })
        .expect("resize succeeds");
    assert_eq!(
        right.window().pane(0).expect("pane 0 exists").geometry(),
        PaneGeometry::new(0, 0, 41, 24)
    );
    assert_eq!(
        right.window().pane(1).expect("pane 1 exists").geometry(),
        PaneGeometry::new(42, 0, 38, 24)
    );
}

#[test]
fn directional_resize_middle_pane_uses_tmux_handle_selection() {
    let mut session = Session::new(
        session_name("alpha"),
        TerminalSize {
            cols: 100,
            rows: 30,
        },
    );
    session.split_active_pane().expect("first split succeeds");
    session.split_pane(1).expect("second split succeeds");
    session.split_pane(2).expect("third split succeeds");
    session.select_pane(2).expect("middle pane exists");

    assert_eq!(
        session.window().pane(2).expect("pane 2 exists").geometry(),
        PaneGeometry::new(76, 0, 12, 30)
    );

    session
        .resize_pane(2, ResizePaneAdjustment::Left { cells: 2 })
        .expect("resize succeeds");

    assert_eq!(
        session.window().pane(1).expect("pane 1 exists").geometry(),
        PaneGeometry::new(51, 0, 24, 30)
    );
    assert_eq!(
        session.window().pane(2).expect("pane 2 exists").geometry(),
        PaneGeometry::new(76, 0, 10, 30)
    );
    assert_eq!(
        session.window().pane(3).expect("pane 3 exists").geometry(),
        PaneGeometry::new(87, 0, 13, 30)
    );
}
