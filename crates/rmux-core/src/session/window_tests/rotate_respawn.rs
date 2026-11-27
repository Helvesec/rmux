use super::*;
use rmux_proto::ResizePaneAdjustment;

#[test]
fn rotate_window_keeps_pane_ids_stable_while_preserving_the_active_slot() {
    let mut session = Session::new(
        session_name("alpha"),
        TerminalSize {
            cols: 120,
            rows: 40,
        },
    );
    session.split_active_pane().expect("first split succeeds");
    session.split_pane(0).expect("second split succeeds");

    let previous_pane_ids = session
        .window()
        .panes()
        .iter()
        .map(|pane| pane.id())
        .collect::<Vec<_>>();
    let previous_geometries = session
        .window()
        .panes()
        .iter()
        .map(|pane| pane.geometry())
        .collect::<Vec<_>>();

    session
        .rotate_window(0, RotateWindowDirection::Up)
        .expect("rotate succeeds");

    assert_eq!(session.active_pane_index(), 1);
    assert_eq!(
        session
            .window()
            .panes()
            .iter()
            .map(|pane| pane.id())
            .collect::<Vec<_>>(),
        vec![
            previous_pane_ids[1],
            previous_pane_ids[2],
            previous_pane_ids[0]
        ]
    );
    assert_eq!(
        session
            .window()
            .panes()
            .iter()
            .map(|pane| pane.geometry())
            .collect::<Vec<_>>(),
        previous_geometries
    );
}

#[test]
fn rotate_window_with_zoom_restores_zoom_when_requested() {
    let mut session = Session::new(
        session_name("alpha"),
        TerminalSize {
            cols: 120,
            rows: 40,
        },
    );
    session.split_active_pane().expect("split succeeds");
    session
        .resize_pane_in_window(0, 1, ResizePaneAdjustment::Zoom)
        .expect("zoom succeeds");

    session
        .rotate_window_with_zoom(0, RotateWindowDirection::Up, true)
        .expect("rotation succeeds");

    let window = session.window_at(0).expect("window exists");
    assert!(window.is_zoomed());
    assert_eq!(window.active_pane_index(), 1);
    assert_eq!(
        window.pane(1).expect("active pane exists").geometry(),
        PaneGeometry::new(0, 0, 120, 40)
    );
}

#[test]
fn respawn_window_retains_first_pane_identity_even_when_another_pane_is_active_like_tmux() {
    let mut session = Session::new(
        session_name("alpha"),
        TerminalSize {
            cols: 120,
            rows: 40,
        },
    );
    session.split_active_pane().expect("first split succeeds");
    session.split_active_pane().expect("second split succeeds");
    session.select_pane(2).expect("pane 2 exists");

    let first_pane_id = session.window().panes()[0].id();
    let active_pane_id = session
        .window()
        .active_pane()
        .expect("active pane exists")
        .id();
    assert_ne!(first_pane_id, active_pane_id);

    let respawned_pane_id = session.respawn_window(0).expect("respawn succeeds");

    assert_eq!(respawned_pane_id, first_pane_id);
    assert_eq!(session.window().panes().len(), 1);
    assert_eq!(session.window().panes()[0].id(), first_pane_id);
    assert_eq!(session.active_pane_index(), 0);
    assert_eq!(
        session.window().panes()[0].geometry(),
        PaneGeometry::new(0, 0, 120, 40)
    );
}

#[test]
fn move_window_to_the_same_index_is_a_noop() {
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
    session.select_window(2).expect("window 2 select succeeds");

    let result = session
        .move_window(2, 2, false, false)
        .expect("self-move succeeds");

    assert_eq!(result, None);
    assert_eq!(session.active_window_index(), 2);
    assert_eq!(session.last_window_index(), Some(0));
}

#[test]
fn move_window_with_does_not_follow_the_active_source_window() {
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
    session.select_window(2).expect("window 2 select succeeds");
    session.select_window(0).expect("window 0 select succeeds");

    session
        .move_window(0, 4, false, false)
        .expect("move succeeds");

    assert_eq!(session.active_window_index(), 2);
    assert_eq!(session.last_window_index(), None);
}

