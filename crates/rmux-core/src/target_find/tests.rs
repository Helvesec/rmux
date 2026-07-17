use super::{
    command_target_metadata, TargetFindContext, TargetFindFlags, TargetFindType, UnresolvedTarget,
};
use crate::{SessionStore, Window};
use rmux_proto::{
    PaneTarget, RmuxError, SessionName, SplitDirection, Target, TerminalSize, WindowTarget,
};

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

fn populated_store() -> SessionStore {
    let mut store = SessionStore::new();
    for name in ["alpha", "beta"] {
        store
            .create_session(session_name(name), TerminalSize { cols: 80, rows: 24 })
            .expect("session create succeeds");
    }
    {
        let session = store
            .session_mut(&session_name("alpha"))
            .expect("alpha exists");
        session
            .insert_window_with_initial_pane(1, TerminalSize { cols: 80, rows: 24 })
            .expect("window 1 insert succeeds");
        session
            .insert_window_with_initial_pane(2, TerminalSize { cols: 80, rows: 24 })
            .expect("window 2 insert succeeds");
        session
            .rename_window(1, "editor".to_owned())
            .expect("window rename succeeds");
        session
            .rename_window(2, "logs".to_owned())
            .expect("window rename succeeds");
        session
            .select_window(2)
            .expect("select active window succeeds");
        session.split_active_pane().expect("split succeeds");
        session.select_pane(1).expect("select pane succeeds");
    }
    store
}

fn current() -> TargetFindContext {
    TargetFindContext::from_target(Target::Pane(PaneTarget::with_window(
        session_name("alpha"),
        2,
        1,
    )))
}

fn resolve(
    store: &SessionStore,
    raw: &str,
    find_type: TargetFindType,
) -> Result<Target, RmuxError> {
    store.resolve_unresolved_target(
        &UnresolvedTarget::new(raw),
        find_type,
        TargetFindFlags::NONE,
        &current(),
    )
}

#[test]
fn resolves_exact_target_to_existing_protocol_types() {
    let store = populated_store();

    let target = resolve(&store, "alpha:2.1", TargetFindType::Pane).expect("target resolves");

    assert_eq!(
        target,
        Target::Pane(PaneTarget::with_window(session_name("alpha"), 2, 1))
    );
}

#[test]
fn resolves_session_prefix_and_pattern_with_ambiguity_errors() {
    let mut store = populated_store();
    store
        .create_session(session_name("alpine"), TerminalSize { cols: 80, rows: 24 })
        .expect("session create succeeds");

    let target = resolve(&store, "bet", TargetFindType::Session).expect("prefix resolves");
    assert_eq!(target, Target::Session(session_name("beta")));

    let target = resolve(&store, "bet*", TargetFindType::Session).expect("glob resolves");
    assert_eq!(target, Target::Session(session_name("beta")));

    let error = resolve(&store, "al", TargetFindType::Session).expect_err("ambiguous prefix fails");
    assert_eq!(
        error,
        RmuxError::invalid_target("al", "ambiguous session match")
    );
}

#[test]
fn exact_session_prefix_suppresses_pattern_fallback() {
    let store = populated_store();

    let error =
        resolve(&store, "=alp*", TargetFindType::Session).expect_err("exact prefix does not glob");

    assert_eq!(
        error,
        RmuxError::invalid_target("alp*", "can't find session: alp*")
    );
}

