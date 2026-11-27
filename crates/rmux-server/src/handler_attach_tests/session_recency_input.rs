//! Accepted attached-client interaction is what advances session recency.
//!
//! The commit sits at the input admission boundary, before the server decides
//! whether the bytes become a prefix, a key binding, a mode key or a pane
//! write. Committing after pane I/O instead would drop every key the server
//! consumes itself, which is the defect these tests pin.
//!
//! Measured against tmux 3.7b on this host: a bare prefix key and a copy-mode
//! navigation key each advanced `session_activity`, while `send-keys` to a
//! detached session and `rename-session` did not.
//!
//! Every fixture pins the public whole seconds *after* the event under test,
//! because that is the condition targetless ranking has to survive: with the
//! seconds equal, only the internal recency order can still separate the
//! sessions. `used` is deliberately the alphabetically last name and `spare`
//! the first, so the legacy name tiebreak cannot produce the expected answer.
//!
//! Erasing the seconds is what those fixtures need and what they must not be
//! the only evidence of: admitting input also advances the *public*
//! `session_activity`, which is a real change to what `list-sessions` reports.
//! The last fixtures therefore pin that payload at the rendered boundary
//! instead of erasing it.

use super::*;
#[cfg(test)]
use crate::session_fixture::SessionSetupExt as _;
use rmux_core::{TargetFindContext, TargetFindFlags, TargetFindType, UnresolvedTarget};

/// One arbitrary but fixed second shared by every session in a fixture.
const PINNED_SECOND: i64 = 1_785_500_000;

/// A second far enough in the past that any clock reading is strictly later.
/// The public-boundary fixtures pin this and then assert movement, so they do
/// not depend on which second the test itself runs in.
const ANCIENT_SECOND: i64 = 1_000_000_000;

/// The session an attached client interacts with. Alphabetically last, so the
/// legacy `activity_at`/`created_at`/name ranking can never select it.
const USED: &str = "zulu";
/// A detached session created after `USED`, so it starts out the most recent.
const SPARE: &str = "alfa";
/// What `USED` is renamed to mid-registration. Still alphabetically after
/// `SPARE`, so the rename cannot hand the legacy name tiebreak the answer the
/// renamed session's own attach is supposed to earn.
const RENAMED_USED: &str = "renamed-used";

/// Collapses every public timestamp onto one second, leaving only the internal
/// recency order able to rank the sessions.
async fn pin_public_seconds(handler: &RequestHandler) {
    pin_public_seconds_to(handler, PINNED_SECOND).await;
}

async fn pin_public_seconds_to(handler: &RequestHandler, seconds: i64) {
    let mut state = handler.state.lock().await;
    let names = state
        .sessions
        .iter()
        .map(|session| (session.name(), session))
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();
    for name in names {
        state
            .sessions
            .session_setup(&name)
            .expect("listed session exists")
            .pin_public_times_for_tests(seconds);
    }
    assert!(
        state
            .sessions
            .iter().map(|session| (session.name(), session))
            .all(|(_, session)| session.created_at() == seconds
                && session.activity_at() == seconds),
        "the fixture must leave no public second able to order the sessions"
    );
}

/// Resolves a targetless session through the core reader, which is the reader a
/// detached scripting client reaches.
async fn default_session(handler: &RequestHandler) -> SessionName {
    let state = handler.state.lock().await;
    let target = state
        .sessions
        .resolve_unresolved_target(
            &UnresolvedTarget::none(),
            TargetFindType::Session,
            TargetFindFlags::NONE,
            &TargetFindContext::new(None),
        )
        .expect("default session resolves");
    let Target::Session(name) = target else {
        panic!("default session target did not resolve to a session: {target:?}");
    };
    name
}

/// Attaches a client to `USED`, then creates `SPARE` afterwards so that `SPARE`
/// — not the attached session — is the session a targetless command selects.
/// Any later win by `USED` therefore has to come from the interaction itself.
async fn interaction_fixture(
    attach_pid: u32,
) -> (RequestHandler, mpsc::UnboundedReceiver<AttachControl>) {
    let handler = RequestHandler::new();
    let control_rx = create_quiet_attached_session(&handler, attach_pid, &session_name(USED)).await;
    create_quiet_session(&handler, &session_name(SPARE)).await;
    pin_public_seconds(&handler).await;
    assert_eq!(
        default_session(&handler).await,
        session_name(SPARE),
        "the fixture must start with the untouched session winning"
    );
    (handler, control_rx)
}

