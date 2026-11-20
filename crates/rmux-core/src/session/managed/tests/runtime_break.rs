use super::*;
use crate::WINLINK_SILENCE;

#[test]
fn cross_session_single_break_seeds_only_new_peer_alerts_and_rolls_back() {
    for destination in ["b", "peer"] {
        for (after, before) in [(false, false), (true, false), (false, true)] {
            for detached in [false, true] {
                let mut store = fixture();
                store
                    .create_grouped_session_with_base_index(name("peer"), size(), 0, name("b"))
                    .unwrap();
                for (member, index, flags) in [
                    ("a", 0, WINLINK_SILENCE),
                    ("a", 4, WINLINK_BELL),
                    ("b", 7, WINLINK_ACTIVITY),
                    ("peer", 7, WINLINK_BELL),
                ] {
                    store
                        .add_winlink_alert_flags(&target(member, index), flags)
                        .unwrap();
                }
                let names = [name("a"), name("b"), name("peer")];
                let previous = names
                    .iter()
                    .map(|n| store.session(n).unwrap().test_snapshot())
                    .collect::<Vec<_>>();
                let undo = store
                    .checkpoint_structure(&[name("a"), name(destination)])
                    .unwrap();
                let result = store
                    .break_pane_for_runtime_transfer(
                        &target("a", 0),
                        0,
                        &name(destination),
                        crate::BreakPaneOptions::new(
                            Some(if after || before { 2 } else { 3 }),
                            None,
                            detached,
                            after,
                            before,
                        ),
                    )
                    .unwrap();
                for member in ["b", "peer"] {
                    let session = store.session(&name(member)).unwrap();
                    assert_eq!(
                        session.winlink_alert_flags(result.window_index),
                        if member == destination && !detached {
                            AlertFlags::empty()
                        } else {
                            WINLINK_SILENCE
                        }
                    );
                    assert_eq!(
                        session.winlink_alert_flags(if after || before { 8 } else { 7 }),
                        if member == "b" {
                            WINLINK_ACTIVITY
                        } else {
                            WINLINK_BELL
                        }
                    );
                    if member == destination && !detached {
                        assert_eq!(session.active_window_index(), result.window_index);
                    } else {
                        assert_eq!(session.active_window_index(), if before { 3 } else { 2 });
                    }
                }
                assert_eq!(
                    store.session(&name("a")).unwrap().winlink_alert_flags(4),
                    WINLINK_BELL
                );
                store.restore_structure_checkpoint(undo).unwrap();
                for (member, snapshot) in names.iter().zip(previous) {
                    assert_eq!(store.session(member).unwrap().test_snapshot(), snapshot);
                }
            }
        }
    }
}

#[test]
fn runtime_single_break_preserves_non_source_aliases_and_the_canonical_owner() {
    for (after, before) in [(false, false), (true, false), (false, true)] {
        let mut store = fixture();
        store
            .create_grouped_session_with_base_index(name("source-peer"), size(), 0, name("a"))
            .unwrap();
        store
            .create_grouped_session_with_base_index(name("destination-peer"), size(), 0, name("b"))
            .unwrap();
        let id = store.window(&target("a", 0)).unwrap().id();
        let address = std::ptr::from_ref(store.window(&target("a", 0)).unwrap());
        let destination = if after || before { 2 } else { 3 };
        let result = store
            .break_pane_for_runtime_transfer(
                &target("a", 0),
                0,
                &name("b"),
                crate::BreakPaneOptions::new(Some(destination), None, true, after, before),
            )
            .unwrap();
        for member in ["a", "source-peer"] {
            assert!(store.window(&target(member, 0)).is_err());
            for index in [4, 9] {
                assert_eq!(store.window(&target(member, index)).unwrap().id(), id);
            }
            let view = store.session(&name(member)).unwrap();
            assert!(view.window_at(view.active_window_index()).is_some());
        }
        for member in ["b", "destination-peer"] {
            let preserved = if after || before { 8 } else { 7 };
            for index in [preserved, result.window_index] {
                let window = store.window(&target(member, index)).unwrap();
                assert_eq!(window.id(), id);
                assert_eq!(std::ptr::from_ref(window), address);
            }
        }
        let removed = result.transfer.destroyed_window.unwrap();
        assert_eq!(removed.links.len(), 2);
        assert!(removed.removed_sessions.is_empty());
        assert!(removed.window.is_none());
    }
}

