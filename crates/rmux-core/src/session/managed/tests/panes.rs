use super::*;

fn options(
    full_size: bool,
    before: bool,
    detached: bool,
    direction: SplitDirection,
) -> ManagedPaneSplit {
    ManagedPaneSplit {
        direction,
        before,
        detached,
        full_size,
    }
}

fn same_panes(a: &Window, b: &Window) {
    assert_eq!(a.active_pane_index(), b.active_pane_index());
    assert_eq!(a.last_pane_index(), b.last_pane_index());
    assert_eq!(a.is_zoomed(), b.is_zoomed());
    assert_eq!(a.layout(), b.layout());
    assert_eq!(a.size(), b.size());
    let panes = |w: &Window| {
        w.panes()
            .iter()
            .map(|p| (p.id(), p.index(), p.geometry(), p.active_point()))
            .collect::<Vec<_>>()
    };
    assert_eq!(panes(a), panes(b));
}

#[test]
fn split_matrix_and_kills_share_standalone_sequencing_through_every_alias() {
    for full in [false, true] {
        for before in [false, true] {
            for detached in [false, true] {
                for direction in [SplitDirection::Vertical, SplitDirection::Horizontal] {
                    for zoom in [false, true] {
                        let mut store = fixture();
                        store
                            .create_grouped_session_with_base_index(
                                name("peer"),
                                size(),
                                0,
                                name("a"),
                            )
                            .unwrap();
                        let t = target("a", 0);
                        let pointer = std::ptr::from_ref(store.window(&t).unwrap());
                        let mut standalone = Session::new(name("a"), size());
                        let seed = store
                            .split_pane(&t, 0, options(false, false, false, direction))
                            .unwrap();
                        standalone
                            .split_pane_in_window_with_id_and_direction(
                                0,
                                0,
                                seed.pane_id,
                                direction,
                            )
                            .unwrap();
                        if zoom {
                            store
                                .window_layout_context_mut(&t)
                                .unwrap()
                                .toggle_zoom(1)
                                .unwrap();
                            standalone
                                .window_layout_context_mut(0)
                                .unwrap()
                                .toggle_zoom(1)
                                .unwrap();
                        }
                        let created = store
                            .split_pane(
                                &target("peer", 4),
                                0,
                                options(full, before, detached, direction),
                            )
                            .unwrap();
                        let index = if full {
                            standalone.split_pane_full_size_in_window_with_id_and_direction_before_detached(0, 0, created.pane_id, direction, before, detached).unwrap()
                        } else {
                            standalone
                                .split_pane_in_window_with_id_and_direction_before_detached(
                                    0,
                                    0,
                                    created.pane_id,
                                    direction,
                                    before,
                                    detached,
                                )
                                .unwrap()
                        };
                        assert_eq!(created.pane_index, index);
                        assert_eq!(created.window_id, standalone.window().id());
                        for alias in [t.clone(), target("a", 9), target("peer", 4), target("b", 7)]
                        {
                            assert_eq!(std::ptr::from_ref(store.window(&alias).unwrap()), pointer);
                            same_panes(store.window(&alias).unwrap(), standalone.window());
                        }
                        store
                            .window_selection_mut(&t)
                            .unwrap()
                            .select_last(false)
                            .unwrap();
                        standalone.last_pane_in_window(0).unwrap();
                        let killed = store.kill_pane(&target("b", 7), index).unwrap();
                        let expected = standalone.kill_pane_in_window(0, index).unwrap();
                        assert_eq!(killed.pane_ids, expected.removed_pane_ids());
                        assert!(killed.destroyed_window.is_none());
                        same_panes(store.window(&t).unwrap(), standalone.window());
                        let killed = store.kill_other_panes(&target("a", 9), 1).unwrap();
                        let expected = standalone.kill_other_panes_in_window(0, 1).unwrap();
                        assert_eq!(killed.pane_ids, expected.removed_pane_ids());
                        same_panes(store.window(&t).unwrap(), standalone.window());
                        let killed = store.kill_other_panes(&t, 0).unwrap();
                        assert!(killed.pane_ids.is_empty());
                        assert!(killed.destroyed_window.is_none());
                        assert_eq!(store.session(&name("a")).unwrap().active_window_index(), 0);
                        assert_eq!(store.session(&name("b")).unwrap().active_window_index(), 2);
                    }
                }
            }
        }
    }
}

#[test]
fn rejected_panes_and_exhausted_ids_are_atomic() {
    let mut store = fixture();
    for full in [false, true] {
        let opts = options(full, true, true, SplitDirection::Vertical);
        for (t, pane) in [
            (target("missing", 0), 0),
            (target("a", 88), 0),
            (target("a", 4), 99),
        ] {
            let before = format!("{store:?}");
            assert!(store.split_pane(&t, pane, opts).is_err());
            assert!(store.kill_pane(&t, pane).is_err());
            assert!(store.kill_other_panes(&t, pane).is_err());
            assert_eq!(format!("{store:?}"), before);
        }
    }
    store.next_pane_id = u64::from(u32::MAX);
    let opts = options(false, false, false, SplitDirection::Vertical);
    let created = store.split_pane(&target("a", 0), 0, opts).unwrap();
    assert_eq!(created.pane_id.as_u32(), u32::MAX);
    let _ = store
        .kill_pane(&target("a", 0), created.pane_index)
        .unwrap();
    for full in [false, true] {
        let before = format!("{store:?}");
        assert!(store
            .split_pane(
                &target("a", 0),
                0,
                options(full, false, false, SplitDirection::Vertical)
            )
            .is_err());
        assert_eq!(format!("{store:?}"), before);
    }
    for direction in [SplitDirection::Vertical, SplitDirection::Horizontal] {
        let mut store = ManagedSessionStore::new();
        store
            .create_session(name("tiny"), TerminalSize::new(2, 2))
            .unwrap();
        let before = format!("{store:?}");
        let error = store
            .split_pane(&target("tiny", 0), 0, options(false, true, true, direction))
            .unwrap_err();
        assert_eq!(error.to_string(), "no space for new pane");
        assert_eq!(format!("{store:?}"), before);
    }
}

