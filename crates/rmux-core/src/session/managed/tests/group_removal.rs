use super::*;

fn grouped_aliases() -> ManagedSessionStore {
    let mut store = ManagedSessionStore::new();
    store.create_session(name("owner"), size()).unwrap();
    store
        .link_window(&target("owner", 4), WindowId::new(0))
        .unwrap();
    store
        .create_grouped_session_with_base_index(name("peer"), size(), 0, name("owner"))
        .unwrap();
    store
}

#[test]
fn grouped_removal_preserves_both_alias_orders_and_transfers_final_owner_once() {
    for indices in [[0, 4], [4, 0]] {
        for addressed in ["owner", "peer"] {
            for outside in [false, true] {
                let mut store = grouped_aliases();
                let pointer = std::ptr::from_ref(store.window(&target("owner", 0)).unwrap());
                if outside {
                    store.create_session(name("outside"), size()).unwrap();
                    store
                        .link_window(&target("outside", 7), WindowId::new(0))
                        .unwrap();
                    store
                        .link_window(&target("outside", 9), WindowId::new(0))
                        .unwrap();
                    store
                        .add_winlink_alert_flags(&target("outside", 7), WINLINK_BELL)
                        .unwrap();
                }
                let external_before = store
                    .session(&name("outside"))
                    .map(|s| format!("{:?} {:?}", s.metadata, s.windows().collect::<Vec<_>>()));
                let removed = store.remove_window(&target(addressed, indices[0])).unwrap();
                assert_eq!(removed.window_id, WindowId::new(0));
                assert!(removed.window.is_none());
                assert!(removed.removed_sessions.is_empty());
                assert_eq!(
                    removed.links,
                    vec![
                        WindowLink {
                            session_id: SessionId::new(0),
                            window_index: indices[0]
                        },
                        WindowLink {
                            session_id: SessionId::new(1),
                            window_index: indices[0]
                        },
                    ]
                );
                for member in ["owner", "peer"] {
                    let s = store.session(&name(member)).unwrap();
                    assert_eq!(
                        s.windows().map(|(i, _)| i).collect::<Vec<_>>(),
                        vec![indices[1]]
                    );
                    assert_eq!(s.active_window_index(), indices[1]);
                    assert_eq!(s.last_window_index(), None);
                    assert_eq!(std::ptr::from_ref(s.window()), pointer);
                    assert_eq!(store.runtime_owner(&name(member)), Some(name("owner")));
                }
                let removed = store.remove_window(&target(addressed, indices[1])).unwrap();
                assert_eq!(
                    removed.removed_sessions,
                    vec![SessionId::new(0), SessionId::new(1)]
                );
                assert_eq!(removed.links.len(), 2);
                assert!(!store.contains_group(&name("owner")));
                assert!(store.runtime_owner(&name("owner")).is_none());
                assert!(store.runtime_owner(&name("peer")).is_none());
                if outside {
                    assert!(removed.window.is_none());
                    assert_eq!(
                        store.session(&name("outside")).map(|s| format!(
                            "{:?} {:?}",
                            s.metadata,
                            s.windows().collect::<Vec<_>>()
                        )),
                        external_before
                    );
                    assert!(store
                        .remove_window(&target("outside", 7))
                        .unwrap()
                        .window
                        .is_none());
                    let final_alias = store.remove_window(&target("outside", 9)).unwrap();
                    assert_eq!(
                        std::ptr::from_ref(final_alias.window.unwrap().as_ref()),
                        pointer
                    );
                    assert!(final_alias.removed_sessions.is_empty());
                    assert_eq!(store.len(), 1);
                } else {
                    assert_eq!(
                        std::ptr::from_ref(removed.window.unwrap().as_ref()),
                        pointer
                    );
                    assert!(store.is_empty());
                }
            }
        }
    }
}