#[tokio::test]
async fn an_accepted_prefix_key_advances_targetless_session_recency() {
    let attach_pid = std::process::id();
    let (handler, _control_rx) = interaction_fixture(attach_pid).await;

    // A bare prefix is consumed by the server and never reaches a pane, so a
    // post-pane-write commit cannot see it at all.
    handler
        .handle_attached_live_input_for_test(attach_pid, b"\x02")
        .await
        .expect("prefix key input succeeds");
    pin_public_seconds(&handler).await;

    assert_eq!(default_session(&handler).await, session_name(USED));
}

#[tokio::test]
async fn an_accepted_key_binding_advances_targetless_session_recency() {
    let attach_pid = std::process::id();
    let (handler, _control_rx) = interaction_fixture(attach_pid).await;

    // `prefix c` is dispatched locally as new-window; no byte reaches a pane.
    handler
        .handle_attached_live_input_for_test(attach_pid, b"\x02c")
        .await
        .expect("prefix c input succeeds");
    pin_public_seconds(&handler).await;

    assert_eq!(default_session(&handler).await, session_name(USED));
}

#[tokio::test]
async fn a_locally_consumed_mode_key_advances_targetless_session_recency() {
    let attach_pid = std::process::id();
    let handler = RequestHandler::new();
    let _control_rx =
        create_quiet_attached_session(&handler, attach_pid, &session_name(USED)).await;
    handler
        .handle_attached_live_input_for_test(attach_pid, b"\x02[")
        .await
        .expect("prefix [ enters copy mode");
    assert!(
        pane_mode_status(&handler, &session_name(USED))
            .await
            .contains("copy-mode"),
        "the fixture must actually be in a mode for its keys to be consumed locally"
    );

    // Only now is the rival created, so copy mode itself cannot be the cause.
    create_quiet_session(&handler, &session_name(SPARE)).await;
    pin_public_seconds(&handler).await;
    assert_eq!(default_session(&handler).await, session_name(SPARE));

    handler
        .handle_attached_live_input_for_test(attach_pid, b"\x1b[A")
        .await
        .expect("copy-mode cursor key succeeds");
    assert!(
        pane_mode_status(&handler, &session_name(USED))
            .await
            .contains("copy-mode"),
        "the cursor key must have been consumed by the mode, not sent to the pane"
    );
    pin_public_seconds(&handler).await;

    assert_eq!(default_session(&handler).await, session_name(USED));
}

#[tokio::test]
async fn successful_pane_input_advances_targetless_session_recency() {
    let attach_pid = std::process::id();
    let (handler, _control_rx) = interaction_fixture(attach_pid).await;

    handler
        .handle_attached_live_input_for_test(attach_pid, b"x")
        .await
        .expect("plain pane input succeeds");
    pin_public_seconds(&handler).await;

    assert_eq!(default_session(&handler).await, session_name(USED));
}

#[tokio::test]
async fn synchronized_pane_input_advances_the_session_at_the_admission_boundary() {
    let attach_pid = std::process::id();
    let (handler, _control_rx) = interaction_fixture(attach_pid).await;
    split_used_window(&handler).await;
    set_synchronize_panes(&handler).await;
    // Splitting is window bookkeeping rather than client interaction, so the
    // rival must still be winning when the one synchronized input arrives.
    pin_public_seconds(&handler).await;
    assert_eq!(default_session(&handler).await, session_name(SPARE));

    handler
        .handle_attached_live_input_for_test(attach_pid, b"y")
        .await
        .expect("synchronized pane input succeeds");
    pin_public_seconds(&handler).await;

    // The commit happens once, before the input fans out to either pane.
    assert_eq!(default_session(&handler).await, session_name(USED));
}

