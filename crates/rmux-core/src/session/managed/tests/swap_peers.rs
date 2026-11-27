use super::*;

fn matching(aliases: usize, groups: [bool; 2]) -> (ManagedSessionStore, SessionStore) {
    let mut managed = ManagedSessionStore::new();
    let mut old = SessionStore::new();
    for s in ["a", "b"] {
        managed.create_session(name(s), size()).unwrap();
        old.create_session(name(s), size()).unwrap();
    }
    for (s, grouped) in ["a", "b"].into_iter().zip(groups) {
        for index in [4, 9].into_iter().take(aliases) {
            let window = old.session(&name(s)).unwrap().window().clone();
            old.session_mut(&name(s))
                .unwrap()
                .link_window(index, window, false, false)
                .unwrap();
            let id = managed.window(&target(s, 0)).unwrap().id();
            managed.link_window(&target(s, index), id).unwrap();
        }
        if grouped {
            let peer = format!("{s}-peer");
            managed
                .create_grouped_session_with_base_index(name(&peer), size(), 0, name(s))
                .unwrap();
            old.create_grouped_session_with_base_index(name(&peer), size(), 0, name(s))
                .unwrap();
        }
    }
    (managed, old)
}

fn assert_matches(managed: &ManagedSessionStore, expected: &Session) {
    let actual = managed.session(expected.name()).unwrap();
    assert_eq!(actual.active_window_index(), expected.active_window_index());
    assert_eq!(actual.last_window_index(), expected.last_window_index());
    assert_eq!(
        actual
            .windows()
            .map(|(i, w)| (i, w.id()))
            .collect::<Vec<_>>(),
        expected
            .windows()
            .iter()
            .map(|(&i, w)| (i, w.id()))
            .collect::<Vec<_>>()
    );
    for &index in expected.windows().keys() {
        assert_eq!(
            actual.winlink_alert_flags(index),
            expected.winlink_alert_flags(index)
        );
    }
    assert_eq!(
        managed.sessions[expected.name()].group_initial_window_id,
        expected.group_initial_window_id
    );
}

fn standalone_swap(old: &mut SessionStore, detached: bool) {
    let flags = ["a", "b"].map(|s| old.session(&name(s)).unwrap().winlink_alert_flags(0));
    let a = old
        .session(&name("a"))
        .unwrap()
        .window_at(0)
        .unwrap()
        .clone();
    let b = old
        .session(&name("b"))
        .unwrap()
        .window_at(0)
        .unwrap()
        .clone();
    old.session_mut(&name("a"))
        .unwrap()
        .replace_window(0, b)
        .unwrap();
    old.session_mut(&name("b"))
        .unwrap()
        .replace_window(0, a)
        .unwrap();
    for (s, incoming) in [("a", flags[1]), ("b", flags[0])] {
        let session = old.session_mut(&name(s)).unwrap();
        session.clear_all_winlink_alert_flags(0);
        session.add_winlink_alert_flags(0, incoming);
        if detached {
            old.session_mut(&name(s)).unwrap().select_window(0).unwrap();
        }
        let source = old.session(&name(s)).unwrap().clone();
        if let Some(peer) = old.session_mut(&name(&format!("{s}-peer"))) {
            peer.synchronize_group_from(&source);
        }
    }
}

#[test]
fn cross_session_swap_peer_navigation_alerts_match_standalone_sequence() {
    swap_peer_matrix([AlertFlags::empty(); 2]);
}

#[test]
fn cross_session_swap_peers_receive_nonempty_incoming_alerts() {
    swap_peer_matrix([WINLINK_BELL, WINLINK_ACTIVITY]);
}

