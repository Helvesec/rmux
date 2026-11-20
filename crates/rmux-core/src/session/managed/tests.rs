use super::*;
use crate::{Session, SessionStore, WINLINK_ACTIVITY, WINLINK_BELL};
use rmux_proto::{LayoutName, SplitDirection};

fn name(value: &str) -> SessionName {
    SessionName::new(value).unwrap()
}
fn target(value: &str, index: u32) -> WindowTarget {
    WindowTarget::with_window(name(value), index)
}
fn size() -> TerminalSize {
    TerminalSize::new(80, 24)
}

fn fixture() -> ManagedSessionStore {
    let mut store = ManagedSessionStore::new();
    store.create_session(name("a"), size()).unwrap();
    store
        .create_session_with_base_index(name("b"), size(), 2)
        .unwrap();
    let id = store.session(&name("a")).unwrap().window().id();
    for target in [target("a", 4), target("a", 9), target("b", 7)] {
        store.link_window(&target, id).unwrap();
    }
    store
}

#[test]
fn creation_borrows_metadata_and_allocates_stable_unique_identities() {
    let mut store = ManagedSessionStore::new();
    assert!(store.is_empty());
    let a = store
        .create_session_with_base_index(name("a"), size(), u32::MAX)
        .unwrap();
    assert!(matches!(
        store.create_session(name("a"), size()),
        Err(RmuxError::DuplicateSession(_))
    ));
    let b = store.create_session(name("b"), size()).unwrap();
    assert_eq!((a.as_u32(), b.as_u32()), (0, 1));
    let first = store.session_by_id(a).unwrap();
    let second = store.session_by_id(b).unwrap();
    assert_eq!(first.active_window_index(), u32::MAX);
    assert_eq!(first.last_window_index(), None);
    assert_eq!(first.created_at(), first.activity_at());
    assert_eq!(first.last_attached_at(), None);
    assert_eq!(first.cwd(), None);
    assert_ne!(first.window().id(), second.window().id());
    assert_ne!(
        first.active_pane().unwrap().id(),
        second.active_pane().unwrap().id()
    );
    assert!(first.recency() < second.recency());
    let address = std::ptr::from_ref(first.window());
    store.rename_session(&name("a"), name("renamed")).unwrap();
    let first = store.session_by_id(a).unwrap();
    assert_eq!(first.name(), &name("renamed"));
    assert_eq!(std::ptr::from_ref(first.window()), address);
    assert!(store.session(&name("a")).is_none());
    assert_eq!(store.len(), 2);
    let mut ids = store.iter().map(|session| session.id()).collect::<Vec<_>>();
    ids.sort();
    assert_eq!(ids, vec![a, b]);
    let before = first.recency();
    store
        .set_cwd(&name("renamed"), Some(PathBuf::from("work")))
        .unwrap();
    store.touch_attached(&name("renamed")).unwrap();
    let attached = store.session_by_id(a).unwrap();
    assert_eq!(attached.cwd(), Some(std::path::Path::new("work")));
    assert!(attached.recency() > before);
    let attached_at = attached.last_attached_at();
    store.touch_activity(&name("renamed")).unwrap();
    assert_eq!(
        store.session_by_id(a).unwrap().last_attached_at(),
        attached_at
    );
    assert_eq!(store.session_by_id(b).unwrap().last_attached_at(), None);
}

