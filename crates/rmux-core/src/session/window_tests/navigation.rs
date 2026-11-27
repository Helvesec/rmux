use super::*;

#[test]
fn create_window_uses_the_lowest_available_window_index() {
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

    let (window_index, pane_id) = session
        .create_window(TerminalSize { cols: 90, rows: 30 })
        .expect("window creation succeeds");

    assert_eq!(window_index, 1);
    assert_eq!(
        pane_id,
        session.pane_id_in_window(1, 0).expect("pane 0 exists")
    );
}

#[test]
fn next_window_wraps_across_sparse_indices_and_updates_last_window() {
    let mut session = Session::new(
        session_name("alpha"),
        TerminalSize {
            cols: 120,
            rows: 40,
        },
    );
    session
        .insert_window_with_initial_pane(3, TerminalSize { cols: 90, rows: 30 })
        .expect("window 3 insert succeeds");
    session
        .insert_window_with_initial_pane(7, TerminalSize { cols: 90, rows: 30 })
        .expect("window 7 insert succeeds");

    assert_eq!(session.next_window().expect("next window exists"), 3);
    assert_eq!(session.active_window_index(), 3);
    assert_eq!(session.last_window_index(), Some(0));

    assert_eq!(session.next_window().expect("next window exists"), 7);
    assert_eq!(session.active_window_index(), 7);
    assert_eq!(session.last_window_index(), Some(3));

    assert_eq!(session.next_window().expect("next window exists"), 0);
    assert_eq!(session.active_window_index(), 0);
    assert_eq!(session.last_window_index(), Some(7));
}

#[test]
fn previous_window_wraps_across_sparse_indices_and_updates_last_window() {
    let mut session = Session::new(
        session_name("alpha"),
        TerminalSize {
            cols: 120,
            rows: 40,
        },
    );
    session
        .insert_window_with_initial_pane(3, TerminalSize { cols: 90, rows: 30 })
        .expect("window 3 insert succeeds");
    session
        .insert_window_with_initial_pane(7, TerminalSize { cols: 90, rows: 30 })
        .expect("window 7 insert succeeds");

    assert_eq!(
        session.previous_window().expect("previous window exists"),
        7
    );
    assert_eq!(session.active_window_index(), 7);
    assert_eq!(session.last_window_index(), Some(0));

    assert_eq!(
        session.previous_window().expect("previous window exists"),
        3
    );
    assert_eq!(session.active_window_index(), 3);
    assert_eq!(session.last_window_index(), Some(7));

    assert_eq!(
        session.previous_window().expect("previous window exists"),
        0
    );
    assert_eq!(session.active_window_index(), 0);
    assert_eq!(session.last_window_index(), Some(3));
}

#[test]
fn next_and_previous_window_reject_single_window_sessions() {
    let mut session = Session::new(
        session_name("alpha"),
        TerminalSize {
            cols: 120,
            rows: 40,
        },
    );

    assert_eq!(
        session.next_window(),
        Err(RmuxError::Message("no next window".to_owned()))
    );
    assert_eq!(
        session.previous_window(),
        Err(RmuxError::Message("no previous window".to_owned()))
    );
}

#[test]
fn last_window_errors_until_a_window_switch_happens_then_toggles_back() {
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

    assert_eq!(
        session.last_window(),
        Err(RmuxError::Message("no last window".to_owned()))
    );

    session.select_window(2).expect("window 2 select succeeds");

    assert_eq!(session.last_window().expect("last window exists"), 0);
    assert_eq!(session.active_window_index(), 0);
    assert_eq!(session.last_window_index(), Some(2));
}

#[test]
fn recreated_window_index_gets_a_new_stable_window_id() {
    let mut session = Session::new(
        session_name("alpha"),
        TerminalSize {
            cols: 120,
            rows: 40,
        },
    );
    let original_id = session.window().id();

    let (window_index, _) = session
        .create_window(TerminalSize { cols: 90, rows: 30 })
        .expect("window creation succeeds");
    let created_id = session
        .window_at(window_index)
        .expect("created window exists")
        .id();
    session
        .remove_window(window_index)
        .expect("window removal succeeds");

    let (reused_index, _) = session
        .create_window(TerminalSize { cols: 90, rows: 30 })
        .expect("window recreation succeeds");
    let reused_id = session
        .window_at(reused_index)
        .expect("recreated window exists")
        .id();

    assert_eq!(window_index, reused_index);
    assert_ne!(created_id, reused_id);
    assert_ne!(original_id, reused_id);
}

#[test]
fn remove_window_prefers_last_window_as_the_active_fallback() {
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
    session
        .insert_window_with_initial_pane(2, TerminalSize { cols: 90, rows: 30 })
        .expect("window 2 insert succeeds");
    session.select_window(2).expect("window 2 select succeeds");
    session.select_window(1).expect("window 1 select succeeds");

    let removed = session.remove_window(1).expect("window removal succeeds");

    assert_eq!(removed.name(), None);
    assert_eq!(session.active_window_index(), 2);
    // Detaching the current window selects the return first, which pushes the
    // window being left, and only then drops the occurrence that goes away
    // (`session.c:346-350`): window 0 is what the session still visited.
    assert_eq!(session.last_window_index(), Some(0));
}

#[test]
fn remove_window_falls_back_to_the_previous_window_when_last_is_unavailable() {
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
    session
        .insert_window_with_initial_pane(2, TerminalSize { cols: 90, rows: 30 })
        .expect("window 2 insert succeeds");
    session.select_window(2).expect("window 2 select succeeds");
    session
        .remove_window(0)
        .expect("removing non-active last window succeeds");

    session
        .remove_window(2)
        .expect("active window removal succeeds");

    assert_eq!(session.active_window_index(), 1);
    assert_eq!(session.last_window_index(), None);
}

#[test]
fn remove_window_falls_back_to_the_next_window_when_no_previous_exists() {
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
        .remove_window(0)
        .expect("active window removal succeeds");

    assert_eq!(session.active_window_index(), 2);
    assert_eq!(session.last_window_index(), None);
}

#[test]
fn remove_window_wraps_to_tmux_previous_window_when_no_history_exists() {
    // tmux 3.7b, measured 2026-07-27: removing active index 0 with no
    // last-window selects the highest surviving index, the cyclic previous
    // window. Capture the stable identity before mutation.
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
        .insert_window_with_initial_pane(7, TerminalSize { cols: 90, rows: 30 })
        .expect("window 7 insert succeeds");
    let expected_window_id = session
        .window_at(7)
        .expect("oracle-selected window exists before removal")
        .id();

    session
        .remove_window(0)
        .expect("active window removal succeeds");

    assert_eq!(session.active_window_index(), 7);
    assert_eq!(session.window().id(), expected_window_id);
    assert_eq!(session.last_window_index(), None);
}

#[test]
fn remove_window_rejects_killing_the_only_window() {
    let mut session = Session::new(
        session_name("alpha"),
        TerminalSize {
            cols: 120,
            rows: 40,
        },
    );

    let error = session
        .remove_window(0)
        .expect_err("single-window removal should fail");

    assert_eq!(
        error,
        RmuxError::Server("cannot kill the only window in session alpha".to_owned())
    );
}