#[test]
fn resolves_session_window_and_pane_id_forms() {
    let store = populated_store();
    let alpha_window_id = store
        .session(&session_name("alpha"))
        .and_then(|session| session.window_at(1))
        .map(Window::id)
        .expect("alpha window id exists");
    let active_window_id = store
        .session(&session_name("alpha"))
        .and_then(|session| session.window_at(2))
        .map(Window::id)
        .expect("alpha active window id exists");

    assert_eq!(
        resolve(&store, "$0", TargetFindType::Session).expect("session id resolves"),
        Target::Session(session_name("alpha"))
    );
    assert_eq!(
        resolve(
            &store,
            &format!("alpha:{alpha_window_id}"),
            TargetFindType::Window,
        )
        .expect("window id resolves"),
        Target::Window(WindowTarget::with_window(session_name("alpha"), 1))
    );
    assert_eq!(
        resolve(&store, "alpha:.%2", TargetFindType::Pane).expect("pane id resolves"),
        Target::Pane(PaneTarget::with_window(session_name("alpha"), 2, 0))
    );
    assert_eq!(
        resolve(
            &store,
            &format!("{active_window_id}.%2"),
            TargetFindType::Pane,
        )
        .expect("window id plus pane id resolves"),
        Target::Pane(PaneTarget::with_window(session_name("alpha"), 2, 0))
    );
    assert_eq!(
        resolve(
            &store,
            &format!("{active_window_id}.1"),
            TargetFindType::Pane,
        )
        .expect("window id plus pane index resolves"),
        Target::Pane(PaneTarget::with_window(session_name("alpha"), 2, 1))
    );
}

#[test]
fn window_ids_remain_unique_after_other_sessions_are_removed() {
    let mut store = SessionStore::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    store
        .create_session(alpha.clone(), TerminalSize { cols: 80, rows: 24 })
        .expect("alpha create succeeds");
    store
        .create_session(beta.clone(), TerminalSize { cols: 80, rows: 24 })
        .expect("beta create succeeds");

    let beta_initial_window_id = store
        .session(&beta)
        .and_then(|session| session.window_at(0))
        .map(Window::id)
        .expect("beta initial window exists");
    store.remove_session(&alpha).expect("alpha remove succeeds");
    store
        .session_mut(&beta)
        .expect("beta exists")
        .insert_window_with_initial_pane(1, TerminalSize { cols: 80, rows: 24 })
        .expect("beta window insert succeeds");
    let beta_new_window_id = store
        .session(&beta)
        .and_then(|session| session.window_at(1))
        .map(Window::id)
        .expect("beta new window exists");

    assert!(beta_new_window_id > beta_initial_window_id);
    assert_eq!(
        store
            .resolve_unresolved_target(
                &UnresolvedTarget::new(beta_new_window_id.to_string()),
                TargetFindType::Window,
                TargetFindFlags::NONE,
                &TargetFindContext::new(None),
            )
            .expect("window id resolves"),
        Target::Window(WindowTarget::with_window(beta, 1))
    );
}

#[test]
fn resolves_window_names_prefix_patterns_offsets_and_tokens() {
    let store = populated_store();

    assert_eq!(
        resolve(&store, "ed", TargetFindType::Window).expect("window prefix resolves"),
        Target::Window(WindowTarget::with_window(session_name("alpha"), 1))
    );
    assert_eq!(
        resolve(&store, "lo*", TargetFindType::Window).expect("window glob resolves"),
        Target::Window(WindowTarget::with_window(session_name("alpha"), 2))
    );
    assert_eq!(
        resolve(&store, "+1", TargetFindType::Window).expect("window offset wraps"),
        Target::Window(WindowTarget::with_window(session_name("alpha"), 0))
    );
    assert_eq!(
        resolve(&store, "{start}", TargetFindType::Window).expect("start token resolves"),
        Target::Window(WindowTarget::with_window(session_name("alpha"), 0))
    );
    assert_eq!(
        resolve(&store, "{last}", TargetFindType::Window).expect("last token resolves"),
        Target::Window(WindowTarget::with_window(session_name("alpha"), 0))
    );
}

#[test]
fn window_index_flag_allows_nonexistent_window_targets() {
    let store = populated_store();

    let target = store
        .resolve_unresolved_target(
            &UnresolvedTarget::new("alpha:9"),
            TargetFindType::Window,
            TargetFindFlags::WINDOW_INDEX,
            &current(),
        )
        .expect("window index resolves");

    assert_eq!(
        target,
        Target::Window(WindowTarget::with_window(session_name("alpha"), 9))
    );
}

#[test]
fn window_index_flag_resolves_plus_window_slots_relative_to_current() {
    let store = populated_store();

    let target = store
        .resolve_unresolved_target(
            &UnresolvedTarget::new("alpha:+3"),
            TargetFindType::Window,
            TargetFindFlags::WINDOW_INDEX,
            &current(),
        )
        .expect("positive destination slots resolve relative to the active window");

    assert_eq!(
        target,
        Target::Window(WindowTarget::with_window(session_name("alpha"), 5))
    );
}

