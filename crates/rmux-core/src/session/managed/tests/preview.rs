use super::*;

#[test]
fn split_projection_preserves_owner_ids_and_matches_commit() {
    for direction in [SplitDirection::Horizontal, SplitDirection::Vertical] {
        for before in [false, true] {
            let mut store = fixture();
            let t = target("a", 0);
            let pointer = std::ptr::from_ref(store.window(&t).unwrap());
            let layout = store.window(&t).unwrap().layout_dump();
            let next_id = store.next_pane_id;
            let (index, geometry) = store.preview_split_pane(&t, 0, direction, before).unwrap();
            assert_eq!(store.next_pane_id, next_id);
            assert_eq!(store.window(&t).unwrap().layout_dump(), layout);
            let checkpoint = store.checkpoint_window(&t).unwrap();
            let creation = store
                .split_pane(
                    &t,
                    0,
                    ManagedPaneSplit {
                        direction,
                        before,
                        detached: false,
                        full_size: false,
                    },
                )
                .unwrap();
            assert_eq!(creation.pane_index, index);
            assert_eq!(
                store.window(&t).unwrap().pane(index).unwrap().geometry(),
                geometry
            );
            store.restore_window_checkpoint(&t, checkpoint).unwrap();
            assert_eq!(store.window(&t).unwrap().layout_dump(), layout);
            assert_eq!(std::ptr::from_ref(store.window(&t).unwrap()), pointer);
            assert_eq!(
                std::ptr::from_ref(store.window(&target("b", 7)).unwrap()),
                pointer
            );
            assert_eq!(store.window(&t).unwrap().pane_count(), 1);
        }
    }
}

#[test]
fn kill_projection_preserves_aliases_and_matches_committed_cleanup() {
    for kill_others in [false, true] {
        let mut store = fixture();
        let t = target("a", 0);
        let _ = store
            .split_pane(
                &t,
                0,
                ManagedPaneSplit {
                    direction: SplitDirection::Horizontal,
                    before: false,
                    detached: false,
                    full_size: false,
                },
            )
            .unwrap();
        let pointer = std::ptr::from_ref(store.window(&t).unwrap());
        let layout = store.window(&t).unwrap().layout_dump();
        let preview = store.preview_kill_pane(&t, 0, kill_others).unwrap();
        assert_eq!(store.window(&t).unwrap().layout_dump(), layout);
        let result = if kill_others {
            store.kill_other_panes(&t, 0)
        } else {
            store.kill_pane(&t, 0)
        }
        .unwrap();
        assert_eq!(preview.removed_pane_ids(), result.pane_ids);
        assert_eq!(
            preview.window_destroyed(),
            result.destroyed_window.is_some()
        );
        assert_eq!(std::ptr::from_ref(store.window(&t).unwrap()), pointer);
    }
    let mut store = fixture();
    let t = target("a", 0);
    let preview = store.preview_kill_pane(&t, 0, false).unwrap();
    assert!(preview.window_destroyed());
    assert!(store.session(&name("a")).is_some());
    let result = store.kill_pane(&t, 0).unwrap();
    assert_eq!(preview.removed_pane_ids(), result.pane_ids);
    assert!(result.destroyed_window.is_some());
}

#[test]
fn layout_projection_unzooms_without_mutating_canonical_aliases() {
    let mut store = fixture();
    let t = target("a", 0);
    let _ = store
        .split_pane(
            &t,
            0,
            ManagedPaneSplit {
                direction: SplitDirection::Horizontal,
                before: false,
                detached: false,
                full_size: false,
            },
        )
        .unwrap();
    let layout = store.window(&t).unwrap().layout_dump();
    store
        .window_layout_context_mut(&t)
        .unwrap()
        .toggle_zoom(1)
        .unwrap();
    let projection = store
        .session(&name("a"))
        .unwrap()
        .layout_window_snapshot(0)
        .unwrap();
    assert!(!projection.is_zoomed());
    assert_eq!(projection.layout_dump(), layout);
    assert!(store.window(&t).unwrap().is_zoomed());
    assert!(store.window(&target("b", 7)).unwrap().is_zoomed());
}