#[test]
fn grouped_remove_then_create_and_reindex_preserves_local_navigation_and_alerts() {
    for addressed in ["owner", "peer"] {
        let mut store = ManagedSessionStore::new();
        store.create_session(name("owner"), size()).unwrap();
        let _ = store
            .insert_window_with_initial_pane(&name("owner"), 4, size())
            .unwrap();
        let _ = store
            .insert_window_with_initial_pane(&name("owner"), 9, size())
            .unwrap();
        store
            .create_grouped_session_with_base_index(name("peer"), size(), 0, name("owner"))
            .unwrap();
        store.select_window(&target("owner", 4)).unwrap();
        store.select_window(&target("owner", 9)).unwrap();
        store.select_window(&target("peer", 9)).unwrap();
        for member in ["owner", "peer"] {
            for index in [0, 4, 9] {
                store
                    .add_winlink_alert_flags(&target(member, index), WINLINK_BELL)
                    .unwrap();
            }
        }
        let pointer = std::ptr::from_ref(store.window(&target("owner", 9)).unwrap());
        let removed = store.remove_window(&target(addressed, 9)).unwrap();
        assert_eq!(
            std::ptr::from_ref(removed.window.unwrap().as_ref()),
            pointer
        );
        assert!(removed.removed_sessions.is_empty());
        // Each member detaches on its own stack (`session.c:346-350`): the owner
        // visited 0 before 4, the peer only ever left 0 to reach 9.
        for (member, active, last, alerted) in [("owner", 4, Some(0), 0), ("peer", 0, None, 4)] {
            let s = store.session(&name(member)).unwrap();
            assert_eq!(s.active_window_index(), active);
            assert_eq!(s.last_window_index(), last);
            assert_eq!(s.winlink_alert_flags(active), AlertFlags::empty());
            assert_eq!(s.winlink_alert_flags(alerted), WINLINK_BELL);
            assert_eq!(s.winlink_alert_flags(9), AlertFlags::empty());
        }
        let _ = store.create_window(&name(addressed), size()).unwrap();
        let _ = store.reindex_windows_from(&name(addressed), 2).unwrap();
        assert_eq!(
            store.index_links(&name("owner")).unwrap(),
            store.index_links(&name("peer")).unwrap()
        );
        for member in ["owner", "peer"] {
            let s = store.session(&name(member)).unwrap();
            assert_eq!(
                s.windows().map(|(i, _)| i).collect::<Vec<_>>(),
                vec![2, 3, 4]
            );
            assert!(s.window_id(s.active_window_index()).is_some());
        }
    }
}

#[test]
fn rejected_group_removal_is_atomic_for_targets_and_inconsistent_peers() {
    let mut store = grouped_aliases();
    store
        .add_winlink_alert_flags(&target("peer", 4), WINLINK_BELL)
        .unwrap();
    let before = format!("{store:?}");
    for t in [target("missing", 0), target("owner", 3), target("peer", 3)] {
        assert!(store.remove_window(&t).is_err());
        assert_eq!(format!("{store:?}"), before);
    }
    unlink_fixture_occurrence(&mut store, &target("peer", 4));
    let before = format!("{store:?}");
    for t in [
        target("owner", 0),
        target("owner", 4),
        target("peer", 0),
        target("peer", 4),
    ] {
        assert!(store.remove_window(&t).is_err());
        assert_eq!(format!("{store:?}"), before);
    }
}

#[test]
fn removing_one_group_session_reports_transfer_and_keeps_peer_usable() {
    for order in [["owner", "peer"], ["peer", "owner"]] {
        let mut store = grouped_aliases();
        store.select_window(&target(order[1], 4)).unwrap();
        store
            .add_winlink_alert_flags(&target(order[1], 0), WINLINK_BELL)
            .unwrap();
        let before = {
            let s = store.session(&name(order[1])).unwrap();
            format!("{:?} {:?}", s.metadata, s.windows().collect::<Vec<_>>())
        };
        let removed = store.remove_session(&name(order[0])).unwrap();
        assert!(removed.windows.is_empty());
        assert_eq!(
            removed.runtime_owner_transfer,
            (order[0] == "owner").then(|| name("peer"))
        );
        assert_eq!(
            {
                let s = store.session(&name(order[1])).unwrap();
                format!("{:?} {:?}", s.metadata, s.windows().collect::<Vec<_>>())
            },
            before
        );
        assert_eq!(store.runtime_owner(&name(order[1])), Some(name(order[1])));
        let _ = store.create_window(&name(order[1]), size()).unwrap();
        let _ = store.reindex_windows_from(&name(order[1]), 2).unwrap();
        let removed = store.remove_session(&name(order[1])).unwrap();
        assert_eq!(removed.windows.len(), 2);
        assert!(removed.runtime_owner_transfer.is_none());
        assert!(!store.contains_group(&name("owner")));
        assert!(store.is_empty());
    }
}

