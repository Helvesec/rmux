use super::{
    validate_hook_registration, validate_hook_scope, HookGlobalRoot, HookSetOptions, HookStore,
};
use rmux_proto::{HookLifecycle, HookName, RmuxError, ScopeSelector, SessionName, WindowTarget};

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

#[test]
fn hook_store_starts_empty() {
    let store = HookStore::new();

    assert!(store.is_empty());
    assert_eq!(store.global_command(HookName::ClientAttached), None);
    assert_eq!(
        store.session_command(&session_name("alpha"), HookName::ClientAttached),
        None
    );
}

#[test]
fn set_without_index_replaces_the_array_at_index_zero() {
    let mut store = HookStore::new();

    store
        .set_with_options(
            ScopeSelector::Global,
            HookName::ClientAttached,
            "printf first".to_owned(),
            HookLifecycle::Persistent,
            HookSetOptions {
                append: true,
                index: None,
            },
        )
        .expect("append succeeds");
    store
        .set_with_options(
            ScopeSelector::Global,
            HookName::ClientAttached,
            "printf second".to_owned(),
            HookLifecycle::Persistent,
            HookSetOptions::default(),
        )
        .expect("replace succeeds");

    assert_eq!(
        store.global_bindings_view(HookGlobalRoot::Session, Some(HookName::ClientAttached)),
        vec![super::HookBindingView {
            hook: HookName::ClientAttached,
            index: 0,
            command: "printf second".to_owned(),
            lifecycle: HookLifecycle::Persistent,
        }]
    );
}

#[test]
fn append_adds_commands_in_index_order_and_dispatches_them_in_that_order() {
    let mut store = HookStore::new();
    let alpha = session_name("alpha");

    for command in ["printf one", "printf two", "printf three"] {
        store
            .set_with_options(
                ScopeSelector::Session(alpha.clone()),
                HookName::ClientAttached,
                command.to_owned(),
                HookLifecycle::Persistent,
                HookSetOptions {
                    append: true,
                    index: None,
                },
            )
            .expect("append succeeds");
    }

    let dispatches = store.dispatch(
        &ScopeSelector::Session(alpha.clone()),
        HookName::ClientAttached,
    );
    assert_eq!(
        dispatches
            .iter()
            .map(super::HookDispatch::command)
            .collect::<Vec<_>>(),
        vec!["printf one", "printf two", "printf three"]
    );
    assert_eq!(
        store.session_command_at(&alpha, HookName::ClientAttached, 2),
        Some("printf three")
    );
}

#[test]
fn indexed_set_and_unset_preserve_other_items() {
    let mut store = HookStore::new();
    let alpha = session_name("alpha");

    store
        .set_with_options(
            ScopeSelector::Session(alpha.clone()),
            HookName::ClientAttached,
            "printf zero".to_owned(),
            HookLifecycle::Persistent,
            HookSetOptions::default(),
        )
        .expect("set succeeds");
    store
        .set_with_options(
            ScopeSelector::Session(alpha.clone()),
            HookName::ClientAttached,
            "printf five".to_owned(),
            HookLifecycle::Persistent,
            HookSetOptions {
                append: false,
                index: Some(5),
            },
        )
        .expect("indexed set succeeds");

    store
        .unset(
            ScopeSelector::Session(alpha.clone()),
            HookName::ClientAttached,
            Some(0),
        )
        .expect("indexed unset succeeds");

    assert_eq!(
        store.session_command_at(&alpha, HookName::ClientAttached, 0),
        None
    );
    assert_eq!(
        store.session_command_at(&alpha, HookName::ClientAttached, 5),
        Some("printf five")
    );
}

