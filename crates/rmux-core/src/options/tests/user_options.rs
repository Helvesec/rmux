use super::*;
use std::collections::BTreeMap;

#[test]
fn string_scalar_append_concatenates_effective_value() {
    let mut store = OptionStore::new();

    store
        .set(
            ScopeSelector::Global,
            OptionName::StatusLeft,
            "[extra]".to_owned(),
            SetOptionMode::Append,
        )
        .expect("status-left append succeeds");

    assert_eq!(
        store.global_value(OptionName::StatusLeft),
        Some("[#{session_name}] [extra]")
    );
}

#[test]
fn user_options_require_a_non_empty_value() {
    let mut store = OptionStore::new();

    let error = store
        .set_by_name(
            OptionScopeSelector::SessionGlobal,
            "@empty",
            None,
            SetOptionMode::Replace,
            false,
            false,
            false,
        )
        .expect_err("missing user option value must fail");

    assert_eq!(error, RmuxError::InvalidSetOption("empty value".to_owned()));
}

#[test]
fn default_size_rejects_values_outside_tmux_pattern() {
    let mut store = OptionStore::new();

    let error = store
        .set(
            ScopeSelector::Session(session_name("alpha")),
            OptionName::DefaultSize,
            "80 by 24".to_owned(),
            SetOptionMode::Replace,
        )
        .expect_err("invalid default-size must fail");

    assert_eq!(
        error,
        RmuxError::InvalidSetOption("value is invalid: 80 by 24".to_owned())
    );
}

#[test]
fn user_option_set_and_resolve_at_session_global_scope() {
    let mut store = OptionStore::new();
    let alpha = session_name("alpha");

    store
        .set_by_name(
            OptionScopeSelector::SessionGlobal,
            "@my-var",
            Some("hello".to_owned()),
            SetOptionMode::Replace,
            false,
            false,
            false,
        )
        .expect("user option set succeeds");

    assert_eq!(
        store.resolve_name(Some(&alpha), "@my-var"),
        Some("hello".to_owned())
    );
}

#[test]
fn show_options_named_user_option_rejects_missing_scope_value() {
    let mut store = OptionStore::new();
    let alpha = session_name("alpha");
    let window = WindowTarget::with_window(alpha, 0);

    store
        .set_by_name(
            OptionScopeSelector::Window(window.clone()),
            "@wfoo",
            Some("win".to_owned()),
            SetOptionMode::Replace,
            false,
            false,
            false,
        )
        .expect("window user option set succeeds");

    assert_eq!(
        store
            .show_options_lines_filtered(&OptionScopeSelector::Window(window), Some("@wfoo"), true,)
            .expect("window user option is visible"),
        vec!["win".to_owned()]
    );
    assert_eq!(
        store
            .show_options_lines_filtered(&OptionScopeSelector::WindowGlobal, Some("@wfoo"), true)
            .expect_err("window-local user option is not global"),
        RmuxError::Message("invalid option: @wfoo".to_owned())
    );
    assert_eq!(
        store
            .show_options_lines_filtered(
                &OptionScopeSelector::SessionGlobal,
                Some("@missing"),
                true,
            )
            .expect_err("missing user option is invalid"),
        RmuxError::Message("invalid option: @missing".to_owned())
    );
}

#[test]
fn user_option_session_local_overrides_global() {
    let mut store = OptionStore::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");

    store
        .set_by_name(
            OptionScopeSelector::SessionGlobal,
            "@color",
            Some("red".to_owned()),
            SetOptionMode::Replace,
            false,
            false,
            false,
        )
        .expect("global set succeeds");

    store
        .set_by_name(
            OptionScopeSelector::Session(alpha.clone()),
            "@color",
            Some("blue".to_owned()),
            SetOptionMode::Replace,
            false,
            false,
            false,
        )
        .expect("session set succeeds");

    assert_eq!(
        store.resolve_name(Some(&alpha), "@color"),
        Some("blue".to_owned())
    );
    assert_eq!(
        store.resolve_name(Some(&beta), "@color"),
        Some("red".to_owned())
    );
}

