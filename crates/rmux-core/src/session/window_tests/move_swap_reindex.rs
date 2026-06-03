use super::*;

#[test]
fn move_window_requires_kill_when_the_destination_exists() {
    let mut session = Session::new(
        session_name("alpha"),
        TerminalSize {
            cols: 120,
            rows: 40,
        },
    );
    session
        .insert_window_with_initial_pane(2, TerminalSize { cols: 90, rows: 30 })
        .expect("window 2 insert succeeds");

    let error = session
        .move_window(0, 2, false, false)
        .expect_err("occupied destination without -k should fail");

    assert_eq!(
        error,
        RmuxError::invalid_target("alpha:2", "window index already exists in session")
    );
}

#[test]
fn move_window_relocates_the_active_window_without_rewriting_pane_ids() {
    let mut session = Session::new(
        session_name("alpha"),
        TerminalSize {
            cols: 120,
            rows: 40,
        },
    );
    let original_pane_id = session.pane_id_in_window(0, 0).expect("pane 0 exists");

    session
        .move_window(0, 4, false, false)
        .expect("move succeeds");

    assert_eq!(
        session.windows().keys().copied().collect::<Vec<_>>(),
        vec![4]
    );
    assert_eq!(session.active_window_index(), 4);
    assert_eq!(session.last_window_index(), None);
    assert_eq!(session.pane_id_in_window(4, 0), Some(original_pane_id));
}

#[test]
fn move_window_clears_last_tracking_when_the_last_window_becomes_active() {
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
    session.select_window(1).expect("window 1 select succeeds");

    let moved_pane_id = session.pane_id_in_window(0, 0).expect("pane 0 exists");

    session
        .move_window(0, 1, true, false)
        .expect("move succeeds");

    assert_eq!(
        session.windows().keys().copied().collect::<Vec<_>>(),
        vec![1]
    );
    assert_eq!(session.active_window_index(), 1);
    assert_eq!(session.last_window_index(), None);
    assert_eq!(session.pane_id_in_window(1, 0), Some(moved_pane_id));
}

#[test]
fn swap_windows_keeps_active_and_last_tracking_with_the_same_windows() {
    let mut session = Session::new(
        session_name("alpha"),
        TerminalSize {
            cols: 120,
            rows: 40,
        },
    );
    session
        .insert_window_with_initial_pane(2, TerminalSize { cols: 90, rows: 30 })
        .expect("window 2 insert succeeds");
    session
        .insert_window_with_initial_pane(5, TerminalSize { cols: 90, rows: 30 })
        .expect("window 5 insert succeeds");
    session.select_window(5).expect("window 5 select succeeds");
    session.select_window(2).expect("window 2 select succeeds");

    session.swap_windows(2, 5).expect("swap succeeds");

    assert_eq!(session.active_window_index(), 2);
    assert_eq!(session.last_window_index(), Some(5));
}

#[test]
fn reindex_windows_compacts_sparse_slots_and_remaps_last_tracking() {
    let mut session = Session::new(
        session_name("alpha"),
        TerminalSize {
            cols: 120,
            rows: 40,
        },
    );
    session
        .move_window(0, 3, false, false)
        .expect("move succeeds");
    session
        .insert_window_with_initial_pane(7, TerminalSize { cols: 90, rows: 30 })
        .expect("window 7 insert succeeds");
    session.select_window(7).expect("window 7 select succeeds");

    let mapping = session.reindex_windows().expect("reindex succeeds");

    assert_eq!(
        mapping.into_iter().collect::<Vec<_>>(),
        vec![(3, 0), (7, 1)]
    );
    assert_eq!(
        session.windows().keys().copied().collect::<Vec<_>>(),
        vec![0, 1]
    );
    assert_eq!(session.active_window_index(), 1);
    assert_eq!(session.last_window_index(), Some(0));
}

#[test]
fn reindex_windows_overflow_preserves_existing_windows() {
    let mut session = Session::new(
        session_name("alpha"),
        TerminalSize {
            cols: 120,
            rows: 40,
        },
    );
    session
        .insert_window_with_initial_pane(2, TerminalSize { cols: 90, rows: 30 })
        .expect("window 2 insert succeeds");
    let before = session.windows().keys().copied().collect::<Vec<_>>();

    let error = session
        .reindex_windows_from(u32::MAX)
        .expect_err("reindex overflow is rejected");

    assert!(error.to_string().contains("window index space exhausted"));
    assert_eq!(
        session.windows().keys().copied().collect::<Vec<_>>(),
        before
    );
    assert_eq!(session.active_window_index(), 0);
}

#[test]
fn reindex_single_window_at_max_index_does_not_drop_the_window() {
    let mut session = Session::new(
        session_name("alpha"),
        TerminalSize {
            cols: 120,
            rows: 40,
        },
    );

    let mapping = session
        .reindex_windows_from(u32::MAX)
        .expect("single-window max reindex succeeds");

    assert_eq!(mapping.into_iter().collect::<Vec<_>>(), vec![(0, u32::MAX)]);
    assert_eq!(
        session.windows().keys().copied().collect::<Vec<_>>(),
        vec![u32::MAX]
    );
    assert_eq!(session.active_window_index(), u32::MAX);
}