#[test]
fn window_index_flag_resolves_negative_window_slots_relative_to_current() {
    let store = populated_store();

    let target = store
        .resolve_unresolved_target(
            &UnresolvedTarget::new("alpha:-1"),
            TargetFindType::Window,
            TargetFindFlags::WINDOW_INDEX,
            &current(),
        )
        .expect("negative destination slots resolve relative to the current window");

    assert_eq!(
        target,
        Target::Window(WindowTarget::with_window(session_name("alpha"), 1))
    );
}

#[test]
fn window_index_flag_resolves_empty_window_part_to_next_available_slot() {
    let store = populated_store();

    let explicit_session = store
        .resolve_unresolved_target(
            &UnresolvedTarget::new("alpha:"),
            TargetFindType::Window,
            TargetFindFlags::WINDOW_INDEX,
            &current(),
        )
        .expect("empty window part resolves as destination slot");
    let current_session = store
        .resolve_unresolved_target(
            &UnresolvedTarget::new(":"),
            TargetFindType::Window,
            TargetFindFlags::WINDOW_INDEX,
            &current(),
        )
        .expect("current-session empty window part resolves as destination slot");

    assert_eq!(
        explicit_session,
        Target::Window(WindowTarget::with_window(session_name("alpha"), 3))
    );
    assert_eq!(
        current_session,
        Target::Window(WindowTarget::with_window(session_name("alpha"), 3))
    );
}

#[test]
fn window_index_flag_still_resolves_special_window_tokens() {
    let store = populated_store();

    let target = store
        .resolve_unresolved_target(
            &UnresolvedTarget::new("alpha:{end}"),
            TargetFindType::Window,
            TargetFindFlags::WINDOW_INDEX,
            &current(),
        )
        .expect("special destination token resolves");

    assert_eq!(
        target,
        Target::Window(WindowTarget::with_window(session_name("alpha"), 2))
    );
}

#[test]
fn window_index_flag_rejects_targets_with_panes() {
    let store = populated_store();

    let error = store
        .resolve_unresolved_target(
            &UnresolvedTarget::new("alpha:9.0"),
            TargetFindType::Window,
            TargetFindFlags::WINDOW_INDEX,
            &current(),
        )
        .expect_err("pane component is invalid for window-index lookup");

    assert_eq!(
        error,
        RmuxError::invalid_target("alpha:9.0", "can't specify pane here")
    );
}

#[test]
fn resolves_pane_offsets_and_directional_tokens() {
    let store = populated_store();

    assert_eq!(
        resolve(&store, "alpha:2.-1", TargetFindType::Pane).expect("pane offset wraps"),
        Target::Pane(PaneTarget::with_window(session_name("alpha"), 2, 0))
    );
    assert_eq!(
        resolve(&store, "alpha:2.{left-of}", TargetFindType::Pane)
            .expect("directional token resolves"),
        Target::Pane(PaneTarget::with_window(session_name("alpha"), 2, 0))
    );
    assert_eq!(
        resolve(&store, "alpha:2.{top}", TargetFindType::Pane).expect("description token resolves"),
        Target::Pane(PaneTarget::with_window(session_name("alpha"), 2, 0))
    );
}

