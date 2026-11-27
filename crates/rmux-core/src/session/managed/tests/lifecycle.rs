use super::*;

#[test]
fn grouped_variants_reuse_owners_and_template_local_state() {
    let mut store = fixture();
    store.select_window(&target("a", 9)).unwrap();
    store
        .add_winlink_alert_flags(&target("a", 4), WINLINK_BELL)
        .unwrap();
    store
        .set_cwd(&name("a"), Some(PathBuf::from("work")))
        .unwrap();
    let pointer = std::ptr::from_ref(store.window(&target("a", 0)).unwrap());
    let mut legacy = SessionStore::new();
    legacy.create_session(name("a"), size()).unwrap();
    let expected = legacy
        .create_grouped_session_with_base_index(name("peer"), size(), 99, name("a"))
        .unwrap();
    let result = store
        .create_grouped_session_with_base_index(
            name("peer"),
            TerminalSize::new(10, 10),
            99,
            name("a"),
        )
        .unwrap();
    assert_eq!(result, expected);
    let peer = store.session(&name("peer")).unwrap();
    assert_eq!(peer.terminal_size(), size());
    assert_eq!(peer.active_window_index(), 9);
    // A new group member has visited nothing of its own yet.
    assert_eq!(peer.last_window_index(), None);
    assert_eq!(peer.winlink_alert_flags(4), WINLINK_BELL);
    assert_eq!(peer.cwd(), Some(std::path::Path::new("work")));
    for (_, window) in peer.windows() {
        assert_eq!(std::ptr::from_ref(window), pointer);
    }
    store.rename_session(&name("a"), name("owner")).unwrap();
    assert_eq!(store.runtime_owner(&name("peer")), Some(name("owner")));
    assert_eq!(store.session_group_name(&name("owner")), Some(&name("a")));
    let result = store
        .create_grouped_session_with_base_index(name("third"), size(), 77, name("a"))
        .unwrap();
    assert_eq!(result.template_session, Some(name("owner")));
    let result = store
        .create_grouped_session_with_base_index(name("fresh"), size(), 77, name("new-group"))
        .unwrap();
    assert_eq!(result.template_session, None);
    assert_eq!(
        store.session(&name("fresh")).unwrap().active_window_index(),
        77
    );
    assert_eq!(
        store.session(&name("fresh")).unwrap().window().id(),
        WindowId::new(2)
    );
    assert_eq!(
        store
            .session(&name("fresh"))
            .unwrap()
            .active_pane()
            .unwrap()
            .id(),
        PaneId::new(2)
    );
}

#[test]
fn both_session_removal_orders_transfer_each_final_box_once() {
    for order in [["owner", "peer"], ["peer", "owner"]] {
        let mut store = ManagedSessionStore::new();
        store.create_session(name("owner"), size()).unwrap();
        store
            .link_window(&target("owner", 4), WindowId::new(0))
            .unwrap();
        store
            .create_grouped_session_with_base_index(name("peer"), size(), 0, name("owner"))
            .unwrap();
        let pointer = std::ptr::from_ref(store.window(&target("owner", 0)).unwrap());
        assert_eq!(
            store.runtime_owner_transfer_target(&name("owner")),
            Some(name("peer"))
        );
        assert!(store
            .remove_session(&name(order[0]))
            .unwrap()
            .windows
            .is_empty());
        assert_eq!(store.runtime_owner(&name(order[1])), Some(name(order[1])));
        assert_eq!(
            std::ptr::from_ref(store.window(&target(order[1], 4)).unwrap()),
            pointer
        );
        let removed = store.remove_session(&name(order[1])).unwrap();
        assert_eq!(removed.windows.len(), 1);
        assert_eq!(std::ptr::from_ref(removed.windows[0].as_ref()), pointer);
        assert!(store.is_empty());
        assert!(!store.contains_group(&name("owner")));
    }
}