#[test]
fn one_shot_items_are_consumed_without_removing_persistent_neighbors() {
    let mut store = HookStore::new();
    let alpha = session_name("alpha");

    store
        .set_with_options(
            ScopeSelector::Session(alpha.clone()),
            HookName::ClientAttached,
            "printf persistent".to_owned(),
            HookLifecycle::Persistent,
            HookSetOptions {
                append: true,
                index: None,
            },
        )
        .expect("persistent append succeeds");
    store
        .set_with_options(
            ScopeSelector::Session(alpha.clone()),
            HookName::ClientAttached,
            "printf once".to_owned(),
            HookLifecycle::OneShot,
            HookSetOptions {
                append: true,
                index: None,
            },
        )
        .expect("one-shot append succeeds");

    let first = store.dispatch(
        &ScopeSelector::Session(alpha.clone()),
        HookName::ClientAttached,
    );
    let second = store.dispatch(
        &ScopeSelector::Session(alpha.clone()),
        HookName::ClientAttached,
    );

    assert_eq!(
        first
            .iter()
            .map(super::HookDispatch::command)
            .collect::<Vec<_>>(),
        vec!["printf persistent", "printf once"]
    );
    assert_eq!(
        second
            .iter()
            .map(super::HookDispatch::command)
            .collect::<Vec<_>>(),
        vec!["printf persistent"]
    );
}

#[test]
fn pane_hooks_fall_back_from_pane_to_window_to_global_window_scope() {
    let mut store = HookStore::new();
    let alpha = session_name("alpha");
    let window = WindowTarget::with_window(alpha.clone(), 1);
    let pane = rmux_proto::PaneTarget::with_window(alpha.clone(), 1, 2);

    store
        .set(
            ScopeSelector::Global,
            HookName::PaneExited,
            "printf global".to_owned(),
            HookLifecycle::Persistent,
        )
        .expect("global set succeeds");
    store
        .set(
            ScopeSelector::Window(window.clone()),
            HookName::PaneExited,
            "printf window".to_owned(),
            HookLifecycle::Persistent,
        )
        .expect("window set succeeds");
    store
        .set(
            ScopeSelector::Pane(pane.clone()),
            HookName::PaneExited,
            "printf pane".to_owned(),
            HookLifecycle::Persistent,
        )
        .expect("pane set succeeds");

    assert_eq!(
        store
            .dispatch(&ScopeSelector::Pane(pane.clone()), HookName::PaneExited)
            .iter()
            .map(super::HookDispatch::command)
            .collect::<Vec<_>>(),
        vec!["printf pane"]
    );
    store
        .unset(
            ScopeSelector::Pane(pane.clone()),
            HookName::PaneExited,
            None,
        )
        .expect("pane unset succeeds");
    assert_eq!(
        store
            .dispatch(&ScopeSelector::Pane(pane.clone()), HookName::PaneExited)
            .iter()
            .map(super::HookDispatch::command)
            .collect::<Vec<_>>(),
        vec!["printf window"]
    );
    store
        .unset(
            ScopeSelector::Window(window.clone()),
            HookName::PaneExited,
            None,
        )
        .expect("window unset succeeds");
    assert_eq!(
        store
            .dispatch(&ScopeSelector::Pane(pane), HookName::PaneExited)
            .iter()
            .map(super::HookDispatch::command)
            .collect::<Vec<_>>(),
        vec!["printf global"]
    );
}

#[test]
fn removing_and_renaming_sessions_updates_window_and_pane_scopes() {
    let mut store = HookStore::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");

    store
        .set(
            ScopeSelector::Window(WindowTarget::with_window(alpha.clone(), 2)),
            HookName::WindowLayoutChanged,
            "printf layout".to_owned(),
            HookLifecycle::Persistent,
        )
        .expect("window set succeeds");
    store
        .set(
            ScopeSelector::Pane(rmux_proto::PaneTarget::with_window(alpha.clone(), 2, 3)),
            HookName::PaneExited,
            "printf pane".to_owned(),
            HookLifecycle::Persistent,
        )
        .expect("pane set succeeds");

    store
        .rename_session(&alpha, beta.clone())
        .expect("rename succeeds");

    assert_eq!(
        store.window_command(
            &WindowTarget::with_window(alpha.clone(), 2),
            HookName::WindowLayoutChanged,
        ),
        None
    );
    assert_eq!(
        store.window_command(
            &WindowTarget::with_window(beta.clone(), 2),
            HookName::WindowLayoutChanged,
        ),
        Some("printf layout")
    );
    assert!(store.remove_session(&beta));
    assert_eq!(
        store.window_command(
            &WindowTarget::with_window(beta.clone(), 2),
            HookName::WindowLayoutChanged,
        ),
        None
    );
    assert_eq!(
        store.pane_command(
            &rmux_proto::PaneTarget::with_window(beta, 2, 3),
            HookName::PaneExited,
        ),
        None
    );
}

