use super::*;

#[test]
fn option_store_starts_without_explicit_overrides_but_resolves_defaults() {
    let store = OptionStore::new();

    assert!(store.is_empty());
    assert_eq!(store.global_value(OptionName::Status), None);
    assert_eq!(
        store.resolve(Some(&session_name("alpha")), OptionName::Status),
        Some("on")
    );
    assert_eq!(
        store.resolve(None, OptionName::DefaultTerminal),
        Some("tmux-256color")
    );
    assert_eq!(
        store.resolve_for_window(&session_name("alpha"), 0, OptionName::AutomaticRenameFormat),
        Some("#{?pane_in_mode,[tmux],#{pane_current_command}}#{?pane_dead,[dead],}")
    );
}

#[test]
fn session_values_override_global_values_at_read_time() {
    let mut store = OptionStore::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    let alpha_window = WindowTarget::with_window(alpha.clone(), 0);
    let beta_window = WindowTarget::with_window(beta.clone(), 0);

    store
        .set(
            ScopeSelector::Global,
            OptionName::PaneBorderStyle,
            "colour1".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("global set succeeds");
    store
        .set(
            ScopeSelector::Window(alpha_window.clone()),
            OptionName::PaneBorderStyle,
            "colour2".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("window set succeeds");

    assert_eq!(
        store.resolve_for_window(&alpha, 0, OptionName::PaneBorderStyle),
        Some("colour2")
    );
    assert_eq!(
        store.resolve_for_window(beta_window.session_name(), 0, OptionName::PaneBorderStyle),
        Some("colour1")
    );
}

#[test]
fn pane_values_override_window_session_and_global_values() {
    let mut store = OptionStore::new();
    let alpha = session_name("alpha");
    let window = WindowTarget::with_window(alpha.clone(), 2);
    let pane = PaneTarget::with_window(alpha.clone(), 2, 3);

    store
        .set(
            ScopeSelector::Global,
            OptionName::WindowStyle,
            "fg=colour1".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("global set succeeds");
    store
        .set(
            ScopeSelector::Window(window.clone()),
            OptionName::WindowStyle,
            "fg=colour2".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("window set succeeds");
    store
        .set(
            ScopeSelector::Pane(pane.clone()),
            OptionName::WindowStyle,
            "fg=colour3".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("pane set succeeds");

    assert_eq!(
        store.resolve_for_pane(&alpha, 2, 3, OptionName::WindowStyle),
        Some("fg=colour3")
    );
    assert_eq!(
        store.resolve_for_pane(&alpha, 2, 4, OptionName::WindowStyle),
        Some("fg=colour2")
    );
    assert_eq!(
        store.resolve_for_pane(&alpha, 5, 0, OptionName::WindowStyle),
        Some("fg=colour1")
    );
}

#[test]
fn terminal_features_append_preserves_default_then_existing_order() {
    let mut store = OptionStore::new();

    store
        .set(
            ScopeSelector::Global,
            OptionName::TerminalFeatures,
            ",xterm*:RGB".to_owned(),
            SetOptionMode::Append,
        )
        .expect("first append succeeds");
    store
        .set(
            ScopeSelector::Global,
            OptionName::TerminalFeatures,
            "screen*:AX".to_owned(),
            SetOptionMode::Append,
        )
        .expect("second append succeeds");

    assert_eq!(
        store.global_value(OptionName::TerminalFeatures),
        Some(
            "xterm*:clipboard:ccolour:cstyle:focus:title,screen*:title,rxvt*:ignorefkeys,xterm*:RGB,screen*:AX"
        )
    );
}

#[test]
fn append_to_non_appendable_options_is_rejected_without_creating_overrides() {
    let mut store = OptionStore::new();

    let error = store
        .set(
            ScopeSelector::Global,
            OptionName::Status,
            "off".to_owned(),
            SetOptionMode::Append,
        )
        .expect_err("status append must fail");

    assert_eq!(
        error,
        rmux_proto::RmuxError::InvalidSetOption("status is not an array option".to_owned())
    );
    assert!(store.is_empty());
}

#[test]
fn allow_passthrough_rejects_all_until_all_pane_routing_exists() {
    let mut store = OptionStore::new();

    let error = store
        .set(
            ScopeSelector::Global,
            OptionName::AllowPassthrough,
            "all".to_owned(),
            SetOptionMode::Replace,
        )
        .expect_err("all-pane passthrough is not implemented");

    assert_eq!(
        error,
        rmux_proto::RmuxError::InvalidSetOption(
            "allow-passthrough expects one of: off, on".to_owned()
        )
    );
    assert!(store.is_empty());
}

#[test]
fn server_only_options_reject_session_scopes() {
    let mut store = OptionStore::new();

    let error = store
        .set(
            ScopeSelector::Session(session_name("alpha")),
            OptionName::BufferLimit,
            "100".to_owned(),
            SetOptionMode::Replace,
        )
        .expect_err("session buffer-limit must fail");

    assert_eq!(
        error,
        rmux_proto::RmuxError::InvalidSetOption(
            "buffer-limit is only supported at global scope".to_owned()
        )
    );
}

#[test]
fn utf8_rendering_options_are_marked_as_render_affecting() {
    assert!(registry::option_affects_rendering(
        OptionName::CodepointWidths
    ));
    assert!(registry::option_affects_rendering(
        OptionName::VariationSelectorAlwaysWide
    ));
    assert!(!registry::option_affects_rendering(OptionName::ExitEmpty));
}

#[test]
fn resolved_snapshots_merge_defaults_global_session_window_and_pane_values() {
    let mut store = OptionStore::new();
    let alpha = session_name("alpha");

    store
        .set(
            ScopeSelector::Global,
            OptionName::Status,
            "off".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("global status succeeds");
    store
        .set(
            ScopeSelector::Session(alpha.clone()),
            OptionName::Status,
            "on".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("session status succeeds");
    store
        .set(
            ScopeSelector::Window(WindowTarget::with_window(alpha.clone(), 1)),
            OptionName::WindowStyle,
            "fg=colour2".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("window style succeeds");
    store
        .set(
            ScopeSelector::Pane(PaneTarget::with_window(alpha.clone(), 1, 2)),
            OptionName::WindowStyle,
            "fg=colour3".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("pane style succeeds");

    let resolved = store.resolved_for_pane(&alpha, 1, 2);
    assert_eq!(
        resolved.get(&OptionName::Status).map(String::as_str),
        Some("on")
    );
    assert_eq!(
        resolved.get(&OptionName::WindowStyle).map(String::as_str),
        Some("fg=colour3")
    );
    assert_eq!(
        resolved
            .get(&OptionName::DefaultTerminal)
            .map(String::as_str),
        Some("tmux-256color")
    );
}

#[test]
fn removing_a_session_discards_session_window_and_pane_values() {
    let mut store = OptionStore::new();
    let alpha = session_name("alpha");

    store
        .set(
            ScopeSelector::Session(alpha.clone()),
            OptionName::Status,
            "off".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("session set succeeds");
    store
        .set(
            ScopeSelector::Window(WindowTarget::with_window(alpha.clone(), 3)),
            OptionName::MainPaneWidth,
            "90".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("window set succeeds");
    store
        .set(
            ScopeSelector::Pane(PaneTarget::with_window(alpha.clone(), 3, 1)),
            OptionName::WindowStyle,
            "default,bold".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("pane set succeeds");

    assert!(store.remove_session(&alpha).is_some());
    assert_eq!(store.resolve(Some(&alpha), OptionName::Status), Some("on"));
    assert_eq!(
        store.resolve_for_pane(&alpha, 3, 1, OptionName::MainPaneWidth),
        Some("80")
    );
    assert_eq!(
        store.resolve_for_pane(&alpha, 3, 1, OptionName::WindowStyle),
        Some("default")
    );
}

#[test]
fn renaming_a_session_rekeys_session_window_and_pane_values() {
    let mut store = OptionStore::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");

    store
        .set(
            ScopeSelector::Session(alpha.clone()),
            OptionName::Status,
            "off".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("session set succeeds");
    store
        .set(
            ScopeSelector::Window(WindowTarget::with_window(alpha.clone(), 3)),
            OptionName::MainPaneWidth,
            "90".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("window set succeeds");
    store
        .set(
            ScopeSelector::Pane(PaneTarget::with_window(alpha.clone(), 3, 1)),
            OptionName::WindowStyle,
            "default,bold".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("pane set succeeds");

    store
        .rename_session(&alpha, beta.clone())
        .expect("rename succeeds");

    assert_eq!(store.resolve(Some(&alpha), OptionName::Status), Some("on"));
    assert_eq!(store.resolve(Some(&beta), OptionName::Status), Some("off"));
    assert_eq!(
        store.resolve_for_window(&beta, 3, OptionName::MainPaneWidth),
        Some("90")
    );
    assert_eq!(
        store.resolve_for_pane(&beta, 3, 1, OptionName::WindowStyle),
        Some("default,bold")
    );
}

#[test]
fn renaming_to_an_existing_option_key_fails_without_mutation() {
    let mut store = OptionStore::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");

    store
        .set(
            ScopeSelector::Window(WindowTarget::with_window(alpha.clone(), 3)),
            OptionName::MainPaneWidth,
            "90".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("window set succeeds");
    store
        .set(
            ScopeSelector::Window(WindowTarget::with_window(beta.clone(), 3)),
            OptionName::MainPaneWidth,
            "70".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("window set succeeds");

    let error = store
        .rename_session(&alpha, beta.clone())
        .expect_err("existing destination rejects rename");

    assert_eq!(
        error,
        RmuxError::Server("window options already exist for beta:3".to_owned())
    );
    assert_eq!(
        store.resolve_for_window(&alpha, 3, OptionName::MainPaneWidth),
        Some("90")
    );
    assert_eq!(
        store.resolve_for_window(&beta, 3, OptionName::MainPaneWidth),
        Some("70")
    );
}
