use std::collections::HashMap;

use rmux_proto::{OptionName, OptionScopeSelector, ScopeSelector, SessionName, SetOptionMode};

use super::update_environment_from_client;
use crate::pane_terminals::HandlerState;

fn put(state: &mut HandlerState, scope: OptionScopeSelector, name: &str, value: &str) {
    state
        .options
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

fn fixture() -> (HandlerState, SessionName, HashMap<String, String>) {
    let mut state = HandlerState::default();
    let session = SessionName::new("array-environment").unwrap();
    let mut client = HashMap::new();
    for suffix in ["A", "B", "C"] {
        let key = format!("L6_{suffix}");
        state.environment.set(
            ScopeSelector::Session(session.clone()),
            key.clone(),
            format!("old_{suffix}"),
        );
        client.insert(key, format!("client_{suffix}"));
    }
    (state, session, client)
}

fn values(state: &HandlerState, session: &SessionName) -> Vec<Option<String>> {
    let mut child = HashMap::new();
    state
        .environment
        .apply_to_process_environment(Some(session), &mut child);
    ["L6_A", "L6_B", "L6_C"]
        .into_iter()
        .map(|key| child.get(key).cloned())
        .collect()
}

#[test]
fn update_environment_indexed_pattern_keeps_internal_space_and_updates_selected_variables() {
    let (mut state, session, client) = fixture();
    let scope = OptionScopeSelector::Session(session.clone());
    put(&mut state, scope.clone(), "update-environment", "");
    put(&mut state, scope, "update-environment[3]", "L6_[A B]");
    update_environment_from_client(&mut state, &session, &client);
    assert_eq!(
        values(&state, &session),
        vec![
            Some("client_A".into()),
            Some("client_B".into()),
            Some("old_C".into())
        ]
    );
    assert_eq!(
        state
            .options
            .resolve_array_values(Some(&session), OptionName::UpdateEnvironment),
        ["L6_[A B]"]
    );
}

#[test]
fn update_environment_array_inheritance_override_unset_and_indexed_append_have_real_effects() {
    let (mut state, session, client) = fixture();
    let global = OptionScopeSelector::SessionGlobal;
    let local = OptionScopeSelector::Session(session.clone());
    put(&mut state, global.clone(), "update-environment", "");
    put(&mut state, global.clone(), "update-environment[0]", "L6_[A");
    state
        .options
        .set_by_name(
            global,
            "update-environment[0]",
            Some(" B]".into()),
            SetOptionMode::Append,
            false,
            false,
            false,
        )
        .unwrap();
    put(&mut state, local.clone(), "update-environment", "L6_C");
    update_environment_from_client(&mut state, &session, &client);
    assert_eq!(
        values(&state, &session),
        vec![
            Some("old_A".into()),
            Some("old_B".into()),
            Some("client_C".into())
        ]
    );
    state
        .options
        .set_by_name(
            local,
            "update-environment",
            None,
            SetOptionMode::Replace,
            false,
            true,
            false,
        )
        .unwrap();
    update_environment_from_client(&mut state, &session, &client);
    assert_eq!(
        values(&state, &session),
        vec![
            Some("client_A".into()),
            Some("client_B".into()),
            Some("client_C".into())
        ]
    );
}

#[test]
fn update_environment_defaults_empty_array_and_missing_exact_variable_preserve_existing_semantics()
{
    let (mut state, session, mut client) = fixture();
    client.insert("DISPLAY".into(), "fixture-display".into());
    update_environment_from_client(&mut state, &session, &client);
    let mut child = HashMap::new();
    state
        .environment
        .apply_to_process_environment(Some(&session), &mut child);
    assert_eq!(
        child.get("DISPLAY").map(String::as_str),
        Some("fixture-display")
    );
    assert_eq!(
        values(&state, &session),
        vec![
            Some("old_A".into()),
            Some("old_B".into()),
            Some("old_C".into())
        ]
    );
    let scope = OptionScopeSelector::Session(session.clone());
    put(&mut state, scope.clone(), "update-environment", "");
    update_environment_from_client(&mut state, &session, &client);
    assert_eq!(
        values(&state, &session),
        vec![
            Some("old_A".into()),
            Some("old_B".into()),
            Some("old_C".into())
        ]
    );
    put(&mut state, scope, "update-environment", "L6_A L6_B");
    client.remove("L6_B");
    update_environment_from_client(&mut state, &session, &client);
    assert_eq!(
        values(&state, &session),
        vec![Some("client_A".into()), None, Some("old_C".into())]
    );
}
