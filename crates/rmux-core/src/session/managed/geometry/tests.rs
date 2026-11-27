use super::*;

fn name(value: &str) -> SessionName {
    SessionName::new(value).unwrap()
}

fn target(value: &str, index: u32) -> WindowTarget {
    WindowTarget::with_window(name(value), index)
}

fn size(cols: u16, rows: u16) -> TerminalSize {
    TerminalSize::new(cols, rows)
}

fn fixture() -> ManagedSessionStore {
    let mut store = ManagedSessionStore::new();
    store.create_session(name("a"), size(80, 24)).unwrap();
    store.create_session(name("b"), size(90, 30)).unwrap();
    let id = store.session(&name("a")).unwrap().window().id();
    let _ = store.insert_alias(&target("a", 4), id).unwrap();
    let _ = store.insert_alias(&target("b", 7), id).unwrap();
    store
}

#[test]
fn attached_geometry_changes_one_owner_and_only_addressed_outer_size() {
    let mut store = fixture();
    let before = std::ptr::from_ref(store.window(&target("a", 0)).unwrap());
    let peer_recency = store.session(&name("b")).unwrap().recency();
    store
        .touched_attached_window_geometry_mut(&target("a", 0))
        .unwrap()
        .resize(size(120, 40), size(120, 38));
    let a = store.session(&name("a")).unwrap();
    let b = store.session(&name("b")).unwrap();
    assert_eq!(a.terminal_size(), size(120, 40));
    assert_eq!(b.terminal_size(), size(90, 30));
    assert!(a.last_attached_at().is_some());
    assert_eq!(b.last_attached_at(), None);
    assert_eq!(b.recency(), peer_recency);
    for target in [target("a", 0), target("a", 4), target("b", 7)] {
        let window = store.window(&target).unwrap();
        assert_eq!(std::ptr::from_ref(window), before);
        assert_eq!(window.size(), size(120, 38));
    }
}

#[test]
fn inactive_alias_geometry_keeps_both_sessions_outer_sizes() {
    let mut store = fixture();
    store
        .attached_window_geometry_mut(&target("b", 7))
        .unwrap()
        .resize(size(120, 40), size(120, 38));
    assert_eq!(
        store.session(&name("a")).unwrap().terminal_size(),
        size(80, 24)
    );
    assert_eq!(
        store.session(&name("b")).unwrap().terminal_size(),
        size(90, 30)
    );
    assert_eq!(store.window(&target("a", 0)).unwrap().size(), size(120, 38));
    assert_eq!(store.window(&target("b", 0)).unwrap().size(), size(90, 30));
}

#[test]
fn touched_switch_selects_local_alias_without_moving_owner_or_peer_navigation() {
    let mut store = fixture();
    let before = std::ptr::from_ref(store.window(&target("a", 0)).unwrap());
    {
        let mut switch = store.touched_window_switch_mut(&target("b", 7)).unwrap();
        switch.select_window();
        switch.resize(size(120, 40), size(120, 38));
    }
    let a = store.session(&name("a")).unwrap();
    let b = store.session(&name("b")).unwrap();
    assert_eq!(a.active_window_index(), 0);
    assert_eq!(a.last_window_index(), None);
    assert_eq!(b.active_window_index(), 7);
    assert_eq!(b.last_window_index(), Some(0));
    assert_eq!(a.terminal_size(), size(80, 24));
    assert_eq!(b.terminal_size(), size(120, 40));
    assert_eq!(std::ptr::from_ref(b.window()), before);
    assert_eq!(b.window().size(), size(120, 38));
    assert!(b.last_attached_at().is_some());
    assert_eq!(a.last_attached_at(), None);
}

#[test]
fn rejected_geometry_and_switch_borrows_preserve_attach_metadata() {
    let mut store = fixture();
    for invalid in [target("a", 99), target("missing", 0)] {
        let before = store.session(&name("a")).unwrap();
        let recency = before.recency();
        let activity = before.activity_at();
        for operation in 0..3 {
            let result = match operation {
                0 => store.attached_window_geometry_mut(&invalid).map(|_| ()),
                1 => store
                    .touched_attached_window_geometry_mut(&invalid)
                    .map(|_| ()),
                _ => store.touched_window_switch_mut(&invalid).map(|_| ()),
            };
            assert!(result.is_err());
            let after = store.session(&name("a")).unwrap();
            assert_eq!(after.recency(), recency);
            assert_eq!(after.activity_at(), activity);
            assert_eq!(after.last_attached_at(), None);
            assert_eq!(after.terminal_size(), size(80, 24));
            assert_eq!(after.active_window_index(), 0);
        }
    }
}