#[tokio::test]
async fn switching_a_client_advances_the_target_session_recency() {
    let attach_pid = std::process::id();
    let handler = RequestHandler::new();
    create_quiet_session(&handler, &session_name(USED)).await;
    create_quiet_session(&handler, &session_name(SPARE)).await;
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(attach_pid, session_name(SPARE), control_tx)
        .await;
    pin_public_seconds(&handler).await;
    assert_eq!(default_session(&handler).await, session_name(SPARE));

    let switched = handler
        .handle(Request::SwitchClient(SwitchClientRequest {
            target: session_name(USED),
        }))
        .await;
    assert!(
        matches!(switched, Response::SwitchClient(_)),
        "{switched:?}"
    );
    pin_public_seconds(&handler).await;

    // tmux 3.7b updates session activity when a client switches into a
    // session, exactly as it does on attach.
    assert_eq!(default_session(&handler).await, session_name(USED));
}

#[tokio::test]
async fn empty_attached_input_does_not_advance_targetless_session_recency() {
    let attach_pid = std::process::id();
    let (handler, _control_rx) = interaction_fixture(attach_pid).await;

    handler
        .handle_attached_live_input_for_test(attach_pid, b"")
        .await
        .expect("empty attached input succeeds");
    pin_public_seconds(&handler).await;

    assert_eq!(default_session(&handler).await, session_name(SPARE));
}

/// Attaches a read-only client to `USED`, then creates `SPARE` afterwards, so
/// the fixture starts out exactly like [`interaction_fixture`].
async fn read_only_fixture(
    attach_pid: u32,
) -> (RequestHandler, mpsc::UnboundedReceiver<AttachControl>) {
    let handler = RequestHandler::new();
    create_quiet_session(&handler, &session_name(USED)).await;
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach_with_closing(
            attach_pid,
            session_name(USED),
            control_tx,
            Arc::new(AtomicBool::new(false)),
            OuterTerminalContext::default(),
            crate::client_flags::ClientFlags::READONLY,
        )
        .await;
    create_quiet_session(&handler, &session_name(SPARE)).await;
    (handler, control_rx)
}

#[tokio::test]
async fn read_only_client_input_does_not_advance_targetless_session_recency() {
    let attach_pid = std::process::id();
    let (handler, _control_rx) = read_only_fixture(attach_pid).await;
    pin_public_seconds(&handler).await;
    assert_eq!(default_session(&handler).await, session_name(SPARE));

    let _ = handler
        .handle_attached_live_input_for_test(attach_pid, b"x")
        .await;
    pin_public_seconds(&handler).await;

    // Read-only clients already do not advance client activity; the session
    // commit reuses that policy rather than inventing a second one.
    assert_eq!(default_session(&handler).await, session_name(SPARE));
}

#[tokio::test]
async fn input_from_a_vanished_attach_does_not_advance_targetless_session_recency() {
    let attach_pid = std::process::id();
    let (handler, _control_rx) = interaction_fixture(attach_pid).await;

    let error = handler
        .handle_attached_live_input_for_test(attach_pid.wrapping_add(1), b"x")
        .await
        .expect_err("input for an unregistered attach must fail");
    assert_eq!(error.kind(), std::io::ErrorKind::Other, "{error:?}");
    pin_public_seconds(&handler).await;

    assert_eq!(default_session(&handler).await, session_name(SPARE));
}

#[tokio::test]
async fn input_must_not_advance_a_same_name_session_recreated_under_the_client() {
    let attach_pid = std::process::id();
    let (handler, _control_rx) = interaction_fixture(attach_pid).await;
    let original_id = session_id(&handler, USED).await;

    // Destroy and recreate the attached name. The attach record still carries
    // the destroyed session's id, so its input belongs to a lifetime that no
    // longer exists and must not be credited to the new one.
    {
        let mut state = handler.state.lock().await;
        let _ = state
            .sessions
            .remove_session(&session_name(USED))
            .expect("session removal succeeds");
        state
            .sessions
            .create_session(session_name(USED), TerminalSize { cols: 80, rows: 24 })
            .expect("replacement session creation succeeds");
    }
    assert_ne!(
        session_id(&handler, USED).await,
        original_id,
        "the recreated session must be a new identity"
    );
    pin_public_seconds(&handler).await;
    assert_eq!(default_session(&handler).await, session_name(USED));

    // Make the rival the most recent again, then replay the stale client.
    touch_spare(&handler).await;
    pin_public_seconds(&handler).await;
    assert_eq!(default_session(&handler).await, session_name(SPARE));

    let _ = handler
        .handle_attached_live_input_for_test(attach_pid, b"x")
        .await;
    pin_public_seconds(&handler).await;

    assert_eq!(default_session(&handler).await, session_name(SPARE));
}