fn swap_peer_matrix(flags: [AlertFlags; 2]) {
    // Includes the supplied third reproduction (one alias, source group,
    // active 0, no history, non-detached), plus both directions and ambiguity.
    for aliases in 0..=2 {
        for groups in [[false, false], [true, false], [false, true], [true, true]] {
            for detached in [false, true] {
                for navigation in [vec![], vec![4], vec![4, 0], vec![0, 4]] {
                    if aliases == 0 && !navigation.is_empty() {
                        continue;
                    }
                    let (mut managed, mut old) = matching(aliases, groups);
                    for s in ["a", "b", "a-peer", "b-peer"] {
                        let Some(session) = old.session_mut(&name(s)) else {
                            continue;
                        };
                        for &index in &navigation {
                            session.select_window(index).unwrap();
                            managed.select_window(&target(s, index)).unwrap();
                        }
                        // Seed peer-local flags independently of addressed flags.
                        if s.ends_with("-peer") {
                            for index in [0, 4, 9].into_iter().take(aliases + 1) {
                                let flags = if index == 0 {
                                    WINLINK_BELL
                                } else {
                                    WINLINK_ACTIVITY
                                };
                                session.add_winlink_alert_flags(index, flags);
                                managed
                                    .add_winlink_alert_flags(&target(s, index), flags)
                                    .unwrap();
                            }
                        }
                    }
                    for (s, flags) in ["a", "b"].into_iter().zip(flags) {
                        old.session_mut(&name(s))
                            .unwrap()
                            .add_winlink_alert_flags(0, flags);
                        managed
                            .add_winlink_alert_flags(&target(s, 0), flags)
                            .unwrap();
                    }
                    standalone_swap(&mut old, detached);
                    managed
                        .swap_window_occurrences_with_selection(
                            &target("a", 0),
                            &target("b", 0),
                            detached,
                        )
                        .unwrap();
                    for (s, incoming) in [("a", flags[1]), ("b", flags[0])] {
                        managed
                            .replace_winlink_alert_flags(&target(s, 0), incoming)
                            .unwrap();
                        if detached {
                            managed.select_window(&target(s, 0)).unwrap();
                        }
                    }
                    for s in ["a", "b", "a-peer", "b-peer"] {
                        if let Some(expected) = old.session(&name(s)) {
                            assert_matches(&managed, expected);
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn cross_session_swap_peer_rollback_restores_metadata_and_owner_identity() {
    let (mut managed, mut old) = matching(1, [true, true]);
    managed.create_session(name("outside"), size()).unwrap();
    for s in ["a", "b", "a-peer", "b-peer"] {
        managed
            .add_winlink_alert_flags(&target(s, 0), WINLINK_BELL)
            .unwrap();
        old.session_mut(&name(s))
            .unwrap()
            .add_winlink_alert_flags(0, WINLINK_BELL);
    }
    let pointers =
        ["a", "b", "outside"].map(|s| std::ptr::from_ref(managed.window(&target(s, 0)).unwrap()));
    let owners = ["a", "b"].map(|s| managed.runtime_owner(&name(s)));
    let undo = managed
        .checkpoint_structure(&[name("a"), name("b")])
        .unwrap();
    managed
        .swap_window_occurrences(&target("a", 0), &target("b", 0))
        .unwrap();
    assert_eq!(
        std::ptr::from_ref(managed.window(&target("a", 0)).unwrap()),
        pointers[1]
    );
    assert_eq!(
        std::ptr::from_ref(managed.window(&target("b-peer", 0)).unwrap()),
        pointers[0]
    );
    managed
        .rename_window(&target("outside", 0), "unrelated edit".to_owned())
        .unwrap();
    managed.restore_structure_checkpoint(undo).unwrap();
    for s in ["a", "b", "a-peer", "b-peer"] {
        assert_matches(&managed, old.session(&name(s)).unwrap());
    }
    for (s, pointer) in ["a", "b", "outside"].into_iter().zip(pointers) {
        assert_eq!(
            std::ptr::from_ref(managed.window(&target(s, 0)).unwrap()),
            pointer
        );
    }
    assert_eq!(
        managed.window(&target("outside", 0)).unwrap().name(),
        Some("unrelated edit")
    );
    assert_eq!(["a", "b"].map(|s| managed.runtime_owner(&name(s))), owners);
}

#[test]
fn cross_session_swap_same_identity_preserves_peer_occurrence_alerts() {
    let (mut managed, _) = matching(2, [true, true]);
    let id = managed.window(&target("a", 0)).unwrap().id();
    let _ = managed
        .install_window_alias(&target("b", 0), id, true, false)
        .unwrap();
    for s in ["a-peer", "b-peer"] {
        managed
            .add_winlink_alert_flags(&target(s, 0), WINLINK_BELL)
            .unwrap();
    }
    for s in ["a", "b"] {
        managed
            .add_winlink_alert_flags(&target(s, 0), WINLINK_ACTIVITY)
            .unwrap();
    }
    managed
        .swap_window_occurrences(&target("a", 0), &target("b", 0))
        .unwrap();
    for s in ["a-peer", "b-peer"] {
        let view = managed.session(&name(s)).unwrap();
        assert_eq!(view.active_window_index(), 0);
        assert_eq!(view.winlink_alert_flags(0), WINLINK_BELL);
    }
}

#[test]
fn cross_session_swap_invalid_targets_and_same_group_leave_peers_unchanged() {
    let (mut managed, old) = matching(1, [true, true]);
    for (source, destination) in [
        (target("a", 0), target("missing", 0)),
        (target("a", 0), target("b", 99)),
        (target("a", 99), target("b", 0)),
        (target("a", 0), target("a-peer", 4)),
        (target("a", 0), target("a", 4)),
    ] {
        assert!(managed
            .swap_window_occurrences(&source, &destination)
            .is_err());
        for s in ["a", "b", "a-peer", "b-peer"] {
            assert_matches(&managed, old.session(&name(s)).unwrap());
        }
    }
}
