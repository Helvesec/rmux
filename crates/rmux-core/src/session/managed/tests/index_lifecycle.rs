use super::*;

fn matching_group(duplicate: bool) -> (ManagedSessionStore, Session, Session) {
    let mut managed = ManagedSessionStore::new();
    managed.create_session(name("a"), size()).unwrap();
    let mut source = Session::new(name("a"), size());
    for index in [1, 2, 4] {
        let _ = managed
            .insert_window_with_initial_pane(&name("a"), index, size())
            .unwrap();
        source
            .insert_window_with_initial_pane(index, size())
            .unwrap();
    }
    if duplicate {
        let id = managed.window(&target("a", 0)).unwrap().id();
        managed.link_window(&target("a", 3), id).unwrap();
        source
            .insert_existing_window(3, source.window_at(0).unwrap().clone())
            .unwrap();
    }
    managed
        .create_grouped_session_with_base_index(name("peer"), size(), 0, name("a"))
        .unwrap();
    let peer = source.clone_as_group_member(name("peer"), name("a"), SessionId::new(1));
    (managed, source, peer)
}

fn assert_local(managed: &ManagedSessionStore, expected: &Session) {
    let view = managed.session(expected.name()).unwrap();
    assert_eq!(view.active_window_index(), expected.active_window_index());
    assert_eq!(view.last_window_index(), expected.last_window_index());
    assert_eq!(
        view.windows().map(|(i, w)| (i, w.id())).collect::<Vec<_>>(),
        expected
            .windows()
            .iter()
            .map(|(&i, w)| (i, w.id()))
            .collect::<Vec<_>>()
    );
    for (&index, window) in expected.windows() {
        assert_eq!(
            view.winlink_alert_flags(index),
            expected.winlink_alert_flags(index)
        );
        let pane_state = |window: &Window| {
            window
                .panes()
                .iter()
                .map(|pane| (pane.id(), pane.geometry()))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            pane_state(view.window_at(index).unwrap()),
            pane_state(window)
        );
    }
    assert_eq!(
        managed.sessions[expected.name()].group_initial_window_id,
        expected.group_initial_window_id
    );
}

fn seed_local(managed: &mut ManagedSessionStore, session: &mut Session, active: u32, last: u32) {
    session.select_window(last).unwrap();
    session.select_window(active).unwrap();
    managed
        .select_window(&WindowTarget::with_window(session.name().clone(), last))
        .unwrap();
    managed
        .select_window(&WindowTarget::with_window(session.name().clone(), active))
        .unwrap();
    for index in session.windows().keys().copied().collect::<Vec<_>>() {
        let flags = if index % 2 == 0 {
            WINLINK_BELL
        } else {
            WINLINK_ACTIVITY
        };
        session.add_winlink_alert_flags(index, flags);
        managed
            .add_winlink_alert_flags(
                &WindowTarget::with_window(session.name().clone(), index),
                flags,
            )
            .unwrap();
    }
}

