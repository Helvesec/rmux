use super::*;
use crate::FloatingPaneRect;

fn rect(x: i32, y: i32, cols: i32, rows: i32) -> FloatingPaneRect {
    FloatingPaneRect::new(x, y, cols, rows).expect("valid floating rect")
}

#[test]
fn aliases_share_floating_transactions_and_window_checkpoint_rolls_them_back() {
    let mut store = fixture();
    let owner = target("a", 0);
    let alias = target("b", 7);
    store.advance_next_pane_id_for_tests(PaneId::new(50));
    let created = store
        .split_pane(
            &owner,
            0,
            ManagedPaneSplit {
                direction: SplitDirection::Vertical,
                before: false,
                detached: false,
                full_size: false,
            },
        )
        .unwrap();
    let checkpoint = store.checkpoint_window(&owner).unwrap();
    let owner_address = std::ptr::from_ref(store.window(&owner).unwrap());

    store
        .window_layout_context_mut(&alias)
        .unwrap()
        .float_pane(created.pane_index, rect(-4, 2, 31, 7))
        .unwrap();

    for target in [&owner, &alias] {
        let window = store.window(target).unwrap();
        assert_eq!(std::ptr::from_ref(window), owner_address);
        assert_eq!(
            window
                .floating_stack()
                .placement(created.pane_id)
                .expect("created pane is floating")
                .rect(),
            rect(-4, 2, 31, 7)
        );
    }

    let before_split_rejected = format!("{store:?}");
    let next_pane_id = store.next_pane_id;
    assert!(store
        .split_pane(
            &alias,
            created.pane_index,
            ManagedPaneSplit {
                direction: SplitDirection::Vertical,
                before: false,
                detached: false,
                full_size: false,
            },
        )
        .is_err());
    assert_eq!(store.next_pane_id, next_pane_id);
    assert_eq!(format!("{store:?}"), before_split_rejected);

    let before_rejected = format!("{store:?}");
    assert!(store
        .window_layout_context_mut(&alias)
        .unwrap()
        .tile_floating_pane(
            created.pane_index,
            Some(created.pane_index),
            SplitDirection::Vertical,
            false,
        )
        .is_err());
    assert_eq!(format!("{store:?}"), before_rejected);

    store
        .restore_window_checkpoint(&owner, checkpoint)
        .expect("checkpoint restores");
    assert!(store.window(&alias).unwrap().floating_stack().is_empty());

    store
        .window_layout_context_mut(&owner)
        .unwrap()
        .float_pane(created.pane_index, rect(1, 1, 20, 5))
        .unwrap();
    store
        .window_layout_context_mut(&alias)
        .unwrap()
        .move_floating_pane(created.pane_index, 9, -3)
        .unwrap();

    assert_eq!(
        store
            .window(&owner)
            .unwrap()
            .floating_stack()
            .placement(created.pane_id)
            .expect("created pane remains floating")
            .rect(),
        rect(9, -3, 20, 5)
    );
}