#[test]
fn aliases_borrow_one_owner_and_existing_contexts_share_mutations() {
    let mut store = fixture();
    let first = target("a", 0);
    let targets = [
        first.clone(),
        target("a", 4),
        target("a", 9),
        target("b", 7),
    ];
    let address = std::ptr::from_ref(store.window(&first).unwrap());
    let id = store.window(&first).unwrap().id();
    // Seed a multi-pane canonical window to exercise selection contexts.
    let link = store.target_link(&first).unwrap();
    store
        .windows
        .resolve_mut(link)
        .unwrap()
        .split_at_position_with_id_and_direction(
            0,
            PaneId::new(50),
            SplitDirection::Vertical,
            false,
        );
    store
        .window_layout_context_mut(&target("a", 4))
        .unwrap()
        .select_layout_with_main_pane_size(LayoutName::EvenVertical, None, None);
    store
        .window_layout_context_mut(&target("b", 7))
        .unwrap()
        .toggle_zoom(1)
        .unwrap();
    store
        .window_selection_mut(&target("a", 9))
        .unwrap()
        .select(0, true)
        .unwrap();
    for target in &targets {
        let window = store.window(target).unwrap();
        assert_eq!(std::ptr::from_ref(window), address);
        assert_eq!(window.id(), id);
        assert_eq!(window.layout(), LayoutName::EvenVertical);
        assert_eq!(window.active_pane_index(), 0);
        assert!(window.is_zoomed());
        assert_eq!(window.last_pane_index(), Some(1));
    }
    let view = store.session(&name("a")).unwrap();
    assert_eq!(
        view.windows().map(|(i, _)| i).collect::<Vec<_>>(),
        vec![0, 4, 9]
    );
    for (_, window) in view.windows() {
        assert!(std::ptr::eq(window, view.window()));
    }
    assert_eq!(view.window_index_for_pane_id(PaneId::new(50)), Some(0));
    assert_eq!(view.window_id(4), Some(id));
    assert_eq!(view.window_id(5), None);
    assert!(!store.session(&name("b")).unwrap().window().is_zoomed());
    store
        .window_selection_mut(&target("b", 7))
        .unwrap()
        .select_last(false)
        .unwrap();
    assert_eq!(store.window(&first).unwrap().active_pane_index(), 1);
    assert!(!store.window(&first).unwrap().is_zoomed());
}

#[test]
fn navigation_alerts_and_outer_geometry_are_local_while_content_is_shared() {
    let mut store = fixture();
    for t in [
        target("a", 0),
        target("a", 4),
        target("a", 9),
        target("b", 7),
    ] {
        assert!(store.add_winlink_alert_flags(&t, WINLINK_BELL).unwrap());
        assert!(!store.add_winlink_alert_flags(&t, WINLINK_BELL).unwrap());
    }
    store.select_window(&target("a", 4)).unwrap();
    let a = store.session(&name("a")).unwrap();
    assert_eq!(a.active_window_index(), 4);
    assert_eq!(a.last_window_index(), Some(0));
    assert_eq!(a.winlink_alert_flags(4), AlertFlags::empty());
    assert_eq!(a.winlink_alert_flags(0), WINLINK_BELL);
    assert_eq!(a.winlink_alert_flags(9), WINLINK_BELL);
    assert_eq!(a.session_alert_flags(), WINLINK_BELL);
    store
        .add_winlink_alert_flags(&target("a", 4), WINLINK_ACTIVITY)
        .unwrap();
    store.select_window(&target("a", 4)).unwrap();
    assert_eq!(
        store.session(&name("a")).unwrap().winlink_alert_flags(4),
        WINLINK_ACTIVITY
    );
    assert_eq!(store.last_window(&name("a")).unwrap(), 0);
    assert_eq!(
        store.session(&name("a")).unwrap().last_window_index(),
        Some(4)
    );
    let outer = TerminalSize::new(101, 31);
    let content = TerminalSize::new(101, 30);
    {
        let mut switch = store.window_switch_mut(&target("b", 7)).unwrap();
        switch.resize(outer, content);
    }
    assert_eq!(store.session(&name("b")).unwrap().terminal_size(), size());
    {
        let mut switch = store.window_switch_mut(&target("b", 7)).unwrap();
        switch.select_window_and_pane(0, false).unwrap();
        switch.resize(outer, content);
    }
    let b = store.session(&name("b")).unwrap();
    assert_eq!(b.terminal_size(), outer);
    assert_eq!(b.last_window_index(), Some(2));
    assert_eq!(b.winlink_alert_flags(7), AlertFlags::empty());
    let a = store.session(&name("a")).unwrap();
    assert_eq!(a.terminal_size(), size());
    assert_eq!(a.window().size(), content);
    assert_eq!(a.active_window_index(), 0);
    assert_eq!(a.winlink_alert_flags(9), WINLINK_BELL);
    assert!(store
        .clear_winlink_alert_flags(&target("a", 9), WINLINK_BELL)
        .unwrap());
    assert!(!store
        .clear_winlink_alert_flags(&target("a", 9), WINLINK_BELL)
        .unwrap());
}