#[test]
fn managed_move_matrix_reuses_standalone_and_group_expected_results() {
    for duplicate in [false, true] {
        for source_index in [0, 1, 4] {
            for destination in [0, 2, 5] {
                for select in [false, true] {
                    for active in [0, 1, 2, 4] {
                        for last in [0, 2, 4] {
                            let (mut managed, mut source, mut peer) = matching_group(duplicate);
                            seed_local(&mut managed, &mut source, active, last);
                            seed_local(&mut managed, &mut peer, last, active);
                            let addresses = managed
                                .session(&name("a"))
                                .unwrap()
                                .windows()
                                .map(|(_, w)| (w.id(), std::ptr::from_ref(w)))
                                .collect::<BTreeMap<_, _>>();
                            let outcome = managed
                                .move_window(&name("a"), source_index, destination, true, select)
                                .unwrap();
                            let removed = source
                                .move_window(source_index, destination, true, select)
                                .unwrap();
                            peer.synchronize_group_from_with_winlink_alert_map(
                                &source,
                                &BTreeMap::from([(source_index, destination)]),
                            );
                            assert_local(&managed, &source);
                            assert_local(&managed, &peer);
                            for view in managed.iter() {
                                for (_, w) in view.windows() {
                                    assert_eq!(std::ptr::from_ref(w), addresses[&w.id()]);
                                }
                            }
                            let final_id = removed
                                .map(|w| w.id())
                                .filter(|id| !source.windows().values().any(|w| w.id() == *id));
                            assert_eq!(
                                outcome
                                    .removed_windows
                                    .iter()
                                    .map(|w| w.id())
                                    .collect::<Vec<_>>(),
                                final_id.into_iter().collect::<Vec<_>>()
                            );
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn managed_swap_room_and_reindex_reuse_existing_group_maps() {
    for duplicate in [false, true] {
        for operation in 0..3 {
            let (mut managed, mut source, mut peer) = matching_group(duplicate);
            seed_local(&mut managed, &mut source, 2, 4);
            seed_local(&mut managed, &mut peer, 1, 0);
            match operation {
                0 => {
                    let destination = if duplicate { 3 } else { 4 };
                    let _ = managed.swap_windows(&name("a"), 0, destination).unwrap();
                    source.swap_windows(0, destination).unwrap();
                    peer.synchronize_group_from_with_window_selection_and_winlink_alert_maps(
                        &source,
                        &BTreeMap::new(),
                        &BTreeMap::from([(0, destination), (destination, 0)]),
                    );
                }
                1 => {
                    let _ = managed.make_room_for_window(&name("a"), 0).unwrap();
                    let map = source.make_room_for_window(0).unwrap();
                    peer.synchronize_group_from_with_window_selection_map(&source, &map);
                }
                _ => {
                    let _ = managed.reindex_windows_from(&name("a"), 2).unwrap();
                    let map = source.reindex_windows_from(2).unwrap();
                    // `server_renumber_session` renumbers every member of the
                    // group in turn (`server-fn.c:229-235`), so the peer keeps
                    // its own occurrences and its visits travel with them.
                    assert_eq!(peer.reindex_windows_from(2).unwrap(), map);
                }
            }
            assert_local(&managed, &source);
            assert_local(&managed, &peer);
        }
    }
}

#[test]
fn managed_creation_aliases_and_names_share_original_allocations() {
    let (mut managed, _, _) = matching_group(true);
    managed.create_session(name("outside"), size()).unwrap();
    let old = managed.window(&target("a", 0)).unwrap().id();
    managed.link_window(&target("outside", 9), old).unwrap();
    let pointer = std::ptr::from_ref(managed.window(&target("a", 0)).unwrap());
    let created = managed
        .create_window_at_or_above(&name("peer"), size(), 3)
        .unwrap();
    assert_eq!(created.window_index, 5);
    assert_eq!(created.change.sessions.len(), 2);
    assert_ne!(
        created.window_id.as_u32(),
        managed.session(&name("peer")).unwrap().id().as_u32()
    );
    let pane_ids = managed
        .iter()
        .flat_map(|s| {
            s.windows()
                .map(|(_, w)| (w.id(), w.panes()[0].id()))
                .collect::<Vec<_>>()
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        pane_ids.len(),
        pane_ids
            .values()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
    );
    let _ = managed
        .rename_window(&target("outside", 9), "manual".to_owned())
        .unwrap();
    let ignored = managed
        .set_automatic_window_name(&target("peer", 3), "ignored".to_owned())
        .unwrap();
    assert_eq!(ignored.name.as_deref(), Some("manual"));
    assert!(!ignored.automatic_rename);
    let automatic = managed
        .set_automatic_window_name(&target("a", 5), "automatic".to_owned())
        .unwrap();
    assert!(automatic.automatic_rename);
    assert_eq!(
        managed.window(&target("peer", 5)).unwrap().name(),
        Some("automatic")
    );
    let change = managed.move_window(&name("a"), 0, 3, true, false).unwrap();
    assert!(change.removed_windows.is_empty());
    for t in [target("a", 3), target("peer", 3), target("outside", 9)] {
        assert_eq!(std::ptr::from_ref(managed.window(&t).unwrap()), pointer);
    }
    let _ = managed
        .insert_alias(&target("peer", 8), created.window_id)
        .unwrap();
    assert!(std::ptr::eq(
        managed.window(&target("a", 8)).unwrap(),
        managed.window(&target("peer", 5)).unwrap()
    ));
    assert!(managed.window(&target("outside", 8)).is_err());
}

#[test]
fn managed_rejections_preserve_every_table_counter_and_allocation() {
    let (mut managed, _, _) = matching_group(true);
    let before = format!("{managed:?}");
    assert!(managed
        .insert_window_with_initial_pane(&name("peer"), 0, size())
        .is_err());
    assert!(managed.move_window(&name("a"), 0, 2, false, false).is_err());
    assert!(managed.move_window(&name("a"), 99, 2, true, true).is_err());
    assert!(managed.swap_windows(&name("a"), 0, 99).is_err());
    assert!(managed.reindex_windows_from(&name("a"), u32::MAX).is_err());
    assert!(managed
        .insert_alias(&target("a", 8), WindowId::new(999))
        .is_err());
    assert!(managed
        .rename_window(&target("a", 99), "bad".to_owned())
        .is_err());
    assert!(managed
        .set_automatic_window_name(&target("a", 99), "bad".to_owned())
        .is_err());
    assert!(managed.create_window(&name("missing"), size()).is_err());
    assert_eq!(format!("{managed:?}"), before);
    let _ = managed
        .insert_window_with_initial_pane(&name("a"), u32::MAX, size())
        .unwrap();
    let before = format!("{managed:?}");
    assert!(managed
        .make_room_for_window(&name("peer"), u32::MAX)
        .is_err());
    assert!(managed
        .create_window_at_or_above(&name("peer"), size(), u32::MAX)
        .is_err());
    assert_eq!(format!("{managed:?}"), before);
    unlink_fixture_occurrence(&mut managed, &target("peer", 4));
    let before = format!("{managed:?}");
    assert!(managed.create_window(&name("a"), size()).is_err());
    assert!(managed.swap_windows(&name("a"), 0, 1).is_err());
    assert_eq!(format!("{managed:?}"), before);
}

#[test]
fn managed_checked_independent_window_and_pane_ids_and_maximum_index() {
    for window_exhausted in [false, true] {
        let mut managed = ManagedSessionStore::new();
        managed.create_session(name("a"), size()).unwrap();
        managed.next_window_id = if window_exhausted {
            u64::from(u32::MAX)
        } else {
            17
        };
        managed.next_pane_id = if window_exhausted {
            31
        } else {
            u64::from(u32::MAX)
        };
        let last = managed
            .insert_window_with_initial_pane(&name("a"), u32::MAX, size())
            .unwrap();
        assert_eq!(
            last.window_id.as_u32(),
            if window_exhausted { u32::MAX } else { 17 }
        );
        assert_eq!(
            last.pane_id.as_u32(),
            if window_exhausted { 31 } else { u32::MAX }
        );
        let before = format!("{managed:?}");
        assert!(managed.create_window(&name("a"), size()).is_err());
        assert_eq!(format!("{managed:?}"), before);
    }
    let mut managed = ManagedSessionStore::new();
    managed.create_session(name("a"), size()).unwrap();
    let address = std::ptr::from_ref(managed.window(&target("a", 0)).unwrap());
    let change = managed.reindex_windows_from(&name("a"), u32::MAX).unwrap();
    assert_eq!(
        change.sessions[0].index_map,
        BTreeMap::from([(0, u32::MAX)])
    );
    assert_eq!(
        std::ptr::from_ref(managed.window(&target("a", u32::MAX)).unwrap()),
        address
    );
}

#[test]
fn managed_occupied_move_keeps_outside_alias_and_transfers_only_final_owner() {
    for outside_alias in [false, true] {
        let (mut managed, _, _) = matching_group(false);
        let replaced = managed.window(&target("a", 2)).unwrap().id();
        let address = std::ptr::from_ref(managed.window(&target("a", 2)).unwrap());
        if outside_alias {
            managed.create_session(name("outside"), size()).unwrap();
            managed
                .link_window(&target("outside", 7), replaced)
                .unwrap();
            managed
                .add_winlink_alert_flags(&target("outside", 7), WINLINK_BELL)
                .unwrap();
        }
        let change = managed
            .move_window(&name("peer"), 0, 2, true, true)
            .unwrap();
        assert_eq!(change.sessions.len(), 2);
        if outside_alias {
            assert!(change.removed_windows.is_empty());
            assert_eq!(
                std::ptr::from_ref(managed.window(&target("outside", 7)).unwrap()),
                address
            );
            let outside = managed.session(&name("outside")).unwrap();
            assert_eq!(outside.winlink_alert_flags(7), WINLINK_BELL);
            assert_eq!(outside.active_window_index(), 0);
        } else {
            assert_eq!(change.removed_windows.len(), 1);
            assert_eq!(change.removed_windows[0].id(), replaced);
            assert_eq!(
                std::ptr::from_ref(change.removed_windows[0].as_ref()),
                address
            );
        }
        assert!(std::ptr::eq(
            managed.window(&target("a", 2)).unwrap(),
            managed.window(&target("peer", 2)).unwrap()
        ));
    }
}