#[test]
fn floating_pane_mut_alias_prevalidations_preserve_owner_and_allocators() {
    use crate::{BreakPaneOptions, Pane, PaneGeometry, PaneJoinOptions};
    for operation in ["split", "join-source", "join-target", "join-alias", "break"] {
        for full in [false, true] {
            let mut store = fixture();
            let owner = target("a", 0);
            let alias = target("b", 7);
            let other = target("b", 2);
            let created = store
                .split_pane(
                    &owner,
                    0,
                    ManagedPaneSplit {
                        direction: SplitDirection::Vertical,
                        before: false,
                        detached: false,
                        full_size: false,
                    },
                )
                .unwrap();
            store
                .window_layout_context_mut(&owner)
                .unwrap()
                .float_pane(created.pane_index, rect(7, 8, 30, 9))
                .unwrap();
            let link = store.target_link(&owner).unwrap();
            *store
                .windows
                .resolve_mut(link)
                .unwrap()
                .pane_mut(created.pane_index)
                .unwrap() = Pane::new_with_id(
                PaneId::new(99),
                created.pane_index,
                PaneGeometry::new(7, 8, 30, 9),
            );
            assert!(store
                .window(&alias)
                .unwrap()
                .floating_stack()
                .placement(PaneId::new(99))
                .is_some());
            let snapshot = format!("{store:?}");
            let options = PaneJoinOptions::new(SplitDirection::Vertical, false, false, full, None);
            // FMT1: a floating source now reaches a tiled target through the
            // same transfer, including when it is addressed through an alias.
            // Every neighbouring refusal keeps its acquired expectation.
            let transfers = operation == "join-source";
            let rejected = match operation {
                "split" => store
                    .split_pane(
                        &alias,
                        created.pane_index,
                        ManagedPaneSplit {
                            direction: SplitDirection::Vertical,
                            before: false,
                            detached: false,
                            full_size: false,
                        },
                    )
                    .is_err(),
                "join-source" => store
                    .join_pane(&alias, created.pane_index, &other, 0, options)
                    .is_err(),
                "join-target" => store
                    .join_pane(&other, 0, &alias, created.pane_index, options)
                    .is_err(),
                "join-alias" => store
                    .join_pane(&owner, 0, &alias, created.pane_index, options)
                    .is_err(),
                "break" => store
                    .break_pane(
                        &alias,
                        created.pane_index,
                        &name("a"),
                        BreakPaneOptions::new(None, None, false, false, false),
                    )
                    .is_err(),
                _ => unreachable!(),
            };
            assert_eq!(rejected, !transfers, "{operation} full={full}");
            if !transfers {
                assert_eq!(format!("{store:?}"), snapshot, "{operation} full={full}");
                continue;
            }
            let destination = store.window(&other).unwrap();
            let moved = destination
                .panes()
                .iter()
                .find(|pane| pane.id() == PaneId::new(99))
                .expect("the replaced floating identity reaches the tiled destination");
            assert!(
                !destination.is_floating_pane_id(moved.id()),
                "join-source full={full}"
            );
            assert!(destination.floating_stack().is_empty());
            assert!(store
                .window(&alias)
                .unwrap()
                .floating_stack()
                .placement(PaneId::new(99))
                .is_none());
        }
    }
}

#[test]
fn floating_implicit_removal_through_alias_raises_in_canonical_owner() {
    let mut store = fixture();
    let owner = target("a", 0);
    let alias = target("b", 7);
    let owner_address = std::ptr::from_ref(store.window(&owner).unwrap());
    let link = store.target_link(&alias).unwrap();
    let window = store.windows.resolve_mut(link).unwrap();
    window.split_after_position_with_id(0, PaneId::new(50));
    window.split_after_position_with_id(1, PaneId::new(51));
    for index in [1, 2] {
        window.float_pane(index, rect(2, 3, 30, 10)).unwrap();
    }
    window.select_pane(1);
    window.select_pane(0);
    window.raise_floating_pane(2).unwrap();
    window.remove_pane(0).unwrap();
    for target in [&owner, &alias] {
        let window = store.window(target).unwrap();
        assert_eq!(std::ptr::from_ref(window), owner_address);
        assert_eq!(window.active_pane().unwrap().id(), PaneId::new(50));
        assert_eq!(window.last_pane_index(), None);
        assert_eq!(
            window
                .floating_stack()
                .placements()
                .iter()
                .map(|p| p.pane_id())
                .collect::<Vec<_>>(),
            vec![PaneId::new(51), PaneId::new(50)]
        );
    }
}

#[test]
fn aliases_resolve_scene_from_replaced_tiled_cell_without_mutation() {
    use crate::{Pane, PaneGeometry};
    let mut store = fixture();
    let owner = target("a", 0);
    let alias = target("b", 7);
    let link = store.target_link(&owner).unwrap();
    *store
        .windows
        .resolve_mut(link)
        .unwrap()
        .pane_mut(0)
        .unwrap() = Pane::new_with_id(PaneId::new(99), 0, PaneGeometry::new(0, 0, 40, 10));
    let before = format!("{store:?}");
    let context = crate::ResolvedSceneContext::new(
        size(),
        crate::ResolvedPaneBorderStatus::Off,
        crate::ResolvedZoomVisibility::Rendered,
    );
    let scene = store.window(&owner).unwrap().resolved_scene(context);
    assert_eq!(
        scene
            .pane_by_id(PaneId::new(99))
            .unwrap()
            .content_clip_geometry(),
        PaneGeometry::new(0, 0, 80, 24)
    );
    assert_eq!(store.window(&alias).unwrap().resolved_scene(context), scene);
    assert_eq!(format!("{store:?}"), before);
}