#[test]
fn partial_group_removal_preserves_outside_alias_order() {
    for (owner, peer) in [("zulu", "alpha"), ("alpha", "zulu")] {
        for duplicate_in_group in [false, true] {
            let mut store = ManagedSessionStore::new();
            store.create_session(name("E"), size()).unwrap();
            store.create_session(name(owner), size()).unwrap();
            let w = store.window(&target(owner, 0)).unwrap().id();
            let b = store
                .insert_window_with_initial_pane(&name(owner), 1, size())
                .unwrap();
            for index in [1, 2] {
                store.link_window(&target("E", index), w).unwrap();
            }
            if duplicate_in_group {
                store.link_window(&target(owner, 2), w).unwrap();
            }
            store
                .create_grouped_session_with_base_index(name(peer), size(), 0, name(owner))
                .unwrap();
            let removed = store.remove_window(&target(owner, 0)).unwrap();
            assert!(removed.window.is_none());
            // tmux 3.7b (S2375 `linked4.sh` case 14): the owner is the source,
            // so the peer's surviving duplicate goes after the outsider's links
            // and the owner's own.
            let mut expected = vec![name("E"), name("E")];
            if duplicate_in_group {
                expected.extend([name(owner), name(peer)]);
            }
            assert_eq!(
                store.window_link_order(&target("E", 1)),
                Some(expected),
                "owner={owner}, duplicate_in_group={duplicate_in_group}"
            );
            assert_eq!(
                store.window_link_order(&target(owner, 1)),
                Some(vec![name(owner), name(peer)])
            );
            assert_eq!(store.window(&target(owner, 1)).unwrap().id(), b.window_id);
        }
    }
}

// A link made in a member of a group, then the group synchronised from that
// member: tmux appends the new winlink to its window's list, then empties and
// recreates every other member's winlinks in join order, index by index
// (`session.c:612-626`, `:633-689`). Measured on tmux 3.7b (S2350 `linked.sh`,
// S2365 `linked2.sh`, private sockets) for the shapes below.
fn order_of(store: &ManagedSessionStore, session: &str, index: u32) -> Option<String> {
    let names = store.window_link_order(&target(session, index))?;
    Some(
        names
            .iter()
            .map(SessionName::as_str)
            .collect::<Vec<_>>()
            .join(","),
    )
}

#[test]
fn a_window_made_by_a_peer_names_that_peer_first_and_rebuilds_the_others() {
    // `new -s p; new -t p -s g; neww -t g:3`: tmux names both windows `g,p`.
    let mut store = ManagedSessionStore::new();
    store.create_session(name("p"), size()).unwrap();
    store
        .create_grouped_session_with_base_index(name("g"), size(), 0, name("p"))
        .unwrap();
    let _ = store
        .insert_window_with_initial_pane(&name("g"), 3, size())
        .unwrap();
    for (session, index) in [("g", 3), ("p", 3), ("g", 0), ("p", 0)] {
        assert_eq!(
            order_of(&store, session, index),
            Some("g,p".to_owned()),
            "{session}:{index}"
        );
    }
}

