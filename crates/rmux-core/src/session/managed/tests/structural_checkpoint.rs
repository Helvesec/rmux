use super::*;

#[test]
fn structure_undo_restores_displaced_allocation_and_leaves_unrelated_owner_alone() {
    let mut store = fixture();
    store.create_session(name("outside"), size()).unwrap();
    let a = target("a", 0);
    let b = target("b", 2);
    let source = store.window(&a).unwrap().id();
    let displaced = std::ptr::from_ref(store.window(&b).unwrap());
    let source_ptr = std::ptr::from_ref(store.window(&a).unwrap());
    let outside = target("outside", 0);
    let outside_ptr = std::ptr::from_ref(store.window(&outside).unwrap());
    let mut undo = store.checkpoint_structure(&[name("a"), name("b")]).unwrap();
    let change = store.install_window_alias(&b, source, true, true).unwrap();
    assert_eq!(change.removed_windows.len(), 1);
    undo.retain_removed(change.removed_windows);
    store
        .rename_window(&outside, "unrelated edit".to_owned())
        .unwrap();
    store.restore_structure_checkpoint(undo).unwrap();
    assert_eq!(std::ptr::from_ref(store.window(&b).unwrap()), displaced);
    assert_eq!(std::ptr::from_ref(store.window(&a).unwrap()), source_ptr);
    assert_eq!(
        std::ptr::from_ref(store.window(&outside).unwrap()),
        outside_ptr
    );
    assert_eq!(
        store.window(&outside).unwrap().name(),
        Some("unrelated edit")
    );
}

#[test]
fn runtime_join_retains_empty_metadata_until_teardown_and_restores_original_owner() {
    let mut store = ManagedSessionStore::new();
    store.create_session(name("a"), size()).unwrap();
    store.create_session(name("b"), size()).unwrap();
    let a = target("a", 0);
    let b = target("b", 0);
    let ptr = std::ptr::from_ref(store.window(&a).unwrap());
    let target_ptr = std::ptr::from_ref(store.window(&b).unwrap());
    let id = store.session(&name("a")).unwrap().id();
    let mut undo = store.checkpoint_structure(&[name("a"), name("b")]).unwrap();
    let result = store
        .join_pane_for_runtime_transfer(
            &a,
            0,
            &b,
            0,
            crate::PaneJoinOptions::new(SplitDirection::Horizontal, false, false, false, None),
        )
        .unwrap();
    assert_eq!(store.session(&name("a")).unwrap().id(), id);
    assert_eq!(store.session(&name("a")).unwrap().windows().count(), 0);
    undo.retain_removed(result.destroyed_window.unwrap().window);
    let removed = store.remove_session(&name("a")).unwrap();
    assert!(removed.windows.is_empty());
    assert!(store.session(&name("a")).is_none());
    store.restore_structure_checkpoint(undo).unwrap();
    assert_eq!(std::ptr::from_ref(store.window(&a).unwrap()), ptr);
    assert_eq!(std::ptr::from_ref(store.window(&b).unwrap()), target_ptr);
    assert_eq!(store.window(&b).unwrap().pane_count(), 1);
}

#[test]
fn runtime_break_and_occurrence_removal_preserve_metadata_until_teardown() {
    let mut store = ManagedSessionStore::new();
    store.create_session(name("a"), size()).unwrap();
    store.create_session(name("b"), size()).unwrap();
    let a = target("a", 0);
    let ptr = std::ptr::from_ref(store.window(&a).unwrap());
    let undo = store.checkpoint_structure(&[name("a"), name("b")]).unwrap();
    let result = store
        .break_pane_for_runtime_transfer(
            &a,
            0,
            &name("b"),
            crate::BreakPaneOptions::new(Some(4), None, false, false, false),
        )
        .unwrap();
    assert_eq!(store.session(&name("a")).unwrap().windows().count(), 0);
    assert_eq!(
        std::ptr::from_ref(store.window(&target("b", 4)).unwrap()),
        ptr
    );
    assert!(result.transfer.destroyed_window.unwrap().window.is_none());
    store.restore_structure_checkpoint(undo).unwrap();
    assert_eq!(std::ptr::from_ref(store.window(&a).unwrap()), ptr);
    let mut undo = store.checkpoint_structure(&[name("a")]).unwrap();
    let removed = store.remove_window_occurrence_for_teardown(&a).unwrap();
    assert!(store.session(&name("a")).is_some());
    undo.retain_removed(removed);
    store.restore_structure_checkpoint(undo).unwrap();
    assert_eq!(std::ptr::from_ref(store.window(&a).unwrap()), ptr);
}

