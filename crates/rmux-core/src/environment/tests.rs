use super::{EnvironmentStore, ShowEnvironmentEntry, ENVIRON_HIDDEN};
use rmux_proto::{RmuxError, ScopeSelector, SessionName};
use std::collections::HashMap;

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

#[test]
fn environment_store_starts_empty() {
    let store = EnvironmentStore::new();

    assert!(store.is_empty());
    assert_eq!(store.global_value("TERM"), None);
    assert_eq!(store.resolve(Some(&session_name("alpha")), "TERM"), None);
    assert_eq!(store.resolved(&session_name("alpha")), HashMap::new());
}

#[test]
fn global_set_then_read_returns_the_exact_value() {
    let mut store = EnvironmentStore::new();

    store.set(
        ScopeSelector::Global,
        "TERM".to_owned(),
        "xterm-256color".to_owned(),
    );

    assert_eq!(store.global_value("TERM"), Some("xterm-256color"));
    assert_eq!(store.resolve(None, "TERM"), Some("xterm-256color"));
}

#[cfg(windows)]
#[test]
fn named_show_environment_lookup_is_case_insensitive_on_windows() {
    let mut store = EnvironmentStore::new();

    store.set(
        ScopeSelector::Global,
        "Path".to_owned(),
        "C:\\rmux".to_owned(),
    );

    let entries = store
        .show_environment_entries(&ScopeSelector::Global, false, Some("PATH"))
        .expect("case-insensitive lookup succeeds");

    assert_eq!(
        entries,
        vec![ShowEnvironmentEntry {
            name: "Path".to_owned(),
            value: Some("C:\\rmux".to_owned()),
            flags: 0,
            value_is_display_escape: false,
        }]
    );
}

#[test]
fn session_values_override_global_values_in_resolved_snapshots() {
    let mut store = EnvironmentStore::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");

    store.set(
        ScopeSelector::Global,
        "TERM".to_owned(),
        "screen".to_owned(),
    );
    store.set(
        ScopeSelector::Global,
        "COLORTERM".to_owned(),
        "truecolor".to_owned(),
    );
    store.set(
        ScopeSelector::Session(alpha.clone()),
        "TERM".to_owned(),
        "tmux-256color".to_owned(),
    );

    assert_eq!(store.resolve(Some(&alpha), "TERM"), Some("tmux-256color"));
    assert_eq!(store.resolve(Some(&beta), "TERM"), Some("screen"));
    assert_eq!(
        store.resolved(&alpha),
        HashMap::from([
            ("COLORTERM".to_owned(), "truecolor".to_owned()),
            ("TERM".to_owned(), "tmux-256color".to_owned()),
        ])
    );
}

#[test]
fn hidden_and_cleared_entries_are_suppressed_from_child_environment() {
    let mut store = EnvironmentStore::new();
    let alpha = session_name("alpha");
    let mut process_environment = HashMap::from([
        ("GLOBAL".to_owned(), "server".to_owned()),
        ("SECRET".to_owned(), "outside".to_owned()),
        ("TERM".to_owned(), "outside-term".to_owned()),
    ]);

    store.set(
        ScopeSelector::Global,
        "GLOBAL".to_owned(),
        "inside".to_owned(),
    );
    store.set_with_flags(
        ScopeSelector::Global,
        "SECRET".to_owned(),
        "value".to_owned(),
        ENVIRON_HIDDEN,
    );
    store.set(
        ScopeSelector::Session(alpha.clone()),
        "TERM".to_owned(),
        "screen".to_owned(),
    );
    store.clear(ScopeSelector::Session(alpha.clone()), "GLOBAL".to_owned());

    store.apply_to_process_environment(Some(&alpha), &mut process_environment);

    assert_eq!(
        process_environment.get("TERM").map(String::as_str),
        Some("screen")
    );
    assert_eq!(process_environment.get("GLOBAL"), None);
    assert_eq!(process_environment.get("SECRET"), None);
}

