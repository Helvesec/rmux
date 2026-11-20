use super::*;

const NAMES: &[&str] = &[
    "window-pane-status-format",
    "window-pane-current-status-format",
    "tree-mode-preview-format",
];

fn put(store: &mut OptionStore, scope: OptionScopeSelector, name: &str, value: &str) {
    store
        .set_by_name(
            scope,
            name,
            Some(value.into()),
            SetOptionMode::Replace,
            false,
            false,
            false,
        )
        .unwrap();
}

#[test]
fn old_metadata_projection_is_the_independently_frozen_149_domain() {
    let frozen: &[(u32, OptionName, &str)] = include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../rmux-proto/tests/fixtures/option_names_4961.rs"
    ));
    for &(_, option, name) in frozen {
        assert_eq!(registry::option_metadata(option).name(), name);
        assert_eq!(
            resolve_option_name(name).unwrap().known_option(),
            Some(option)
        );
    }
    let store = OptionStore::new();
    assert_eq!(store.resolved(&session_name("a")).len(), 149);
    assert_eq!(store.resolved_for_pane(&session_name("a"), 0, 0).len(), 149);
}

#[test]
fn named_only_options_keep_registered_roots_visibility_and_mutations() {
    let session = session_name("a");
    let window = WindowTarget::with_window(session.clone(), 0);
    let local = OptionScopeSelector::Window(window.clone());
    for &name in NAMES {
        let mut store = OptionStore::new();
        let query = resolve_option_name(name).unwrap();
        assert!(!query.is_user());
        assert_eq!(query.known_option(), None);
        assert_eq!(query.canonical_name(), name);
        assert_eq!(query.effects(), registry::EFFECT_RENDER);
        assert_eq!(
            default_global_scope_for_option_name(name).unwrap(),
            OptionScopeSelector::WindowGlobal
        );
        let default = store
            .resolve_name_for_window(&session, 0, name)
            .unwrap()
            .to_owned();
        assert!(!default.is_empty());
        put(
            &mut store,
            OptionScopeSelector::ServerGlobal,
            name,
            "SERVER",
        );
        put(
            &mut store,
            OptionScopeSelector::SessionGlobal,
            name,
            "SESSION",
        );
        assert_eq!(
            store.resolve_name_for_window(&session, 0, name).as_deref(),
            Some("SERVER")
        );
        assert_eq!(
            store
                .show_options_lines_filtered(&OptionScopeSelector::WindowGlobal, Some(name), true)
                .unwrap(),
            std::slice::from_ref(&default)
        );
        let mut store = OptionStore::new();
        put(
            &mut store,
            OptionScopeSelector::WindowGlobal,
            name,
            "GLOBAL",
        );
        assert_eq!(
            store.resolve_name_for_window(&session, 0, name).as_deref(),
            Some("GLOBAL")
        );
        let outcome = store
            .set_by_name(
                local.clone(),
                name,
                Some("LOCAL".into()),
                SetOptionMode::Replace,
                true,
                false,
                false,
            )
            .unwrap();
        assert!(outcome.changed);
        assert_eq!(outcome.known_option, None);
        assert!(
            !store
                .set_by_name(
                    local.clone(),
                    name,
                    Some("LOCAL".into()),
                    SetOptionMode::Replace,
                    false,
                    false,
                    false
                )
                .unwrap()
                .changed
        );
        assert!(store
            .set_by_name(
                local.clone(),
                name,
                Some("NO".into()),
                SetOptionMode::Replace,
                true,
                false,
                false
            )
            .is_err());
        store
            .set_by_name(
                local.clone(),
                name,
                Some("+".into()),
                SetOptionMode::Append,
                false,
                false,
                false,
            )
            .unwrap();
        assert_eq!(
            store.resolve_name_for_window(&session, 0, name).as_deref(),
            Some("LOCAL+")
        );
        put(&mut store, local.clone(), name, "");
        assert_eq!(
            store
                .show_options_lines_filtered(&local, Some(name), true)
                .unwrap(),
            [""]
        );
        for mode in [
            ShowOptionsMode::Explicit,
            ShowOptionsMode::Resolved,
            ShowOptionsMode::ResolvedWithInheritanceMarkers,
        ] {
            let lines = store
                .show_options_lines_with_mode(&local, false, mode)
                .unwrap();
            assert_eq!(
                lines
                    .iter()
                    .filter(|line| line.split([' ', '*']).next() == Some(name))
                    .count(),
                1,
                "{lines:?}"
            );
        }
        assert!(store
            .set_by_name(
                local.clone(),
                name,
                None,
                SetOptionMode::Replace,
                false,
                false,
                false
            )
            .is_err());
        store
            .set_by_name(
                local.clone(),
                name,
                None,
                SetOptionMode::Replace,
                false,
                true,
                false,
            )
            .unwrap();
        assert_eq!(
            store.resolve_name_for_window(&session, 0, name).as_deref(),
            Some("GLOBAL")
        );
        store
            .set_by_name(
                local.clone(),
                name,
                Some("new".into()),
                SetOptionMode::Append,
                false,
                false,
                false,
            )
            .unwrap();
        assert_eq!(
            store.resolve_name_for_window(&session, 0, name).as_deref(),
            Some("new")
        );
        let copy = WindowTarget::with_window(session.clone(), 1);
        store.copy_window_overrides(&window, &copy);
        assert_eq!(
            store.resolve_name_for_window(&session, 1, name).as_deref(),
            Some("new")
        );
        let pane = PaneTarget::with_window(session.clone(), 0, 0);
        put(
            &mut store,
            OptionScopeSelector::Pane(pane.clone()),
            name,
            "raw-pane",
        );
        assert_eq!(
            store.resolve_name_for_pane(&session, 0, 0, name).as_deref(),
            Some("raw-pane")
        );
        let moved = PaneTarget::with_window(session.clone(), 1, 3);
        store.transfer_pane_overrides(&pane, &moved);
        assert_eq!(
            store.resolve_name_for_pane(&session, 1, 3, name).as_deref(),
            Some("raw-pane")
        );
        store.remove_pane(&moved);
        assert_eq!(
            store.resolve_name_for_pane(&session, 1, 3, name).as_deref(),
            Some("new")
        );
        store
            .set_by_name(
                OptionScopeSelector::WindowGlobal,
                name,
                None,
                SetOptionMode::Replace,
                false,
                true,
                false,
            )
            .unwrap();
        assert_eq!(
            store.resolve_name_for_window(&session, 2, name).as_deref(),
            Some(default.as_str())
        );
    }
    for (prefix, name) in [("window-pane-s", NAMES[0]), ("window-pane-c", NAMES[1])] {
        assert_eq!(resolve_option_name(prefix).unwrap().canonical_name(), name);
    }
}

