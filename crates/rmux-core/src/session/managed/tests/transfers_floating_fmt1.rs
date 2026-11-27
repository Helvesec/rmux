//! FMT1 through the managed store: a floating source reaching a tiled target
//! while the same canonical window is addressed by several winlinks.
//!
//! `fixture()` links one window id at `a:0`, `a:4`, `a:9` and `b:7`, so two
//! links of one `WindowId` are the natural way to prove a single mutation.

use super::*;
use crate::{FloatingPaneRect, PaneJoinOptions};

fn rect(x: i32, y: i32, cols: i32, rows: i32) -> FloatingPaneRect {
    FloatingPaneRect::new(x, y, cols, rows).expect("valid floating rect")
}

fn tiled_split(store: &mut ManagedSessionStore, target: &WindowTarget, pane_index: u32) -> u32 {
    store
        .split_pane(
            target,
            pane_index,
            ManagedPaneSplit {
                direction: SplitDirection::Vertical,
                before: false,
                detached: false,
                full_size: false,
            },
        )
        .expect("split succeeds")
        .pane_index
}

/// Adds a floating source and a floating witness to the shared window `a:0`.
/// Returns their pane ids; the original tile keeps index 0.
fn floating_fixture(store: &mut ManagedSessionStore) -> (PaneId, PaneId) {
    let owner = target("a", 0);
    let source = tiled_split(store, &owner, 0);
    let witness = tiled_split(store, &owner, source);
    let source_id = store.window(&owner).unwrap().pane(source).unwrap().id();
    let witness_id = store.window(&owner).unwrap().pane(witness).unwrap().id();
    store
        .window_layout_context_mut(&owner)
        .unwrap()
        .float_pane(source, rect(3, 2, 30, 8))
        .unwrap();
    store
        .window_layout_context_mut(&owner)
        .unwrap()
        .float_pane(witness, rect(50, 4, 12, 5))
        .unwrap();
    let link = store.target_link(&owner).unwrap();
    assert!(store.windows.resolve_mut(link).unwrap().select_pane(0));
    (source_id, witness_id)
}

fn index_of(store: &ManagedSessionStore, target: &WindowTarget, pane_id: PaneId) -> u32 {
    store
        .window(target)
        .unwrap()
        .panes()
        .iter()
        .find(|pane| pane.id() == pane_id)
        .expect("pane survives")
        .index()
}

fn stack_ids(store: &ManagedSessionStore, target: &WindowTarget) -> Vec<PaneId> {
    store
        .window(target)
        .unwrap()
        .floating_stack()
        .placements()
        .iter()
        .map(|placement| placement.pane_id())
        .collect()
}

#[test]
fn two_winlinks_of_one_window_share_a_single_floating_transfer() {
    // Source addressed through `a:0`, target through `b:7`: the same WindowId,
    // so the store must run one in-window transfer and leave every alias equal.
    let mut store = fixture();
    let (source_id, witness_id) = floating_fixture(&mut store);
    let source_link = target("a", 0);
    let alias_link = target("b", 7);
    let owner_id = store.window(&source_link).unwrap().id();
    assert_eq!(store.window(&alias_link).unwrap().id(), owner_id);
    let source_index = index_of(&store, &source_link, source_id);

    let transfer = store
        .join_pane(
            &source_link,
            source_index,
            &alias_link,
            0,
            PaneJoinOptions::new(SplitDirection::Vertical, false, false, false, None),
        )
        .expect("a floating source may share a tiled target through an alias");

    assert_eq!(transfer.source_window_id, owner_id);
    assert_eq!(transfer.target_window_id, owner_id);
    assert_eq!(transfer.pane_id, source_id);
    assert!(transfer.destroyed_window.is_none());
    for link in [
        target("a", 0),
        target("a", 4),
        target("a", 9),
        target("b", 7),
    ] {
        let window = store.window(&link).unwrap();
        assert_eq!(window.id(), owner_id);
        assert_eq!(window.pane_count(), 3, "{link:?} sees one mutation");
        assert!(!window.is_floating_pane_id(source_id), "{link:?}");
        assert_eq!(stack_ids(&store, &link), vec![witness_id], "{link:?}");
        assert_eq!(
            window.pane(transfer.pane_index).unwrap().id(),
            source_id,
            "{link:?}"
        );
    }
}