#[test]
fn implicit_globals_can_be_skipped_for_client_based_spawn_environments() {
    let mut store = EnvironmentStore::new();
    let alpha = session_name("alpha");
    let mut default_process_environment = HashMap::from([("FOO".to_owned(), "client".to_owned())]);
    let mut client_based_environment = default_process_environment.clone();

    store.set_implicit_global("FOO".to_owned(), "server".to_owned());
    store.set_implicit_global("BAR".to_owned(), "server-only".to_owned());

    store.apply_to_process_environment(Some(&alpha), &mut default_process_environment);
    assert_eq!(
        default_process_environment.get("FOO").map(String::as_str),
        Some("server")
    );
    assert_eq!(
        default_process_environment.get("BAR").map(String::as_str),
        Some("server-only")
    );

    store.apply_to_process_environment_without_implicit_globals(
        Some(&alpha),
        &mut client_based_environment,
    );
    assert_eq!(
        client_based_environment.get("FOO").map(String::as_str),
        Some("client")
    );
    assert_eq!(client_based_environment.get("BAR"), None);

    store.set(
        ScopeSelector::Global,
        "FOO".to_owned(),
        "explicit-global".to_owned(),
    );
    store.apply_to_process_environment_without_implicit_globals(
        Some(&alpha),
        &mut client_based_environment,
    );
    assert_eq!(
        client_based_environment.get("FOO").map(String::as_str),
        Some("explicit-global")
    );
}

#[test]
fn display_only_implicit_globals_are_not_applied_to_child_environments() {
    let mut store = EnvironmentStore::new();
    let alpha = session_name("alpha");
    let mut process_environment = HashMap::from([("RAW".to_owned(), "raw-bytes".to_owned())]);

    store.set_implicit_global_display("RAW".to_owned(), "A\\377B".to_owned());

    assert_eq!(store.global_value("RAW"), None);
    store.apply_to_process_environment(Some(&alpha), &mut process_environment);
    assert_eq!(
        process_environment.get("RAW").map(String::as_str),
        Some("raw-bytes")
    );
    assert_eq!(
        store
            .show_environment_entries(&ScopeSelector::Global, false, Some("RAW"))
            .expect("show env"),
        vec![ShowEnvironmentEntry {
            name: "RAW".to_owned(),
            value: Some("A\\377B".to_owned()),
            flags: 0,
            value_is_display_escape: true,
        }]
    );
}

#[test]
fn clear_creates_tombstone_and_unset_removes_entry() {
    let mut store = EnvironmentStore::new();
    let alpha = session_name("alpha");

    store.set(
        ScopeSelector::Session(alpha.clone()),
        "TERM".to_owned(),
        "screen".to_owned(),
    );
    store.clear(ScopeSelector::Session(alpha.clone()), "TERM".to_owned());

    assert!(store.contains_entry(&ScopeSelector::Session(alpha.clone()), "TERM"));
    assert_eq!(store.resolve(Some(&alpha), "TERM"), None);
    assert_eq!(store.resolved(&alpha).get("TERM"), None);

    assert!(store.unset(ScopeSelector::Session(alpha.clone()), "TERM"));
    assert!(!store.contains_entry(&ScopeSelector::Session(alpha), "TERM"));
}

#[test]
fn unset_global_suppresses_process_environment_but_session_unset_removes_only_local_entry() {
    let mut store = EnvironmentStore::new();
    let alpha = session_name("alpha");
    let mut process_environment = HashMap::from([
        ("FOO".to_owned(), "client".to_owned()),
        ("BAR".to_owned(), "client".to_owned()),
    ]);

    assert!(!store.unset(ScopeSelector::Global, "FOO"));
    store.unset(ScopeSelector::Session(alpha.clone()), "BAR");

    store.apply_to_process_environment(Some(&alpha), &mut process_environment);

    assert!(!store.contains_entry(&ScopeSelector::Global, "FOO"));
    assert_eq!(process_environment.get("FOO"), None);
    assert_eq!(
        process_environment.get("BAR").map(String::as_str),
        Some("client")
    );
    assert!(store
        .show_environment_entries(&ScopeSelector::Global, false, None)
        .expect("show env")
        .is_empty());
    assert_eq!(
        store.suppressed_process_environment_names(Some(&alpha), true),
        ["FOO".to_owned()].into_iter().collect()
    );
}