#[test]
fn respawn_projection_commit_and_rollback_keep_the_original_owner() {
    let mut store = fixture();
    let t = target("a", 0);
    let created = store
        .split_pane(
            &t,
            0,
            ManagedPaneSplit {
                direction: SplitDirection::Horizontal,
                before: true,
                detached: false,
                full_size: false,
            },
        )
        .unwrap();
    store
        .window_layout_context_mut(&t)
        .unwrap()
        .toggle_zoom(0)
        .unwrap();
    let pointer = std::ptr::from_ref(store.window(&t).unwrap());
    let before = store.window(&t).unwrap().clone();
    let checkpoint = store.checkpoint_window(&t).unwrap();
    let projected = store.preview_respawn_window(&t, created.pane_id).unwrap();
    assert_eq!(store.window(&t).unwrap(), &before);
    store.respawn_window(&t, created.pane_id).unwrap();
    // Creation/activity timestamps may cross a second boundary between preview
    // and commit, so compare the structural state and retained identity.
    let committed = store.window(&t).unwrap();
    assert_eq!(committed.pane_count(), 1);
    assert_eq!(committed.active_pane().unwrap().id(), created.pane_id);
    assert_eq!(committed.layout_dump(), projected.layout_dump());
    assert_eq!(committed.is_zoomed(), projected.is_zoomed());
    assert_eq!(std::ptr::from_ref(committed), pointer);
    assert_eq!(
        std::ptr::from_ref(store.window(&target("b", 7)).unwrap()),
        pointer
    );
    store.restore_window_checkpoint(&t, checkpoint).unwrap();
    assert_eq!(store.window(&t).unwrap(), &before);
    assert_eq!(std::ptr::from_ref(store.window(&t).unwrap()), pointer);
    assert!(store.respawn_window(&t, PaneId::new(u32::MAX)).is_err());
    assert_eq!(store.window(&t).unwrap(), &before);
}

#[test]
fn presentation_overrides_are_local_and_unmodified_windows_stay_borrowed() {
    let mut store = fixture();
    let t = target("a", 0);
    let created = store
        .split_pane(
            &t,
            0,
            ManagedPaneSplit {
                direction: SplitDirection::Horizontal,
                before: false,
                detached: false,
                full_size: false,
            },
        )
        .unwrap();
    let before = store.window(&t).unwrap().clone();
    let view = store.session(&name("a")).unwrap();
    let mut presentation = ManagedSessionPresentation::new(view);
    assert!(std::ptr::eq(presentation.view().window(), view.window()));
    presentation.set_terminal_size(TerminalSize::new(100, 40));
    presentation
        .resize_window(0, TerminalSize::new(90, 30))
        .unwrap();
    presentation
        .window_switch_mut(0)
        .unwrap()
        .select_window_and_pane(created.pane_index, true)
        .unwrap();
    assert_eq!(
        presentation.view().terminal_size(),
        TerminalSize::new(100, 40)
    );
    assert_eq!(
        presentation.view().window().size(),
        TerminalSize::new(90, 30)
    );
    assert!(!std::ptr::eq(presentation.view().window(), view.window()));
    // A sibling alias is not given a mutable placement copy by this override.
    assert!(std::ptr::eq(
        presentation.view().window_at(9).unwrap(),
        view.window_at(9).unwrap()
    ));
    assert_eq!(store.window(&t).unwrap(), &before);
    presentation.select_window(9).unwrap();
    assert_eq!(presentation.view().active_window_index(), 9);
    assert_eq!(presentation.view().last_window_index(), Some(0));
    assert_eq!(view.active_window_index(), 0);
    assert!(presentation.select_window(u32::MAX).is_err());
    assert_eq!(presentation.view().active_window_index(), 9);
}
