use super::*;

fn set_named(store: &mut OptionStore, scope: OptionScopeSelector, name: &str, value: &str) {
    store
        .set_by_name(
            scope,
            name,
            Some(value.to_owned()),
            SetOptionMode::Replace,
            false,
            false,
            false,
        )
        .expect("valid explicit override");
}

fn populated_store() -> (OptionStore, PaneTarget, PaneTarget) {
    let alpha = session_name("alpha");
    let pane = PaneTarget::with_window(alpha.clone(), 0, 0);
    let neighbor = PaneTarget::with_window(alpha.clone(), 1, 2);
    let mut store = OptionStore::new();
    set_named(
        &mut store,
        OptionScopeSelector::ServerGlobal,
        "@server",
        "server",
    );
    set_named(
        &mut store,
        OptionScopeSelector::Session(alpha.clone()),
        "@session",
        "session",
    );
    set_named(
        &mut store,
        OptionScopeSelector::Window(WindowTarget::with_window(alpha, 0)),
        "@window",
        "window",
    );
    for target in [&pane, &neighbor] {
        let scope = OptionScopeSelector::Pane(target.clone());
        set_named(&mut store, scope.clone(), "@pane", &target.to_string());
        set_named(&mut store, scope.clone(), "history-limit", "1234");
        set_named(&mut store, scope.clone(), "status-format[1]", "first");
        set_named(&mut store, scope, "status-format[4]", "last");
    }
    (store, pane, neighbor)
}

#[test]
fn pane_self_swap_preserves_empty_store_without_panicking() {
    let mut store = OptionStore::new();
    let pane = PaneTarget::with_window(session_name("alpha"), 0, 0);
    let before = store.clone();

    store.swap_pane_overrides(&pane, &pane);

    assert_eq!(store, before);
    assert!(store.is_empty());
}

#[test]
fn pane_self_swap_preserves_all_populated_store_contents_without_panicking() {
    let (mut store, pane, _) = populated_store();
    let before = store.clone();

    store.swap_pane_overrides(&pane, &pane.clone());

    assert_eq!(store, before);
}

#[test]
fn pane_self_swap_preserves_populated_store_when_target_has_no_overrides() {
    let (mut store, _, _) = populated_store();
    let missing = PaneTarget::with_window(session_name("beta"), 8, 3);
    let before = store.clone();

    store.swap_pane_overrides(&missing, &missing);

    assert_eq!(store, before);
}

#[test]
fn pane_rekey_still_rejects_duplicate_destinations_atomically() {
    let (mut store, pane, neighbor) = populated_store();
    let before = store.clone();
    let result = store.rekey_pane_overrides(&[
        (pane.clone(), Some(neighbor.clone())),
        (neighbor.clone(), Some(neighbor)),
    ]);

    assert!(matches!(result, Err(RmuxError::Server(message))
        if message.contains("duplicate destination")));
    assert_eq!(store, before);

    let mut empty = OptionStore::new();
    assert!(empty
        .rekey_pane_overrides(&[
            (pane.clone(), Some(pane.clone())),
            (pane.clone(), Some(pane))
        ])
        .is_err());
    assert!(empty.is_empty());
}

#[test]
fn sibling_pane_copy_transfer_and_window_self_swap_preserve_contents() {
    let (mut store, pane, _) = populated_store();
    let before = store.clone();

    store.copy_pane_overrides(&pane, &pane);
    assert_eq!(store, before);
    store.transfer_pane_overrides(&pane, &pane);
    assert_eq!(store, before);
    let window = WindowTarget::with_window(session_name("alpha"), 0);
    store.swap_window_overrides(&window, &window);
    assert_eq!(store, before);
}