#[test]
fn structure_undo_restores_swapped_links_alerts_and_selection_without_new_allocations() {
    let mut store = fixture();
    let a = target("a", 0);
    let b = target("b", 2);
    let a_ptr = std::ptr::from_ref(store.window(&a).unwrap());
    let b_ptr = std::ptr::from_ref(store.window(&b).unwrap());
    store.add_winlink_alert_flags(&a, WINLINK_BELL).unwrap();
    let undo = store.checkpoint_structure(&[name("a"), name("b")]).unwrap();
    store.swap_window_occurrences(&a, &b).unwrap();
    assert_eq!(std::ptr::from_ref(store.window(&a).unwrap()), b_ptr);
    store
        .replace_winlink_alert_flags(&a, AlertFlags::empty())
        .unwrap();
    store.select_window(&target("a", 9)).unwrap();
    store.restore_structure_checkpoint(undo).unwrap();
    assert_eq!(std::ptr::from_ref(store.window(&a).unwrap()), a_ptr);
    assert_eq!(std::ptr::from_ref(store.window(&b).unwrap()), b_ptr);
    assert_eq!(
        store.session(&name("a")).unwrap().winlink_alert_flags(0),
        WINLINK_BELL
    );
    assert_eq!(store.session(&name("a")).unwrap().active_window_index(), 0);
}

#[test]
fn nested_structure_undo_retains_displaced_owner_after_inner_commit() {
    let mut store = fixture();
    let target = target("b", 2);
    let ptr = std::ptr::from_ref(store.window(&target).unwrap());
    let source_id = store.session(&name("a")).unwrap().window().id();
    let mut outer = store.checkpoint_structure(&[name("a"), name("b")]).unwrap();
    let mut inner = store.checkpoint_structure(&[name("a"), name("b")]).unwrap();
    let change = store
        .install_window_alias(&target, source_id, true, false)
        .unwrap();
    inner.retain_removed(change.removed_windows);
    outer.retain_removed(inner.take_removed());
    drop(inner);
    store.restore_structure_checkpoint(outer).unwrap();
    assert_eq!(std::ptr::from_ref(store.window(&target).unwrap()), ptr);
}

#[test]
fn runtime_join_keeps_every_empty_group_peer_until_explicit_session_teardown() {
    let mut store = ManagedSessionStore::new();
    store.create_session(name("a"), size()).unwrap();
    store
        .create_grouped_session_with_base_index(name("peer"), size(), 0, name("a"))
        .unwrap();
    store.create_session(name("b"), size()).unwrap();
    let ptr = std::ptr::from_ref(store.window(&target("a", 0)).unwrap());
    let owner = store.runtime_owner(&name("a"));
    let mut undo = store.checkpoint_structure(&[name("a"), name("b")]).unwrap();
    let result = store
        .join_pane_for_runtime_transfer(
            &target("a", 0),
            0,
            &target("b", 0),
            0,
            crate::PaneJoinOptions::new(SplitDirection::Vertical, false, false, false, None),
        )
        .unwrap();
    for session in ["a", "peer"] {
        assert_eq!(store.session(&name(session)).unwrap().windows().count(), 0);
    }
    assert_eq!(store.runtime_owner(&name("a")), owner);
    undo.retain_removed(result.destroyed_window.unwrap().window);
    for session in ["peer", "a"] {
        let _ = store.remove_session(&name(session)).unwrap();
    }
    store.restore_structure_checkpoint(undo).unwrap();
    assert_eq!(
        std::ptr::from_ref(store.window(&target("a", 0)).unwrap()),
        ptr
    );
    assert_eq!(
        std::ptr::from_ref(store.window(&target("peer", 0)).unwrap()),
        ptr
    );
    assert_eq!(store.runtime_owner(&name("a")), owner);
}