#[test]
fn a_member_linking_its_own_window_names_every_winlink_once() {
    // `new -s p; neww -t p:1; new -t p -s g; link-window -s p:0 -t p:5`:
    // tmux names the twice-linked window `p,p,g,g` and the other `p,g`.
    let mut store = ManagedSessionStore::new();
    store.create_session(name("p"), size()).unwrap();
    let _ = store
        .insert_window_with_initial_pane(&name("p"), 1, size())
        .unwrap();
    store
        .create_grouped_session_with_base_index(name("g"), size(), 0, name("p"))
        .unwrap();
    let shared = store.window(&target("p", 0)).unwrap().id();
    store.link_window(&target("p", 5), shared).unwrap();
    for (session, index) in [("p", 0), ("g", 0), ("p", 5), ("g", 5)] {
        assert_eq!(
            order_of(&store, session, index),
            Some("p,p,g,g".to_owned()),
            "{session}:{index}"
        );
    }
    assert_eq!(order_of(&store, "g", 1), Some("p,g".to_owned()));
}

#[test]
fn an_outside_window_linked_into_a_group_names_its_owner_first() {
    // `new -s p; new -t p -s g; new -s other; link-window -s other:0 -t p:5`:
    // tmux names that window `other,p,g` and the group's own `p,g`.
    let mut store = ManagedSessionStore::new();
    store.create_session(name("p"), size()).unwrap();
    store
        .create_grouped_session_with_base_index(name("g"), size(), 0, name("p"))
        .unwrap();
    store.create_session(name("other"), size()).unwrap();
    let outside = store.window(&target("other", 0)).unwrap().id();
    store.link_window(&target("p", 5), outside).unwrap();
    assert_eq!(order_of(&store, "other", 0), Some("other,p,g".to_owned()));
    assert_eq!(order_of(&store, "g", 5), Some("other,p,g".to_owned()));
    assert_eq!(order_of(&store, "g", 0), Some("p,g".to_owned()));
}

#[test]
fn the_peers_are_rebuilt_in_the_order_they_joined_not_by_name() {
    // `new -s p; new -t p -s h; new -t p -s g; neww -t h:1`: the peers of `h`
    // joined as p then g, and tmux rebuilds them in that order.
    let mut store = ManagedSessionStore::new();
    store.create_session(name("p"), size()).unwrap();
    for member in ["h", "g"] {
        store
            .create_grouped_session_with_base_index(name(member), size(), 0, name("p"))
            .unwrap();
    }
    assert_eq!(order_of(&store, "g", 0), Some("p,h,g".to_owned()));
    let _ = store
        .insert_window_with_initial_pane(&name("h"), 1, size())
        .unwrap();
    assert_eq!(order_of(&store, "p", 1), Some("h,p,g".to_owned()));
    assert_eq!(order_of(&store, "p", 0), Some("h,p,g".to_owned()));
}

// A window that leaves one member of a group, the member surviving: tmux
// detaches it there and synchronises the group from that member
// (`session_detach`, `session.c:340-358`), so every other member's winlinks
// are recreated at the tail of their windows' lists, in join order. Which
// member is the source depends on the command: `unlink-window` detaches from
// the session it addressed, a destroyed window from the group's first member
// by name (`server_kill_window` walks the sessions by name). Measured on tmux
// 3.7b (S2365 `linked2.sh` cases 6 and 7, S2375 `linked4.sh`).

/// `z` holds windows 0, 1 and 2; `a` joins it, so every window names `z,a`.
fn grouped_three_windows() -> ManagedSessionStore {
    let mut store = ManagedSessionStore::new();
    store.create_session(name("z"), size()).unwrap();
    for index in [1, 2] {
        let _ = store
            .insert_window_with_initial_pane(&name("z"), index, size())
            .unwrap();
    }
    store
        .create_grouped_session_with_base_index(name("a"), size(), 0, name("z"))
        .unwrap();
    store
}

