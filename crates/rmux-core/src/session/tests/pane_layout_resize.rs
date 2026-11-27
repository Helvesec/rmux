use super::*;

#[test]
fn kill_pane_merges_the_removed_leaf_into_the_previous_sibling() {
    let mut session = Session::new(
        session_name("alpha"),
        TerminalSize {
            cols: 120,
            rows: 40,
        },
    );
    session.split_active_pane().expect("first split succeeds");
    session.split_pane(1).expect("second split succeeds");

    let outcome = session.kill_pane(1).expect("kill succeeds");

    assert_eq!(outcome.removed_pane_ids(), &[PaneId::new(1)]);
    assert!(!outcome.window_destroyed());
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
        PaneGeometry::new(0, 0, 90, 40)
    );
    assert_eq!(
        session.window().pane(1).expect("pane 1 exists").geometry(),
        PaneGeometry::new(91, 0, 29, 40)
    );
}

#[test]
fn killing_the_last_pane_in_a_non_last_window_removes_the_window() {
    let mut session = Session::new(
        session_name("alpha"),
        TerminalSize {
            cols: 120,
            rows: 40,
        },
    );
    session
        .insert_window_with_initial_pane(
            3,
            TerminalSize {
                cols: 120,
                rows: 40,
            },
        )
        .expect("window 3 insert succeeds");
    session.select_window(3).expect("window 3 exists");
    session.select_window(0).expect("window 0 exists");

    let outcome = session
        .kill_pane_in_window(3, 0)
        .expect("killing the sole pane should remove the window");

    assert_eq!(outcome.removed_pane_ids(), &[PaneId::new(1)]);
    assert!(outcome.window_destroyed());
    assert_eq!(
        session.windows().keys().copied().collect::<Vec<_>>(),
        vec![0]
    );
    assert_eq!(session.active_window_index(), 0);
    assert_eq!(session.last_window_index(), None);
}

#[test]
fn killing_the_last_pane_in_the_only_window_propagates_the_window_error() {
    let mut session = Session::new(
        session_name("alpha"),
        TerminalSize {
            cols: 120,
            rows: 40,
        },
    );

    let error = session.kill_pane(0).expect_err("kill should fail");

    assert_eq!(
        error,
        RmuxError::Server("cannot kill the only window in session alpha".to_owned())
    );
}

#[test]
fn select_pane_in_window_updates_only_the_addressed_window() {
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
    session
        .split_pane_in_window(5, 0)
        .expect("window 5 split succeeds");

    session
        .select_pane_in_window(5, 1)
        .expect("pane exists in window 5");

    assert_eq!(session.active_window_index(), 0);
    assert_eq!(session.active_pane_index(), 0);
    assert_eq!(
        session
            .window_at(5)
            .expect("window 5 exists")
            .active_pane_index(),
        1
    );
}

#[test]
fn resize_terminal_preserves_existing_layout_proportions() {
    let mut session = Session::new(
        session_name("alpha"),
        TerminalSize {
            cols: 120,
            rows: 40,
        },
    );
    session.split_active_pane().expect("split succeeds");
    session
        .resize_pane(1, ResizePaneAdjustment::AbsoluteWidth { columns: 34 })
        .expect("pane exists");

    session.resize_terminal(TerminalSize {
        cols: 200,
        rows: 50,
    });

    assert_eq!(
        session.window().pane(0).expect("pane 0 exists").geometry(),
        PaneGeometry::new(0, 0, 125, 50)
    );
    assert_eq!(
        session.window().pane(1).expect("pane 1 exists").geometry(),
        PaneGeometry::new(126, 0, 74, 50)
    );
}

#[test]
fn resize_terminal_resizes_all_windows_not_just_the_active_one() {
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
    session
        .split_pane_in_window(5, 0)
        .expect("window 5 split succeeds");

    session.resize_terminal(TerminalSize {
        cols: 200,
        rows: 50,
    });

    assert_eq!(
        session
            .window()
            .pane(0)
            .expect("window 0 pane 0 exists")
            .geometry(),
        PaneGeometry::new(0, 0, 200, 50)
    );
    let window5 = session.window_at(5).expect("window 5 exists");
    assert_eq!(
        window5.pane(0).expect("window 5 pane 0 exists").geometry(),
        PaneGeometry::new(0, 0, 100, 50)
    );
    assert_eq!(
        window5.pane(1).expect("window 5 pane 1 exists").geometry(),
        PaneGeometry::new(101, 0, 99, 50)
    );
}

#[test]
fn select_layout_preserves_supported_layout_and_recalculates() {
    let mut session = Session::new(
        session_name("alpha"),
        TerminalSize {
            cols: 120,
            rows: 40,
        },
    );
    session.split_active_pane().expect("split succeeds");
    session
        .resize_pane(1, ResizePaneAdjustment::AbsoluteWidth { columns: 34 })
        .expect("pane exists");

    session.select_layout(LayoutName::MainVertical);

    assert_eq!(session.window().layout(), LayoutName::MainVertical);
    assert_eq!(
        session.window().pane(0).expect("pane 0 exists").geometry(),
        PaneGeometry::new(0, 0, 80, 40)
    );
}

#[test]
fn pane_id_for_index_returns_none_for_missing_pane() {
    let session = Session::new(
        session_name("alpha"),
        TerminalSize {
            cols: 120,
            rows: 40,
        },
    );

    assert_eq!(session.pane_id(9), None);
}

#[test]
fn pane_ids_are_unique_across_successive_splits_even_if_the_counter_regresses() {
    let mut session = Session::new(
        session_name("alpha"),
        TerminalSize {
            cols: 120,
            rows: 40,
        },
    );
    session.split_active_pane().expect("first split succeeds");
    session.next_pane_id = 0;

    let new_index = session.split_pane(0).expect("second split succeeds");
    let pane_ids = session
        .window()
        .panes()
        .iter()
        .map(|pane| pane.id())
        .collect::<Vec<_>>();

    assert_eq!(new_index, 1);
    assert_eq!(
        session
            .window()
            .pane(new_index)
            .expect("new pane exists")
            .id(),
        PaneId::new(2)
    );
    assert_eq!(
        pane_ids.iter().copied().collect::<BTreeSet<_>>().len(),
        pane_ids.len()
    );
}

#[test]
fn floating_resize_exact_and_new_split_reject_without_mutation() {
    for mixed in [false, true] {
        let mut session = Session::new(
            session_name("float-resize"),
            TerminalSize {
                cols: 120,
                rows: 40,
            },
        );
        if mixed {
            session.split_active_pane().unwrap();
            session.split_active_pane().unwrap();
        }
        session
            .window_layout_context_mut(0)
            .unwrap()
            .float_pane(
                u32::from(mixed),
                crate::FloatingPaneRect::new(3, 4, 30, 10).unwrap(),
            )
            .unwrap();
        let target = u32::from(mixed);
        if mixed {
            session.toggle_zoom_in_window(0, 0).unwrap();
        }
        for direction in [SplitDirection::Vertical, SplitDirection::Horizontal] {
            for operation in 0..3 {
                let before = session.window().clone();
                let result = match operation {
                    0 => session.resize_pane_to_in_window(0, target, direction, 20),
                    _ => session.resize_new_split_pane_to_in_window(
                        0,
                        target,
                        direction,
                        20,
                        operation == 1,
                    ),
                };
                assert_eq!(session.window(), &before);
                assert!(matches!(result, Err(RmuxError::Server(_))));
            }
        }
    }
}
