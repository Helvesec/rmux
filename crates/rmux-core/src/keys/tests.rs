use super::{
    key_code_lookup_bits, key_code_to_bytes, key_string_lookup_key, key_string_lookup_string,
    parse_binding_command_tokens, KeyBindingSortOrder, KeyBindingStore, KEYC_ANY, KEYC_CTRL,
    KEYC_META, KEYC_SHIFT,
};

#[test]
fn key_lookup_round_trips_named_keys_and_modifiers() {
    let key = key_string_lookup_string("C-M-Left").expect("key parses");
    assert_eq!(key_string_lookup_key(key, false), "C-M-Left");
    assert_eq!(
        key_string_lookup_key(key_code_lookup_bits(key), false),
        "C-M-Left"
    );
}

#[test]
fn key_lookup_accepts_hex_and_user_keys() {
    assert_eq!(key_string_lookup_string("0x41"), Some(b'A' as u64));
    assert_eq!(key_string_lookup_string("Any"), Some(KEYC_ANY));
    assert_eq!(
        key_string_lookup_string("user42"),
        key_string_lookup_string("User42")
    );
    assert_eq!(
        key_string_lookup_key(key_string_lookup_string("User42").expect("user key"), false),
        "User42"
    );
}

#[test]
fn key_lookup_accepts_mouse_keys() {
    let key = key_string_lookup_string("MouseDown1Pane").expect("mouse key parses");
    assert_eq!(key_string_lookup_key(key, false), "MouseDown1Pane");
}

#[test]
fn key_lookup_accepts_short_ctrl_notation() {
    let key = key_string_lookup_string("^B").expect("short ctrl parses");
    assert_eq!(key, (b'b' as u64) | KEYC_CTRL);
}

#[test]
fn key_lookup_canonicalizes_ctrl_bracket_as_escape() {
    for input in ["C-[", "^["] {
        let key = key_string_lookup_string(input).expect("ctrl bracket parses");
        assert_eq!(key_string_lookup_key(key, false), "Escape");
    }
}

#[test]
fn single_string_binding_rejects_unknown_percent_directives() {
    let error = parse_binding_command_tokens(&["display-message -p %foo".to_owned()])
        .expect_err("unknown percent directive should fail like tmux");

    assert_eq!(error.to_string(), "syntax error");
}

#[test]
fn key_code_to_bytes_encodes_ascii_control_and_utf8() {
    assert_eq!(
        key_code_to_bytes(key_string_lookup_string("Enter").unwrap()),
        Some(vec![13])
    );
    assert_eq!(
        key_code_to_bytes(key_string_lookup_string("S-Enter").unwrap()),
        Some(vec![10])
    );
    assert_eq!(
        key_code_to_bytes(key_string_lookup_string("C-c").unwrap()),
        Some(vec![3])
    );
    let utf8 = key_code_to_bytes(key_string_lookup_string("é").unwrap()).expect("utf8 bytes");
    assert_eq!(String::from_utf8(utf8).unwrap(), "é");
}

#[test]
fn default_store_loads_prefix_root_and_copy_tables() {
    let store = KeyBindingStore::default();
    assert!(store.table("prefix").is_some());
    assert!(store.table("root").is_some());
    assert!(store.table("copy-mode").is_some());
    assert!(store.table("copy-mode-vi").is_some());
}

#[test]
fn default_store_keeps_prefix_meta_layout_bindings() {
    let store = KeyBindingStore::default();
    for (key, expected) in [
        ("M-1", "select-layout even-horizontal"),
        ("M-2", "select-layout even-vertical"),
        ("M-3", "select-layout main-horizontal"),
        ("M-4", "select-layout main-vertical"),
        ("M-5", "select-layout tiled"),
        ("M-6", "select-layout main-horizontal-mirrored"),
        ("M-7", "select-layout main-vertical-mirrored"),
    ] {
        let binding = store
            .get_binding("prefix", key_string_lookup_string(key).expect("key parses"))
            .unwrap_or_else(|| panic!("missing default prefix binding for {key}"));
        assert_eq!(binding.commands().to_tmux_string(), expected);
    }
}

