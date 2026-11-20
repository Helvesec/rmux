use super::*;

#[test]
fn relative_move_resolves_peer_navigation_from_original_links() {
    for duplicate in [false, true] {
        let mut store = ManagedSessionStore::new();
        store.create_session(name("a"), size()).unwrap();
        if duplicate {
            let id = store.window(&target("a", 0)).unwrap().id();
            store.link_window(&target("a", 1), id).unwrap();
        } else {
            let _ = store.create_window(&name("a"), size()).unwrap();
        }
        store
            .create_grouped_session_with_base_index(name("peer"), size(), 0, name("a"))
            .unwrap();
        for index in [1, 0] {
            store.select_window(&target("peer", index)).unwrap();
        }
        store
            .add_winlink_alert_flags(&target("peer", 0), WINLINK_ACTIVITY)
            .unwrap();
        store
            .add_winlink_alert_flags(&target("peer", 1), WINLINK_BELL)
            .unwrap();
        let navigation = store.group_peer_navigation(&name("a")).unwrap();
        let _ = store.make_room_for_window(&name("a"), 0).unwrap();
        let _ = store.move_window(&name("a"), 2, 0, false, false).unwrap();
        store.reconcile_group_peer_navigation(&navigation).unwrap();
        let peer = store.session(&name("peer")).unwrap();
        assert_eq!(peer.active_window_index(), if duplicate { 0 } else { 1 });
        // The peer's visit stays on the slot it was made on (`session.c:663-678`);
        // when the selection it kept lands on that same slot there is nothing
        // left to return to.
        assert_eq!(
            peer.last_window_index(),
            if duplicate { Some(1) } else { None }
        );
        assert_eq!(peer.winlink_alert_flags(0), WINLINK_BELL);
        assert_eq!(peer.winlink_alert_flags(1), WINLINK_ACTIVITY);
    }
}
