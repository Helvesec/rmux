use std::path::Path;

use rmux_proto::{SessionName, TerminalSize};

use super::{
    DeferredInitialPaneInputDrain, DeferredInitialPaneSpawn, HandlerState, InitialPaneSpawnOptions,
};

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

fn prepare_deferred_session(
    state: &mut HandlerState,
    session_name: &SessionName,
) -> DeferredInitialPaneSpawn {
    state
        .sessions
        .create_session(session_name.clone(), TerminalSize { cols: 80, rows: 24 })
        .expect("session creation succeeds");
    state
        .prepare_deferred_initial_session_terminal(
            session_name,
            InitialPaneSpawnOptions {
                socket_path: Path::new(r"\\.\pipe\rmux-deferred-identity-test"),
                spawn_environment: None,
                raw_spawn_environment: None,
                environment_overrides: None,
                command: None,
                pane_alert_callback: None,
                pane_exit_callback: None,
            },
        )
        .expect("deferred initial pane preparation succeeds")
}

#[test]
fn deferred_input_finisher_follows_runtime_session_rename() {
    let mut state = HandlerState::default();
    let original = session_name("deferred-original");
    let renamed = session_name("deferred-renamed");
    let job = prepare_deferred_session(&mut state, &original);

    state
        .rename_session(&original, &renamed)
        .expect("session rename succeeds");

    let drain = state
        .take_deferred_initial_pane_input_or_finish(&original, job.identity)
        .expect("finisher resolves the renamed runtime");
    assert!(matches!(
        drain,
        DeferredInitialPaneInputDrain::Finished {
            runtime_session_name
        } if runtime_session_name == renamed
    ));
    assert!(
        !state.active_pane_is_starting(&renamed),
        "the renamed pane must leave Starting state"
    );
    state.shutdown_terminals_for_test();
}

#[test]
fn deferred_identity_ignores_reused_name_and_stale_generation() {
    let mut state = HandlerState::default();
    let original = session_name("deferred-reused");
    let renamed = session_name("deferred-owner");
    let original_job = prepare_deferred_session(&mut state, &original);
    state
        .rename_session(&original, &renamed)
        .expect("session rename succeeds");
    let replacement_job = prepare_deferred_session(&mut state, &original);

    let resolved = state
        .starting_runtime_session_for_identity(&original, original_job.identity)
        .expect("old pane identity remains resolvable");
    assert_eq!(
        resolved, renamed,
        "a reused name must not capture the old job"
    );

    let drain = state
        .take_deferred_initial_pane_input_or_finish(&original, original_job.identity)
        .expect("old finisher resolves by stable identity");
    assert!(matches!(
        drain,
        DeferredInitialPaneInputDrain::Finished {
            runtime_session_name
        } if runtime_session_name == renamed
    ));
    assert!(
        state.active_pane_is_starting(&original),
        "finishing the old job must preserve the replacement pane"
    );

    let replacement_runtime = replacement_job.runtime_session_name.clone();
    let replacement_pane_id = replacement_job.identity.pane_id();
    let replacement = state
        .starting_panes
        .get_mut(&replacement_runtime)
        .and_then(|panes| panes.get_mut(&replacement_pane_id))
        .expect("replacement starting pane exists");
    replacement.generation = replacement.generation.saturating_add(1);

    state.finish_deferred_initial_pane_input_after_error(
        &replacement_runtime,
        replacement_job.identity,
    );
    assert!(
        state.active_pane_is_starting(&original),
        "a stale generation must not remove a newer pane incarnation"
    );
    state.shutdown_terminals_for_test();
}