#[tokio::test]
async fn a_live_attach_registration_credits_the_session_it_attached_to() {
    let attach_pid = std::process::id();
    let handler = RequestHandler::new();
    create_quiet_session(&handler, &session_name(USED)).await;
    create_quiet_session(&handler, &session_name(SPARE)).await;
    pin_public_seconds(&handler).await;
    assert_eq!(
        default_session(&handler).await,
        session_name(SPARE),
        "the fixture must start with the session nobody attached to winning"
    );

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(attach_pid, session_name(USED), control_tx)
        .await;
    pin_public_seconds(&handler).await;

    // Attaching is use. The identity guard the two fixtures below exercise has
    // to reject only a registration whose session lifetime or client is gone —
    // never the ordinary one — and nothing else pins that half: `USED` is both
    // the older session and the alphabetically last name, so only the attach
    // itself can make it win.
    assert_eq!(default_session(&handler).await, session_name(USED));
}

#[tokio::test]
async fn attach_registration_must_not_credit_a_same_name_replacement_session() {
    let attach_pid = std::process::id();
    let handler = Arc::new(RequestHandler::new());
    create_quiet_session(&handler, &session_name(USED)).await;
    create_quiet_session(&handler, &session_name(SPARE)).await;
    let original_id = session_id(&handler, USED).await;

    // Registration publishes the attach, releases the state lock, and only then
    // credits the session. Park it inside exactly that window.
    let pause = handler.install_attach_registration_activity_pause();
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let registering_handler = Arc::clone(&handler);
    let registering = tokio::spawn(async move {
        registering_handler
            .register_attach(attach_pid, session_name(USED), control_tx)
            .await
    });
    tokio::time::timeout(ATTACH_LIFECYCLE_TIMEOUT, pause.reached.notified())
        .await
        .expect("attach registration reaches its activity commit");

    // Destroy the lifetime the registration captured and reuse its name, then
    // make the rival the most recent session again.
    {
        let mut state = handler.state.lock().await;
        let _ = state
            .sessions
            .remove_session(&session_name(USED))
            .expect("session removal succeeds");
        state
            .sessions
            .create_session(session_name(USED), TerminalSize { cols: 80, rows: 24 })
            .expect("replacement session creation succeeds");
    }
    assert_ne!(
        session_id(&handler, USED).await,
        original_id,
        "the replacement session must be a new identity"
    );
    touch_spare(&handler).await;
    pin_public_seconds(&handler).await;
    assert_eq!(default_session(&handler).await, session_name(SPARE));

    pause.release.notify_one();
    registering.await.expect("attach registration task joins");
    pin_public_seconds(&handler).await;

    // Nobody ever attached to the replacement, so it must not inherit the
    // attach of the destroyed session that merely shared its name.
    assert_eq!(default_session(&handler).await, session_name(SPARE));
}