#[test]
fn occurrence_removal_preserves_aliases_and_removes_final_session() {
    for indices in [[0, 4], [4, 0]] {
        let mut store = ManagedSessionStore::new();
        store.create_session(name("a"), size()).unwrap();
        store
            .link_window(&target("a", 4), WindowId::new(0))
            .unwrap();
        let pointer = std::ptr::from_ref(store.window(&target("a", 0)).unwrap());
        let removed = store.remove_window(&target("a", indices[0])).unwrap();
        assert!(removed.window.is_none());
        assert!(removed.removed_sessions.is_empty());
        assert_eq!(
            store.session(&name("a")).unwrap().active_window_index(),
            indices[1]
        );
        assert_eq!(
            std::ptr::from_ref(store.window(&target("a", indices[1])).unwrap()),
            pointer
        );
        let removed = store.remove_window(&target("a", indices[1])).unwrap();
        assert_eq!(removed.removed_sessions, vec![SessionId::new(0)]);
        assert_eq!(
            std::ptr::from_ref(removed.window.unwrap().as_ref()),
            pointer
        );
        assert!(store.is_empty());
    }
}

#[test]
fn checked_counters_group_without_phantom_allocations_and_reject_atomically() {
    for exhausted in ["session", "window", "pane"] {
        let mut store = fixture();
        match exhausted {
            "session" => store.next_session_id = u64::from(u32::MAX) + 1,
            "window" => store.next_window_id = u64::from(u32::MAX) + 1,
            _ => store.next_pane_id = u64::from(u32::MAX) + 1,
        }
        let before = format!("{store:?}");
        assert!(store.create_session(name("fail"), size()).is_err());
        assert!(store
            .create_grouped_session_with_base_index(name("fail"), size(), 0, name("new"))
            .is_err());
        assert!(store.remove_window(&target("a", 88)).is_err());
        assert!(store.remove_window(&target("absent", 0)).is_err());
        assert!(store.remove_session(&name("absent")).is_err());
        assert!(store
            .create_grouped_session_with_base_index(name("a"), size(), 0, name("b"))
            .is_err());
        assert_eq!(format!("{store:?}"), before);
        let result =
            store.create_grouped_session_with_base_index(name("peer"), size(), 0, name("a"));
        assert_eq!(result.is_err(), exhausted == "session");
    }
}

#[test]
fn automatic_names_skip_collisions_and_roll_back_on_exhaustion() {
    let mut store = ManagedSessionStore::new();
    store.create_session(name("1"), size()).unwrap();
    assert_eq!(
        store
            .create_auto_named_session_with_base_index(size(), 0)
            .unwrap(),
        name("2")
    );
    let peer = store
        .create_auto_grouped_session_with_base_index(size(), 99, name("1"))
        .unwrap();
    assert_eq!(peer.session_name, name("1-3"));
    assert_eq!(store.next_window_id, 2);
    assert_eq!(store.next_pane_id, 2);
    store.next_pane_id = u64::from(u32::MAX) + 1;
    let before = format!("{store:?}");
    assert!(store
        .create_auto_named_session_with_base_index(size(), 0)
        .is_err());
    assert!(store
        .create_auto_grouped_session_with_base_index(size(), 0, name("new"))
        .is_err());
    assert_eq!(format!("{store:?}"), before);
}

#[test]
fn automatic_grouped_names_append_to_the_stored_backslash_group_name() {
    // tmux 3.7b: `new -s 'b\c'` stores `b\\c`, then `new -t 'b\\c'` names
    // the peer `b\\c-1`; the stored group name is not escaped again.
    let group = name("b\\c");
    assert_eq!(group.as_str(), "b\\\\c");
    let mut managed = ManagedSessionStore::new();
    managed.create_session(group.clone(), size()).unwrap();
    let peer = managed
        .create_auto_grouped_session_with_base_index(size(), 0, group.clone())
        .unwrap();
    assert_eq!(peer.session_name.as_str(), "b\\\\c-1");

    let mut legacy = SessionStore::new();
    legacy.create_session(group.clone(), size()).unwrap();
    assert_eq!(
        legacy.next_grouped_session_name(&group).as_str(),
        "b\\\\c-1"
    );
    let peer = legacy
        .create_auto_grouped_session_with_base_index(size(), 0, group)
        .unwrap();
    assert_eq!(peer.session_name.as_str(), "b\\\\c-1");
}