#[cfg(windows)]
#[test]
fn unset_global_suppresses_process_environment_case_insensitively() {
    let mut store = EnvironmentStore::new();
    let alpha = session_name("alpha");
    let mut process_environment = HashMap::from([("Path".to_owned(), "client".to_owned())]);

    assert!(!store.unset(ScopeSelector::Global, "PATH"));

    store.apply_to_process_environment(Some(&alpha), &mut process_environment);

    assert_eq!(process_environment.get("Path"), None);
}

#[cfg(windows)]
#[test]
fn explicit_global_set_replaces_implicit_case_insensitively() {
    let mut store = EnvironmentStore::new();
    let alpha = session_name("alpha");
    let mut process_environment = HashMap::from([("RmuxCase".to_owned(), "client".to_owned())]);

    store.set_implicit_global("RmuxCase".to_owned(), "base".to_owned());
    store.set(
        ScopeSelector::Global,
        "RMUXCASE".to_owned(),
        "override".to_owned(),
    );
    store.apply_to_process_environment(Some(&alpha), &mut process_environment);

    assert_eq!(process_environment.len(), 1);
    assert_eq!(
        process_environment.get("RMUXCASE").map(String::as_str),
        Some("override")
    );
}

#[test]
fn show_environment_filters_hidden_entries_and_preserves_tombstones() {
    let mut store = EnvironmentStore::new();
    let alpha = session_name("alpha");

    store.set(
        ScopeSelector::Session(alpha.clone()),
        "TERM".to_owned(),
        "screen".to_owned(),
    );
    store.set_with_flags(
        ScopeSelector::Session(alpha.clone()),
        "SECRET".to_owned(),
        "value".to_owned(),
        ENVIRON_HIDDEN,
    );
    store.clear(ScopeSelector::Session(alpha.clone()), "EMPTY".to_owned());

    assert_eq!(
        store
            .show_environment_entries(&ScopeSelector::Session(alpha.clone()), false, None)
            .expect("visible entries"),
        vec![
            ShowEnvironmentEntry {
                name: "EMPTY".to_owned(),
                value: None,
                flags: 0,
                value_is_display_escape: false,
            },
            ShowEnvironmentEntry {
                name: "TERM".to_owned(),
                value: Some("screen".to_owned()),
                flags: 0,
                value_is_display_escape: false,
            },
        ]
    );
    assert_eq!(
        store
            .show_environment_entries(&ScopeSelector::Session(alpha), true, None)
            .expect("hidden entries"),
        vec![ShowEnvironmentEntry {
            name: "SECRET".to_owned(),
            value: Some("value".to_owned()),
            flags: ENVIRON_HIDDEN,
            value_is_display_escape: false,
        }]
    );
}

#[test]
fn update_copies_matching_variables_and_clears_misses() {
    let mut store = EnvironmentStore::new();
    let alpha = session_name("alpha");
    let source = HashMap::from([
        ("DISPLAY".to_owned(), ":1".to_owned()),
        ("SSH_AUTH_SOCK".to_owned(), "/tmp/rmux-auth-sock".to_owned()),
    ]);

    store.update(
        &alpha,
        &["DISPLAY".to_owned(), "SSH_*".to_owned(), "KRB*".to_owned()],
        &source,
    );

    assert_eq!(store.resolve(Some(&alpha), "DISPLAY"), Some(":1"));
    assert_eq!(
        store.resolve(Some(&alpha), "SSH_AUTH_SOCK"),
        Some("/tmp/rmux-auth-sock")
    );
    assert!(store.contains_entry(&ScopeSelector::Session(alpha.clone()), "KRB*"));
    assert_eq!(store.resolve(Some(&alpha), "KRB*"), None);
    assert_eq!(store.resolved(&alpha).get("KRB*"), None);
}

#[test]
fn global_overwrite_replaces_previous_value() {
    let mut store = EnvironmentStore::new();

    store.set(
        ScopeSelector::Global,
        "TERM".to_owned(),
        "screen".to_owned(),
    );
    store.set(
        ScopeSelector::Global,
        "TERM".to_owned(),
        "xterm-256color".to_owned(),
    );

    assert_eq!(store.global_value("TERM"), Some("xterm-256color"));
}

