use std::path::Path;

use rmux_proto::{SessionName, TerminalSize};

use super::{
    DeferredInitialPaneInput, DeferredInitialPaneInputDrain, DeferredInitialPaneSpawn,
    HandlerState, InitialPaneSpawnOptions, PasteDelimiters,
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
fn observed_exit_metadata_is_not_masked_by_pending_windows_spawn() {
    let mut state = HandlerState::default();
    let session = session_name("observed-pending-spawn");
    let job = prepare_deferred_session(&mut state, &session);
    let pane_id = job.identity.pane_id();
    let target = state
        .pane_target_for_runtime_pane(&job.runtime_session_name, pane_id)
        .expect("pending pane has a target");

    assert_eq!(
        state
            .observe_runtime_pane_exit(&job.runtime_session_name, pane_id)
            .expect("pending pane can be observed"),
        None,
        "a pending spawn alone is not an observed exit"
    );
    state
        .mark_pane_dead_without_exit_details(&target)
        .expect("exit observation can precede spawn completion");
    assert!(state.active_pane_is_starting(&session));
    assert_eq!(
        state
            .observe_runtime_pane_exit(&job.runtime_session_name, pane_id)
            .expect("recorded exit remains observable"),
        Some(super::super::PaneExitMetadata::without_exit_details()),
        "pending Windows spawn must not mask already recorded exit metadata"
    );
    state.shutdown_terminals_for_test();
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
        &rmux_proto::RmuxError::Server("stale flush".to_owned()),
        super::DeferredInputTally::default(),
    );
    assert!(
        state.active_pane_is_starting(&original),
        "a stale generation must not remove a newer pane incarnation"
    );
    state.shutdown_terminals_for_test();
}

#[test]
fn starting_pane_keeps_bracketed_paste_distinct_from_plain_bytes() {
    let mut state = HandlerState::default();
    let session = session_name("deferred-bracketed-paste");
    let job = prepare_deferred_session(&mut state, &session);
    let payload = b"\x1b[200~alpha\nbeta\x1b[201~";

    assert!(state
        .queue_starting_pane_paste_input(&session, 0, 0, payload, PasteDelimiters::Wrapped)
        .expect("bracketed paste queues while the pane starts"));

    let starting = state
        .starting_panes
        .get(&job.runtime_session_name)
        .and_then(|panes| panes.get(&job.identity.pane_id()))
        .expect("starting pane remains registered");
    assert_eq!(
        starting.queued_input.front(),
        Some(&DeferredInitialPaneInput::Paste {
            bytes: payload.to_vec(),
            delimiters: PasteDelimiters::Wrapped,
        })
    );
    assert_eq!(starting.queued_input_bytes, payload.len());
    state.shutdown_terminals_for_test();
}

/// A body queued for a destination that never announced `?2004h` is still a
/// paste, so it must reach the flush as one. Queuing it as plain bytes is what
/// sent it through the raw ConPTY pipe, where a legacy input state machine
/// parses and consumes the control sequences inside the pasted content.
#[test]
fn a_bare_paste_body_is_queued_as_a_paste_and_keeps_its_disposition() {
    let mut state = HandlerState::default();
    let session = session_name("deferred-bare-paste");
    let job = prepare_deferred_session(&mut state, &session);
    let payload = "alpha\r\n\u{1b}[9;2u omega".as_bytes();

    assert!(state
        .queue_starting_pane_paste_input(&session, 0, 0, payload, PasteDelimiters::Bare)
        .expect("a bare paste body queues while the pane starts"));

    let starting = state
        .starting_panes
        .get(&job.runtime_session_name)
        .and_then(|panes| panes.get(&job.identity.pane_id()))
        .expect("starting pane remains registered");
    assert_eq!(
        starting.queued_input.front(),
        Some(&DeferredInitialPaneInput::Paste {
            bytes: payload.to_vec(),
            delimiters: PasteDelimiters::Bare,
        }),
        "a bare body must not be demoted to plain queued bytes"
    );
    assert_eq!(starting.queued_input_bytes, payload.len());
    state.shutdown_terminals_for_test();
}

