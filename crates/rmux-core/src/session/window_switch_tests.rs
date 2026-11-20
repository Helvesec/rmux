use crate::{PaneId, Session, SessionId, SessionStore, WINLINK_BELL};
use rmux_proto::{SessionName, TerminalSize, WindowTarget};

#[test]
fn borrowed_switch_matches_session_selection_and_uses_post_selection_geometry() {
    let source_name = SessionName::new("switch-source").unwrap();
    let peer_name = SessionName::new("switch-peer").unwrap();
    let group_name = SessionName::new("switch-group").unwrap();
    let initial_size = TerminalSize { cols: 80, rows: 24 };
    let terminal_size = TerminalSize {
        cols: 111,
        rows: 43,
    };
    let content_size = TerminalSize {
        cols: 111,
        rows: 42,
    };

    let mut source = Session::new(source_name, initial_size);
    let (window_index, _) = source.create_window(initial_size).unwrap();
    let pane_index = source
        .split_pane_in_window_with_id_and_direction_before(
            window_index,
            0,
            PaneId::new(99),
            rmux_proto::SplitDirection::Vertical,
            true,
        )
        .unwrap();
    source
        .toggle_zoom_in_window(window_index, 1)
        .expect("target window zooms");
    assert!(source.add_winlink_alert_flags(window_index, WINLINK_BELL));

    let peer = source.clone_as_group_member(peer_name.clone(), group_name, SessionId::new(7));
    let mut expected = peer.clone();
    expected
        .toggle_zoom_in_window(window_index, 1)
        .expect("standalone target unzooms");
    expected
        .select_window(window_index)
        .expect("standalone window selection succeeds");
    expected
        .select_pane_in_window(window_index, pane_index)
        .expect("standalone pane selection succeeds");
    expected
        .toggle_zoom_in_window(window_index, pane_index)
        .expect("standalone target rezooms");
    expected.resize_active_window_geometry(terminal_size, content_size);

    let mut store = SessionStore::default();
    store.insert_existing_session(peer).unwrap();
    let target = WindowTarget::with_window(peer_name.clone(), window_index);
    {
        let mut switch = store.window_switch_mut(&target).unwrap();
        assert_eq!(switch.window_id(), expected.window().id());
        assert_eq!(switch.pane_id(pane_index), Some(PaneId::new(99)));
        switch.select_window_and_pane(pane_index, true).unwrap();
        switch.resize(terminal_size, content_size);
    }

    let actual = store.session(&peer_name).expect("peer remains stored");
    assert_eq!(actual, &expected);
    assert_eq!(actual.terminal_size(), terminal_size);
    assert_eq!(actual.window().size(), content_size);
    assert_eq!(actual.last_window_index(), Some(0));
    assert!(actual.winlink_alert_flags(window_index).is_empty());
    assert!(actual.window().is_zoomed());
}