#[test]
fn default_store_matches_tmux37_lot6_root_and_copy_bindings() {
    let store = KeyBindingStore::default();
    for (table, key, expected) in [
        ("root", "MouseDown1Status", "switch-client -t="),
        (
            "root",
            "WheelUpPane",
            r##"if-shell -F "#{||:#{alternate_on},#{pane_in_mode},#{mouse_any_flag}}" { send-keys -M } { copy-mode -e }"##,
        ),
        ("root", "C-MouseDown1Pane", "swap-pane -s @"),
        ("root", "C-MouseDown1Status", "swap-window -t @"),
        (
            "root",
            "MouseDown1ScrollbarUp",
            r##"if-shell -F -t= "#{pane_in_mode}" { send-keys -X page-up } { copy-mode -u }"##,
        ),
        (
            "root",
            "MouseDown1ScrollbarDown",
            r##"if-shell -F -t= "#{pane_in_mode}" { send-keys -X page-down } { copy-mode -d }"##,
        ),
        (
            "root",
            "MouseDrag1ScrollbarSlider",
            r##"if-shell -F -t= "#{pane_in_mode}" { send-keys -X scroll-to-mouse } { copy-mode -S }"##,
        ),
        ("root", "MouseDown1Control8", "resize-pane -Z"),
        ("root", "MouseDown1Border", "select-pane -M"),
        (
            "root",
            "MouseDown1Control9",
            r#"display-menu -O -T "Kill pane #{pane_index}?" -t= -xM -yM Yes y { kill-pane -t= } No n {  }"#,
        ),
        ("copy-mode", "C-l", "send-keys -X recentre-top-bottom"),
        ("copy-mode", "M-l", "send-keys -X cursor-centre-horizontal"),
        ("copy-mode", "C-[", "send-keys -X cancel"),
        ("copy-mode-vi", "C-[", "send-keys -X clear-selection"),
    ] {
        let binding = store
            .list_bindings(Some(table), KeyBindingSortOrder::Key, false)
            .into_iter()
            .find(|binding| binding.key_string() == key)
            .unwrap_or_else(|| panic!("missing default {table} binding for {key}"));
        assert_eq!(
            binding.binding().commands().to_tmux_string(),
            expected,
            "{table} {key}"
        );
    }
}