#[test]
fn automatic_names_in_a_new_group_escape_the_target_text_as_their_prefix() {
    // tmux 3.7b (`cmd-new-session.c`): `new -t 'u\v'` on a group that does not
    // exist yet keeps `u\v` as the group name, but prefixes the session with
    // `clean_name(group)`, giving `u\\v-<id>`. Once the group exists, its stored
    // name is the prefix as is (`u\v-<id>`). `w\\x` is escaped again likewise.
    let raw = |text: &str| SessionName::from_canonical(text).unwrap();
    let mut managed = ManagedSessionStore::new();
    let mut legacy = SessionStore::new();
    for (target, group, prefix) in [
        ("u\\v", "u\\v", "u\\\\v"),
        ("w\\\\x", "w\\\\x", "w\\\\\\\\x"),
    ] {
        let created = managed
            .create_auto_grouped_session_with_base_index(size(), 0, raw(target))
            .unwrap();
        let expected = legacy
            .create_auto_grouped_session_with_base_index(size(), 0, raw(target))
            .unwrap();
        for created in [created, expected] {
            assert_eq!(created.group_name.as_str(), group);
            let suffix = created.session_name.as_str().strip_prefix(prefix);
            assert!(
                suffix.is_some_and(|suffix| suffix.starts_with('-')),
                "{} does not start with {prefix}-",
                created.session_name
            );
        }
    }
    let peer = managed
        .create_auto_grouped_session_with_base_index(size(), 0, raw("u\\v"))
        .unwrap();
    assert_eq!(peer.session_name.as_str(), "u\\v-2");
    let peer = legacy
        .create_auto_grouped_session_with_base_index(size(), 0, raw("u\\v"))
        .unwrap();
    assert_eq!(peer.session_name.as_str(), "u\\v-2");
}

#[test]
fn removal_selection_reuses_standalone_group_expectations() {
    for navigate in [false, true] {
        let mut legacy = Session::new(name("source"), size());
        legacy.create_window(size()).unwrap();
        legacy.create_window(size()).unwrap();
        let mut store = ManagedSessionStore::new();
        for session in ["source", "one", "two"] {
            store.create_session(name(session), size()).unwrap();
        }
        store
            .link_window(&target("source", 1), WindowId::new(1))
            .unwrap();
        store
            .link_window(&target("source", 2), WindowId::new(2))
            .unwrap();
        store.select_window(&target("source", 1)).unwrap();
        store.select_window(&target("source", 2)).unwrap();
        legacy.select_window(1).unwrap();
        legacy.select_window(2).unwrap();
        let mut peer =
            legacy.clone_as_group_member(name("peer"), name("source"), SessionId::new(3));
        store
            .create_grouped_session_with_base_index(name("peer"), size(), 0, name("source"))
            .unwrap();
        // Remove inherited last selection, leaving the group-initial fallback.
        peer.remove_window(1).unwrap();
        let _ = store.remove_window(&target("peer", 1)).unwrap();
        if navigate {
            peer.select_window(0).unwrap();
            peer.select_window(2).unwrap();
            store.select_window(&target("peer", 0)).unwrap();
            store.select_window(&target("peer", 2)).unwrap();
        }
        peer.remove_window(2).unwrap();
        let removed = store.remove_window(&target("peer", 2)).unwrap();
        assert!(removed.window.is_none());
        let actual = store.session(&name("peer")).unwrap();
        assert_eq!(actual.active_window_index(), peer.active_window_index());
        assert_eq!(actual.last_window_index(), peer.last_window_index());
        assert_eq!(
            store
                .session(&name("source"))
                .unwrap()
                .active_window_index(),
            0
        );
        assert_eq!(store.session(&name("source")).unwrap().windows().count(), 1);
    }
}

#[test]
fn final_independent_window_and_pane_ids_are_issued_once() {
    let mut store = ManagedSessionStore {
        next_window_id: u64::from(u32::MAX),
        next_pane_id: u64::from(u32::MAX),
        ..Default::default()
    };
    store.create_session(name("a"), size()).unwrap();
    assert_eq!(
        store.session(&name("a")).unwrap().window().id(),
        WindowId::new(u32::MAX)
    );
    assert_eq!(
        store
            .session(&name("a"))
            .unwrap()
            .active_pane()
            .unwrap()
            .id(),
        PaneId::new(u32::MAX)
    );
    store
        .create_grouped_session_with_base_index(name("peer"), size(), 0, name("a"))
        .unwrap();
    let before = format!("{store:?}");
    assert!(store.create_session(name("b"), size()).is_err());
    assert_eq!(format!("{store:?}"), before);
}
