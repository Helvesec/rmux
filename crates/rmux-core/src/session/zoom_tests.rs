use rmux_proto::{
    ResizePaneAdjustment, SelectPaneDirection, SessionName, SplitDirection, TerminalSize,
};

use super::{PaneSwapOptions, Session, SessionPaneTarget};
use crate::PaneGeometry;

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

#[test]
fn zoom_state_survives_switching_away_and_back_to_a_window() {
    let mut session = Session::new(session_name("alpha"), TerminalSize { cols: 80, rows: 24 });
    session.split_active_pane().expect("split succeeds");
    session
        .resize_pane(1, ResizePaneAdjustment::Zoom)
        .expect("zoom succeeds");
    session
        .create_window(TerminalSize { cols: 80, rows: 24 })
        .expect("new window succeeds");

    session.select_window(1).expect("window 1 select succeeds");
    assert!(
        session.window_at(0).expect("window 0 exists").is_zoomed(),
        "departing window must keep zoom state"
    );

    session.select_window(0).expect("window 0 select succeeds");

    let window = session.window_at(0).expect("window 0 exists");
    assert!(window.is_zoomed());
    assert_eq!(
        window.pane(1).expect("pane 1 exists").geometry(),
        PaneGeometry::new(0, 0, 80, 24)
    );
}

#[test]
fn zoom_toggle_targets_a_non_active_window_explicitly() {
    let mut session = Session::new(session_name("alpha"), TerminalSize { cols: 80, rows: 24 });
    session
        .create_window(TerminalSize { cols: 80, rows: 24 })
        .expect("new window succeeds");
    session
        .split_pane_in_window(1, 0)
        .expect("split window 1 succeeds");

    session
        .resize_pane_in_window(1, 1, ResizePaneAdjustment::Zoom)
        .expect("zoom window 1 succeeds");

    assert_eq!(session.active_window_index(), 0);
    assert!(!session.window_at(0).expect("window 0 exists").is_zoomed());
    assert!(session.window_at(1).expect("window 1 exists").is_zoomed());
    assert_eq!(
        session
            .window_at(1)
            .expect("window 1 exists")
            .pane(1)
            .expect("pane 1 exists")
            .geometry(),
        PaneGeometry::new(0, 0, 80, 24)
    );
}

#[test]
fn select_pane_preserves_zoom_when_requested() {
    let mut session = Session::new(session_name("alpha"), TerminalSize { cols: 80, rows: 24 });
    session.split_active_pane().expect("split succeeds");
    session
        .resize_pane_in_window(0, 0, ResizePaneAdjustment::Zoom)
        .expect("zoom succeeds");

    session
        .select_pane_in_window_with_zoom(0, 1, true)
        .expect("pane select succeeds");

    let window = session.window_at(0).expect("window exists");
    assert!(window.is_zoomed());
    assert_eq!(window.active_pane_index(), 1);
    assert_eq!(
        window.pane(1).expect("pane 1 exists").geometry(),
        PaneGeometry::new(0, 0, 80, 24)
    );
}

#[test]
fn last_pane_preserves_zoom_when_requested() {
    let mut session = Session::new(session_name("alpha"), TerminalSize { cols: 80, rows: 24 });
    session.split_active_pane().expect("split succeeds");
    session
        .resize_pane_in_window(0, 1, ResizePaneAdjustment::Zoom)
        .expect("zoom succeeds");

    let selected = session
        .last_pane_in_window_with_zoom(0, true)
        .expect("last pane succeeds");

    let window = session.window_at(0).expect("window exists");
    assert_eq!(selected, 0);
    assert!(window.is_zoomed());
    assert_eq!(window.active_pane_index(), 0);
    assert_eq!(
        window.pane(0).expect("pane 0 exists").geometry(),
        PaneGeometry::new(0, 0, 80, 24)
    );
}

