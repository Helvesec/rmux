use super::*;
use crate::PaneId;
use rmux_proto::{LayoutName, TerminalSize};

fn link(session: u32, index: u32) -> WindowLink {
    WindowLink {
        session_id: SessionId::new(session),
        window_index: index,
    }
}

fn window(id: u32) -> Window {
    Window::new_with_initial_pane(
        TerminalSize::new(80, 24),
        PaneId::new(id + 100),
        WindowId::new(id),
    )
}

#[test]
fn aliases_read_the_same_allocation_and_observe_borrowed_mutations() {
    let mut store = WindowStateStore::new();
    let first = link(1, 0);
    let aliases = [link(1, 4), link(2, 0)];
    let id = store.introduce(first, window(7)).unwrap();
    for alias in aliases {
        store.link(alias, id).unwrap();
        assert!(std::ptr::eq(
            store.resolve(first).unwrap(),
            store.resolve(alias).unwrap()
        ));
    }
    store
        .layout_context(aliases[0], SessionName::new("one").unwrap())
        .unwrap()
        .select_layout_with_main_pane_size(LayoutName::EvenVertical, None, None);
    for entry in [first, aliases[0], aliases[1]] {
        let owned = store.resolve(entry).unwrap();
        assert_eq!(owned.layout(), LayoutName::EvenVertical);
        assert_eq!(owned.id(), id);
        assert_eq!(owned.pane_id(0), Some(PaneId::new(107)));
    }
    store
        .layout_context(aliases[1], SessionName::new("two").unwrap())
        .unwrap()
        .save_old_layout();
    let owned = store.resolve(first).unwrap();
    assert_eq!(owned.old_layout(), Some(owned.layout_dump().as_str()));
}

#[test]
fn only_final_unlink_transfers_the_original_owner() {
    for reverse in [false, true] {
        let mut store = WindowStateStore::new();
        let first = link(1, 0);
        let second = link(2, 5);
        let id = store.introduce(first, window(9)).unwrap();
        store.link(second, id).unwrap();
        let address = std::ptr::from_ref(store.resolve(first).unwrap());
        let (removed, retained) = if reverse {
            (second, first)
        } else {
            (first, second)
        };
        assert!(store.unlink(removed).unwrap().is_none());
        assert_eq!(
            store.resolve(removed),
            Err(WindowStateError::MissingLink(removed))
        );
        assert_eq!(
            std::ptr::from_ref(store.resolve(retained).unwrap()),
            address
        );
        let owner = store.unlink(retained).unwrap().unwrap();
        assert_eq!(std::ptr::from_ref(owner.as_ref()), address);
        assert_eq!(owner.id(), id);
        assert_eq!(owner.pane_id(0), Some(PaneId::new(109)));
        assert!(store.owners.is_empty());
        assert!(store.links.is_empty());
        assert_eq!(
            store.link(first, id),
            Err(WindowStateError::MissingWindow(id))
        );
        store.introduce(first, *owner).unwrap();
        assert!(store.unlink(first).unwrap().is_some());
    }
}

// Snapshots are test-only values, never live alias mirrors.
fn snapshot(
    store: &WindowStateStore,
) -> (
    BTreeMap<WindowId, Box<Window>>,
    BTreeMap<WindowLink, WindowId>,
) {
    (store.owners.clone(), store.links.clone())
}

#[test]
fn rejected_operations_preserve_both_tables_and_existing_allocations() {
    let mut store = WindowStateStore::new();
    let occupied = link(1, 0);
    let alias = link(2, 0);
    let vacant = link(1, 8);
    let id = store.introduce(occupied, window(3)).unwrap();
    store.link(alias, id).unwrap();
    let before = snapshot(&store);
    let address = std::ptr::from_ref(store.resolve(occupied).unwrap());
    for (target, supplied, reason) in [
        (vacant, window(3), WindowStateError::DuplicateWindow(id)),
        (
            occupied,
            window(4),
            WindowStateError::OccupiedLink(occupied),
        ),
        (occupied, window(3), WindowStateError::DuplicateWindow(id)),
    ] {
        let expected = supplied.clone();
        let rejected = store.introduce(target, supplied).unwrap_err();
        assert_eq!(rejected.reason, reason);
        assert_eq!(*rejected.window, expected);
        assert_eq!(snapshot(&store), before);
    }
    for (target, id, reason) in [
        (occupied, id, WindowStateError::OccupiedLink(occupied)),
        (
            occupied,
            WindowId::new(99),
            WindowStateError::OccupiedLink(occupied),
        ),
        (
            vacant,
            WindowId::new(99),
            WindowStateError::MissingWindow(WindowId::new(99)),
        ),
    ] {
        assert_eq!(store.link(target, id), Err(reason));
        assert_eq!(snapshot(&store), before);
    }
    assert_eq!(
        store.resolve(vacant),
        Err(WindowStateError::MissingLink(vacant))
    );
    assert_eq!(
        store.unlink(vacant),
        Err(WindowStateError::MissingLink(vacant))
    );
    assert_eq!(
        store
            .layout_context(vacant, SessionName::new("one").unwrap())
            .err(),
        Some(WindowStateError::MissingLink(vacant))
    );
    assert_eq!(snapshot(&store), before);
    assert_eq!(
        std::ptr::from_ref(store.resolve(occupied).unwrap()),
        address
    );
    assert!(std::ptr::eq(
        store.resolve(occupied).unwrap(),
        store.resolve(alias).unwrap()
    ));
    // An occupied-link rejection must not leave an unlinked owner behind.
    store.introduce(vacant, window(4)).unwrap();
}

#[test]
fn atomic_session_tables_validate_all_owners_before_mutating() {
    let mut store = WindowStateStore::new();
    let first = store.introduce(link(1, 0), window(7)).unwrap();
    store.link(link(2, 0), first).unwrap();
    let pointer = std::ptr::from_ref(store.resolve(link(1, 0)).unwrap());
    let before = format!("{store:?}");
    let bad = BTreeMap::from([
        (SessionId::new(1), BTreeMap::from([(4, first)])),
        (SessionId::new(2), BTreeMap::from([(9, WindowId::new(99))])),
    ]);
    assert_eq!(
        store
            .replace_session_links(&bad, None, LinkOrderPlan::Rebuilt)
            .unwrap_err(),
        WindowStateError::MissingWindow(WindowId::new(99))
    );
    assert!(store
        .replace_session_links(&bad, Some(window(7)), LinkOrderPlan::Rebuilt)
        .is_err());
    assert!(store
        .replace_session_links(&bad, Some(window(8)), LinkOrderPlan::Rebuilt)
        .is_err());
    assert_eq!(format!("{store:?}"), before);
    let valid = BTreeMap::from([
        (SessionId::new(1), BTreeMap::from([(4, first), (5, first)])),
        (SessionId::new(2), BTreeMap::from([(9, first)])),
    ]);
    assert!(store
        .replace_session_links(&valid, None, LinkOrderPlan::Rebuilt)
        .unwrap()
        .is_empty());
    for occurrence in [link(1, 4), link(1, 5), link(2, 9)] {
        assert_eq!(
            std::ptr::from_ref(store.resolve(occurrence).unwrap()),
            pointer
        );
    }
    let final_tables = BTreeMap::from([
        (SessionId::new(1), BTreeMap::new()),
        (SessionId::new(2), BTreeMap::new()),
    ]);
    let removed = store
        .replace_session_links(&final_tables, None, LinkOrderPlan::Rebuilt)
        .unwrap();
    assert_eq!(removed.len(), 1);
    assert_eq!(std::ptr::from_ref(removed[0].as_ref()), pointer);
}
