use super::*;

#[test]
fn window_by_index_returns_none_for_missing_window() {
    let session = Session::new(
        session_name("alpha"),
        TerminalSize {
            cols: 120,
            rows: 40,
        },
    );

    assert!(session.window_at(1).is_none());
}

#[test]
fn clone_preserves_full_session_state_for_rollback() {
    let mut session = Session::new(
        session_name("alpha"),
        TerminalSize {
            cols: 200,
            rows: 50,
        },
    );
    session.split_active_pane().expect("split succeeds");
    session.select_pane(1).expect("pane 1 exists");
    session
        .resize_pane(1, ResizePaneAdjustment::AbsoluteWidth { columns: 34 })
        .expect("pane exists");

    let cloned = session.clone();

    assert_eq!(cloned, session);
    assert_eq!(cloned.active_window_index(), 0);
    assert_eq!(cloned.active_pane_index(), 1);
    assert_eq!(cloned.window().last_pane_index(), Some(0));
}

#[test]
fn create_session_inserts_a_new_entry() {
    let mut store = SessionStore::new();
    let name = session_name("alpha");

    store
        .create_session(name.clone(), TerminalSize { cols: 80, rows: 24 })
        .expect("insert succeeds");

    assert_eq!(store.len(), 1);
    assert!(store.contains_session(&name));
}

#[test]
fn pane_and_window_identity_allocation_stays_monotonic_after_close_and_respawn() {
    let mut store = SessionStore::new();
    let name = session_name("alpha");
    let size = TerminalSize { cols: 80, rows: 24 };
    store
        .create_session(name.clone(), size)
        .expect("insert succeeds");

    let initial_pane = store
        .session(&name)
        .expect("session exists")
        .pane_id_in_window(0, 0)
        .expect("initial pane exists");
    let initial_window = store
        .session(&name)
        .expect("session exists")
        .window_at(0)
        .expect("initial window exists")
        .id();
    assert_eq!(initial_pane, crate::PaneId::new(0));
    assert_eq!(initial_window, crate::WindowId::new(0));

    let closed_split_pane = store.allocate_pane_id();
    let split_index = store
        .session_mut(&name)
        .expect("session exists")
        .split_pane_in_window_with_id_and_direction(
            0,
            0,
            closed_split_pane,
            SplitDirection::Vertical,
        )
        .expect("split succeeds");
    assert_eq!(closed_split_pane, crate::PaneId::new(1));
    store
        .session_mut(&name)
        .expect("session exists")
        .kill_pane_in_window(0, split_index)
        .expect("kill split succeeds");

    let respawned_pane = store
        .session_mut(&name)
        .expect("session exists")
        .respawn_window(0)
        .expect("respawn window succeeds");
    assert_eq!(respawned_pane, initial_pane);

    let post_close_split_pane = store.allocate_pane_id();
    store
        .session_mut(&name)
        .expect("session exists")
        .split_pane_in_window_with_id_and_direction(
            0,
            0,
            post_close_split_pane,
            SplitDirection::Vertical,
        )
        .expect("split after close succeeds");
    assert_eq!(post_close_split_pane, crate::PaneId::new(2));

    let first_new_window_pane = store.allocate_pane_id();
    let first_new_window_index = store
        .session_mut(&name)
        .expect("session exists")
        .create_window_at_or_above_with_pane_id(size, 0, first_new_window_pane)
        .expect("new window succeeds")
        .0;
    let first_new_window_id = store
        .session(&name)
        .expect("session exists")
        .window_at(first_new_window_index)
        .expect("new window exists")
        .id();
    assert_eq!(first_new_window_pane, crate::PaneId::new(3));
    assert_eq!(first_new_window_id, crate::WindowId::new(1));

    store
        .session_mut(&name)
        .expect("session exists")
        .remove_window(first_new_window_index)
        .expect("remove new window succeeds");
    let second_new_window_pane = store.allocate_pane_id();
    let second_new_window_index = store
        .session_mut(&name)
        .expect("session exists")
        .create_window_at_or_above_with_pane_id(size, 0, second_new_window_pane)
        .expect("second new window succeeds")
        .0;
    let second_new_window_id = store
        .session(&name)
        .expect("session exists")
        .window_at(second_new_window_index)
        .expect("second new window exists")
        .id();

    assert_eq!(second_new_window_pane, crate::PaneId::new(4));
    assert_eq!(second_new_window_id, crate::WindowId::new(2));
}

#[test]
fn explicit_standalone_grouped_session_uses_reserved_session_id() {
    let mut store = SessionStore::new();
    for name in ["0", "1", "bob"] {
        store
            .create_session(session_name(name), TerminalSize { cols: 80, rows: 24 })
            .expect("seed session succeeds");
    }

    let grouped = store
        .create_grouped_session_with_base_index(
            session_name("named"),
            TerminalSize { cols: 80, rows: 24 },
            0,
            session_name("stacy"),
        )
        .expect("standalone grouped session succeeds");

    let session = store
        .session(&grouped.session_name)
        .expect("grouped session exists");
    assert_eq!(session.id(), crate::SessionId::new(3));
    assert_eq!(store.next_session_id(), crate::SessionId::new(4));
}

#[test]
fn create_session_rejects_duplicates() {
    let mut store = SessionStore::new();
    let name = session_name("alpha");
    store
        .create_session(name.clone(), TerminalSize { cols: 80, rows: 24 })
        .expect("insert succeeds");

    let error = store
        .create_session(
            name.clone(),
            TerminalSize {
                cols: 100,
                rows: 30,
            },
        )
        .expect_err("duplicate should fail");

    assert_eq!(error, RmuxError::DuplicateSession("alpha".to_owned()));
}

#[test]
fn remove_session_returns_the_removed_session() {
    let mut store = SessionStore::new();
    let name = session_name("alpha");
    store
        .create_session(name.clone(), TerminalSize { cols: 80, rows: 24 })
        .expect("insert succeeds");

    let removed = store.remove_session(&name).expect("session exists");

    assert_eq!(removed.name(), &name);
    assert!(store.is_empty());
}

#[test]
fn remove_session_reports_missing_sessions() {
    let mut store = SessionStore::new();
    let name = session_name("alpha");

    let error = store.remove_session(&name).expect_err("session is absent");

    assert_eq!(error, RmuxError::SessionNotFound("alpha".to_owned()));
}

#[test]
fn rename_session_updates_the_store_key_and_internal_name() {
    let mut store = SessionStore::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    store
        .create_session(alpha.clone(), TerminalSize { cols: 80, rows: 24 })
        .expect("insert succeeds");

    store
        .rename_session(&alpha, beta.clone())
        .expect("rename succeeds");

    assert!(!store.contains_session(&alpha));
    let renamed = store.session(&beta).expect("renamed session exists");
    assert_eq!(renamed.name(), &beta);
}

#[test]
fn rename_session_rejects_existing_destinations_without_mutation() {
    let mut store = SessionStore::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    for session_name in [alpha.clone(), beta.clone()] {
        store
            .create_session(session_name, TerminalSize { cols: 80, rows: 24 })
            .expect("insert succeeds");
    }

    let error = store
        .rename_session(&alpha, beta.clone())
        .expect_err("existing destination rejects rename");

    assert_eq!(error, RmuxError::DuplicateSession("beta".to_owned()));
    assert_eq!(
        store
            .session(&alpha)
            .expect("original session exists")
            .name(),
        &alpha
    );
    assert_eq!(
        store
            .session(&beta)
            .expect("destination session exists")
            .name(),
        &beta
    );
}