#[test]
fn directional_pane_tokens_wrap_at_window_edges_like_tmux() {
    let mut store = SessionStore::new();
    let alpha = session_name("alpha");
    store
        .create_session(
            alpha.clone(),
            TerminalSize {
                cols: 120,
                rows: 35,
            },
        )
        .expect("session create succeeds");
    {
        let session = store.session_mut(&alpha).expect("alpha exists");
        session
            .split_pane_with_direction(0, SplitDirection::Vertical)
            .expect("right split succeeds");
        session
            .split_pane_with_direction(0, SplitDirection::Horizontal)
            .expect("bottom split succeeds");
        session.select_pane(1).expect("select bottom-left pane");
    }

    let left_of = store
        .resolve_unresolved_target(
            &UnresolvedTarget::new("alpha:0.{left-of}"),
            TargetFindType::Pane,
            TargetFindFlags::NONE,
            &TargetFindContext::from_target(Target::Pane(PaneTarget::with_window(
                alpha.clone(),
                0,
                1,
            ))),
        )
        .expect("left-of wraps to the right edge");
    assert_eq!(
        left_of,
        Target::Pane(PaneTarget::with_window(alpha.clone(), 0, 2))
    );

    store
        .session_mut(&alpha)
        .expect("alpha exists")
        .select_pane(2)
        .expect("select right pane");
    let right_of = store
        .resolve_unresolved_target(
            &UnresolvedTarget::new("alpha:0.{right-of}"),
            TargetFindType::Pane,
            TargetFindFlags::NONE,
            &TargetFindContext::from_target(Target::Pane(PaneTarget::with_window(
                alpha.clone(),
                0,
                2,
            ))),
        )
        .expect("right-of wraps to the left edge");
    assert_eq!(right_of, Target::Pane(PaneTarget::with_window(alpha, 0, 1)));
}

#[test]
fn pane_targets_accept_session_only_forms_without_current_context() {
    let store = populated_store();

    let target = store
        .resolve_unresolved_target(
            &UnresolvedTarget::new("alpha"),
            TargetFindType::Pane,
            TargetFindFlags::NONE,
            &TargetFindContext::new(None),
        )
        .expect("session-only pane target resolves");

    assert_eq!(
        target,
        Target::Pane(PaneTarget::with_window(session_name("alpha"), 2, 1))
    );
}

#[test]
fn pane_targets_accept_window_only_forms_without_current_context() {
    let store = populated_store();

    let target = store
        .resolve_unresolved_target(
            &UnresolvedTarget::new("editor"),
            TargetFindType::Pane,
            TargetFindFlags::NONE,
            &TargetFindContext::new(None),
        )
        .expect("window-only pane target resolves");

    assert_eq!(
        target,
        Target::Pane(PaneTarget::with_window(session_name("alpha"), 1, 0))
    );
}

#[test]
fn session_targets_treat_dot_names_as_session_names() {
    let mut store = populated_store();
    store
        .create_session(
            session_name("bad_name"),
            TerminalSize { cols: 80, rows: 24 },
        )
        .expect("session create succeeds");

    let target = store
        .resolve_unresolved_target(
            &UnresolvedTarget::new("bad.name"),
            TargetFindType::Session,
            TargetFindFlags::NONE,
            &TargetFindContext::new(None),
        )
        .expect("session target resolves");

    assert_eq!(target, Target::Session(session_name("bad_name")));
}

#[test]
fn recognizes_gated_mouse_and_marked_whole_target_forms() {
    let store = populated_store();

    let mouse = resolve(&store, "=", TargetFindType::Pane).expect_err("mouse is deferred");
    assert!(mouse
        .to_string()
        .contains("target form {mouse} is recognized"));

    let marked = resolve(&store, "~", TargetFindType::Pane).expect_err("marked is unavailable");
    assert!(marked.to_string().contains("can't find pane: {marked}"));
}

#[test]
fn resolves_mouse_targets_when_context_carries_mouse_state() {
    let store = populated_store();
    let context = TargetFindContext::new(None).with_mouse_target(Some(Target::Window(
        WindowTarget::with_window(session_name("alpha"), 1),
    )));

    let pane = store
        .resolve_unresolved_target(
            &UnresolvedTarget::new("="),
            TargetFindType::Pane,
            TargetFindFlags::NONE,
            &context,
        )
        .expect("mouse pane resolves");
    let window = store
        .resolve_unresolved_target(
            &UnresolvedTarget::new("{mouse}"),
            TargetFindType::Window,
            TargetFindFlags::NONE,
            &context,
        )
        .expect("mouse window resolves");

    assert_eq!(
        pane,
        Target::Pane(PaneTarget::with_window(session_name("alpha"), 1, 0))
    );
    assert_eq!(
        window,
        Target::Window(WindowTarget::with_window(session_name("alpha"), 1))
    );
}

