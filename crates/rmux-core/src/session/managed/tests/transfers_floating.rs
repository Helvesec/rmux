use super::*;
use crate::PaneJoinOptions;

// FMT1 R2: the `before` arms used to be refused when the destination's pane
// list predecessor was floating, because the split target was derived from the
// list position. The named destination is split now, so every arm transfers and
// the surviving float keeps its placement.
#[test]
fn join_floating_predecessor_transfers_through_every_owner_and_flag() {
    for same_owner in [false, true] {
        for cross_session in [false, true] {
            for floating in [false, true] {
                for before in [false, true] {
                    for full in [false, true] {
                        for detached in [false, true] {
                            for direction in [SplitDirection::Vertical, SplitDirection::Horizontal]
                            {
                                let mut store = fixture();
                                let destination = target("a", 4);
                                for pane_index in [0, 1] {
                                    let _ = store
                                        .split_pane(
                                            &destination,
                                            pane_index,
                                            ManagedPaneSplit {
                                                direction: SplitDirection::Vertical,
                                                before: false,
                                                detached: false,
                                                full_size: false,
                                            },
                                        )
                                        .unwrap();
                                }
                                if floating {
                                    store
                                        .window_layout_context_mut(&destination)
                                        .unwrap()
                                        .float_pane(
                                            1,
                                            crate::FloatingPaneRect::new(3, 2, 20, 8).unwrap(),
                                        )
                                        .unwrap();
                                }
                                let source = if same_owner {
                                    target(
                                        if cross_session { "b" } else { "a" },
                                        if cross_session { 7 } else { 9 },
                                    )
                                } else {
                                    if !cross_session {
                                        let id = store.window(&target("b", 2)).unwrap().id();
                                        store.link_window(&target("a", 6), id).unwrap();
                                    }
                                    target(
                                        if cross_session { "b" } else { "a" },
                                        if cross_session { 2 } else { 6 },
                                    )
                                };
                                store
                                    .window_layout_context_mut(&source)
                                    .unwrap()
                                    .toggle_zoom(0)
                                    .unwrap();
                                if !same_owner {
                                    store
                                        .window_layout_context_mut(&destination)
                                        .unwrap()
                                        .toggle_zoom(2)
                                        .unwrap();
                                }
                                let snapshot = format!("{store:?}");
                                let moved_id = store.window(&source).unwrap().pane(0).unwrap().id();
                                let result = store.join_pane(
                                    &source,
                                    0,
                                    &destination,
                                    2,
                                    PaneJoinOptions::new(direction, detached, before, full, None),
                                );
                                let _ = &snapshot;
                                let transfer =
                                    result.expect("the named tiled destination is split");
                                assert_eq!(transfer.pane_id, moved_id);
                                let window = store.window(&destination).unwrap();
                                assert_eq!(
                                    window.pane(transfer.pane_index).unwrap().id(),
                                    moved_id
                                );
                                assert_eq!(window.pane_count(), if same_owner { 3 } else { 4 });
                                assert_eq!(window.floating_stack().is_empty(), !floating);
                            }
                        }
                    }
                }
            }
        }
    }
}
