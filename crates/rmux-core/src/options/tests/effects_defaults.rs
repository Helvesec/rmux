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
fn tmux37_default_option_surface_matches_pinned_oracle() {
    let store = OptionStore::new();

    assert_eq!(store.resolve(None, OptionName::EscapeTime), Some("10"));
    assert_eq!(
        store.resolve(None, OptionName::TerminalOverrides),
        Some("linux*:AX@")
    );
    assert_eq!(
        store.resolve(None, OptionName::UpdateEnvironment),
        Some(
            "DISPLAY KRB5CCNAME MSYSTEM SSH_ASKPASS SSH_AUTH_SOCK SSH_AGENT_PID SSH_CONNECTION WAYLAND_DISPLAY WINDOWID XAUTHORITY XDG_CURRENT_DESKTOP XDG_SESSION_DESKTOP XDG_SESSION_TYPE"
        )
    );
    assert_eq!(
        store.resolve(None, OptionName::ModeStyle),
        Some("noattr,bg=yellow,fg=black")
    );
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
fn append_on_choice_type_behaves_like_replace() {
    let mut store = OptionStore::new();

    store
        .set(
            ScopeSelector::Global,
            OptionName::StatusJustify,
            "centre".to_owned(),
            SetOptionMode::Append,
        )
        .expect("choice append behaves like set");

    assert_eq!(
        store.global_value(OptionName::StatusJustify),
        Some("centre")
    );
}

#[test]
fn invalid_choice_value_uses_tmux_error_text() {
    let mut store = OptionStore::new();

    let error = store
        .set(
            ScopeSelector::Global,
            OptionName::Status,
            "maybe".to_owned(),
            SetOptionMode::Replace,
        )
        .expect_err("invalid choice must fail");

    assert_eq!(
        error,
        RmuxError::InvalidSetOption("unknown value: maybe".to_owned())
    );
}

#[test]
fn explicit_empty_choice_value_is_invalid_like_tmux() {
    let mut store = OptionStore::new();

    let error = store
        .set_by_name(
            OptionScopeSelector::WindowGlobal,
            "mode-keys",
            Some(String::new()),
            SetOptionMode::Replace,
            false,
            false,
            false,
        )
        .expect_err("explicit empty choice value must fail");

    assert_eq!(
        error,
        RmuxError::InvalidSetOption("unknown value: ".to_owned())
    );
}

#[test]
fn invalid_flag_value_uses_tmux_error_text() {
    let mut store = OptionStore::new();

    let error = store
        .set_by_name(
            OptionScopeSelector::WindowGlobal,
            "automatic-rename",
            Some("false".to_owned()),
            SetOptionMode::Replace,
            false,
            false,
            false,
        )
        .expect_err("invalid flag value must fail");

    assert_eq!(
        error,
        RmuxError::InvalidSetOption("bad value: false".to_owned())
    );
}

#[test]
fn append_on_number_type_behaves_like_replace() {
    let mut store = OptionStore::new();

    store
        .set(
            ScopeSelector::Global,
            OptionName::HistoryLimit,
            "100".to_owned(),
            SetOptionMode::Append,
        )
        .expect("number append behaves like set");

    assert_eq!(store.global_value(OptionName::HistoryLimit), Some("100"));
}

#[test]
fn invalid_number_values_use_tmux_error_text() {
    let mut store = OptionStore::new();

    let too_small = store
        .set(
            ScopeSelector::Global,
            OptionName::HistoryLimit,
            "-5".to_owned(),
            SetOptionMode::Replace,
        )
        .expect_err("negative history-limit must fail");
    assert_eq!(
        too_small,
        RmuxError::InvalidSetOption("value is too small: -5".to_owned())
    );

    let invalid = store
        .set(
            ScopeSelector::Global,
            OptionName::DisplayTime,
            "abc".to_owned(),
            SetOptionMode::Replace,
        )
        .expect_err("non-numeric display-time must fail");
    assert_eq!(
        invalid,
        RmuxError::InvalidSetOption("value is invalid: abc".to_owned())
    );

    for option in [
        OptionName::BaseIndex,
        OptionName::StatusInterval,
        OptionName::RepeatTime,
        OptionName::MessageLimit,
    ] {
        let too_large = store
            .set(
                ScopeSelector::Global,
                option,
                "2147483648".to_owned(),
                SetOptionMode::Replace,
            )
            .expect_err("numbers above INT_MAX must fail");
        assert_eq!(
            too_large,
            RmuxError::InvalidSetOption("value is too large: 2147483648".to_owned())
        );

        store
            .set(
                ScopeSelector::Global,
                option,
                "2147483647".to_owned(),
                SetOptionMode::Replace,
            )
            .expect("INT_MAX itself is accepted");
    }
}

#[test]
fn unset_pane_overrides_at_non_window_scope_acts_like_plain_unset() {
    // Oracle probe 2026-07-09: tmux `set -U` at a non-window scope behaves
    // exactly like -u there and leaves pane overrides untouched.
    let mut store = OptionStore::new();
    let alpha = session_name("alpha");
    let pane = PaneTarget::with_window(alpha.clone(), 1, 0);

    store
        .set(
            ScopeSelector::Global,
            OptionName::WindowStyle,
            "fg=colour7".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("global set succeeds");
    store
        .set(
            ScopeSelector::Pane(pane.clone()),
            OptionName::WindowStyle,
            "fg=colour8".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("pane set succeeds");

    store
        .set_by_name(
            OptionScopeSelector::SessionGlobal,
            "window-style",
            None,
            SetOptionMode::Replace,
            false,
            true,
            true,
        )
        .expect("-U at session scope acts like -u");

    assert_eq!(
        store.pane_value(&pane, OptionName::WindowStyle),
        Some("fg=colour8"),
        "-U at a non-window scope must not clear pane overrides"
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
    assert!(
        value.contains("#[align=left range=left"),
        "missing primary status line entry"
    );
    assert!(value.contains("#{P:"), "missing pane status-format entry");
    assert!(
        value.contains("#{S:"),
        "missing session status-format entry"
    );
}

#[cfg(windows)]
#[test]
fn status_right_default_uses_host_short_on_windows() {
    let store = OptionStore::new();
    let alpha = session_name("alpha");
    let value = store
        .resolve(Some(&alpha), OptionName::StatusRight)
        .expect("status-right default resolves");

    assert!(
        value.contains("#{=21:host_short}"),
        "status-right should show the host name, got {value:?}"
    );
    assert!(
        !value.contains("pane_title"),
        "status-right must not depend on pane title updates, got {value:?}"
    );
}

#[cfg(not(windows))]
#[test]
fn status_right_default_uses_pane_title_on_unix_like_platforms() {
    let store = OptionStore::new();
    let alpha = session_name("alpha");
    let value = store
        .resolve(Some(&alpha), OptionName::StatusRight)
        .expect("status-right default resolves");

    assert!(
        value.contains("#{=21:pane_title}"),
        "status-right should match tmux 3.7b pane-title default, got {value:?}"
    );
    assert!(
        !value.contains("host_short"),
        "status-right must not use host name on non-Windows, got {value:?}"
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

    // tmux 3.7b frozen: 25 server, 54 session, 70 window (52 window-only + 18 window|pane)
    assert_eq!(server_count, 25, "server options");
    assert_eq!(session_count, 54, "session options");
    assert_eq!(
        window_only_count + window_pane_count,
        70,
        "window options total"
    );
    assert_eq!(window_pane_count, 18, "window|pane dual-scope options");
}
