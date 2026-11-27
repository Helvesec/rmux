use super::*;
use crate::{BreakPaneOptions, SessionPaneTarget};

fn history(store: &mut ManagedSessionStore, session: &str) {
    for index in [1, 0] {
        store.select_window(&target(session, index)).unwrap();
    }
}

fn navigation(store: &ManagedSessionStore, session: &str) -> (u32, Option<u32>) {
    let session = store.session(&name(session)).unwrap();
    (session.active_window_index(), session.last_window_index())
}

#[test]
fn single_break_group_alias_cleanup_preserves_selected_relocation_history() {
    for detached in [false, true] {
        for before in [false, true] {
            for duplicate in [false, true] {
                for destination in ["a", "peer"] {
                    let mut store = ManagedSessionStore::new();
                    store.create_session(name("a"), size()).unwrap();
                    let _ = store.create_window(&name("a"), size()).unwrap();
                    let owner = store.window(&target("a", 0)).unwrap().id();
                    let pointer = std::ptr::from_ref(store.window(&target("a", 0)).unwrap());
                    if duplicate {
                        store.link_window(&target("a", 4), owner).unwrap();
                    }
                    store
                        .create_grouped_session_with_base_index(name("peer"), size(), 0, name("a"))
                        .unwrap();
                    store.create_session(name("outside"), size()).unwrap();
                    store.link_window(&target("outside", 7), owner).unwrap();
                    store.select_window(&target("outside", 7)).unwrap();
                    history(&mut store, destination);
                    let other = if destination == "a" { "peer" } else { "a" };
                    store.select_window(&target(other, 1)).unwrap();
                    let tables = ["a", "peer", "outside"].map(|s| {
                        (
                            name(s),
                            store.index_links(&name(s)).unwrap(),
                            navigation(&store, s),
                        )
                    });
                    let counter = store.next_window_id;
                    let result = store
                        .break_pane(
                            &target("a", 0),
                            0,
                            &name(destination),
                            BreakPaneOptions::new(
                                Some(if before { 0 } else { 2 }),
                                None,
                                detached,
                                false,
                                before,
                            ),
                        )
                        .unwrap();
                    let index = result.window_index;
                    let survivor = if before { 2 } else { 1 };
                    assert_eq!(
                        navigation(&store, destination),
                        if detached {
                            (survivor, None)
                        } else {
                            (index, Some(survivor))
                        }
                    );
                    // The other member keeps its visit on the slot it left
                    // (`session.c:663-678`): a relocation onto that very slot
                    // leaves it a return, an ordinary one takes the slot away.
                    assert_eq!(navigation(&store, other), (survivor, before.then_some(0)));
                    assert_eq!(navigation(&store, "outside"), (0, None));
                    assert_eq!(
                        store.index_links(&name("a")).unwrap(),
                        store.index_links(&name("peer")).unwrap()
                    );
                    assert_eq!(store.session(&name("a")).unwrap().windows().count(), 2);
                    assert_eq!(
                        pointer,
                        std::ptr::from_ref(store.window(&target(destination, index)).unwrap())
                    );
                    assert_eq!(result.transfer.source_window_id, owner);
                    assert_eq!(result.transfer.target_window_id, owner);
                    assert_eq!(store.next_window_id, counter);
                    assert!(result.transfer.change.removed_windows.is_empty());
                    let removal = result.transfer.destroyed_window.unwrap();
                    assert!(removal.window.is_none());
                    assert!(removal.removed_sessions.is_empty());
                    assert_eq!(removal.links.len(), if duplicate { 5 } else { 3 });
                    assert_eq!(result.transfer.change.sessions.len(), 3);
                    for (session, table, nav) in tables {
                        let view = store.session(&session).unwrap();
                        let change = result
                            .transfer
                            .change
                            .sessions
                            .iter()
                            .find(|c| c.session_id == view.id())
                            .unwrap();
                        assert_eq!(change.before, table);
                        assert_eq!(change.after, store.index_links(&session).unwrap());
                        assert_eq!((change.previous_active, change.previous_last), nav);
                        assert_eq!(
                            (change.active, change.last),
                            navigation(&store, session.as_str())
                        );
                        let expected = if session == name("outside") {
                            BTreeMap::from([(0, 0)])
                        } else {
                            BTreeMap::from([(0, index), (1, survivor)])
                        };
                        assert_eq!(change.index_map, expected);
                    }
                    if !detached {
                        assert_eq!(store.last_window(&name(destination)).unwrap(), survivor);
                    }
                }
            }
        }
    }
}

#[test]
fn single_break_cross_session_keeps_standalone_removal_and_destination_history() {
    for detached in [false, true] {
        for before in [false, true] {
            let mut store = ManagedSessionStore::new();
            let mut standalone = [
                Session::new(name("a"), size()),
                Session::new(name("b"), size()),
            ];
            for (s, session) in ["a", "b"].into_iter().zip(&mut standalone) {
                store.create_session(name(s), size()).unwrap();
                let _ = store.create_window(&name(s), size()).unwrap();
                session.insert_window_with_initial_pane(1, size()).unwrap();
                history(&mut store, s);
                for index in [1, 0] {
                    session.select_window(index).unwrap();
                }
            }
            let options = BreakPaneOptions::new(
                Some(if before { 0 } else { 2 }),
                None,
                detached,
                false,
                before,
            );
            let result = store
                .break_pane(&target("a", 0), 0, &name("b"), options.clone())
                .unwrap();
            let [source, destination] = &mut standalone;
            let index = source
                .break_pane_to_session(SessionPaneTarget::new(0, 0), destination, options)
                .unwrap();
            assert_eq!(result.window_index, index);
            for (s, session) in ["a", "b"].into_iter().zip(&standalone) {
                assert_eq!(
                    navigation(&store, s),
                    (session.active_window_index(), session.last_window_index())
                );
                let change = result
                    .transfer
                    .change
                    .sessions
                    .iter()
                    .find(|c| c.session_id == store.session(&name(s)).unwrap().id())
                    .unwrap();
                assert_eq!((change.active, change.last), navigation(&store, s));
                assert_eq!(change.after, store.index_links(&name(s)).unwrap());
                assert_eq!((change.previous_active, change.previous_last), (0, Some(1)));
                let expected = if s == "a" {
                    BTreeMap::from([(1, 1)])
                } else if before {
                    BTreeMap::from([(0, 1), (1, 2)])
                } else {
                    BTreeMap::from([(0, 0), (1, 1)])
                };
                assert_eq!(change.index_map, expected);
            }
            let removal = result.transfer.destroyed_window.unwrap();
            assert!(removal.window.is_none());
            assert_eq!(removal.links.len(), 1);
            assert!(removal.removed_sessions.is_empty());
        }
    }
}