#[test]
fn runtime_user_option_resolution_prefers_server_global_before_context_roots() {
    let mut store = OptionStore::new();
    let alpha = session_name("alpha");
    let window = WindowTarget::with_window(alpha.clone(), 2);
    let pane = PaneTarget::with_window(alpha.clone(), 2, 1);

    store
        .set_by_name(
            OptionScopeSelector::ServerGlobal,
            "@theme",
            Some("server".to_owned()),
            SetOptionMode::Replace,
            false,
            false,
            false,
        )
        .expect("server set succeeds");
    store
        .set_by_name(
            OptionScopeSelector::Session(alpha.clone()),
            "@theme",
            Some("session".to_owned()),
            SetOptionMode::Replace,
            false,
            false,
            false,
        )
        .expect("session set succeeds");
    store
        .set_by_name(
            OptionScopeSelector::Window(window),
            "@theme",
            Some("window".to_owned()),
            SetOptionMode::Replace,
            false,
            false,
            false,
        )
        .expect("window set succeeds");
    store
        .set_by_name(
            OptionScopeSelector::Pane(pane),
            "@theme",
            Some("pane".to_owned()),
            SetOptionMode::Replace,
            false,
            false,
            false,
        )
        .expect("pane set succeeds");

    assert_eq!(
        store.resolve_name(Some(&alpha), "@theme"),
        Some("server".to_owned())
    );
    assert_eq!(
        store.resolve_name_for_window(&alpha, 2, "@theme"),
        Some("server".to_owned())
    );
    assert_eq!(
        store.resolve_name_for_pane(&alpha, 2, 1, "@theme"),
        Some("server".to_owned())
    );
}

#[test]
fn runtime_user_option_resolution_prefers_window_chain_before_session_chain() {
    let mut store = OptionStore::new();
    let alpha = session_name("alpha");
    let pane = PaneTarget::with_window(alpha.clone(), 3, 0);

    store
        .set_by_name(
            OptionScopeSelector::Session(alpha.clone()),
            "@theme",
            Some("session".to_owned()),
            SetOptionMode::Replace,
            false,
            false,
            false,
        )
        .expect("session set succeeds");
    store
        .set_by_name(
            OptionScopeSelector::WindowGlobal,
            "@theme",
            Some("window-global".to_owned()),
            SetOptionMode::Replace,
            false,
            false,
            false,
        )
        .expect("window global set succeeds");
    store
        .set_by_name(
            OptionScopeSelector::Pane(pane),
            "@theme",
            Some("pane".to_owned()),
            SetOptionMode::Replace,
            false,
            false,
            false,
        )
        .expect("pane set succeeds");

    assert_eq!(
        store.resolve_name_for_window(&alpha, 3, "@theme"),
        Some("window-global".to_owned())
    );
    assert_eq!(
        store.resolve_name_for_pane(&alpha, 3, 0, "@theme"),
        Some("pane".to_owned())
    );
}

#[test]
fn user_option_rejects_array_index_syntax() {
    let result = super::resolve_option_name("@my-var[0]");
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("array indexes"));
}

#[test]
fn user_option_mutation_outcome_reports_old_new_and_changed() {
    let mut store = OptionStore::new();
    let pane = PaneTarget::with_window(session_name("alpha"), 0, 0);
    let scope = OptionScopeSelector::Pane(pane);

    let first = store
        .set_by_name(
            scope.clone(),
            "@agent.state",
            Some("waiting".to_owned()),
            SetOptionMode::Replace,
            false,
            false,
            false,
        )
        .expect("initial set succeeds");
    assert_eq!(first.old_explicit, None);
    assert_eq!(first.new_explicit, Some("waiting".to_owned()));
    assert!(first.changed);

    let idempotent = store
        .set_by_name(
            scope.clone(),
            "@agent.state",
            Some("waiting".to_owned()),
            SetOptionMode::Replace,
            false,
            false,
            false,
        )
        .expect("idempotent set succeeds");
    assert_eq!(idempotent.old_explicit, Some("waiting".to_owned()));
    assert_eq!(idempotent.new_explicit, Some("waiting".to_owned()));
    assert!(!idempotent.changed);

    let unset = store
        .set_by_name(
            scope,
            "@agent.state",
            None,
            SetOptionMode::Replace,
            false,
            true,
            false,
        )
        .expect("unset succeeds");
    assert_eq!(unset.old_explicit, Some("waiting".to_owned()));
    assert_eq!(unset.new_explicit, None);
    assert!(unset.changed);
}