#[test]
fn rejected_targets_leave_metadata_links_and_windows_unchanged() {
    let mut store = fixture();
    store
        .add_winlink_alert_flags(&target("a", 4), WINLINK_BELL)
        .unwrap();
    let before = format!("{store:?}");
    let address = std::ptr::from_ref(store.window(&target("a", 0)).unwrap());
    for t in [target("missing", 0), target("a", 77)] {
        assert!(store.window(&t).is_err());
        assert!(store.window_layout_context_mut(&t).is_err());
        assert!(store.window_selection_mut(&t).is_err());
        assert!(store.window_switch_mut(&t).is_err());
        assert!(store.select_window(&t).is_err());
        assert!(store.add_winlink_alert_flags(&t, WINLINK_BELL).is_err());
        assert!(store.clear_winlink_alert_flags(&t, WINLINK_BELL).is_err());
    }
    assert!(store
        .window_selection_mut(&target("a", 4))
        .unwrap()
        .select(99, true)
        .is_err());
    assert!(store
        .window_switch_mut(&target("a", 4))
        .unwrap()
        .select_window_and_pane(99, true)
        .is_err());
    assert!(store
        .window_layout_context_mut(&target("a", 4))
        .unwrap()
        .toggle_zoom(99)
        .is_err());
    assert!(store
        .link_window(&target("missing", 0), WindowId::new(0))
        .is_err());
    assert!(store
        .link_window(&target("a", 0), WindowId::new(0))
        .is_err());
    assert!(store
        .link_window(&target("a", 77), WindowId::new(99))
        .is_err());
    assert!(store.rename_session(&name("a"), name("b")).is_err());
    assert!(store.last_window(&name("a")).is_err());
    assert_eq!(format!("{store:?}"), before);
    assert_eq!(
        std::ptr::from_ref(store.window(&target("a", 0)).unwrap()),
        address
    );
}

#[test]
fn identity_exhaustion_is_rejected_without_recycling() {
    let mut store = ManagedSessionStore {
        next_session_id: u64::from(u32::MAX),
        ..Default::default()
    };
    let id = store.create_session(name("last"), size()).unwrap();
    assert_eq!(id.as_u32(), u32::MAX);
    let before = format!("{store:?}");
    assert!(store.create_session(name("overflow"), size()).is_err());
    assert_eq!(format!("{store:?}"), before);
}

#[test]
fn standalone_constructors_references_and_clones_keep_value_semantics() {
    let session = Session::new(name("standalone"), size());
    let original: &Window = session.window();
    let mut cloned = session.clone();
    assert_eq!(&cloned, &session);
    assert!(!std::ptr::eq(original, cloned.window()));
    cloned.select_layout(LayoutName::EvenVertical);
    assert_ne!(original.layout(), cloned.window().layout());
    let mut store = SessionStore::new();
    store.create_session(name("stored"), size()).unwrap();
    let copied = store.clone();
    let stored: &Session = store.session(&name("stored")).unwrap();
    let copied_session: &Session = copied.session(&name("stored")).unwrap();
    assert_eq!(stored, copied_session);
    assert!(!std::ptr::eq(stored.window(), copied_session.window()));
}

mod lifecycle;

mod target_find;

mod index_lifecycle;

// Deliberately asymmetric internal fixtures for resolver and rejection tests.
// Production managed operations must never make this state reachable.
fn unlink_fixture_occurrence(store: &mut ManagedSessionStore, target: &WindowTarget) {
    let link = store.target_link(target).unwrap();
    assert!(store.windows.unlink(link).unwrap().is_none());
    let session = store.sessions.get_mut(target.session_name()).unwrap();
    assert_ne!(session.active_window, link.window_index);
    session.winlink_alert_flags.remove(&link.window_index);
    if session.last_window == Some(link.window_index) {
        session.last_window = None;
    }
}

mod group_removal;

mod panes;

mod floating;

mod transfers;

mod break_history;
mod group_name_reuse;

mod preview;

mod structural_checkpoint;

mod group_navigation;
mod runtime_break;
mod swap_peers;

mod transfers_floating;

mod transfers_floating_fmt1;