#[test]
fn resolves_marked_targets_when_context_carries_marked_state() {
    let store = populated_store();
    let context = TargetFindContext::new(None).with_marked_target(Some(Target::Pane(
        PaneTarget::with_window(session_name("alpha"), 1, 0),
    )));

    let pane = store
        .resolve_unresolved_target(
            &UnresolvedTarget::new("~"),
            TargetFindType::Pane,
            TargetFindFlags::NONE,
            &context,
        )
        .expect("marked pane resolves");
    let window = store
        .resolve_unresolved_target(
            &UnresolvedTarget::new("{marked}"),
            TargetFindType::Window,
            TargetFindFlags::NONE,
            &context,
        )
        .expect("marked window resolves");

    assert_eq!(
        pane,
        Target::Pane(PaneTarget::with_window(session_name("alpha"), 1, 0))
    );
    assert_eq!(
        window,
        Target::Window(WindowTarget::with_window(session_name("alpha"), 1))
    );
}

#[test]
fn command_metadata_captures_find_type_and_flags() {
    let break_pane = command_target_metadata("break-pane").expect("metadata exists");
    assert_eq!(
        break_pane.target.expect("target spec").find_type,
        TargetFindType::Window
    );
    assert!(break_pane
        .target
        .expect("target spec")
        .flags
        .contains(TargetFindFlags::WINDOW_INDEX));

    let swap_window = command_target_metadata("swap-window").expect("metadata exists");
    assert!(swap_window
        .source
        .expect("source spec")
        .flags
        .contains(TargetFindFlags::DEFAULT_MARKED));

    let link_window = command_target_metadata("link-window").expect("metadata exists");
    assert_eq!(
        link_window.source.expect("source spec").find_type,
        TargetFindType::Window
    );
    assert!(link_window
        .target
        .expect("target spec")
        .flags
        .contains(TargetFindFlags::WINDOW_INDEX));

    let show_environment = command_target_metadata("show-environment").expect("metadata exists");
    assert!(show_environment
        .target
        .expect("target spec")
        .flags
        .contains(TargetFindFlags::CANFAIL));

    let attach_session = command_target_metadata("attach-session").expect("metadata exists");
    assert!(attach_session
        .target
        .expect("target spec")
        .flags
        .contains(TargetFindFlags::PREFER_UNATTACHED));

    let switch_client = command_target_metadata("switch-client").expect("metadata exists");
    let switch_client_target = switch_client.target.expect("target spec");
    assert_eq!(switch_client_target.find_type, TargetFindType::Window);
    assert_eq!(switch_client_target.flags, TargetFindFlags::NONE);

    let respawn_window = command_target_metadata("respawn-window").expect("metadata exists");
    assert_eq!(
        respawn_window.target.expect("target spec").find_type,
        TargetFindType::Window
    );

    let set_option = command_target_metadata("set-option").expect("metadata exists");
    let set_option_target = set_option.target.expect("target spec");
    assert_eq!(set_option_target.find_type, TargetFindType::Session);
    assert!(set_option_target.flags.contains(TargetFindFlags::CANFAIL));

    let set_window_option = command_target_metadata("set-window-option").expect("metadata exists");
    let set_window_option_target = set_window_option.target.expect("target spec");
    assert_eq!(set_window_option_target.find_type, TargetFindType::Window);
    assert!(set_window_option_target
        .flags
        .contains(TargetFindFlags::CANFAIL));

    for command in ["resize-window", "unlink-window"] {
        let target = command_target_metadata(command)
            .unwrap_or_else(|| panic!("{command} metadata exists"))
            .target
            .expect("target spec");
        assert_eq!(target.find_type, TargetFindType::Window);
        assert_eq!(target.flags, TargetFindFlags::NONE);
    }

    let show_window_options =
        command_target_metadata("show-window-options").expect("metadata exists");
    let show_window_options_target = show_window_options.target.expect("target spec");
    assert_eq!(show_window_options_target.find_type, TargetFindType::Window);
    assert!(show_window_options_target
        .flags
        .contains(TargetFindFlags::CANFAIL));

    let send_prefix = command_target_metadata("send-prefix").expect("metadata exists");
    let send_prefix_target = send_prefix.target.expect("target spec");
    assert_eq!(send_prefix_target.find_type, TargetFindType::Pane);
    assert_eq!(send_prefix_target.flags, TargetFindFlags::NONE);
}