#[tokio::test]
async fn attach_registration_must_not_credit_a_client_that_finished_first() {
    let attach_pid = std::process::id();
    let handler = Arc::new(RequestHandler::new());
    create_quiet_session(&handler, &session_name(USED)).await;
    create_quiet_session(&handler, &session_name(SPARE)).await;

    let pause = handler.install_attach_registration_activity_pause();
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let registering_handler = Arc::clone(&handler);
    let registering = tokio::spawn(async move {
        registering_handler
            .register_attach(attach_pid, session_name(USED), control_tx)
            .await
    });
    tokio::time::timeout(ATTACH_LIFECYCLE_TIMEOUT, pause.reached.notified())
        .await
        .expect("attach registration reaches its activity commit");

    // The attach is published, so the client can already leave through the
    // ordinary teardown before registration reaches its own commit.
    let attach_id = handler
        .active_attach
        .lock()
        .await
        .by_pid
        .get(&attach_pid)
        .expect("the registration published its attach")
        .id;
    handler.finish_attach(attach_pid, attach_id).await;
    touch_spare(&handler).await;
    pin_public_seconds(&handler).await;
    assert_eq!(default_session(&handler).await, session_name(SPARE));

    pause.release.notify_one();
    registering.await.expect("attach registration task joins");
    pin_public_seconds(&handler).await;

    // The attach this registration was crediting no longer exists.
    assert_eq!(default_session(&handler).await, session_name(SPARE));
}

#[tokio::test]
async fn attach_registration_credit_follows_a_rename_of_the_same_session_lifetime() {
    let attach_pid = std::process::id();
    let handler = Arc::new(RequestHandler::new());
    create_quiet_session(&handler, &session_name(USED)).await;
    create_quiet_session(&handler, &session_name(SPARE)).await;
    let attached_id = session_id(&handler, USED).await;

    let pause = handler.install_attach_registration_activity_pause();
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let registering_handler = Arc::clone(&handler);
    let registering = tokio::spawn(async move {
        registering_handler
            .register_attach(attach_pid, session_name(USED), control_tx)
            .await
    });
    tokio::time::timeout(ATTACH_LIFECYCLE_TIMEOUT, pause.reached.notified())
        .await
        .expect("attach registration reaches its activity commit");

    // Rename the exact session this registration published its attach to. The
    // lifetime and the attach both survive; only the key it is stored under
    // moves, which is the one thing the captured name can no longer follow.
    let renamed = session_name(RENAMED_USED);
    let response = handler
        .handle(Request::RenameSession(RenameSessionRequest {
            target: session_name(USED),
            new_name: renamed.clone(),
        }))
        .await;
    assert!(
        matches!(response, Response::RenameSession(_)),
        "{response:?}"
    );
    assert_eq!(
        session_id(&handler, RENAMED_USED).await,
        attached_id,
        "renaming must preserve the attached session's identity"
    );
    touch_spare(&handler).await;
    pin_public_seconds(&handler).await;
    assert_eq!(default_session(&handler).await, session_name(SPARE));

    pause.release.notify_one();
    registering.await.expect("attach registration task joins");
    pin_public_seconds(&handler).await;

    // A client did attach to this session, and renaming it is not detaching.
    assert_eq!(default_session(&handler).await, renamed);
}

#[tokio::test]
async fn detaching_does_not_advance_targetless_session_recency() {
    let attach_pid = std::process::id();
    let handler = RequestHandler::new();
    create_quiet_session(&handler, &session_name(USED)).await;
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let attach_id = handler
        .register_attach(attach_pid, session_name(USED), control_tx)
        .await;
    create_quiet_session(&handler, &session_name(SPARE)).await;
    pin_public_seconds(&handler).await;
    assert_eq!(default_session(&handler).await, session_name(SPARE));

    handler.finish_attach(attach_pid, attach_id).await;
    pin_public_seconds(&handler).await;

    // Leaving a session is not using it.
    assert_eq!(default_session(&handler).await, session_name(SPARE));
}

#[tokio::test]
async fn explicit_send_keys_to_a_detached_session_does_not_advance_its_recency() {
    let handler = RequestHandler::new();
    create_quiet_session(&handler, &session_name(USED)).await;
    create_quiet_session(&handler, &session_name(SPARE)).await;
    pin_public_seconds(&handler).await;
    assert_eq!(default_session(&handler).await, session_name(SPARE));

    let response = handler
        .handle(Request::SendKeys(SendKeysRequest {
            target: PaneTarget::new(session_name(USED), 0),
            keys: vec!["x".to_owned()],
        }))
        .await;
    assert!(matches!(response, Response::SendKeys(_)), "{response:?}");
    pin_public_seconds(&handler).await;

    // tmux 3.7b measured: `send-keys` to a detached session left its
    // `session_activity` unchanged. Command input is not client interaction.
    assert_eq!(default_session(&handler).await, session_name(SPARE));
}