#[test]
fn default_store_matches_tmux37_status_and_pane_menu_rows() {
    let store = KeyBindingStore::default();
    for (table, key, expected) in [
        (
            "root",
            "MouseDown3StatusLeft",
            r##"display-menu -t= -xM -yW -T "#[align=centre]#{session_name}" Next n { switch-client -n } Previous p { switch-client -p } '' Renumber N { move-window -r } Rename r { command-prompt -I "#S" { rename-session -- "%%" } } Detach d { detach-client } '' "New Session" s { new-session } "New Window" w { new-window }"##,
        ),
        (
            "root",
            "M-MouseDown3StatusLeft",
            r##"display-menu -t= -xM -yW -T "#[align=centre]#{session_name}" Next n { switch-client -n } Previous p { switch-client -p } '' Renumber N { move-window -r } Rename r { command-prompt -I "#S" { rename-session -- "%%" } } Detach d { detach-client } '' "New Session" s { new-session } "New Window" w { new-window }"##,
        ),
        (
            "prefix",
            ">",
            r##"display-menu -xP -yP -T "#[align=centre]#{pane_index} (#{pane_id})" "#{?#{m/r:(copy|view)-mode,#{pane_mode}},Go To Top,}" < { send-keys -X history-top } "#{?#{m/r:(copy|view)-mode,#{pane_mode}},Go To Bottom,}" > { send-keys -X history-bottom } '' "#{?#{&&:#{buffer_size},#{!:#{pane_in_mode}}},Paste #[underscore]#{=/9/...:buffer_sample},}" p { paste-buffer } '' "#{?mouse_word,Search For #[underscore]#{=/9/...:mouse_word},}" C-r { if-shell -F "#{?#{m/r:(copy|view)-mode,#{pane_mode}},0,1}" "copy-mode -t=" ; send-keys -Xt= search-backward -- "#{q:mouse_word}" } "#{?mouse_word,Type #[underscore]#{=/9/...:mouse_word},}" C-y { copy-mode -q ; send-keys -l -- "#{q:mouse_word}" } "#{?mouse_word,Copy #[underscore]#{=/9/...:mouse_word},}" c { copy-mode -q ; set-buffer -- "#{q:mouse_word}" } "#{?mouse_line,Copy Line,}" l { copy-mode -q ; set-buffer -- "#{q:mouse_line}" } '' "#{?mouse_hyperlink,Type #[underscore]#{=/9/...:mouse_hyperlink},}" C-h { copy-mode -q ; send-keys -l -- "#{q:mouse_hyperlink}" } "#{?mouse_hyperlink,Copy #[underscore]#{=/9/...:mouse_hyperlink},}" h { copy-mode -q ; set-buffer -- "#{q:mouse_hyperlink}" } '' "Horizontal Split" h { split-window -h } "Vertical Split" v { split-window -v } '' "#{?#{>:#{window_panes},1},,-}Swap Up" u { swap-pane -U } "#{?#{>:#{window_panes},1},,-}Swap Down" d { swap-pane -D } "#{?pane_marked_set,,-}Swap Marked" s { swap-pane } '' Kill X { kill-pane } Respawn R { respawn-pane -k } "#{?pane_marked,Unmark,Mark}" m { select-pane -m } "#{?#{>:#{window_panes},1},,-}#{?window_zoomed_flag,Unzoom,Zoom}" z { resize-pane -Z }"##,
        ),
        (
            "root",
            "MouseDown3Pane",
            r##"if-shell -Ft= "#{||:#{mouse_any_flag},#{&&:#{pane_in_mode},#{?#{m/r:(copy|view)-mode,#{pane_mode}},0,1}}}" { select-pane -t= ; send-keys -M } { display-menu -t= -xM -yM -T "#[align=centre]#{pane_index} (#{pane_id})" "#{?#{m/r:(copy|view)-mode,#{pane_mode}},Go To Top,}" < { send-keys -X history-top } "#{?#{m/r:(copy|view)-mode,#{pane_mode}},Go To Bottom,}" > { send-keys -X history-bottom } '' "#{?#{&&:#{buffer_size},#{!:#{pane_in_mode}}},Paste #[underscore]#{=/9/...:buffer_sample},}" p { paste-buffer } '' "#{?mouse_word,Search For #[underscore]#{=/9/...:mouse_word},}" C-r { if-shell -F "#{?#{m/r:(copy|view)-mode,#{pane_mode}},0,1}" "copy-mode -t=" ; send-keys -Xt= search-backward -- "#{q:mouse_word}" } "#{?mouse_word,Type #[underscore]#{=/9/...:mouse_word},}" C-y { copy-mode -q ; send-keys -l -- "#{q:mouse_word}" } "#{?mouse_word,Copy #[underscore]#{=/9/...:mouse_word},}" c { copy-mode -q ; set-buffer -- "#{q:mouse_word}" } "#{?mouse_line,Copy Line,}" l { copy-mode -q ; set-buffer -- "#{q:mouse_line}" } '' "#{?mouse_hyperlink,Type #[underscore]#{=/9/...:mouse_hyperlink},}" C-h { copy-mode -q ; send-keys -l -- "#{q:mouse_hyperlink}" } "#{?mouse_hyperlink,Copy #[underscore]#{=/9/...:mouse_hyperlink},}" h { copy-mode -q ; set-buffer -- "#{q:mouse_hyperlink}" } '' "Horizontal Split" h { split-window -h } "Vertical Split" v { split-window -v } '' "#{?#{>:#{window_panes},1},,-}Swap Up" u { swap-pane -U } "#{?#{>:#{window_panes},1},,-}Swap Down" d { swap-pane -D } "#{?pane_marked_set,,-}Swap Marked" s { swap-pane } '' Kill X { kill-pane } Respawn R { respawn-pane -k } "#{?pane_marked,Unmark,Mark}" m { select-pane -m } "#{?#{>:#{window_panes},1},,-}#{?window_zoomed_flag,Unzoom,Zoom}" z { resize-pane -Z } }"##,
        ),
    ] {
        let binding = store
            .list_bindings(Some(table), KeyBindingSortOrder::Key, false)
            .into_iter()
            .find(|binding| binding.key_string() == key)
            .unwrap_or_else(|| panic!("missing default {table} binding for {key}"));
        assert_eq!(
            binding.binding().commands().to_tmux_string(),
            expected,
            "{table} {key}"
        );
    }
}

#[test]
fn default_key_string_format_exposes_unescaped_key_names() {
    let store = KeyBindingStore::default();
    let prefix_bindings = store.list_bindings(Some("prefix"), KeyBindingSortOrder::Key, false);

    for key in ["\"", "#", "$", "'", ";"] {
        assert!(
            prefix_bindings
                .iter()
                .any(|binding| binding.key_string() == key),
            "missing unescaped prefix key string {key:?}"
        );
    }
    assert!(
        !prefix_bindings
            .iter()
            .any(|binding| matches!(binding.key_string(), "\\\"" | "\\#" | "\\$" | "\\'" | "\\;")),
        "default key_string values must not expose command-line escaping"
    );
}

