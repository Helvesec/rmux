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
fn modifiers_are_case_insensitive() {
    let key = key_string_lookup_string("c-m-s-a").expect("modifiers parse");
    assert_eq!(key_string_lookup_key(key, false), format!("C-M-S-{}", 'a'));
    assert_eq!(
        key & (KEYC_CTRL | KEYC_META | KEYC_SHIFT),
        KEYC_CTRL | KEYC_META | KEYC_SHIFT
    );
}