#[test]
fn swapping_windows_rekeys_window_and_pane_hooks() {
    let mut store = HookStore::new();
    let alpha = session_name("alpha");
    let source = WindowTarget::with_window(alpha.clone(), 1);
    let target = WindowTarget::with_window(alpha.clone(), 3);
    let source_pane = rmux_proto::PaneTarget::with_window(alpha.clone(), 1, 0);
    let target_pane = rmux_proto::PaneTarget::with_window(alpha, 3, 0);

    store
        .set(
            ScopeSelector::Window(source.clone()),
            HookName::WindowLayoutChanged,
            "printf window".to_owned(),
            HookLifecycle::Persistent,
        )
        .expect("window hook set succeeds");
    store
        .set(
            ScopeSelector::Pane(source_pane.clone()),
            HookName::PaneExited,
            "printf pane".to_owned(),
            HookLifecycle::Persistent,
        )
        .expect("pane hook set succeeds");

    store.swap_window_hooks(&source, &target);

    assert_eq!(
        store.window_command(&source, HookName::WindowLayoutChanged),
        None
    );
    assert_eq!(store.pane_command(&source_pane, HookName::PaneExited), None);
    assert_eq!(
        store.window_command(&target, HookName::WindowLayoutChanged),
        Some("printf window")
    );
    assert_eq!(
        store.pane_command(&target_pane, HookName::PaneExited),
        Some("printf pane")
    );
}

#[test]
fn moving_windows_rekeys_window_and_pane_hooks() {
    let mut store = HookStore::new();
    let alpha = session_name("alpha");
    let source = WindowTarget::with_window(alpha.clone(), 2);
    let target = WindowTarget::with_window(alpha.clone(), 4);
    let source_pane = rmux_proto::PaneTarget::with_window(alpha.clone(), 2, 0);
    let target_pane = rmux_proto::PaneTarget::with_window(alpha, 4, 0);

    store
        .set(
            ScopeSelector::Window(source.clone()),
            HookName::WindowLayoutChanged,
            "printf window".to_owned(),
            HookLifecycle::Persistent,
        )
        .expect("window hook set succeeds");
    store
        .set(
            ScopeSelector::Pane(source_pane.clone()),
            HookName::PaneExited,
            "printf pane".to_owned(),
            HookLifecycle::Persistent,
        )
        .expect("pane hook set succeeds");

    store.move_window_hooks(&source, &target);

    assert_eq!(
        store.window_command(&source, HookName::WindowLayoutChanged),
        None
    );
    assert_eq!(store.pane_command(&source_pane, HookName::PaneExited), None);
    assert_eq!(
        store.window_command(&target, HookName::WindowLayoutChanged),
        Some("printf window")
    );
    assert_eq!(
        store.pane_command(&target_pane, HookName::PaneExited),
        Some("printf pane")
    );
}

#[test]
fn incompatible_hook_scope_pairs_are_rejected() {
    let alpha = session_name("alpha");

    let error = validate_hook_scope(
        HookName::WindowLayoutChanged,
        &ScopeSelector::Session(alpha),
    )
    .expect_err("window hooks reject session scope");

    assert_eq!(
        error,
        RmuxError::Server("window-layout-changed does not support session scope".to_owned())
    );
}