#[test]
fn window_unset_reports_related_pane_override_mutations() {
    let mut store = OptionStore::new();
    let session = session_name("alpha");
    let window = WindowTarget::with_window(session.clone(), 0);
    let pane = PaneTarget::with_window(session.clone(), 0, 1);

    store
        .set_by_name(
            OptionScopeSelector::Window(window.clone()),
            "@agent.state",
            Some("window".to_owned()),
            SetOptionMode::Replace,
            false,
            false,
            false,
        )
        .expect("window option set succeeds");
    store
        .set_by_name(
            OptionScopeSelector::Pane(pane.clone()),
            "@agent.state",
            Some("pane".to_owned()),
            SetOptionMode::Replace,
            false,
            false,
            false,
        )
        .expect("pane option set succeeds");

    let outcome = store
        .set_by_name(
            OptionScopeSelector::Window(window),
            "@agent.state",
            None,
            SetOptionMode::Replace,
            false,
            true,
            true,
        )
        .expect("window option unset succeeds");

    assert_eq!(outcome.old_explicit, Some("window".to_owned()));
    assert_eq!(outcome.new_explicit, None);
    assert_eq!(outcome.related.len(), 1);
    assert_eq!(outcome.related[0].scope, OptionScopeSelector::Pane(pane));
    assert_eq!(outcome.related[0].old_explicit, Some("pane".to_owned()));
    assert_eq!(outcome.related[0].new_explicit, None);
    assert!(outcome.related[0].changed);
    assert_eq!(
        store.resolve_name_for_pane(&session, 0, 1, "@agent.state"),
        None
    );
}

#[test]
fn pane_overrides_follow_pane_index_remap() {
    let mut store = OptionStore::new();
    let alpha = session_name("alpha");
    let pane_zero = PaneTarget::with_window(alpha.clone(), 2, 0);
    let pane_two = PaneTarget::with_window(alpha.clone(), 2, 2);

    store
        .set_by_name(
            OptionScopeSelector::Pane(pane_zero),
            "@agent.state",
            Some("idle".to_owned()),
            SetOptionMode::Replace,
            false,
            false,
            false,
        )
        .expect("pane 0 option set succeeds");
    store
        .set_by_name(
            OptionScopeSelector::Pane(pane_two),
            "@agent.state",
            Some("working".to_owned()),
            SetOptionMode::Replace,
            false,
            false,
            false,
        )
        .expect("pane 2 option set succeeds");

    store
        .remap_pane_indices(&alpha, 2, &BTreeMap::from([(2, 1)]))
        .expect("pane remap succeeds");

    assert_eq!(
        store.resolve_name_for_pane(&alpha, 2, 0, "@agent.state"),
        Some("idle".to_owned())
    );
    assert_eq!(
        store.resolve_name_for_pane(&alpha, 2, 1, "@agent.state"),
        Some("working".to_owned())
    );
    assert_eq!(
        store.resolve_name_for_pane(&alpha, 2, 2, "@agent.state"),
        None
    );
}

#[test]
fn pane_overrides_transfer_between_pane_slots() {
    let mut store = OptionStore::new();
    let alpha = session_name("alpha");
    let source = PaneTarget::with_window(alpha.clone(), 1, 2);
    let target = PaneTarget::with_window(alpha.clone(), 3, 0);

    store
        .set_by_name(
            OptionScopeSelector::Pane(source.clone()),
            "@agent.kind",
            Some("opencode".to_owned()),
            SetOptionMode::Replace,
            false,
            false,
            false,
        )
        .expect("source pane option set succeeds");
    store
        .set_by_name(
            OptionScopeSelector::Pane(target.clone()),
            "@agent.kind",
            Some("stale".to_owned()),
            SetOptionMode::Replace,
            false,
            false,
            false,
        )
        .expect("target pane option set succeeds");

    store.transfer_pane_overrides(&source, &target);

    assert_eq!(
        store.resolve_name_for_pane(&alpha, 1, 2, "@agent.kind"),
        None
    );
    assert_eq!(
        store.resolve_name_for_pane(&alpha, 3, 0, "@agent.kind"),
        Some("opencode".to_owned())
    );
}

#[test]
fn pane_overrides_swap_between_pane_slots() {
    let mut store = OptionStore::new();
    let alpha = session_name("alpha");
    let left = PaneTarget::with_window(alpha.clone(), 0, 0);
    let right = PaneTarget::with_window(alpha.clone(), 1, 0);

    store
        .set_by_name(
            OptionScopeSelector::Pane(left.clone()),
            "@agent.kind",
            Some("left-kind".to_owned()),
            SetOptionMode::Replace,
            false,
            false,
            false,
        )
        .expect("left pane option set succeeds");
    store
        .set_by_name(
            OptionScopeSelector::Pane(right.clone()),
            "@agent.kind",
            Some("right-kind".to_owned()),
            SetOptionMode::Replace,
            false,
            false,
            false,
        )
        .expect("right pane option set succeeds");

    store.swap_pane_overrides(&left, &right);

    assert_eq!(
        store.resolve_name_for_pane(&alpha, 0, 0, "@agent.kind"),
        Some("right-kind".to_owned())
    );
    assert_eq!(
        store.resolve_name_for_pane(&alpha, 1, 0, "@agent.kind"),
        Some("left-kind".to_owned())
    );
}
