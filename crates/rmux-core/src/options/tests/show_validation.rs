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
        .expect("show terminal-overrides succeeds");
    assert_eq!(
        server_lines,
        vec!["terminal-overrides[0] linux*:AX@".to_owned()]
    );

    let values = store
        .show_options_lines(&OptionScopeSelector::Session(alpha), true)
        .expect("show values succeeds");
    assert!(values.contains(&"off".to_owned()));
    assert!(!values.iter().any(|line| line.starts_with("status ")));
}

#[test]
fn show_options_quotes_whitespace_and_renders_array_indexes() {
    let mut store = OptionStore::new();

    store
        .set_by_name(
            OptionScopeSelector::SessionGlobal,
            "@path",
            Some("$HOME/bin".to_owned()),
            rmux_proto::SetOptionMode::Replace,
            false,
            false,
            false,
        )
        .expect("user option set succeeds");

    let user_path = store
        .show_options_lines_filtered(&OptionScopeSelector::SessionGlobal, Some("@path"), false)
        .expect("user option show succeeds");
    assert_eq!(user_path, vec!["@path \"\\$HOME/bin\"".to_owned()]);

    store
        .set_by_name(
            OptionScopeSelector::SessionGlobal,
            "@tilde",
            Some("~/bin".to_owned()),
            rmux_proto::SetOptionMode::Replace,
            false,
            false,
            false,
        )
        .expect("tilde user option set succeeds");
    assert_eq!(
        store
            .show_options_lines_filtered(&OptionScopeSelector::SessionGlobal, Some("@tilde"), false)
            .expect("tilde user option show succeeds"),
        vec!["@tilde \\~/bin".to_owned()]
    );

    store
        .set_by_name(
            OptionScopeSelector::SessionGlobal,
            "@quote",
            Some("has\"quote".to_owned()),
            rmux_proto::SetOptionMode::Replace,
            false,
            false,
            false,
        )
        .expect("quote user option set succeeds");
    assert_eq!(
        store
            .show_options_lines_filtered(&OptionScopeSelector::SessionGlobal, Some("@quote"), false)
            .expect("quote user option show succeeds"),
        vec!["@quote 'has\"quote'".to_owned()]
    );

    store
        .set_by_name(
            OptionScopeSelector::SessionGlobal,
            "@control",
            Some("line1\nline2\tbell\u{7}".to_owned()),
            rmux_proto::SetOptionMode::Replace,
            false,
            false,
            false,
        )
        .expect("control user option set succeeds");
    assert_eq!(
        store
            .show_options_lines_filtered(
                &OptionScopeSelector::SessionGlobal,
                Some("@control"),
                false
            )
            .expect("control user option show succeeds"),
        vec!["@control line1\\nline2\\tbell\\a".to_owned()]
    );

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
fn show_options_matches_tmux37_quote_and_dollar_rules() {
    let mut store = OptionStore::new();

    for (name, value, expected) in [
        ("@semi", "a;b", "@semi \"a;b\""),
        ("@brace", "a}b", "@brace \"a}b\""),
        ("@percent", "a%b", "@percent \"a%b\""),
        ("@quote", "a'b", "@quote \"a'b\""),
        ("@digit", "x$1y", "@digit \"x$1y\""),
        ("@space", "x$ y", "@space \"x$ y\""),
        ("@final", "x$", "@final \"x$\""),
        ("@bracevar", "x${z}", "@bracevar \"x\\${z}\""),
        ("@namevar", "x$_y", "@namevar \"x\\$_y\""),
    ] {
        store
            .set_by_name(
                OptionScopeSelector::SessionGlobal,
                name,
                Some(value.to_owned()),
                rmux_proto::SetOptionMode::Replace,
                false,
                false,
                false,
            )
            .expect("user option set succeeds");
        let lines = store
            .show_options_lines_filtered(&OptionScopeSelector::SessionGlobal, Some(name), false)
            .expect("user option show succeeds");
        assert_eq!(lines, vec![expected.to_owned()], "{name}");
    }

    let word_separators = store
        .show_options_lines_filtered(
            &OptionScopeSelector::SessionGlobal,
            Some("word-separators"),
            false,
        )
        .expect("word-separators show succeeds");
    assert_eq!(
        word_separators,
        vec!["word-separators \"!\\\"#$%&'()*+,-./:;<=>?@[\\\\]^`{|}~\"".to_owned()]
    );
}

#[test]
fn show_options_lists_user_options_before_builtin_options_like_tmux() {
    let mut store = OptionStore::new();

    for (name, value) in [("@z", "1"), ("@a", "2")] {
        store
            .set_by_name(
                OptionScopeSelector::SessionGlobal,
                name,
                Some(value.to_owned()),
                rmux_proto::SetOptionMode::Replace,
                false,
                false,
                false,
            )
            .expect("user option set succeeds");
    }

    let lines = store
        .show_options_lines(&OptionScopeSelector::SessionGlobal, false)
        .expect("show-options succeeds");
    assert_eq!(lines[0], "@a 2");
    assert_eq!(lines[1], "@z 1");
    assert!(
        lines
            .iter()
            .position(|line| line.starts_with("activity-action "))
            .is_some_and(|index| index > 1),
        "built-in options should follow user options: {lines:?}"
    );
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
fn input_buffer_size_rejects_values_below_tmux_minimum() {
    let mut store = OptionStore::new();

    let error = store
        .set_by_name(
            OptionScopeSelector::ServerGlobal,
            "input-buffer-size",
            Some("0".to_owned()),
            SetOptionMode::Replace,
            false,
            false,
            false,
        )
        .expect_err("input-buffer-size rejects values below tmux minimum");
    assert_eq!(
        error.to_string(),
        "invalid set-option request: value is too small: 0"
    );

    store
        .set_by_name(
            OptionScopeSelector::ServerGlobal,
            "input-buffer-size",
            Some("1048576".to_owned()),
            SetOptionMode::Replace,
            false,
            false,
            false,
        )
        .expect("input-buffer-size accepts tmux minimum");

    assert_eq!(
        store.global_value(OptionName::InputBufferSize),
        Some("1048576")
    );
}

#[test]
fn bare_choice_options_toggle_by_choice_index_like_tmux() {
    let mut store = OptionStore::new();

    store
        .set_by_name(
            OptionScopeSelector::WindowGlobal,
            "mode-keys",
            None,
            SetOptionMode::Replace,
            false,
            false,
            false,
        )
        .expect("bare mode-keys toggles index 0 to index 1");
    assert_eq!(store.global_value(OptionName::ModeKeys), Some("vi"));

    store
        .set_by_name(
            OptionScopeSelector::SessionGlobal,
            "status-position",
            None,
            SetOptionMode::Replace,
            false,
            false,
            false,
        )
        .expect("bare status-position toggles index 1 to index 0");
    assert_eq!(store.global_value(OptionName::StatusPosition), Some("top"));

    store
        .set_by_name(
            OptionScopeSelector::ServerGlobal,
            "set-clipboard",
            Some("external".to_owned()),
            SetOptionMode::Replace,
            false,
            false,
            false,
        )
        .expect("set-clipboard external set succeeds");
    store
        .set_by_name(
            OptionScopeSelector::ServerGlobal,
            "set-clipboard",
            None,
            SetOptionMode::Replace,
            false,
            false,
            false,
        )
        .expect("bare set-clipboard toggles index 1 to index 0");
    assert_eq!(store.global_value(OptionName::SetClipboard), Some("off"));

    store
        .set_by_name(
            OptionScopeSelector::ServerGlobal,
            "set-clipboard",
            Some("on".to_owned()),
            SetOptionMode::Replace,
            false,
            false,
            false,
        )
        .expect("set-clipboard on set succeeds");
    store
        .set_by_name(
            OptionScopeSelector::ServerGlobal,
            "set-clipboard",
            None,
            SetOptionMode::Replace,
            false,
            false,
            false,
        )
        .expect("bare set-clipboard leaves index >=2 unchanged");
    assert_eq!(store.global_value(OptionName::SetClipboard), Some("on"));
}

#[test]
fn explicit_window_and_pane_scopes_store_known_options_even_when_registry_scope_differs() {
    let mut store = OptionStore::new();
    let alpha = session_name("alpha");
    let window = WindowTarget::with_window(alpha.clone(), 0);
    let pane = PaneTarget::with_window(alpha.clone(), 0, 0);

    store
        .set_by_name(
            OptionScopeSelector::Window(window.clone()),
            "status",
            Some("off".to_owned()),
            SetOptionMode::Replace,
            false,
            false,
            false,
        )
        .expect("session-scoped option can be stored at explicit window scope");
    store
        .set_by_name(
            OptionScopeSelector::Pane(pane.clone()),
            "message-limit",
            Some("77".to_owned()),
            SetOptionMode::Replace,
            false,
            false,
            false,
        )
        .expect("server-scoped option can be stored at explicit pane scope");

    assert_eq!(store.window_value(&window, OptionName::Status), Some("off"));
    assert_eq!(
        store.pane_value(&pane, OptionName::MessageLimit),
        Some("77")
    );
    assert_eq!(
        store.resolve_for_window(&alpha, 0, OptionName::Status),
        Some("off")
    );
    assert_eq!(
        store.resolve_for_pane(&alpha, 0, 0, OptionName::MessageLimit),
        Some("77")
    );

    let shown_window = store
        .show_options_lines_filtered(&OptionScopeSelector::Window(window), Some("status"), true)
        .expect("window-scoped status show succeeds");
    assert_eq!(shown_window, vec!["off".to_owned()]);
    let shown_pane = store
        .show_options_lines_filtered(
            &OptionScopeSelector::Pane(pane),
            Some("message-limit"),
            true,
        )
        .expect("pane-scoped message-limit show succeeds");
    assert_eq!(shown_pane, vec!["77".to_owned()]);
}

#[test]
fn named_local_show_queries_fall_back_to_natural_global_defaults() {
    let store = OptionStore::new();
    let alpha = session_name("alpha");
    let window = WindowTarget::with_window(alpha.clone(), 0);
    let pane = PaneTarget::with_window(alpha, 0, 0);

    let session_server_option = store
        .show_options_lines_filtered(
            &OptionScopeSelector::Session(session_name("beta")),
            Some("buffer-limit"),
            false,
        )
        .expect("session-scoped server-table query succeeds");
    assert_eq!(session_server_option, vec!["buffer-limit 50".to_owned()]);

    let window_server_option = store
        .show_options_lines_filtered(
            &OptionScopeSelector::Window(window),
            Some("message-limit"),
            false,
        )
        .expect("window-scoped server-table query succeeds");
    assert_eq!(window_server_option, vec!["message-limit 1000".to_owned()]);

    let pane_server_option = store
        .show_options_lines_filtered(
            &OptionScopeSelector::Pane(pane),
            Some("message-limit"),
            true,
        )
        .expect("pane-scoped server-table value query succeeds");
    assert_eq!(pane_server_option, vec!["1000".to_owned()]);
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
fn pane_border_styles_are_visible_in_pane_show_scope() {
    let mut store = OptionStore::new();
    let alpha = session_name("alpha");
    let pane = PaneTarget::with_window(alpha, 0, 0);

    store
        .set(
            ScopeSelector::Pane(pane.clone()),
            OptionName::PaneBorderStyle,
            "fg=red".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("pane border style set succeeds");
    store
        .set(
            ScopeSelector::Pane(pane.clone()),
            OptionName::PaneActiveBorderStyle,
            "fg=green".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("pane active border style set succeeds");

    let border = store
        .show_options_lines_filtered(
            &OptionScopeSelector::Pane(pane.clone()),
            Some("pane-border-style"),
            true,
        )
        .expect("pane border show succeeds");
    let active = store
        .show_options_lines_filtered(
            &OptionScopeSelector::Pane(pane),
            Some("pane-active-border-style"),
            true,
        )
        .expect("pane active border show succeeds");

    assert_eq!(border, vec!["fg=red".to_owned()]);
    assert_eq!(active, vec!["fg=green".to_owned()]);
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