#[test]
fn named_preview_style_validates_atomically_and_preserves_raw_dynamic_values() {
    let name = "tree-mode-preview-style";
    let scope = OptionScopeSelector::WindowGlobal;
    let mut store = OptionStore::new();
    let query = resolve_option_name(name).unwrap();
    assert!(!query.is_user());
    assert_eq!(query.known_option(), None);
    assert_eq!(
        query.effects(),
        registry::EFFECT_RENDER.union(registry::EFFECT_STYLE_PARSE)
    );
    put(&mut store, scope.clone(), name, "fg=red");
    let before = store
        .show_options_lines_filtered(&scope, None, false)
        .unwrap();
    for mode in [SetOptionMode::Replace, SetOptionMode::Append] {
        assert!(store
            .set_by_name(
                scope.clone(),
                name,
                Some("not-a-style".into()),
                mode,
                false,
                false,
                false
            )
            .is_err());
        assert_eq!(
            store
                .show_options_lines_filtered(&scope, None, false)
                .unwrap(),
            before
        );
    }
    store
        .set_by_name(
            scope.clone(),
            name,
            Some("bg=blue".into()),
            SetOptionMode::Append,
            false,
            false,
            false,
        )
        .unwrap();
    assert_eq!(
        store
            .show_options_lines_filtered(&scope, Some(name), true)
            .unwrap(),
        ["fg=red,bg=blue"]
    );
    let dynamic = "#{?pane_active,not-a-style,fg=green}";
    put(&mut store, scope.clone(), name, dynamic);
    assert_eq!(
        store
            .show_options_lines_filtered(&scope, Some(name), true)
            .unwrap(),
        [dynamic]
    );
    assert!(resolve_option_name("tree-mode-preview-").is_err());
    for (prefix, full) in [
        ("tree-mode-preview-f", "tree-mode-preview-format"),
        ("tree-mode-preview-s", name),
    ] {
        assert_eq!(resolve_option_name(prefix).unwrap().canonical_name(), full);
    }
    let pane = PaneTarget::with_window(session_name("a"), 0, 0);
    put(
        &mut store,
        OptionScopeSelector::Pane(pane.clone()),
        name,
        "fg=cyan",
    );
    let outcome = store
        .set_by_name(
            OptionScopeSelector::Window(WindowTarget::with_window(session_name("a"), 0)),
            name,
            None,
            SetOptionMode::Replace,
            false,
            true,
            true,
        )
        .unwrap();
    assert!(!outcome.changed);
    assert_eq!(outcome.related.len(), 1);
    assert!(outcome.related[0].changed);
    assert_eq!(
        store
            .resolve_name_for_pane(&session_name("a"), 0, 0, name)
            .as_deref(),
        Some(dynamic)
    );
}