#[test]
fn final_pane_destroys_all_links_transferring_original_owner_once() {
    for addressed in ["a", "peer", "b"] {
        let mut store = fixture();
        store
            .create_grouped_session_with_base_index(name("peer"), size(), 0, name("a"))
            .unwrap();
        store.create_session(name("unrelated"), size()).unwrap();
        let unrelated = format!("{:?}", store.window(&target("unrelated", 0)).unwrap());
        let pointer = std::ptr::from_ref(store.window(&target("a", 0)).unwrap());
        let b_pointer = std::ptr::from_ref(store.window(&target("b", 2)).unwrap());
        store.select_window(&target("b", 7)).unwrap();
        let t = target(addressed, if addressed == "b" { 7 } else { 4 });
        let result = store.kill_pane(&t, 0).unwrap();
        assert_eq!(result.pane_ids, vec![PaneId::new(0)]);
        let removal = result.destroyed_window.unwrap();
        assert_eq!(removal.links.len(), 7);
        assert_eq!(removal.removed_sessions.len(), 2);
        let window = removal.window.unwrap();
        assert_eq!(std::ptr::from_ref(window.as_ref()), pointer);
        assert_eq!(window.panes()[0].id(), PaneId::new(0));
        assert!(store.session(&name("a")).is_none());
        assert!(store.session(&name("peer")).is_none());
        assert!(!store.contains_group(&name("a")));
        let b = store.session(&name("b")).unwrap();
        assert_eq!(b.active_window_index(), 2);
        assert_eq!(b.last_window_index(), None);
        assert_eq!(std::ptr::from_ref(b.window()), b_pointer);
        assert_eq!(
            format!("{:?}", store.window(&target("unrelated", 0)).unwrap()),
            unrelated
        );
        assert_eq!(store.len(), 2);
    }
}

#[test]
fn global_destruction_prevalidates_outside_groups_before_any_unlink() {
    let mut store = fixture();
    store
        .create_grouped_session_with_base_index(name("peer"), size(), 0, name("b"))
        .unwrap();
    unlink_fixture_occurrence(&mut store, &target("peer", 7));
    let before = format!("{store:?}");
    assert!(store.kill_pane(&target("a", 0), 0).is_err());
    assert_eq!(format!("{store:?}"), before);
}

#[test]
fn global_kill_preserves_group_local_fallbacks_and_unrelated_alerts() {
    for addressed in ["owner", "peer"] {
        let mut store = ManagedSessionStore::new();
        store.create_session(name("owner"), size()).unwrap();
        for index in [4, 9] {
            let _ = store
                .insert_window_with_initial_pane(&name("owner"), index, size())
                .unwrap();
        }
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
        let result = store.kill_pane(&target(addressed, 9), 0).unwrap();
        let removal = result.destroyed_window.unwrap();
        assert_eq!(removal.links.len(), 2);
        assert!(removal.removed_sessions.is_empty());
        assert_eq!(
            result.pane_ids,
            vec![removal.window.unwrap().panes()[0].id()]
        );
        // Each member detaches on its own stack (`session.c:346-350`): the owner
        // visited 0 before 4, the peer only ever left 0 to reach 9.
        for (member, active, last, alerted) in [("owner", 4, Some(0), 0), ("peer", 0, None, 4)] {
            let session = store.session(&name(member)).unwrap();
            assert_eq!(session.active_window_index(), active);
            assert_eq!(session.last_window_index(), last);
            assert_eq!(session.winlink_alert_flags(active), AlertFlags::empty());
            assert_eq!(session.winlink_alert_flags(alerted), WINLINK_BELL);
            assert_eq!(session.windows().count(), 2);
        }
    }
}

#[test]
fn kill_others_keeps_single_pane_and_unzooms_multi_pane_owner() {
    let mut store = fixture();
    let t = target("a", 4);
    store
        .window_layout_context_mut(&t)
        .unwrap()
        .toggle_zoom(0)
        .unwrap();
    let result = store.kill_other_panes(&t, 0).unwrap();
    assert!(result.pane_ids.is_empty());
    assert!(!store.window(&t).unwrap().is_zoomed());
    let created = store
        .split_pane(&t, 0, options(false, true, true, SplitDirection::Vertical))
        .unwrap();
    let window = store.window(&t).unwrap();
    assert!(!window.is_zoomed());
    assert_eq!(window.active_pane().unwrap().id(), PaneId::new(0));
    assert_eq!(window.last_pane_index(), None);
    store
        .window_layout_context_mut(&t)
        .unwrap()
        .toggle_zoom(created.pane_index)
        .unwrap();
    let before = format!("{store:?}");
    assert!(store.kill_other_panes(&t, 99).is_err());
    assert!(store.kill_pane(&t, 99).is_err());
    assert_eq!(format!("{store:?}"), before);
    let result = store.kill_other_panes(&t, created.pane_index).unwrap();
    assert_eq!(result.pane_ids, vec![PaneId::new(0)]);
    let window = store.window(&target("b", 7)).unwrap();
    assert!(!window.is_zoomed());
    assert_eq!(window.active_pane_index(), 0);
    assert_eq!(window.last_pane_index(), None);
    assert_eq!(window.panes()[0].id(), created.pane_id);
    assert_eq!(
        window.panes()[0].geometry(),
        crate::PaneGeometry::new(0, 0, 80, 24)
    );
}