#[test]
fn runtime_single_break_moves_the_selected_group_peer_occurrence() {
    for duplicate in [false, true] {
        let mut store = ManagedSessionStore::new();
        store.create_session(name("a"), size()).unwrap();
        let _ = store.create_window(&name("a"), size()).unwrap();
        let source = if duplicate {
            let id = store.window(&target("a", 1)).unwrap().id();
            store.link_window(&target("a", 2), id).unwrap();
            2
        } else {
            1
        };
        store
            .create_grouped_session_with_base_index(name("peer"), size(), 0, name("a"))
            .unwrap();
        store.select_window(&target("peer", source)).unwrap();
        let id = store.window(&target("a", source)).unwrap().id();
        let result = store
            .break_pane_for_runtime_transfer(
                &target("a", source),
                0,
                &name("a"),
                crate::BreakPaneOptions::new(Some(0), None, true, false, true),
            )
            .unwrap();
        let peer = store.session(&name("peer")).unwrap();
        assert_eq!(result.window_index, 0);
        assert_eq!(peer.active_window_index(), 0);
        assert_eq!(peer.window().id(), id);
        assert_eq!(store.session(&name("a")).unwrap().active_window_index(), 1);
    }
}

#[test]
fn runtime_single_break_preserves_peer_alerts_navigation_and_rollback() {
    for duplicate in [false, true] {
        for (after, before) in [(false, false), (false, true), (true, false)] {
            for detached in [false, true] {
                for active in [0, 4, 9] {
                    let mut store = ManagedSessionStore::new();
                    store.create_session(name("a"), size()).unwrap();
                    let _ = store
                        .insert_window_with_initial_pane(&name("a"), 4, size())
                        .unwrap();
                    if duplicate {
                        let id = store.window(&target("a", 4)).unwrap().id();
                        store.link_window(&target("a", 9), id).unwrap();
                    } else {
                        let _ = store
                            .insert_window_with_initial_pane(&name("a"), 9, size())
                            .unwrap();
                    }
                    for member in ["peer", "other-peer"] {
                        store
                            .create_grouped_session_with_base_index(
                                name(member),
                                size(),
                                0,
                                name("a"),
                            )
                            .unwrap();
                        store.select_window(&target(member, 4)).unwrap();
                        store.select_window(&target(member, active)).unwrap();
                        let flags = if member == "peer" {
                            [WINLINK_ACTIVITY, WINLINK_BELL, WINLINK_SILENCE]
                        } else {
                            [WINLINK_SILENCE, WINLINK_ACTIVITY, WINLINK_BELL]
                        };
                        for (index, flags) in [0, 4, 9].into_iter().zip(flags) {
                            store
                                .add_winlink_alert_flags(&target(member, index), flags)
                                .unwrap();
                        }
                    }
                    let names = [name("a"), name("peer"), name("other-peer")];
                    let original = names
                        .iter()
                        .map(|n| store.session(n).unwrap().test_snapshot())
                        .collect::<Vec<_>>();
                    let undo = store.checkpoint_structure(&[name("a")]).unwrap();
                    let pointer = std::ptr::from_ref(store.window(&target("a", 4)).unwrap());
                    let result = store
                        .break_pane_for_runtime_transfer(
                            &target("a", 4),
                            0,
                            &name("a"),
                            crate::BreakPaneOptions::new(
                                Some(if after || before { 0 } else { 7 }),
                                None,
                                detached,
                                after,
                                before,
                            ),
                        )
                        .unwrap();
                    let moved = result.window_index;
                    let remap = |index| {
                        if index == 4 {
                            moved
                        } else if before || (after && index > 0) {
                            index + 1
                        } else {
                            index
                        }
                    };
                    for (member, previous) in names.iter().zip(&original).skip(1) {
                        let peer = store.session(member).unwrap();
                        assert_eq!(peer.active_window_index(), remap(active));
                        // A break links then unlinks (`cmd-break-pane.c:81-93`)
                        // and both halves end in the group synchronisation that
                        // rebuilds every peer from the indices it already had,
                        // dropping the ones that went away (`session.c:335,353`
                        // and `session.c:663-678`). The occurrence a break
                        // relocates is no exception for that peer, so for initial
                        // actives 0, 4 and 9 the primary leaves, with no shift,
                        // None / Some(0) / Some(0); with -b at 0, None / None /
                        // Some(0); with -a at 0, None / Some(0) / Some(0). These
                        // are written from the primary, never read back from the
                        // session under test.
                        let expected_last = if active == 0 || (before && active == 4) {
                            None
                        } else {
                            Some(0)
                        };
                        assert_eq!(peer.last_window_index(), expected_last);
                        for old in [0, 4, 9] {
                            assert_eq!(
                                peer.winlink_alert_flags(remap(old)),
                                previous.metadata.winlink_alert_flags[&old]
                            );
                        }
                        assert_eq!(std::ptr::from_ref(peer.window_at(moved).unwrap()), pointer);
                    }
                    store.restore_structure_checkpoint(undo).unwrap();
                    for (member, previous) in names.iter().zip(original) {
                        assert_eq!(store.session(member).unwrap().test_snapshot(), previous);
                    }
                    assert_eq!(
                        std::ptr::from_ref(store.window(&target("a", 4)).unwrap()),
                        pointer
                    );
                }
            }
        }
    }
}