#[test]
fn swap_windows_with_the_same_index_is_a_noop() {
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
    session.select_window(2).expect("window 2 select succeeds");

    session.swap_windows(2, 2).expect("self-swap succeeds");

    assert_eq!(session.active_window_index(), 2);
    assert_eq!(session.last_window_index(), Some(0));
}

#[test]
fn swap_windows_rejects_nonexistent_source() {
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
        .swap_windows(99, 2)
        .expect_err("nonexistent source should fail");

    assert_eq!(
        error,
        RmuxError::invalid_target("alpha:99", "window index does not exist in session")
    );
}

#[test]
fn swap_windows_rejects_nonexistent_destination() {
    let mut session = Session::new(
        session_name("alpha"),
        TerminalSize {
            cols: 120,
            rows: 40,
        },
    );

    let error = session
        .swap_windows(0, 99)
        .expect_err("nonexistent destination should fail");

    assert_eq!(
        error,
        RmuxError::invalid_target("alpha:99", "window index does not exist in session")
    );
}

#[test]
fn reindex_windows_on_already_contiguous_indices_is_an_identity_transform() {
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
    session.select_window(1).expect("window 1 select succeeds");

    let mapping = session.reindex_windows().expect("reindex succeeds");

    assert_eq!(
        mapping.into_iter().collect::<Vec<_>>(),
        vec![(0, 0), (1, 1), (2, 2)]
    );
    assert_eq!(
        session.windows().keys().copied().collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    assert_eq!(session.active_window_index(), 1);
    assert_eq!(session.last_window_index(), Some(0));
}

#[test]
fn rotate_window_down_selects_the_previous_pane_in_window_order() {
    let mut session = Session::new(
        session_name("alpha"),
        TerminalSize {
            cols: 120,
            rows: 40,
        },
    );
    session.split_active_pane().expect("first split succeeds");
    session.split_pane(0).expect("second split succeeds");

    let previous_pane_ids = session
        .window()
        .panes()
        .iter()
        .map(|pane| pane.id())
        .collect::<Vec<_>>();
    let previous_geometries = session
        .window()
        .panes()
        .iter()
        .map(|pane| pane.geometry())
        .collect::<Vec<_>>();

    session
        .rotate_window(0, RotateWindowDirection::Down)
        .expect("rotate succeeds");

    assert_eq!(session.active_pane_index(), 1);
    assert_eq!(
        session
            .window()
            .panes()
            .iter()
            .map(|pane| pane.id())
            .collect::<Vec<_>>(),
        vec![
            previous_pane_ids[2],
            previous_pane_ids[0],
            previous_pane_ids[1]
        ]
    );
    assert_eq!(
        session
            .window()
            .panes()
            .iter()
            .map(|pane| pane.geometry())
            .collect::<Vec<_>>(),
        previous_geometries
    );
}

#[test]
fn rotate_window_tracks_last_pane_as_previous_active_identity_like_tmux() {
    let mut session = Session::new(
        session_name("alpha"),
        TerminalSize {
            cols: 120,
            rows: 40,
        },
    );
    session.split_active_pane().expect("first split succeeds");
    session.split_pane(0).expect("second split succeeds");
    session.select_pane(0).expect("pane 0 exists");
    session.select_pane(1).expect("pane 1 exists");

    assert_eq!(session.active_pane_index(), 1);
    assert_eq!(session.window().last_pane_index(), Some(0));
    let previous_active_pane_id = session
        .window()
        .active_pane()
        .expect("active pane exists")
        .id();

    session
        .rotate_window(0, RotateWindowDirection::Down)
        .expect("rotate succeeds");

    assert_eq!(session.active_pane_index(), 1);
    let last_pane_index = session
        .window()
        .last_pane_index()
        .expect("last pane survives rotation");
    assert_eq!(
        session
            .window()
            .pane(last_pane_index)
            .expect("last pane exists")
            .id(),
        previous_active_pane_id
    );
}