#[test]
fn adjacent_select_preserves_zoom_when_requested() {
    let mut session = Session::new(session_name("alpha"), TerminalSize { cols: 80, rows: 24 });
    session
        .split_pane_with_direction(0, SplitDirection::Vertical)
        .expect("split succeeds");
    session
        .resize_pane_in_window(0, 1, ResizePaneAdjustment::Zoom)
        .expect("zoom succeeds");

    let selected = session
        .select_adjacent_pane_in_window_with_zoom(0, 1, SelectPaneDirection::Left, true)
        .expect("adjacent select succeeds");

    let window = session.window_at(0).expect("window exists");
    assert_eq!(selected, 0);
    assert!(window.is_zoomed());
    assert_eq!(window.active_pane_index(), 0);
    assert_eq!(
        window.pane(0).expect("pane 0 exists").geometry(),
        PaneGeometry::new(0, 0, 80, 24)
    );
}

#[test]
fn adjacent_select_unzooms_when_zoomed_neighbor_exists() {
    let mut session = Session::new(session_name("alpha"), TerminalSize { cols: 80, rows: 24 });
    session
        .split_pane_with_direction(0, SplitDirection::Vertical)
        .expect("split succeeds");
    session
        .resize_pane_in_window(0, 1, ResizePaneAdjustment::Zoom)
        .expect("zoom succeeds");

    let selected = session
        .select_adjacent_pane_in_window(0, 1, SelectPaneDirection::Left)
        .expect("adjacent select succeeds");

    let window = session.window_at(0).expect("window exists");
    assert_eq!(selected, 0);
    assert!(!window.is_zoomed());
    assert_eq!(window.active_pane_index(), 0);
}

#[test]
fn adjacent_select_keeps_zoom_when_zoomed_no_neighbor_exists() {
    let mut session = Session::new(session_name("alpha"), TerminalSize { cols: 80, rows: 24 });
    session
        .split_pane_with_direction(0, SplitDirection::Vertical)
        .expect("split succeeds");
    session
        .resize_pane_in_window(0, 1, ResizePaneAdjustment::Zoom)
        .expect("zoom succeeds");

    let selected = session
        .select_adjacent_pane_in_window(0, 1, SelectPaneDirection::Up)
        .expect("adjacent select succeeds");

    let window = session.window_at(0).expect("window exists");
    assert_eq!(selected, 1);
    assert!(window.is_zoomed());
    assert_eq!(window.active_pane_index(), 1);
}

#[test]
fn cross_window_swap_preserves_zoom_state_when_requested() {
    let mut session = Session::new(session_name("alpha"), TerminalSize { cols: 80, rows: 24 });
    session
        .split_active_pane()
        .expect("window 0 split succeeds");
    session
        .create_window(TerminalSize { cols: 80, rows: 24 })
        .expect("window 1 create succeeds");
    session
        .split_pane_in_window(1, 0)
        .expect("window 1 split succeeds");

    let source_pane_id = session
        .pane_id_in_window(0, 1)
        .expect("window 0 pane 1 id exists");
    let target_pane_id = session
        .pane_id_in_window(1, 1)
        .expect("window 1 pane 1 id exists");

    session
        .toggle_zoom_in_window(0, 1)
        .expect("window 0 zoom succeeds");
    session
        .toggle_zoom_in_window(1, 1)
        .expect("window 1 zoom succeeds");

    session
        .swap_panes(
            SessionPaneTarget::new(0, 1),
            SessionPaneTarget::new(1, 1),
            PaneSwapOptions::new(true, true),
        )
        .expect("cross-window swap succeeds");

    assert!(session.window_at(0).expect("window 0 exists").is_zoomed());
    assert!(session.window_at(1).expect("window 1 exists").is_zoomed());
    assert_eq!(session.pane_id_in_window(0, 1), Some(target_pane_id));
    assert_eq!(session.pane_id_in_window(1, 1), Some(source_pane_id));
}