#[test]
fn reset_restores_defaults_from_snapshot() {
    let mut store = KeyBindingStore::default();
    let original = store
        .get_binding("prefix", key_string_lookup_string("C-b").unwrap())
        .expect("default binding")
        .commands()
        .to_tmux_string();
    let new_commands =
        parse_binding_command_tokens(&["display-message changed".to_owned()]).unwrap();
    store.add_binding(
        "prefix",
        key_string_lookup_string("C-b").unwrap(),
        None,
        false,
        Some(new_commands),
    );
    assert_ne!(
        store
            .get_binding("prefix", key_string_lookup_string("C-b").unwrap())
            .unwrap()
            .commands()
            .to_tmux_string(),
        original
    );
    store.reset_binding("prefix", key_string_lookup_string("C-b").unwrap());
    assert_eq!(
        store
            .get_binding("prefix", key_string_lookup_string("C-b").unwrap())
            .unwrap()
            .commands()
            .to_tmux_string(),
        original
    );
}

#[test]
fn reset_restores_removed_defaults_from_snapshot() {
    let mut store = KeyBindingStore::default();
    let key = key_string_lookup_string("C-b").unwrap();
    assert!(store.remove_binding("prefix", key));
    assert!(store.get_binding("prefix", key).is_none());

    store.reset_binding("prefix", key);

    assert!(store.get_binding("prefix", key).is_some());
}

#[test]
fn listed_bindings_escape_command_separators_for_resourcing() {
    let mut store = KeyBindingStore::default();
    let key = key_string_lookup_string("Y").expect("key parses");
    let commands =
        parse_binding_command_tokens(
            &[r#"run-shell "echo one" ; run-shell "echo two""#.to_owned()],
        )
        .expect("binding command parses");

    store.add_binding("prefix", key, None, false, Some(commands));
    let bindings = store.list_bindings(Some("prefix"), KeyBindingSortOrder::default(), false);
    let binding = bindings
        .iter()
        .find(|binding| binding.key_string() == "Y")
        .expect("binding is listed");

    assert_eq!(
        binding.command_string(),
        r#"run-shell "echo one" \; run-shell "echo two""#
    );
}

#[test]
fn remove_table_clears_active_bindings_but_preserves_default_snapshot() {
    let mut store = KeyBindingStore::default();
    assert!(store.remove_table("prefix"));

    let table = store.table("prefix").expect("default table should persist");
    assert!(table.active().is_empty());
    assert!(!table.defaults().is_empty());
}

#[test]
fn binding_updates_do_not_leak_table_references() {
    let mut store = KeyBindingStore::new();
    assert!(store.add_binding(
        "scratch",
        key_string_lookup_string("C-a").unwrap(),
        None,
        false,
        Some(parse_binding_command_tokens(&["display-message test".to_owned()]).unwrap()),
    ));
    let table = store.table("scratch").expect("table created");
    assert_eq!(table.references(), 0);
}

#[test]
fn list_bindings_sorts_and_widths() {
    let store = KeyBindingStore::default();
    let mut bindings = store.list_bindings(Some("prefix"), KeyBindingSortOrder::Key, false);
    assert!(!bindings.is_empty());
    let first = bindings.remove(0);
    assert!(!first.key_string().is_empty());
    assert!(!first.command_string().is_empty());
    assert!(
        KeyBindingStore::key_string_width(&store.list_bindings(
            Some("prefix"),
            KeyBindingSortOrder::Key,
            false
        )) > 0
    );
}

#[test]
fn list_bindings_key_sort_handles_custom_modified_keys() {
    let mut store = KeyBindingStore::default();
    let command = parse_binding_command_tokens(&["display-message custom".to_owned()]).unwrap();
    for key in ["C-/", "M-a"] {
        let parsed = key_string_lookup_string(key).expect("key parses");
        assert!(store.add_binding("prefix", parsed, None, false, Some(command.clone())));
    }

    let bindings = store.list_bindings(None, KeyBindingSortOrder::Key, false);
    assert!(bindings.iter().any(|binding| binding.key_string() == "C-/"));
    assert!(bindings.iter().any(|binding| binding.key_string() == "M-a"));
}

#[test]
fn modifiers_are_case_insensitive() {
    let key = key_string_lookup_string("c-m-s-a").expect("modifiers parse");
    assert_eq!(key_string_lookup_key(key, false), format!("C-M-S-{}", 'a'));
    assert_eq!(
        key & (KEYC_CTRL | KEYC_META | KEYC_SHIFT),
        KEYC_CTRL | KEYC_META | KEYC_SHIFT
    );
}