#[test]
fn an_unlinked_slot_synchronises_the_group_from_the_session_it_addressed() {
    // `new -s z; neww -t z:1; new -t z -s a; link-window -s z:0 -t z:5;
    // unlink-window -t <addressed>:5`: tmux names both windows with the
    // addressed member first, `z,a` from `z` and `a,z` from `a`.
    for (addressed, expected) in [("z", "z,a"), ("a", "a,z")] {
        let mut store = ManagedSessionStore::new();
        store.create_session(name("z"), size()).unwrap();
        let _ = store
            .insert_window_with_initial_pane(&name("z"), 1, size())
            .unwrap();
        store
            .create_grouped_session_with_base_index(name("a"), size(), 0, name("z"))
            .unwrap();
        let shared = store.window(&target("z", 0)).unwrap().id();
        store.link_window(&target("z", 5), shared).unwrap();
        let removed = store.remove_window(&target(addressed, 5)).unwrap();
        assert!(removed.window.is_none());
        for (session, index) in [("z", 0), ("a", 0), ("z", 1), ("a", 1)] {
            assert_eq!(
                order_of(&store, session, index).as_deref(),
                Some(expected),
                "unlink from {addressed}, read {session}:{index}"
            );
        }
    }
}

#[test]
fn a_destroyed_window_synchronises_the_group_from_its_first_member_by_name() {
    // `new -s z; neww -t z:1; neww -t z:2; new -t z -s a; kill-window -t
    // <either>:2`: tmux names the surviving windows `a,z` whichever member
    // was addressed, and whichever occurrence the caller removes first.
    for walk in [["z", "a"], ["a", "z"]] {
        let mut store = grouped_three_windows();
        for session in walk {
            let _ = store
                .remove_window_occurrence_for_teardown(&target(session, 2))
                .unwrap();
        }
        for (session, index) in [("z", 0), ("a", 0), ("z", 1), ("a", 1)] {
            assert_eq!(
                order_of(&store, session, index).as_deref(),
                Some("a,z"),
                "walk {walk:?}, read {session}:{index}"
            );
        }
    }
}

#[test]
fn a_last_pane_killed_in_a_group_synchronises_from_its_first_member_by_name() {
    // `kill-pane -t <either>:2` on the window's only pane: `a,z` (S2375
    // `linked4.sh` case 10).
    for addressed in ["z", "a"] {
        let mut store = grouped_three_windows();
        let _ = store.kill_pane(&target(addressed, 2), 0).unwrap();
        assert_eq!(order_of(&store, "z", 0).as_deref(), Some("a,z"));
        assert_eq!(order_of(&store, "a", 1).as_deref(), Some("a,z"));
    }
}

#[test]
fn a_group_detach_keeps_an_outsider_where_its_link_put_it() {
    // `new -s z; neww -t z:1; new -s o; link-window -s z:0 -t o:3;
    // new -t z -s a`, then `unlink` or `kill-window`, measured on tmux 3.7b
    // (S2375 `linked4.sh` cases 7 and 8).
    let world = || {
        let mut store = ManagedSessionStore::new();
        store.create_session(name("z"), size()).unwrap();
        let _ = store
            .insert_window_with_initial_pane(&name("z"), 1, size())
            .unwrap();
        store.create_session(name("o"), size()).unwrap();
        let shared = store.window(&target("z", 0)).unwrap().id();
        store.link_window(&target("o", 3), shared).unwrap();
        store
            .create_grouped_session_with_base_index(name("a"), size(), 0, name("z"))
            .unwrap();
        (store, shared)
    };
    // `link-window -s z:0 -t z:5; unlink-window -t z:5`: `z,o,a`.
    let (mut store, shared) = world();
    store.link_window(&target("z", 5), shared).unwrap();
    let _ = store.remove_window(&target("z", 5)).unwrap();
    assert_eq!(order_of(&store, "o", 3).as_deref(), Some("z,o,a"));
    assert_eq!(order_of(&store, "a", 1).as_deref(), Some("z,a"));
    // `kill-window -t z:1`: the source is `a`, so `z` goes after `o`.
    let (mut store, _) = world();
    for session in ["z", "a"] {
        let _ = store
            .remove_window_occurrence_for_teardown(&target(session, 1))
            .unwrap();
    }
    assert_eq!(order_of(&store, "o", 3).as_deref(), Some("o,a,z"));
}
