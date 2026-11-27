use super::*;
use crate::{BreakPaneOptions, PaneJoinOptions, PaneSwapOptions, SessionPaneTarget};
use rmux_proto::PaneSplitSize;

fn split(store: &mut ManagedSessionStore, target: &WindowTarget) -> PaneId {
    store
        .split_pane(
            target,
            0,
            ManagedPaneSplit {
                direction: SplitDirection::Vertical,
                before: false,
                detached: false,
                full_size: false,
            },
        )
        .unwrap()
        .pane_id
}
fn join(detached: bool, before: bool, full: bool) -> PaneJoinOptions {
    PaneJoinOptions::new(
        SplitDirection::Vertical,
        detached,
        before,
        full,
        Some(PaneSplitSize::Percentage(30)),
    )
}
type WindowState = (
    Vec<(PaneId, u32, crate::PaneGeometry, u64)>,
    u32,
    Option<u32>,
    bool,
    String,
);
fn state(window: &Window) -> WindowState {
    (
        window
            .panes()
            .iter()
            .map(|p| (p.id(), p.index(), p.geometry(), p.active_point()))
            .collect(),
        window.active_pane_index(),
        window.last_pane_index(),
        window.is_zoomed(),
        window.layout_dump(),
    )
}

#[test]
fn same_owner_aliases_share_standalone_transfer_matrix() {
    for detached in [false, true] {
        for before in [false, true] {
            for full in [false, true] {
                for zoom in [false, true] {
                    let mut store = fixture();
                    let t = target("a", 0);
                    let id = split(&mut store, &t);
                    let mut standalone = Session::new(name("a"), size());
                    standalone
                        .split_pane_in_window_with_id_and_direction(
                            0,
                            0,
                            id,
                            SplitDirection::Vertical,
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
                    let pointer = std::ptr::from_ref(store.window(&t).unwrap());
                    let options = join(detached, before, full);
                    let result = store
                        .join_pane(&target("b", 7), 1, &target("a", 4), 0, options)
                        .unwrap();
                    standalone
                        .join_pane(
                            SessionPaneTarget::new(0, 1),
                            SessionPaneTarget::new(0, 0),
                            options,
                        )
                        .unwrap();
                    assert_eq!(state(store.window(&t).unwrap()), state(standalone.window()));
                    assert_eq!(
                        pointer,
                        std::ptr::from_ref(store.window(&target("b", 7)).unwrap())
                    );
                    assert!(result.destroyed_window.is_none());
                    assert_eq!(store.session(&name("a")).unwrap().active_window_index(), 0);
                    let options = PaneSwapOptions::new(detached, zoom);
                    let _ = store
                        .swap_panes(&target("a", 9), 0, &target("b", 7), 1, options)
                        .unwrap();
                    standalone
                        .swap_panes(
                            SessionPaneTarget::new(0, 0),
                            SessionPaneTarget::new(0, 1),
                            options,
                        )
                        .unwrap();
                    assert_eq!(state(store.window(&t).unwrap()), state(standalone.window()));
                }
            }
        }
    }
}

#[test]
fn distinct_group_owners_swap_and_join_without_replacing_allocations() {
    for detached in [false, true] {
        for zoom in [false, true] {
            let mut store = fixture();
            for (peer, group) in [("ap", "a"), ("bp", "b")] {
                store
                    .create_grouped_session_with_base_index(name(peer), size(), 0, name(group))
                    .unwrap();
            }
            let a = target("ap", 4);
            let b = target("bp", 2);
            split(&mut store, &a);
            split(&mut store, &b);
            let ap = std::ptr::from_ref(store.window(&a).unwrap());
            let bp = std::ptr::from_ref(store.window(&b).unwrap());
            let a_id = store.window(&a).unwrap().panes()[0].id();
            let b_id = store.window(&b).unwrap().panes()[1].id();
            if zoom {
                store
                    .window_layout_context_mut(&a)
                    .unwrap()
                    .toggle_zoom(1)
                    .unwrap();
                store
                    .window_layout_context_mut(&b)
                    .unwrap()
                    .toggle_zoom(1)
                    .unwrap();
            }
            let _ = store
                .swap_panes(&a, 0, &b, 1, PaneSwapOptions::new(detached, zoom))
                .unwrap();
            assert_eq!(store.window(&a).unwrap().panes()[0].id(), b_id);
            assert_eq!(store.window(&b).unwrap().panes()[1].id(), a_id);
            assert_eq!(store.window(&a).unwrap().is_zoomed(), zoom);
            let moved = store
                .join_pane(&a, 0, &b, 0, join(detached, false, false))
                .unwrap();
            assert!(moved.destroyed_window.is_none());
            assert_eq!(moved.pane_id, b_id);
            assert_eq!(ap, std::ptr::from_ref(store.window(&a).unwrap()));
            assert_eq!(bp, std::ptr::from_ref(store.window(&b).unwrap()));
            assert!(!store.window(&a).unwrap().is_zoomed());
            assert!(!store.window(&b).unwrap().is_zoomed());
            assert_eq!(
                store.index_links(&name("a")).unwrap(),
                store.index_links(&name("ap")).unwrap()
            );
            assert_eq!(
                store.index_links(&name("b")).unwrap(),
                store.index_links(&name("bp")).unwrap()
            );
        }
    }
}

#[test]
fn final_join_destroys_every_alias_but_never_the_moved_runtime_pane() {
    let mut store = fixture();
    store
        .create_grouped_session_with_base_index(name("peer"), size(), 0, name("a"))
        .unwrap();
    let pane = store.window(&target("a", 0)).unwrap().panes()[0].id();
    let result = store
        .join_pane(
            &target("a", 4),
            0,
            &target("b", 2),
            0,
            join(false, false, false),
        )
        .unwrap();
    let cleanup = result.destroyed_window.unwrap();
    assert_eq!(cleanup.links.len(), 7);
    assert_eq!(cleanup.removed_sessions.len(), 2);
    assert_eq!(cleanup.window.unwrap().pane_count(), 0);
    assert!(store.session(&name("a")).is_none());
    assert!(store.window(&target("b", 7)).is_err());
    assert!(store
        .window(&target("b", 2))
        .unwrap()
        .panes()
        .iter()
        .any(|p| p.id() == pane));
}

#[test]
fn transfer_rejections_preserve_owners_links_zoom_and_counters() {
    let mut store = fixture();
    let t = target("a", 0);
    split(&mut store, &t);
    store
        .window_layout_context_mut(&t)
        .unwrap()
        .toggle_zoom(1)
        .unwrap();
    let before = format!("{store:?}");
    assert!(store
        .join_pane(&t, 0, &target("b", 7), 0, join(false, false, false))
        .is_err());
    assert_eq!(before, format!("{store:?}"));
    assert!(store
        .swap_panes(
            &t,
            99,
            &target("b", 2),
            0,
            PaneSwapOptions::new(false, false)
        )
        .is_err());
    assert_eq!(before, format!("{store:?}"));
    assert!(store
        .break_pane(
            &t,
            0,
            &name("b"),
            BreakPaneOptions::new(Some(2), None, false, false, false)
        )
        .is_err());
    assert_eq!(before, format!("{store:?}"));
    store.next_window_id = u64::from(u32::MAX) + 1;
    let before = format!("{store:?}");
    assert!(store
        .break_pane(
            &t,
            0,
            &name("b"),
            BreakPaneOptions::new(Some(2), None, false, true, false)
        )
        .is_err());
    assert_eq!(before, format!("{store:?}"));
}

#[test]
fn break_checks_allocation_and_commits_group_shifts_and_final_indices() {
    for detached in [false, true] {
        let mut store = fixture();
        store
            .create_grouped_session_with_base_index(name("peer"), size(), 0, name("b"))
            .unwrap();
        let t = target("a", 0);
        let pane = split(&mut store, &t);
        store.next_window_id = u64::from(u32::MAX);
        let result = store
            .break_pane(
                &t,
                1,
                &name("b"),
                BreakPaneOptions::new(Some(2), Some("broken".into()), detached, false, true),
            )
            .unwrap();
        assert_eq!(result.transfer.target_window_id, WindowId::new(u32::MAX));
        assert_eq!(store.next_window_id, u64::from(u32::MAX) + 1);
        assert_eq!(store.window(&target("b", 2)).unwrap().panes()[0].id(), pane);
        assert_eq!(
            store.window(&target("b", 2)).unwrap().name(),
            Some("broken")
        );
        assert_eq!(
            store.index_links(&name("b")).unwrap(),
            store.index_links(&name("peer")).unwrap()
        );
        for change in &result.transfer.change.sessions {
            let session = store.session_by_id(change.session_id).unwrap();
            assert_eq!(change.active, session.active_window_index());
            assert_eq!(change.after, store.index_links(session.name()).unwrap());
        }
        assert_eq!(
            store.session(&name("b")).unwrap().active_window_index(),
            if detached { 3 } else { 2 }
        );
    }
}

#[test]
fn final_break_relocates_one_owner_and_removes_original_aliases_without_allocation() {
    let mut store = fixture();
    let t = target("a", 0);
    let pointer = std::ptr::from_ref(store.window(&t).unwrap());
    let id = store.window(&t).unwrap().id();
    store.next_window_id = u64::MAX;
    let result = store
        .break_pane(
            &t,
            0,
            &name("b"),
            BreakPaneOptions::new(Some(3), None, false, false, false),
        )
        .unwrap();
    assert_eq!(result.transfer.target_window_id, id);
    assert_eq!(
        pointer,
        std::ptr::from_ref(store.window(&target("b", 3)).unwrap())
    );
    assert_eq!(store.next_window_id, u64::MAX);
    assert!(store.session(&name("a")).is_none());
    assert!(store.window(&target("b", 7)).is_err());
    assert!(result.transfer.destroyed_window.unwrap().window.is_none());
}

#[test]
fn no_space_join_rejection_preserves_zoom_and_layout_through_aliases() {
    let mut store = fixture();
    let t = target("a", 0);
    split(&mut store, &t);
    let link = store.target_link(&t).unwrap();
    store
        .windows
        .resolve_mut(link)
        .unwrap()
        .set_size(TerminalSize::new(3, 24));
    store
        .window_layout_context_mut(&t)
        .unwrap()
        .toggle_zoom(1)
        .unwrap();
    let before = format!("{store:?}");
    let error = store
        .join_pane(
            &target("b", 7),
            1,
            &target("a", 9),
            0,
            join(false, false, false),
        )
        .unwrap_err();
    assert!(error.to_string().contains("cannot split target pane"));
    assert_eq!(before, format!("{store:?}"));
}

#[test]
fn break_index_overflow_and_inconsistent_groups_reject_before_extracting() {
    let mut store = fixture();
    split(&mut store, &target("a", 0));
    let _ = store
        .insert_alias(
            &target("b", u32::MAX),
            store.window(&target("b", 2)).unwrap().id(),
        )
        .unwrap();
    let before = format!("{store:?}");
    assert!(store
        .break_pane(
            &target("a", 0),
            1,
            &name("b"),
            BreakPaneOptions::new(Some(2), None, false, false, true)
        )
        .is_err());
    assert_eq!(before, format!("{store:?}"));
    store
        .create_grouped_session_with_base_index(name("peer"), size(), 0, name("b"))
        .unwrap();
    let link = store.target_link(&target("peer", 7)).unwrap();
    store.windows.unlink(link).unwrap();
    let before = format!("{store:?}");
    assert!(store
        .break_pane(
            &target("a", 0),
            1,
            &name("b"),
            BreakPaneOptions::new(Some(3), None, false, false, false)
        )
        .is_err());
    assert_eq!(before, format!("{store:?}"));
    assert!(store
        .join_pane(
            &target("a", 0),
            1,
            &target("b", 2),
            0,
            join(false, false, false)
        )
        .is_err());
    assert_eq!(before, format!("{store:?}"));
}

#[test]
fn break_same_session_matches_existing_single_and_multi_pane_fixtures() {
    for single in [false, true] {
        for detached in [false, true] {
            for (after, before) in [(false, false), (true, false), (false, true)] {
                for active_source in [true, false] {
                    let mut store = ManagedSessionStore::new();
                    store.create_session(name("a"), size()).unwrap();
                    let _ = store.create_window(&name("a"), size()).unwrap();
                    let mut standalone = Session::new(name("a"), size());
                    standalone
                        .insert_window_with_initial_pane(1, size())
                        .unwrap();
                    if !single {
                        let id = split(&mut store, &target("a", 0));
                        standalone
                            .split_pane_in_window_with_id_and_direction(
                                0,
                                0,
                                id,
                                SplitDirection::Vertical,
                            )
                            .unwrap();
                    }
                    for index in [1, 0] {
                        store.select_window(&target("a", index)).unwrap();
                        standalone.select_window(index).unwrap();
                    }
                    if !active_source {
                        store.select_window(&target("a", 1)).unwrap();
                        standalone.select_window(1).unwrap();
                    }
                    let options = BreakPaneOptions::new(
                        Some(if after || before { 0 } else { 2 }),
                        Some("broken".into()),
                        detached,
                        after,
                        before,
                    );
                    let result = store
                        .break_pane(&target("a", 0), 0, &name("a"), options.clone())
                        .unwrap();
                    let index = standalone
                        .break_pane(SessionPaneTarget::new(0, 0), options)
                        .unwrap();
                    assert_eq!(result.window_index, index);
                    let managed = store.session(&name("a")).unwrap();
                    assert_eq!(
                        managed.active_window_index(),
                        standalone.active_window_index()
                    );
                    assert_eq!(managed.last_window_index(), standalone.last_window_index());
                    assert_eq!(
                        state(store.window(&target("a", index)).unwrap()),
                        state(standalone.window_at(index).unwrap())
                    );
                }
            }
        }
    }
}

#[test]
fn single_break_to_its_own_slot_preserves_other_aliases_and_local_selection() {
    for detached in [false, true] {
        let mut store = fixture();
        let before_a = store.index_links(&name("a")).unwrap();
        let before_b = store.index_links(&name("b")).unwrap();
        let result = store
            .break_pane(
                &target("a", 4),
                0,
                &name("a"),
                BreakPaneOptions::new(Some(4), Some("same".into()), detached, false, false),
            )
            .unwrap();
        assert!(result.transfer.destroyed_window.is_none());
        assert!(result.transfer.change.sessions.is_empty());
        assert_eq!(before_a, store.index_links(&name("a")).unwrap());
        assert_eq!(before_b, store.index_links(&name("b")).unwrap());
        assert_eq!(store.session(&name("a")).unwrap().active_window_index(), 0);
        assert_eq!(store.window(&target("b", 7)).unwrap().name(), Some("same"));
    }
}

#[test]
fn cross_owner_join_matches_existing_session_algorithm_on_both_axes() {
    for direction in [SplitDirection::Vertical, SplitDirection::Horizontal] {
        for detached in [false, true] {
            for before in [false, true] {
                for full in [false, true] {
                    let mut store = fixture();
                    let a = target("a", 0);
                    let b = target("b", 2);
                    let first = store.window(&a).unwrap();
                    let mut standalone_a = Session::new_with_initial_window(
                        name("a"),
                        size(),
                        0,
                        first.panes()[0].id(),
                        first.id(),
                    );
                    let second = store.window(&b).unwrap();
                    let mut standalone_b = Session::new_with_initial_window(
                        name("b"),
                        size(),
                        2,
                        second.panes()[0].id(),
                        second.id(),
                    );
                    for (t, session) in [(&a, &mut standalone_a), (&b, &mut standalone_b)] {
                        let id = split(&mut store, t);
                        session
                            .split_pane_in_window_with_id_and_direction(
                                t.window_index(),
                                0,
                                id,
                                SplitDirection::Vertical,
                            )
                            .unwrap();
                        store
                            .window_layout_context_mut(t)
                            .unwrap()
                            .toggle_zoom(1)
                            .unwrap();
                        session
                            .window_layout_context_mut(t.window_index())
                            .unwrap()
                            .toggle_zoom(1)
                            .unwrap();
                    }
                    let options = PaneJoinOptions::new(
                        direction,
                        detached,
                        before,
                        full,
                        Some(PaneSplitSize::Absolute(7)),
                    );
                    let _ = store.join_pane(&a, 1, &b, 0, options).unwrap();
                    standalone_b
                        .join_pane_from_session(
                            SessionPaneTarget::new(2, 0),
                            &mut standalone_a,
                            SessionPaneTarget::new(0, 1),
                            options,
                        )
                        .unwrap();
                    assert_eq!(
                        state(store.window(&a).unwrap()),
                        state(standalone_a.window())
                    );
                    assert_eq!(
                        state(store.window(&b).unwrap()),
                        state(standalone_b.window())
                    );
                }
            }
        }
    }
}