#[test]
fn append_with_u32_max_index_finds_next_free_slot() {
    let mut store = HookStore::new();

    // Plant an entry at u32::MAX to exercise the overflow path.
    store
        .set_with_options(
            ScopeSelector::Global,
            HookName::ClientAttached,
            "printf max".to_owned(),
            HookLifecycle::Persistent,
            HookSetOptions {
                append: false,
                index: Some(u32::MAX),
            },
        )
        .expect("indexed set succeeds");

    // Append should find index 0 (the first free slot), not collide with u32::MAX.
    let assigned = store
        .set_with_options(
            ScopeSelector::Global,
            HookName::ClientAttached,
            "printf appended".to_owned(),
            HookLifecycle::Persistent,
            HookSetOptions {
                append: true,
                index: None,
            },
        )
        .expect("append succeeds");

    assert_eq!(assigned, 0);
    assert_eq!(
        store.global_command_at(HookName::ClientAttached, u32::MAX),
        Some("printf max")
    );
    assert_eq!(
        store.global_command_at(HookName::ClientAttached, 0),
        Some("printf appended")
    );
}

#[test]
fn unset_of_nonexistent_index_is_idempotent() {
    let mut store = HookStore::new();
    let alpha = session_name("alpha");

    store
        .set(
            ScopeSelector::Session(alpha.clone()),
            HookName::ClientAttached,
            "printf present".to_owned(),
            HookLifecycle::Persistent,
        )
        .expect("set succeeds");

    // Unsetting a non-existent index should not disturb existing entries.
    store
        .unset(
            ScopeSelector::Session(alpha.clone()),
            HookName::ClientAttached,
            Some(99),
        )
        .expect("unset is idempotent");

    assert_eq!(
        store.session_command(&alpha, HookName::ClientAttached),
        Some("printf present")
    );
}

#[test]
fn dispatch_returns_commands_in_sparse_index_order() {
    let mut store = HookStore::new();

    for (index, command) in [(10, "printf ten"), (2, "printf two"), (7, "printf seven")] {
        store
            .set_with_options(
                ScopeSelector::Global,
                HookName::ClientAttached,
                command.to_owned(),
                HookLifecycle::Persistent,
                HookSetOptions {
                    append: false,
                    index: Some(index),
                },
            )
            .expect("indexed set succeeds");
    }

    let dispatches = store.dispatch(&ScopeSelector::Global, HookName::ClientAttached);
    assert_eq!(
        dispatches
            .iter()
            .map(super::HookDispatch::command)
            .collect::<Vec<_>>(),
        vec!["printf two", "printf seven", "printf ten"]
    );
}

#[test]
fn removing_a_window_also_removes_its_pane_hooks() {
    let mut store = HookStore::new();
    let alpha = session_name("alpha");
    let window = WindowTarget::with_window(alpha.clone(), 1);
    let pane = rmux_proto::PaneTarget::with_window(alpha.clone(), 1, 0);
    let other_pane = rmux_proto::PaneTarget::with_window(alpha.clone(), 2, 0);

    store
        .set(
            ScopeSelector::Window(window.clone()),
            HookName::WindowLayoutChanged,
            "printf window-hook".to_owned(),
            HookLifecycle::Persistent,
        )
        .expect("window set succeeds");
    store
        .set(
            ScopeSelector::Pane(pane.clone()),
            HookName::PaneExited,
            "printf pane-hook".to_owned(),
            HookLifecycle::Persistent,
        )
        .expect("pane set succeeds");
    store
        .set(
            ScopeSelector::Pane(other_pane.clone()),
            HookName::PaneExited,
            "printf other-pane".to_owned(),
            HookLifecycle::Persistent,
        )
        .expect("other pane set succeeds");

    assert!(store.remove_window(&window));

    // Window hooks and pane hooks under that window should be gone.
    assert_eq!(
        store.window_command(&window, HookName::WindowLayoutChanged),
        None
    );
    assert_eq!(store.pane_command(&pane, HookName::PaneExited), None);

    // Panes under a different window should survive.
    assert_eq!(
        store.pane_command(&other_pane, HookName::PaneExited),
        Some("printf other-pane")
    );
}

