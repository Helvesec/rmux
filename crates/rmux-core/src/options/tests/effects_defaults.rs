use super::*;

#[test]
fn known_options_do_not_cross_global_roots_during_resolve() {
    let mut store = OptionStore::new();
    let alpha = session_name("alpha");

    // Set a session-scoped option at server_global scope (which would not normally happen
    // via set validation, but let's test the resolve chain directly).
    // Instead, set status at session_global and verify it does NOT appear
    // when resolving a server-scoped option context.
    store
        .set(
            ScopeSelector::Global,
            OptionName::Status,
            "off".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("set succeeds");

    // status is session-scoped, resolve with no session should still find default "on"
    // because server context only looks at server_global.
    assert_eq!(store.resolve(None, OptionName::Status), Some("on"));

    // But resolve with a session should find "off" from session_global.
    assert_eq!(store.resolve(Some(&alpha), OptionName::Status), Some("off"));
}

#[cfg(unix)]
#[test]
fn unix_default_shell_starts_empty_for_runtime_resolution() {
    let store = OptionStore::new();

    assert_eq!(store.resolve(None, OptionName::DefaultShell), Some(""));
}

#[test]
fn notification_effects_are_reported_for_known_options() {
    let mut store = OptionStore::new();

    let outcome = store
        .set(
            ScopeSelector::Global,
            OptionName::Status,
            "off".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("set succeeds");

    assert_eq!(outcome.name, "status");
    assert_eq!(outcome.known_option, Some(OptionName::Status));
    assert_eq!(outcome.notifications.len(), 1);
    assert!(!outcome.notifications[0].effects.is_empty());
}

#[test]
fn notification_effects_for_user_options_default_to_pane_style() {
    let mut store = OptionStore::new();

    let outcome = store
        .set_by_name(
            OptionScopeSelector::SessionGlobal,
            "@theme",
            Some("dark".to_owned()),
            SetOptionMode::Replace,
            false,
            false,
            false,
        )
        .expect("user option set succeeds");

    assert_eq!(outcome.name, "@theme");
    assert_eq!(outcome.known_option, None);
    assert_eq!(outcome.notifications.len(), 1);
}

#[test]
fn default_size_rejects_empty_width_or_height() {
    let mut store = OptionStore::new();

    for invalid in ["x24", "80x", "x", "x0", "0x"] {
        let error = store
            .set(
                ScopeSelector::Session(session_name("alpha")),
                OptionName::DefaultSize,
                invalid.to_owned(),
                SetOptionMode::Replace,
            )
            .expect_err(&format!("'{invalid}' must fail"));
        assert_eq!(
            error,
            RmuxError::InvalidSetOption(format!("value is invalid: {invalid}"))
        );
    }

    // Valid forms still pass.
    store
        .set(
            ScopeSelector::Session(session_name("alpha")),
            OptionName::DefaultSize,
            "0x0".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("0x0 is valid");
    store
        .set(
            ScopeSelector::Session(session_name("alpha")),
            OptionName::DefaultSize,
            "200x50".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("200x50 is valid");
}

#[test]
fn append_on_choice_type_rejects_with_non_array_error() {
    let mut store = OptionStore::new();

    let error = store
        .set(
            ScopeSelector::Global,
            OptionName::StatusJustify,
            "centre".to_owned(),
            SetOptionMode::Append,
        )
        .expect_err("choice append must fail");

    assert_eq!(
        error,
        RmuxError::InvalidSetOption("status-justify is not an array option".to_owned())
    );
}

#[test]
fn append_on_number_type_rejects_with_non_array_error() {
    let mut store = OptionStore::new();

    let error = store
        .set(
            ScopeSelector::Global,
            OptionName::HistoryLimit,
            "100".to_owned(),
            SetOptionMode::Append,
        )
        .expect_err("number append must fail");

    assert_eq!(
        error,
        RmuxError::InvalidSetOption("history-limit is not an array option".to_owned())
    );
}

#[test]
fn unset_pane_overrides_rejects_non_window_scopes() {
    let mut store = OptionStore::new();

    let error = store
        .set_by_name(
            OptionScopeSelector::SessionGlobal,
            "window-style",
            None,
            SetOptionMode::Replace,
            false,
            true,
            true,
        )
        .expect_err("session scope must fail for -U");

    assert_eq!(
        error,
        RmuxError::InvalidSetOption("unset pane overrides only supports window scope".to_owned())
    );
}

#[test]
fn status_format_array_default_resolves_tmux_entries_in_snapshot() {
    let store = OptionStore::new();
    let alpha = session_name("alpha");

    // resolve() returns Option<&str> which cannot handle DefaultValue::Array,
    // but resolved() (the snapshot method) must fall back to default_value_as_string.
    let snapshot = store.resolved(&alpha);
    let value = snapshot
        .get(&OptionName::StatusFormat)
        .expect("status-format must be in snapshot");
    assert!(value.contains("#[align=left"), "missing status line entry");
    assert!(
        value.contains("#[align=centre]"),
        "missing pane-mode status entry"
    );
}

#[test]
fn status_right_default_uses_machine_name() {
    let store = OptionStore::new();
    let alpha = session_name("alpha");
    let value = store
        .resolve(Some(&alpha), OptionName::StatusRight)
        .expect("status-right default resolves");

    assert!(
        value.contains("#{=21:host_short}"),
        "status-right should show the machine name, got {value:?}"
    );
}

#[test]
fn empty_array_default_resolves_to_empty_string() {
    let store = OptionStore::new();
    let alpha = session_name("alpha");

    // pane-colours has an empty scalar default and is an array option.
    let value = store.resolve_for_window(&alpha, 0, OptionName::PaneColours);
    assert_eq!(value, Some(""));
}

#[test]
fn colour_alias_prefix_does_not_match_wrong_canonical_name() {
    // "cursor-col" should unambiguously resolve to "cursor-colour"
    // (via its "cursor-color" alias), not conflict with anything else.
    let query = resolve_option_name("cursor-col").expect("prefix resolves");
    assert_eq!(query.canonical_name(), "cursor-colour");
}

#[test]
fn frozen_registry_scope_counts_match_tmux_partitioning() {
    let metadata = registry::registry();
    let server_count = metadata
        .iter()
        .filter(|entry| entry.scope_mask() == registry::SCOPE_SERVER)
        .count();
    let session_count = metadata
        .iter()
        .filter(|entry| entry.scope_mask() == registry::SCOPE_SESSION)
        .count();
    let window_only_count = metadata
        .iter()
        .filter(|entry| entry.scope_mask() == registry::SCOPE_WINDOW)
        .count();
    let window_pane_count = metadata
        .iter()
        .filter(|entry| entry.scope_mask() == (registry::SCOPE_WINDOW | registry::SCOPE_PANE))
        .count();

    // tmux frozen: 25 server, 54 session, 67 window (51 window-only + 16 window|pane)
    assert_eq!(server_count, 25, "server options");
    assert_eq!(session_count, 54, "session options");
    assert_eq!(
        window_only_count + window_pane_count,
        67,
        "window options total"
    );
    assert_eq!(window_pane_count, 16, "window|pane dual-scope options");
}
