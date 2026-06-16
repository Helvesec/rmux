use super::*;

#[test]
fn show_options_lines_render_names_or_values_for_the_selected_scope() {
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

    let lines = store
        .show_options_lines(&OptionScopeSelector::Session(alpha.clone()), false)
        .expect("show options succeeds");
    assert!(lines.contains(&"status off".to_owned()));
    assert!(lines.contains(&"base-index 0".to_owned()));

    let server_lines = store
        .show_options_lines_filtered(
            &OptionScopeSelector::ServerGlobal,
            Some("terminal-overrides"),
            false,
        )
        .expect("show empty option succeeds");
    assert_eq!(server_lines, vec!["terminal-overrides".to_owned()]);

    let values = store
        .show_options_lines(&OptionScopeSelector::Session(alpha), true)
        .expect("show values succeeds");
    assert!(values.contains(&"off".to_owned()));
    assert!(!values.iter().any(|line| line.starts_with("status ")));
}

#[test]
fn show_options_quotes_whitespace_and_renders_array_indexes() {
    let mut store = OptionStore::new();

    let status_left = store
        .show_options_lines_filtered(
            &OptionScopeSelector::SessionGlobal,
            Some("status-left"),
            false,
        )
        .expect("status-left show succeeds");
    assert_eq!(
        status_left,
        vec!["status-left \"[#{session_name}] \"".to_owned()]
    );

    let command_alias = store
        .show_options_lines_filtered(
            &OptionScopeSelector::SessionGlobal,
            Some("command-alias"),
            false,
        )
        .expect("command-alias show succeeds");
    assert_eq!(
        command_alias,
        vec![
            "command-alias[0] split-pane=split-window".to_owned(),
            "command-alias[1] splitp=split-window".to_owned(),
            "command-alias[2] \"server-info=show-messages -JT\"".to_owned(),
            "command-alias[3] \"info=show-messages -JT\"".to_owned(),
            "command-alias[4] \"choose-window=choose-tree -w\"".to_owned(),
            "command-alias[5] \"choose-session=choose-tree -s\"".to_owned(),
        ]
    );

    let pane_colours = store
        .show_options_lines_filtered(
            &OptionScopeSelector::WindowGlobal,
            Some("pane-colours"),
            false,
        )
        .expect("pane-colours show succeeds");
    assert_eq!(pane_colours, vec!["pane-colours".to_owned()]);

    let local_pane_colours = store
        .show_options_lines_filtered(
            &OptionScopeSelector::Window(WindowTarget::with_window(session_name("alpha"), 0)),
            Some("pane-colours"),
            false,
        )
        .expect("local pane-colours show succeeds");
    assert!(local_pane_colours.is_empty());

    let inherited_local_pane_colours = store
        .show_options_lines_with_mode_filtered(
            &OptionScopeSelector::Window(WindowTarget::with_window(session_name("alpha"), 0)),
            Some("pane-colours"),
            false,
            ShowOptionsMode::ResolvedWithInheritanceMarkers,
        )
        .expect("local pane-colours -A show succeeds");
    assert_eq!(
        inherited_local_pane_colours,
        vec!["pane-colours".to_owned()]
    );
    let inherited_pane_pane_colours = store
        .show_options_lines_with_mode_filtered(
            &OptionScopeSelector::Pane(PaneTarget::with_window(session_name("alpha"), 0, 0)),
            Some("pane-colours"),
            false,
            ShowOptionsMode::ResolvedWithInheritanceMarkers,
        )
        .expect("pane pane-colours -A show succeeds");
    assert_eq!(inherited_pane_pane_colours, vec!["pane-colours".to_owned()]);

    let terminal_features = store
        .show_options_lines_filtered(
            &OptionScopeSelector::WindowGlobal,
            Some("terminal-features"),
            false,
        )
        .expect("terminal-features show succeeds");
    assert_eq!(
        terminal_features,
        vec![
            "terminal-features[0] xterm*:clipboard:ccolour:cstyle:focus:title".to_owned(),
            "terminal-features[1] screen*:title".to_owned(),
            "terminal-features[2] rxvt*:ignorefkeys".to_owned(),
        ]
    );

    store
        .set(
            ScopeSelector::Global,
            OptionName::StatusLeft,
            "#S".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("status-left set succeeds");
    let status_left_hash = store
        .show_options_lines_filtered(
            &OptionScopeSelector::SessionGlobal,
            Some("status-left"),
            false,
        )
        .expect("status-left show after hash-only set succeeds");
    assert_eq!(status_left_hash, vec!["status-left \"#S\"".to_owned()]);
}

#[test]
fn numeric_and_flag_values_are_canonicalized_before_storage() {
    let mut store = OptionStore::new();
    let alpha = session_name("alpha");
    let window = WindowTarget::with_window(alpha.clone(), 0);

    store
        .set(
            ScopeSelector::Session(alpha.clone()),
            OptionName::BaseIndex,
            "0007".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("numeric set succeeds");
    store
        .set(
            ScopeSelector::Global,
            OptionName::AutomaticRename,
            "0".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("flag set succeeds");

    assert_eq!(
        store.session_value(&alpha, OptionName::BaseIndex),
        Some("7")
    );
    assert_eq!(store.global_value(OptionName::AutomaticRename), Some("off"));

    let session_lines = store
        .show_options_lines(&OptionScopeSelector::Session(alpha), false)
        .expect("session show options succeeds");
    assert!(session_lines.contains(&"base-index 7".to_owned()));

    let window_lines = store
        .show_options_lines(&OptionScopeSelector::Window(window), false)
        .expect("window show options succeeds");
    assert!(window_lines.contains(&"automatic-rename off".to_owned()));
}

#[test]
fn literal_style_options_reject_invalid_styles() {
    let mut store = OptionStore::new();

    let error = store
        .set(
            ScopeSelector::Global,
            OptionName::StatusStyle,
            "fg=not-a-colour".to_owned(),
            SetOptionMode::Replace,
        )
        .expect_err("invalid style should be rejected");

    assert_eq!(
        error,
        RmuxError::InvalidSetOption("invalid style: fg=not-a-colour".to_owned())
    );
    assert_eq!(store.global_value(OptionName::StatusStyle), None);
}

#[test]
fn style_options_with_format_expansions_skip_eager_literal_validation() {
    let mut store = OptionStore::new();
    let alpha = session_name("alpha");
    let window = WindowTarget::with_window(alpha.clone(), 0);

    store
        .set(
            ScopeSelector::Window(window),
            OptionName::PaneActiveBorderStyle,
            "#{?pane_active,red,blue}".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("format-backed style should be accepted");

    assert_eq!(
        store.resolve_for_window(&alpha, 0, OptionName::PaneActiveBorderStyle),
        Some("#{?pane_active,red,blue}")
    );
}

#[test]
fn colour_options_canonicalize_bare_decimal_palette_indices() {
    let mut store = OptionStore::new();

    store
        .set(
            ScopeSelector::Global,
            OptionName::ClockModeColour,
            "214".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("bare decimal colour should be accepted");

    assert_eq!(
        store.global_value(OptionName::ClockModeColour),
        Some("colour214")
    );
}

#[test]
fn removing_a_window_discards_window_and_owned_pane_values() {
    let mut store = OptionStore::new();
    let alpha = session_name("alpha");
    let window = WindowTarget::with_window(alpha.clone(), 3);
    let pane_a = PaneTarget::with_window(alpha.clone(), 3, 0);
    let pane_b = PaneTarget::with_window(alpha.clone(), 3, 1);
    let other_pane = PaneTarget::with_window(alpha.clone(), 5, 0);

    store
        .set(
            ScopeSelector::Window(window.clone()),
            OptionName::MainPaneWidth,
            "100".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("window set succeeds");
    store
        .set(
            ScopeSelector::Pane(pane_a.clone()),
            OptionName::WindowStyle,
            "fg=colour4".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("pane a set succeeds");
    store
        .set(
            ScopeSelector::Pane(pane_b.clone()),
            OptionName::WindowStyle,
            "fg=colour5".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("pane b set succeeds");
    store
        .set(
            ScopeSelector::Pane(other_pane.clone()),
            OptionName::WindowStyle,
            "fg=colour6".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("other pane set succeeds");

    assert!(store.remove_window(&window).is_some());
    assert_eq!(store.window_value(&window, OptionName::MainPaneWidth), None);
    assert_eq!(store.pane_value(&pane_a, OptionName::WindowStyle), None);
    assert_eq!(store.pane_value(&pane_b, OptionName::WindowStyle), None);
    // Panes in other windows should be unaffected
    assert_eq!(
        store.pane_value(&other_pane, OptionName::WindowStyle),
        Some("fg=colour6")
    );
}

#[test]
fn show_options_server_scope_excludes_session_and_window_options() {
    let store = OptionStore::new();

    let lines = store
        .show_options_lines(&OptionScopeSelector::ServerGlobal, false)
        .expect("server show-options succeeds");
    assert!(lines
        .iter()
        .any(|line| line.starts_with("default-terminal ")));
    assert!(lines.iter().any(|line| line.starts_with("buffer-limit ")));
    assert!(!lines.iter().any(|line| line.starts_with("status ")));
    assert!(!lines
        .iter()
        .any(|line| line.starts_with("main-pane-width ")));
}

#[test]
fn show_options_inheritance_markers_do_not_mark_global_defaults() {
    let store = OptionStore::new();

    let session_global = store
        .show_options_lines_with_mode_filtered(
            &OptionScopeSelector::SessionGlobal,
            Some("status"),
            false,
            ShowOptionsMode::ResolvedWithInheritanceMarkers,
        )
        .expect("global session option shows");
    assert_eq!(session_global, vec!["status on".to_owned()]);

    let window_global = store
        .show_options_lines_with_mode_filtered(
            &OptionScopeSelector::WindowGlobal,
            Some("pane-border-lines"),
            false,
            ShowOptionsMode::ResolvedWithInheritanceMarkers,
        )
        .expect("global window option shows");
    assert_eq!(window_global, vec!["pane-border-lines single".to_owned()]);
}