#[test]
fn all_one_shot_entries_consumed_leaves_hook_clean() {
    let mut store = HookStore::new();
    let alpha = session_name("alpha");

    store
        .set(
            ScopeSelector::Session(alpha.clone()),
            HookName::ClientAttached,
            "printf once".to_owned(),
            HookLifecycle::OneShot,
        )
        .expect("set succeeds");

    let first = store.dispatch(
        &ScopeSelector::Session(alpha.clone()),
        HookName::ClientAttached,
    );
    assert_eq!(first.len(), 1);

    // After the one-shot is consumed, the session bindings should be empty
    // and fall through to global.
    let second = store.dispatch(
        &ScopeSelector::Session(alpha.clone()),
        HookName::ClientAttached,
    );
    assert!(second.is_empty());
}

#[test]
fn hook_inventory_matches_tmux_order_and_keeps_notify_only_hooks_out_of_the_table_tail() {
    let inventory = super::hook_inventory();

    assert_eq!(inventory.len(), 70);
    assert_eq!(inventory[0], HookName::AfterBindKey);
    assert_eq!(inventory[27], HookName::AfterSelectWindow);
    assert_eq!(inventory[42], HookName::ClientAttached);
    assert_eq!(inventory[43], HookName::ClientDetached);
    assert_eq!(inventory[50], HookName::CommandError);
    assert_eq!(inventory[67], HookName::WindowUnlinked);
    assert_eq!(inventory[68], HookName::PasteBufferChanged);
    assert_eq!(inventory[69], HookName::PasteBufferDeleted);
}

#[test]
fn new_hook_scope_classes_match_tmux_inventory() {
    let alpha = session_name("alpha");
    let window = WindowTarget::with_window(alpha.clone(), 1);
    let pane = rmux_proto::PaneTarget::with_window(alpha.clone(), 1, 2);

    assert!(validate_hook_scope(HookName::AlertBell, &ScopeSelector::Global).is_ok());
    assert!(
        validate_hook_scope(HookName::AlertBell, &ScopeSelector::Session(alpha.clone())).is_ok()
    );
    assert!(
        validate_hook_scope(HookName::AlertBell, &ScopeSelector::Window(window.clone())).is_err()
    );

    assert!(validate_hook_scope(HookName::WindowResized, &ScopeSelector::Global).is_ok());
    assert!(validate_hook_scope(
        HookName::WindowResized,
        &ScopeSelector::Window(window.clone())
    )
    .is_ok());
    assert!(
        validate_hook_scope(HookName::WindowResized, &ScopeSelector::Pane(pane.clone())).is_err()
    );

    assert!(validate_hook_scope(HookName::PaneTitleChanged, &ScopeSelector::Global).is_ok());
    assert!(
        validate_hook_scope(HookName::PaneTitleChanged, &ScopeSelector::Window(window)).is_ok()
    );
    assert!(validate_hook_scope(HookName::PaneTitleChanged, &ScopeSelector::Pane(pane)).is_ok());
}

#[test]
fn shipped_hooks_accept_registration_at_supported_scopes() {
    let alpha = session_name("alpha");
    let window = WindowTarget::with_window(alpha.clone(), 1);
    let pane = rmux_proto::PaneTarget::with_window(alpha.clone(), 1, 2);

    assert!(validate_hook_registration(HookName::ClientAttached, &ScopeSelector::Global).is_ok());
    assert!(validate_hook_registration(
        HookName::WindowLayoutChanged,
        &ScopeSelector::Window(window.clone())
    )
    .is_ok());
    assert!(
        validate_hook_registration(HookName::WindowResized, &ScopeSelector::Window(window)).is_ok()
    );
    assert!(validate_hook_registration(HookName::PaneExited, &ScopeSelector::Pane(pane)).is_ok());
    assert!(validate_hook_registration(HookName::AfterShowOptions, &ScopeSelector::Global).is_ok());
}

#[test]
fn undispatched_hooks_are_rejected_for_registration() {
    assert!(validate_hook_registration(HookName::ClientDarkTheme, &ScopeSelector::Global).is_err());
    assert!(
        validate_hook_registration(HookName::PaneTitleChanged, &ScopeSelector::Global).is_err()
    );
}
