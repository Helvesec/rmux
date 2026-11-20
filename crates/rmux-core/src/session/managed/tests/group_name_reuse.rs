use super::*;

#[test]
fn reused_group_name_rejects_atomically_for_named_and_automatic_creation() {
    for automatic in [false, true] {
        for reuse_by_rename in [false, true] {
            let mut store = ManagedSessionStore::new();
            store.create_session(name("a"), size()).unwrap();
            store
                .create_grouped_session_with_base_index(name("peer"), size(), 0, name("a"))
                .unwrap();
            store.rename_session(&name("a"), name("renamed")).unwrap();
            if reuse_by_rename {
                store.create_session(name("standalone"), size()).unwrap();
                store
                    .rename_session(&name("standalone"), name("a"))
                    .unwrap();
            } else {
                store.create_session(name("a"), size()).unwrap();
            }
            // Force automatic naming to scan past an occupied suffix as well.
            store.create_session(name("a-4"), size()).unwrap();
            let before = format!("{store:?}");
            let result = if automatic {
                store.create_auto_grouped_session_with_base_index(size(), 99, name("a"))
            } else {
                store.create_grouped_session_with_base_index(
                    name("new-peer"),
                    size(),
                    99,
                    name("a"),
                )
            };
            assert!(
                result.is_err(),
                "ambiguous standalone target must be rejected"
            );
            assert_eq!(format!("{store:?}"), before);
            assert_eq!(store.runtime_owner(&name("renamed")), Some(name("renamed")));
            assert_eq!(store.runtime_owner(&name("a")), Some(name("a")));
            assert_eq!(
                store.sessions_in_group(&name("a")),
                vec![name("peer"), name("renamed")]
            );
            assert_eq!(
                (
                    store.next_session_id,
                    store.next_window_id,
                    store.next_pane_id
                ),
                (4, 3, 3)
            );

            // An explicit member still resolves the original group despite name reuse.
            let peer = store
                .create_grouped_session_with_base_index(
                    name("valid-peer"),
                    size(),
                    99,
                    name("renamed"),
                )
                .unwrap();
            assert_eq!(peer.runtime_owner, name("renamed"));
            let _ = store.create_window(&name("renamed"), size()).unwrap();
            let _ = store.reindex_windows_from(&name("peer"), 2).unwrap();
            assert_eq!(
                store.index_links(&name("peer")).unwrap(),
                store.index_links(&name("valid-peer")).unwrap()
            );
            let _ = store.remove_window(&target("valid-peer", 3)).unwrap();
            assert_eq!(
                store.session(&name("a")).unwrap().window().id(),
                WindowId::new(1)
            );

            // Moving the standalone away makes the original group key unambiguous.
            store.rename_session(&name("a"), name("moved")).unwrap();
            let created = store
                .create_auto_grouped_session_with_base_index(size(), 99, name("a"))
                .unwrap();
            assert_eq!(created.group_name, name("a"));
            assert_eq!(created.runtime_owner, name("renamed"));
            assert_eq!(
                store.index_links(&created.session_name).unwrap(),
                store.index_links(&name("peer")).unwrap()
            );
        }
    }
}