/// The typed bracketed-paste entry must be accounted for exactly like plain
/// bytes, otherwise the starting-pane queue silently stops enforcing the
/// 64-KiB bound that protects a pane that never finishes starting.
#[test]
fn queued_bracketed_paste_is_charged_against_the_starting_input_limit() {
    let mut state = HandlerState::default();
    let session = session_name("deferred-bracketed-capacity");
    let job = prepare_deferred_session(&mut state, &session);
    let limit = super::STARTING_PANE_INPUT_MAX_BYTES;
    let chunk = vec![b'p'; 4096];

    let mut queued = 0;
    while queued + chunk.len() <= limit {
        assert!(
            state
                .queue_starting_pane_paste_input(&session, 0, 0, &chunk, PasteDelimiters::Wrapped)
                .expect("a bracketed paste below the limit queues"),
            "the pane is still starting, so the paste must be queued"
        );
        queued += chunk.len();
    }
    assert_eq!(
        queued, limit,
        "the chunk size must divide the limit exactly"
    );

    let overflow = state
        .queue_starting_pane_paste_input(&session, 0, 0, b"x", PasteDelimiters::Wrapped)
        .expect_err("one byte past the limit must be rejected");
    assert!(
        overflow.to_string().contains("input queue is full"),
        "unexpected overflow error: {overflow}"
    );

    let starting = state
        .starting_panes
        .get(&job.runtime_session_name)
        .and_then(|panes| panes.get(&job.identity.pane_id()))
        .expect("starting pane remains registered");
    assert_eq!(
        starting.queued_input_bytes, limit,
        "every queued bracketed paste must contribute its own byte length"
    );
    assert_eq!(
        starting.queued_input.len(),
        limit / chunk.len(),
        "the rejected byte must not have been appended"
    );
    state.shutdown_terminals_for_test();
}

fn last_message(state: &HandlerState) -> Option<&str> {
    state.message_log.back().map(|entry| entry.msg.as_str())
}

fn queue_two_inputs(state: &mut HandlerState, session: &SessionName) {
    assert!(state
        .queue_starting_pane_input(session, 0, 0, b"echo")
        .expect("plain input queues while the pane starts"));
    assert!(state
        .queue_starting_pane_paste_input(session, 0, 0, b"alpha", PasteDelimiters::Bare)
        .expect("a paste queues while the pane starts"));
}

/// A pane that stops before its deferred input is flushed must say what it
/// discarded: the caller of `send-keys` already received success.
#[test]
fn cancelling_a_starting_pane_reports_the_input_it_discards() {
    let mut state = HandlerState::default();
    let session = session_name("deferred-cancel-report");
    let job = prepare_deferred_session(&mut state, &session);
    queue_two_inputs(&mut state, &session);

    assert!(state.cancel_starting_pane(&job.runtime_session_name, job.identity.pane_id()));

    assert_eq!(
        last_message(&state),
        Some(
            format!(
                "deferred pane input was not delivered: the pane stopped starting; \
                 discarded 2 queued inputs (9 bytes) for pane %{}",
                job.identity.pane_id().as_u32()
            )
            .as_str()
        ),
        "the discarded input must be named, not dropped in silence"
    );
    state.shutdown_terminals_for_test();
}

/// kill-session destroys the runtime of a pane that is still starting. The
/// input queued for it must be named like any other stopped start: the
/// caller of `send-keys` already received success, and the deferred flush
/// later reports only the batch it holds.
#[test]
fn killing_a_session_reports_the_input_its_starting_pane_discards() {
    let mut state = HandlerState::default();
    let session = session_name("deferred-kill-session-report");
    let job = prepare_deferred_session(&mut state, &session);
    queue_two_inputs(&mut state, &session);
    let before = state.message_log.len();

    let current_runtime_owner = state.sessions.runtime_owner(&session);
    let next_runtime_owner = state.sessions.runtime_owner_transfer_target(&session);
    assert_eq!(current_runtime_owner.as_ref(), Some(&session));
    assert_eq!(next_runtime_owner, None);
    let _ = state
        .sessions
        .remove_session(&session)
        .expect("the killed session exists");
    state
        .remove_session_terminals(
            &session,
            current_runtime_owner.as_ref(),
            next_runtime_owner.as_ref(),
        )
        .expect("kill-session removes the session runtime");

    assert!(!state.active_pane_is_starting(&session));
    assert!(
        state
            .starting_runtime_session_for_identity(&job.runtime_session_name, job.identity)
            .is_none(),
        "the starting pane must leave with its session"
    );
    assert_eq!(
        state.message_log.len(),
        before + 1,
        "exactly one entry names the discarded queue"
    );
    assert_eq!(
        last_message(&state),
        Some(
            format!(
                "deferred pane input was not delivered: the pane stopped starting; \
                 discarded 2 queued inputs (9 bytes) for pane %{}",
                job.identity.pane_id().as_u32()
            )
            .as_str()
        ),
        "kill-session must name the queue it discards"
    );
    state.shutdown_terminals_for_test();
}