#[test]
fn grouped_peer_shares_content_but_retains_local_attach_and_outer_geometry() {
    let mut store = fixture();
    store
        .create_grouped_session_with_base_index(name("peer"), size(100, 32), 0, name("a"))
        .unwrap();
    let peer_before = store.session(&name("peer")).unwrap();
    let outer_before = peer_before.terminal_size();
    let recency_before = peer_before.recency();
    let owner_before = store.runtime_owner(&name("peer"));
    let address = std::ptr::from_ref(peer_before.window());
    store
        .touched_attached_window_geometry_mut(&target("a", 0))
        .unwrap()
        .resize(size(120, 40), size(120, 38));
    let peer_after = store.session(&name("peer")).unwrap();
    assert_eq!(peer_after.terminal_size(), outer_before);
    assert_eq!(peer_after.recency(), recency_before);
    assert_eq!(peer_after.last_attached_at(), None);
    assert_eq!(std::ptr::from_ref(peer_after.window()), address);
    assert_eq!(peer_after.window().size(), size(120, 38));
    assert_eq!(store.runtime_owner(&name("peer")), owner_before);
}

#[test]
fn window_checkpoint_restores_alias_content_and_local_metadata_in_place() {
    let mut store = fixture();
    let addressed = target("b", 7);
    let address = std::ptr::from_ref(store.window(&addressed).unwrap());
    let before = store.window(&addressed).unwrap().clone();
    let b_recency = store.session(&name("b")).unwrap().recency();
    let checkpoint = store.checkpoint_window(&addressed).unwrap();
    {
        let mut switch = store.touched_window_switch_mut(&addressed).unwrap();
        switch.select_window();
        switch.resize(size(100, 40), size(100, 38));
    }
    store.touch_activity(&name("a")).unwrap();
    let unrelated_recency = store.session(&name("a")).unwrap().recency();
    store
        .restore_window_checkpoint(&addressed, checkpoint)
        .unwrap();
    assert_eq!(store.session(&name("b")).unwrap().active_window_index(), 0);
    assert_eq!(store.session(&name("b")).unwrap().recency(), b_recency);
    assert_eq!(store.session(&name("b")).unwrap().last_attached_at(), None);
    assert_eq!(
        store.session(&name("a")).unwrap().recency(),
        unrelated_recency
    );
    for alias in [target("a", 0), target("a", 4), addressed] {
        let window = store.window(&alias).unwrap();
        assert_eq!(std::ptr::from_ref(window), address);
        assert_eq!(window, &before);
    }
}

#[test]
fn window_checkpoint_rejects_replaced_occurrence_without_mutating_replacement() {
    let mut store = fixture();
    let addressed = target("b", 7);
    let checkpoint = store.checkpoint_window(&addressed).unwrap();
    let _ = store.remove_window(&addressed).unwrap();
    let id = store.window(&target("b", 0)).unwrap().id();
    let _ = store.insert_alias(&addressed, id).unwrap();
    let before = store.window(&addressed).unwrap().clone();
    assert!(store
        .restore_window_checkpoint(&addressed, checkpoint)
        .is_err());
    assert_eq!(store.window(&addressed).unwrap(), &before);
}

#[test]
fn adjacent_navigation_matches_standalone_sparse_order_and_local_alerts() {
    for forward in [false, true] {
        for alerts_only in [false, true] {
            let mut store = fixture();
            let mut standalone = crate::Session::new(name("a"), size(80, 24));
            standalone
                .insert_window_with_initial_pane(4, size(80, 24))
                .unwrap();
            standalone.add_winlink_alert_flags(4, crate::WINLINK_BELL);
            store
                .add_winlink_alert_flags(&target("a", 4), crate::WINLINK_BELL)
                .unwrap();
            for _ in 0..3 {
                let expected = match (forward, alerts_only) {
                    (true, true) => standalone.next_window_with_alerts(),
                    (true, false) => standalone.next_window(),
                    (false, true) => standalone.previous_window_with_alerts(),
                    (false, false) => standalone.previous_window(),
                };
                let actual = store.select_adjacent_window(&name("a"), forward, alerts_only);
                assert_eq!(
                    actual.as_ref().map_err(ToString::to_string),
                    expected.as_ref().map_err(ToString::to_string)
                );
                let session = store.session(&name("a")).unwrap();
                assert_eq!(
                    session.active_window_index(),
                    standalone.active_window_index()
                );
                assert_eq!(session.last_window_index(), standalone.last_window_index());
                assert_eq!(store.session(&name("b")).unwrap().active_window_index(), 0);
            }
        }
    }
}