#[tokio::test]
async fn accepted_input_advances_the_rendered_session_activity() {
    let attach_pid = std::process::id();
    let (handler, _control_rx) = interaction_fixture(attach_pid).await;
    pin_public_seconds_to(&handler, ANCIENT_SECOND).await;

    handler
        .handle_attached_live_input_for_test(attach_pid, b"x")
        .await
        .expect("plain pane input succeeds");

    // tmux 3.7b measured (`m39-oracle/probe3-out.txt`, probes 12,
    // 13 and 16): a key from an attached client advances `session_activity`.
    // The recency token is internal, but this second is not — it is the
    // payload `list-sessions` and `#{session_activity}` publish, so admitting
    // input is a deliberate, tmux-backed change to that output.
    assert!(
        rendered_session_activity(&handler, USED).await > ANCIENT_SECOND,
        "an accepted interaction must advance the rendered #{{session_activity}}"
    );
    assert_eq!(
        rendered_session_activity(&handler, SPARE).await,
        ANCIENT_SECOND,
        "only the session the client interacted with may advance"
    );
}

#[tokio::test]
async fn read_only_input_leaves_the_rendered_session_activity_alone() {
    let attach_pid = std::process::id();
    let (handler, _control_rx) = read_only_fixture(attach_pid).await;
    pin_public_seconds_to(&handler, ANCIENT_SECOND).await;

    let _ = handler
        .handle_attached_live_input_for_test(attach_pid, b"x")
        .await;

    // tmux does advance activity for a read-only client's key; RMUX reuses its
    // existing client-activity predicate instead. That divergence is visible
    // here, at the same public boundary, rather than only in the ranking.
    assert_eq!(
        rendered_session_activity(&handler, USED).await,
        ANCIENT_SECOND,
        "rejected input must not reach the published activity second"
    );
}

/// Reads one session's `#{session_activity}` back out of the `list-sessions`
/// payload the CLI receives, rather than out of the in-process accessor.
async fn rendered_session_activity(handler: &RequestHandler, name: &str) -> i64 {
    let response = handler
        .handle(Request::ListSessions(rmux_proto::ListSessionsRequest {
            format: Some("#{session_name}=#{session_activity}".to_owned()),
            filter: None,
            sort_order: None,
            reversed: false,
        }))
        .await;
    let Response::ListSessions(listed) = response else {
        panic!("expected list-sessions response, got {response:?}");
    };
    let rendered =
        String::from_utf8(listed.output.stdout().to_vec()).expect("list-sessions stdout is utf-8");
    let prefix = format!("{name}=");
    rendered
        .lines()
        .find_map(|line| line.strip_prefix(prefix.as_str()))
        .unwrap_or_else(|| panic!("{name} is missing from list-sessions output {rendered:?}"))
        .parse()
        .expect("#{session_activity} renders whole seconds")
}

async fn session_id(handler: &RequestHandler, name: &str) -> rmux_proto::SessionId {
    handler
        .state
        .lock()
        .await
        .sessions
        .session(&session_name(name))
        .expect("session exists")
        .id()
}

/// Marks `SPARE` as used through the ordinary attach path.
async fn touch_spare(handler: &RequestHandler) {
    handler
        .state
        .lock()
        .await
        .sessions
        .session_setup(&session_name(SPARE))
        .expect("spare session exists")
        .touch_attached();
}

async fn split_used_window(handler: &RequestHandler) {
    let response = handler
        .handle(Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Session(session_name(USED)),
            direction: rmux_proto::SplitDirection::Vertical,
            before: false,
            environment: None,
        }))
        .await;
    assert!(matches!(response, Response::SplitWindow(_)), "{response:?}");
}

async fn set_synchronize_panes(handler: &RequestHandler) {
    let response = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Window(WindowTarget::with_window(session_name(USED), 0)),
            option: OptionName::SynchronizePanes,
            value: "on".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(response, Response::SetOption(_)), "{response:?}");
}