/// Cancelling a pane with nothing queued is the ordinary path and stays silent.
#[test]
fn cancelling_a_starting_pane_without_queued_input_adds_no_message() {
    let mut state = HandlerState::default();
    let session = session_name("deferred-cancel-empty");
    let job = prepare_deferred_session(&mut state, &session);
    let before = state.message_log.len();

    assert!(state.cancel_starting_pane(&job.runtime_session_name, job.identity.pane_id()));

    assert_eq!(state.message_log.len(), before);
    state.shutdown_terminals_for_test();
}

/// The pane has no live terminal yet, so resolving its input sink fails. The
/// queue must survive that error so the error report can count it.
#[test]
fn a_failed_input_sink_lookup_keeps_the_queue_for_the_error_report() {
    let mut state = HandlerState::default();
    let session = session_name("deferred-sink-error");
    let job = prepare_deferred_session(&mut state, &session);
    queue_two_inputs(&mut state, &session);

    let Err(error) = state.take_deferred_initial_pane_input_or_finish(&session, job.identity)
    else {
        panic!("a pane without a live terminal has no input sink");
    };
    let starting = state
        .starting_panes
        .get(&job.runtime_session_name)
        .and_then(|panes| panes.get(&job.identity.pane_id()))
        .expect("the starting pane survives the failed lookup");
    assert_eq!(
        starting.queued_input.len(),
        2,
        "the error must not drain the queue"
    );
    assert_eq!(starting.queued_input_bytes, 9);

    state.finish_deferred_initial_pane_input_after_error(
        &session,
        job.identity,
        &error,
        super::DeferredInputTally::default(),
    );
    assert_eq!(
        last_message(&state),
        Some(
            format!(
                "{error}; discarded 2 queued inputs (9 bytes) for pane %{}",
                job.identity.pane_id().as_u32()
            )
            .as_str()
        )
    );
    assert!(!state.active_pane_is_starting(&session));
    state.shutdown_terminals_for_test();
}

/// A failed flush reports the part of its batch the pane may not have
/// received together with the input queued while the batch was in flight.
#[test]
fn a_failed_flush_reports_its_undelivered_batch_and_the_input_queued_since() {
    let mut state = HandlerState::default();
    let session = session_name("deferred-flush-error");
    let job = prepare_deferred_session(&mut state, &session);
    assert!(state
        .queue_starting_pane_input(&session, 0, 0, b"x")
        .expect("input queued during the flush"));
    let in_flight = [
        DeferredInitialPaneInput::Bytes(b"ab".to_vec()),
        DeferredInitialPaneInput::Bytes(b"cde".to_vec()),
    ];

    state.finish_deferred_initial_pane_input_after_error(
        &session,
        job.identity,
        &rmux_proto::RmuxError::Server("failed to flush deferred pane input: denied".to_owned()),
        super::DeferredInputTally::of(&in_flight),
    );

    let expected = format!(
        "{}; discarded 3 queued inputs (6 bytes) for pane %{}",
        rmux_proto::RmuxError::Server("failed to flush deferred pane input: denied".to_owned()),
        job.identity.pane_id().as_u32()
    );
    assert_eq!(last_message(&state), Some(expected.as_str()));
    assert!(!state.active_pane_is_starting(&session));
    state.shutdown_terminals_for_test();
}