#[test]
fn a_floating_source_reaches_another_owner_and_renumbers_only_its_own_window() {
    let mut store = fixture();
    let (source_id, witness_id) = floating_fixture(&mut store);
    let owner = target("a", 0);
    let destination = target("b", 2);
    let owner_id = store.window(&owner).unwrap().id();
    let destination_id = store.window(&destination).unwrap().id();
    assert_ne!(owner_id, destination_id);
    let destination_pane_id = store.window(&destination).unwrap().pane(0).unwrap().id();
    let source_index = index_of(&store, &owner, source_id);

    let transfer = store
        .join_pane(
            &owner,
            source_index,
            &destination,
            0,
            PaneJoinOptions::new(SplitDirection::Vertical, false, false, false, None),
        )
        .expect("a floating source may cross owners");

    assert_eq!(transfer.source_window_id, owner_id);
    assert_eq!(transfer.target_window_id, destination_id);
    assert_eq!(transfer.pane_id, source_id);
    assert!(transfer.destroyed_window.is_none());
    let moved = store.window(&destination).unwrap();
    assert_eq!(moved.pane_count(), 2);
    assert_eq!(moved.pane(transfer.pane_index).unwrap().id(), source_id);
    assert!(!moved.is_floating_pane_id(source_id));
    assert!(moved.floating_stack().is_empty());
    assert_eq!(
        moved
            .panes()
            .iter()
            .map(|pane| (pane.index(), pane.id()))
            .collect::<Vec<_>>(),
        vec![(0, destination_pane_id), (1, source_id)]
    );
    for link in [
        target("a", 0),
        target("a", 4),
        target("a", 9),
        target("b", 7),
    ] {
        let window = store.window(&link).unwrap();
        assert_eq!(window.pane_count(), 2, "{link:?}");
        assert_eq!(stack_ids(&store, &link), vec![witness_id], "{link:?}");
    }
}

#[test]
fn the_last_floating_pane_of_a_linked_window_destroys_every_source_alias() {
    let mut store = fixture();
    let owner = target("a", 0);
    let destination = target("b", 2);
    let owner_id = store.window(&owner).unwrap().id();
    let source_id = store.window(&owner).unwrap().pane(0).unwrap().id();
    store
        .window_layout_context_mut(&owner)
        .unwrap()
        .float_pane(0, rect(3, 2, 30, 8))
        .expect("float the only pane");
    assert!(store.window(&owner).unwrap().layout_dump().is_empty());

    let transfer = store
        .join_pane(
            &owner,
            0,
            &destination,
            0,
            PaneJoinOptions::new(SplitDirection::Vertical, false, false, false, None),
        )
        .expect("the last floating pane may leave a linked window");

    assert_eq!(transfer.source_window_id, owner_id);
    assert_eq!(transfer.pane_id, source_id);
    assert!(
        transfer.destroyed_window.is_some(),
        "the emptied owner and all its links are destroyed"
    );
    for link in [
        target("a", 0),
        target("a", 4),
        target("a", 9),
        target("b", 7),
    ] {
        assert!(store.window(&link).is_err(), "{link:?} must be gone");
    }
    let moved = store.window(&destination).unwrap();
    assert_eq!(moved.pane(transfer.pane_index).unwrap().id(), source_id);
    assert!(!moved.is_floating_pane_id(source_id));
}

#[test]
fn a_floating_target_refusal_leaves_every_alias_and_owner_untouched() {
    let mut store = fixture();
    let (source_id, witness_id) = floating_fixture(&mut store);
    let owner = target("a", 0);
    let alias = target("b", 7);
    let source_index = index_of(&store, &owner, source_id);
    let witness_index = index_of(&store, &owner, witness_id);
    let snapshot = format!("{store:?}");

    for (from, to, from_link, to_link) in [
        (source_index, witness_index, owner.clone(), alias.clone()),
        (0, witness_index, alias.clone(), owner.clone()),
        (witness_index, witness_index, owner.clone(), alias.clone()),
    ] {
        let error = store
            .join_pane(
                &from_link,
                from,
                &to_link,
                to,
                PaneJoinOptions::new(SplitDirection::Vertical, false, false, false, None),
            )
            .expect_err("a floating target stays refused");
        assert!(
            error.to_string().contains("floating placement")
                || error
                    .to_string()
                    .contains("source and target panes must be different"),
            "unexpected refusal: {error}"
        );
        assert_eq!(format!("{store:?}"), snapshot, "refusal changed the store");
    }
}