#[test]
fn remove_nonexistent_session_returns_none() {
    let mut store = EnvironmentStore::new();

    store.set(
        ScopeSelector::Global,
        "TERM".to_owned(),
        "screen".to_owned(),
    );

    assert_eq!(store.remove_session(&session_name("never-set")), None);
    assert_eq!(store.global_value("TERM"), Some("screen"));
}

#[test]
fn removing_a_session_discards_only_its_local_values() {
    let mut store = EnvironmentStore::new();
    let alpha = session_name("alpha");

    store.set(
        ScopeSelector::Global,
        "TERM".to_owned(),
        "screen".to_owned(),
    );
    store.set(
        ScopeSelector::Session(alpha.clone()),
        "TERM".to_owned(),
        "tmux-256color".to_owned(),
    );

    let removed = store
        .remove_session(&alpha)
        .expect("session values are present");

    assert_eq!(
        removed.get("TERM").map(String::as_str),
        Some("tmux-256color")
    );
    assert_eq!(store.resolve(Some(&alpha), "TERM"), Some("screen"));
}

#[test]
fn removing_a_session_discards_its_unset_tombstones() {
    let mut store = EnvironmentStore::new();
    let alpha = session_name("alpha");
    let mut process_environment = HashMap::from([("SECRET".to_owned(), "client".to_owned())]);

    store.unset(ScopeSelector::Session(alpha.clone()), "SECRET");
    let _ = store.remove_session(&alpha);
    store.apply_to_process_environment(Some(&alpha), &mut process_environment);

    assert_eq!(
        process_environment.get("SECRET").map(String::as_str),
        Some("client")
    );
}

#[test]
fn renaming_a_session_moves_only_its_local_values() {
    let mut store = EnvironmentStore::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");

    store.set(
        ScopeSelector::Global,
        "TERM".to_owned(),
        "screen".to_owned(),
    );
    store.set(
        ScopeSelector::Session(alpha.clone()),
        "TERM".to_owned(),
        "tmux-256color".to_owned(),
    );

    store
        .rename_session(&alpha, beta.clone())
        .expect("rename succeeds");

    assert_eq!(store.resolve(Some(&alpha), "TERM"), Some("screen"));
    assert_eq!(store.resolve(Some(&beta), "TERM"), Some("tmux-256color"));
}

#[test]
fn renaming_a_session_keeps_session_unset_as_local_removal_only() {
    let mut store = EnvironmentStore::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    let mut old_environment = HashMap::from([("SECRET".to_owned(), "old".to_owned())]);
    let mut new_environment = HashMap::from([("SECRET".to_owned(), "new".to_owned())]);

    store.unset(ScopeSelector::Session(alpha.clone()), "SECRET");
    store
        .rename_session(&alpha, beta.clone())
        .expect("rename succeeds");

    store.apply_to_process_environment(Some(&alpha), &mut old_environment);
    store.apply_to_process_environment(Some(&beta), &mut new_environment);

    assert_eq!(
        old_environment.get("SECRET").map(String::as_str),
        Some("old")
    );
    assert_eq!(
        new_environment.get("SECRET").map(String::as_str),
        Some("new")
    );
}

#[test]
fn renaming_to_an_existing_environment_key_fails_without_mutation() {
    let mut store = EnvironmentStore::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");

    store.set(
        ScopeSelector::Session(alpha.clone()),
        "TERM".to_owned(),
        "screen".to_owned(),
    );
    store.set(
        ScopeSelector::Session(beta.clone()),
        "TERM".to_owned(),
        "tmux-256color".to_owned(),
    );

    let error = store
        .rename_session(&alpha, beta.clone())
        .expect_err("existing destination rejects rename");

    assert_eq!(
        error,
        RmuxError::Server("environment already exists for session beta".to_owned())
    );
    assert_eq!(store.session_value(&alpha, "TERM"), Some("screen"));
    assert_eq!(store.session_value(&beta, "TERM"), Some("tmux-256color"));
}
