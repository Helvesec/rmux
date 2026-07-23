use std::time::Instant;

use super::*;
use crate::handler::attach_support::ActiveAttachIdentity;

async fn current_identity(handler: &RequestHandler, attach_pid: u32) -> ActiveAttachIdentity {
    let active_attach = handler.active_attach.lock().await;
    active_attach
        .by_pid
        .get(&attach_pid)
        .expect("attached client remains registered")
        .identity(attach_pid)
}

async fn arm_prefix_timer(
    handler: &RequestHandler,
    identity: ActiveAttachIdentity,
    session: &SessionName,
    key_table_set_at: Instant,
) -> u64 {
    set_key_table_for_timer(handler, identity, session, "prefix", key_table_set_at).await
}

async fn set_key_table_for_timer(
    handler: &RequestHandler,
    identity: ActiveAttachIdentity,
    session: &SessionName,
    table_name: &str,
    key_table_set_at: Instant,
) -> u64 {
    handler
        .set_attached_key_table_for_client_session_identity(
            identity,
            session,
            identity.session_id(),
            Some(table_name.to_owned()),
            Some(key_table_set_at),
        )
        .await
        .expect("key table is armed for the exact attach identity");
    let active_attach = handler.active_attach.lock().await;
    active_attach
        .by_pid
        .get(&identity.attach_pid())
        .filter(|active| identity.matches(identity.attach_pid(), session, active))
        .expect("key table timer identity remains current")
        .key_table_generation
}

async fn arm_repeat_timer(
    handler: &RequestHandler,
    identity: ActiveAttachIdentity,
    session: &SessionName,
    repeat_deadline: Instant,
) -> u64 {
    arm_repeat_timer_in_table(handler, identity, session, "prefix", repeat_deadline).await
}

async fn arm_repeat_timer_in_table(
    handler: &RequestHandler,
    identity: ActiveAttachIdentity,
    session: &SessionName,
    table_name: &str,
    repeat_deadline: Instant,
) -> u64 {
    let key_table_generation =
        set_key_table_for_timer(handler, identity, session, table_name, repeat_deadline).await;
    let mut active_attach = handler.active_attach.lock().await;
    let active = active_attach
        .by_pid
        .get_mut(&identity.attach_pid())
        .filter(|active| identity.matches(identity.attach_pid(), session, active))
        .expect("repeat timer identity remains current");
    active.repeat_active = true;
    active.repeat_deadline = Some(repeat_deadline);
    key_table_generation
}

async fn prefix_references(handler: &RequestHandler) -> usize {
    table_references(handler, "prefix")
        .await
        .expect("default prefix table exists")
}

async fn table_references(handler: &RequestHandler, table_name: &str) -> Option<usize> {
    handler
        .state
        .lock()
        .await
        .key_bindings
        .table(table_name)
        .map(|table| table.references())
}

#[tokio::test]
async fn normal_shutdown_cancels_pending_prefix_and_repeat_timers_without_mutation() {
    let handler = RequestHandler::new();
    let session = session_name("key-timer-pending-shutdown");
    let prefix_pid = u32::MAX - 801;
    let repeat_pid = u32::MAX - 802;
    let _prefix_rx = create_attached_session(&handler, prefix_pid, &session).await;
    let (repeat_tx, _repeat_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(repeat_pid, session.clone(), repeat_tx)
        .await;
    let prefix_identity = current_identity(&handler, prefix_pid).await;
    let repeat_identity = current_identity(&handler, repeat_pid).await;
    let prefix_set_at = Instant::now();
    let repeat_deadline = prefix_set_at + Duration::from_secs(60 * 60);
    let prefix_generation =
        arm_prefix_timer(&handler, prefix_identity, &session, prefix_set_at).await;
    let repeat_generation =
        arm_repeat_timer(&handler, repeat_identity, &session, repeat_deadline).await;
    assert_eq!(prefix_references(&handler).await, 2);

    let prefix_timer = handler
        .schedule_attached_prefix_timeout_for_test(
            prefix_identity,
            prefix_set_at,
            prefix_generation,
            Duration::from_secs(60 * 60),
        )
        .expect("normal lane admits pending prefix timer");
    let repeat_timer = handler
        .schedule_attached_repeat_timeout_for_test(
            repeat_identity,
            repeat_deadline,
            repeat_generation,
            Duration::from_secs(60 * 60),
        )
        .expect("normal lane admits pending repeat timer");

    tokio::time::timeout(
        ATTACH_LIFECYCLE_TIMEOUT,
        handler.close_normal_and_drain_lifecycle_producers(),
    )
    .await
    .expect("normal shutdown cancels pending key table timers");
    prefix_timer.await.expect("prefix timer task joins");
    repeat_timer.await.expect("repeat timer task joins");

    let active_attach = handler.active_attach.lock().await;
    let prefix = active_attach
        .by_pid
        .get(&prefix_pid)
        .expect("prefix attach remains registered");
    assert_eq!(prefix.key_table_name.as_deref(), Some("prefix"));
    assert_eq!(prefix.key_table_set_at, Some(prefix_set_at));
    let repeat = active_attach
        .by_pid
        .get(&repeat_pid)
        .expect("repeat attach remains registered");
    assert_eq!(repeat.key_table_name.as_deref(), Some("prefix"));
    assert!(repeat.repeat_active);
    assert_eq!(repeat.repeat_deadline, Some(repeat_deadline));
    drop(active_attach);
    assert_eq!(prefix_references(&handler).await, 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_waits_for_local_timer_mutation_but_not_blocked_refresh() {
    let handler = RequestHandler::new();
    let session = session_name("key-timer-mutation-boundary");
    let attach_pid = u32::MAX - 803;
    let _control_rx = create_attached_session(&handler, attach_pid, &session).await;
    let identity = current_identity(&handler, attach_pid).await;
    let key_table_set_at = Instant::now();
    let key_table_generation =
        arm_prefix_timer(&handler, identity, &session, key_table_set_at).await;
    assert_eq!(prefix_references(&handler).await, 1);

    let mutation_pause = handler.install_attached_key_table_timer_mutation_pause();
    let refresh_pause = handler.install_attached_key_table_timer_refresh_pause();
    let timer = handler
        .schedule_attached_prefix_timeout_for_test(
            identity,
            key_table_set_at,
            key_table_generation,
            Duration::ZERO,
        )
        .expect("normal lane admits expiring prefix timer");
    tokio::time::timeout(ATTACH_LIFECYCLE_TIMEOUT, mutation_pause.reached.notified())
        .await
        .expect("timer reaches its local mutation");

    let close_handler = handler.clone();
    let close = tokio::spawn(async move {
        close_handler
            .close_normal_and_drain_lifecycle_producers()
            .await;
    });
    handler
        .wait_until_normal_lifecycle_producers_closing_for_test()
        .await;
    assert!(
        !close.is_finished(),
        "normal shutdown waits for the admitted local mutation"
    );

    mutation_pause.release();
    tokio::time::timeout(ATTACH_LIFECYCLE_TIMEOUT, refresh_pause.reached.notified())
        .await
        .expect("timer reaches the cancellable refresh boundary");
    tokio::time::timeout(ATTACH_LIFECYCLE_TIMEOUT, close)
        .await
        .expect("normal shutdown does not wait for blocked refresh")
        .expect("normal shutdown task joins");
    timer.await.expect("expired timer task joins");

    let active_attach = handler.active_attach.lock().await;
    let active = active_attach
        .by_pid
        .get(&attach_pid)
        .expect("attach remains registered");
    assert_eq!(active.key_table_name, None);
    assert_eq!(active.key_table_set_at, None);
    drop(active_attach);
    assert_eq!(prefix_references(&handler).await, 0);
}

#[tokio::test]
async fn stale_repeat_timer_cannot_touch_same_pid_replacement_generation() {
    let handler = RequestHandler::new();
    let session = session_name("key-timer-replaced-generation");
    let attach_pid = u32::MAX - 804;
    let _original_rx = create_attached_session(&handler, attach_pid, &session).await;
    let stale_identity = current_identity(&handler, attach_pid).await;
    let repeat_deadline = Instant::now() + Duration::from_secs(30);
    let stale_generation =
        arm_repeat_timer(&handler, stale_identity, &session, repeat_deadline).await;
    let expiry_pause = handler.install_attached_key_table_timer_expiry_pause();
    let stale_timer = handler
        .schedule_attached_repeat_timeout_for_test(
            stale_identity,
            repeat_deadline,
            stale_generation,
            Duration::ZERO,
        )
        .expect("normal lane admits stale repeat timer probe");
    tokio::time::timeout(ATTACH_LIFECYCLE_TIMEOUT, expiry_pause.reached.notified())
        .await
        .expect("old generation timer reaches its expiry boundary");

    let (replacement_tx, _replacement_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(attach_pid, session.clone(), replacement_tx)
        .await;
    let replacement_identity = current_identity(&handler, attach_pid).await;
    assert_ne!(stale_identity, replacement_identity);
    let _replacement_generation =
        arm_repeat_timer(&handler, replacement_identity, &session, repeat_deadline).await;
    assert_eq!(prefix_references(&handler).await, 1);

    expiry_pause.release.notify_one();
    tokio::time::timeout(ATTACH_LIFECYCLE_TIMEOUT, stale_timer)
        .await
        .expect("stale timer finishes promptly")
        .expect("stale timer task joins");

    let active_attach = handler.active_attach.lock().await;
    let replacement = active_attach
        .by_pid
        .get(&attach_pid)
        .expect("replacement remains registered");
    assert_eq!(replacement.identity(attach_pid), replacement_identity);
    assert_eq!(replacement.key_table_name.as_deref(), Some("prefix"));
    assert_eq!(replacement.key_table_set_at, Some(repeat_deadline));
    assert!(replacement.repeat_active);
    assert_eq!(replacement.repeat_deadline, Some(repeat_deadline));
    drop(active_attach);
    assert_eq!(prefix_references(&handler).await, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn concurrent_key_table_transitions_commit_refs_in_state_lock_order() {
    let handler = RequestHandler::new();
    let session = session_name("key-table-transition-order");
    let attach_pid = u32::MAX - 805;
    let _control_rx = create_attached_session(&handler, attach_pid, &session).await;
    let identity = current_identity(&handler, attach_pid).await;
    let initial_generation =
        set_key_table_for_timer(&handler, identity, &session, "old-table", Instant::now()).await;

    let transition_pause = handler.install_attached_key_table_transition_pause();
    let first_handler = handler.clone();
    let first_session = session.clone();
    let first = tokio::spawn(async move {
        first_handler
            .set_attached_key_table_for_client_session_identity(
                identity,
                &first_session,
                identity.session_id(),
                Some("foo".to_owned()),
                None,
            )
            .await
    });
    tokio::time::timeout(
        ATTACH_LIFECYCLE_TIMEOUT,
        transition_pause.reached.notified(),
    )
    .await
    .expect("first transition reaches its atomic commit interposition");

    let second_handler = handler.clone();
    let second_session = session.clone();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let second = tokio::spawn(async move {
        let _ = started_tx.send(());
        second_handler
            .set_attached_key_table_for_client_session_identity(
                identity,
                &second_session,
                identity.session_id(),
                Some("bar".to_owned()),
                None,
            )
            .await
    });
    started_rx
        .await
        .expect("second transition starts while the first holds both locks");
    tokio::task::yield_now().await;
    assert!(
        !second.is_finished(),
        "second transition must wait for the first state-to-active transaction"
    );

    transition_pause.release();
    tokio::time::timeout(ATTACH_LIFECYCLE_TIMEOUT, first)
        .await
        .expect("first transition completes after release")
        .expect("first transition task joins")
        .expect("first transition succeeds");
    tokio::time::timeout(ATTACH_LIFECYCLE_TIMEOUT, second)
        .await
        .expect("second transition completes after the first")
        .expect("second transition task joins")
        .expect("second transition succeeds");

    let state = handler.state.lock().await;
    let active_attach = handler.active_attach.lock().await;
    let active = active_attach
        .by_pid
        .get(&attach_pid)
        .expect("attach remains registered after both transitions");
    assert_eq!(active.key_table_name.as_deref(), Some("bar"));
    assert_eq!(
        active.key_table_generation,
        initial_generation.wrapping_add(2)
    );
    assert!(state.key_bindings.table("old-table").is_none());
    assert!(state.key_bindings.table("foo").is_none());
    assert_eq!(
        state
            .key_bindings
            .table("bar")
            .expect("final table remains referenced")
            .references(),
        1
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_dispatch_cannot_rearm_repeat_after_table_switch() {
    let handler = RequestHandler::new();
    let session = session_name("dispatch-repeat-generation-cas");
    let attach_pid = std::process::id();
    let _control_rx = create_attached_session(&handler, attach_pid, &session).await;
    let identity = current_identity(&handler, attach_pid).await;
    let response = handler
        .handle(Request::BindKey(Box::new(rmux_proto::BindKeyRequest {
            table_name: "old-repeat".to_owned(),
            key: "r".to_owned(),
            note: Some("dispatch generation CAS regression".to_owned()),
            repeat: true,
            command: Some(vec![
                "set-buffer".to_owned(),
                "-b".to_owned(),
                "dispatch-generation-cas".to_owned(),
                "hit".to_owned(),
            ]),
        })))
        .await;
    assert!(matches!(response, Response::BindKey(_)), "{response:?}");
    let lookup_generation =
        set_key_table_for_timer(&handler, identity, &session, "old-repeat", Instant::now()).await;

    let dispatch_pause = handler.install_attached_key_dispatch_commit_pause(attach_pid);
    let dispatch_handler = handler.clone();
    let dispatch = tokio::spawn(async move {
        dispatch_handler
            .handle_attached_live_input_for_test(attach_pid, b"r")
            .await
    });
    tokio::time::timeout(ATTACH_LIFECYCLE_TIMEOUT, dispatch_pause.reached.notified())
        .await
        .expect("dispatch pauses after resolving the old-table binding");

    let response = handler
        .handle(Request::SwitchClientExt(
            rmux_proto::SwitchClientExtRequest {
                target: None,
                key_table: Some("root".to_owned()),
            },
        ))
        .await;
    assert!(
        matches!(response, Response::SwitchClient(_)),
        "{response:?}"
    );
    let switched_generation = {
        let active_attach = handler.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get(&attach_pid)
            .expect("attach remains registered after switch-client");
        assert_eq!(active.key_table_name.as_deref(), Some("root"));
        assert!(!active.repeat_active);
        assert_eq!(active.repeat_deadline, None);
        active.key_table_generation
    };
    assert_ne!(switched_generation, lookup_generation);
    assert_eq!(table_references(&handler, "old-repeat").await, Some(0));
    assert_eq!(table_references(&handler, "root").await, Some(1));

    dispatch_pause.release.notify_one();
    tokio::time::timeout(ATTACH_LIFECYCLE_TIMEOUT, dispatch)
        .await
        .expect("stale dispatch completes after release")
        .expect("stale dispatch task joins")
        .expect("stale dispatch succeeds");

    let active_attach = handler.active_attach.lock().await;
    let active = active_attach
        .by_pid
        .get(&attach_pid)
        .expect("attach remains registered after stale dispatch");
    assert_eq!(active.key_table_name.as_deref(), Some("root"));
    assert_eq!(active.key_table_generation, switched_generation);
    assert!(!active.repeat_active);
    assert_eq!(active.repeat_deadline, None);
    drop(active_attach);
    let response = handler
        .handle(Request::ShowBuffer(rmux_proto::ShowBufferRequest {
            name: Some("dispatch-generation-cas".to_owned()),
        }))
        .await;
    let output = response
        .command_output()
        .expect("the stale dispatch still executes its resolved command");
    assert_eq!(output.stdout(), b"hit");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_key_table_transition_stress_keeps_refs_balanced() {
    const TRANSITIONS: u64 = 64;

    let handler = RequestHandler::new();
    let session = session_name("key-table-transition-stress");
    let attach_pid = u32::MAX - 806;
    let _control_rx = create_attached_session(&handler, attach_pid, &session).await;
    let identity = current_identity(&handler, attach_pid).await;
    let initial_generation = {
        let active_attach = handler.active_attach.lock().await;
        active_attach
            .by_pid
            .get(&attach_pid)
            .expect("attach is registered before stress")
            .key_table_generation
    };

    let mut transitions = Vec::new();
    for index in 0..TRANSITIONS {
        let transition_handler = handler.clone();
        let transition_session = session.clone();
        transitions.push(tokio::spawn(async move {
            let table_name = if index % 2 == 0 {
                "stress-a"
            } else {
                "stress-b"
            };
            transition_handler
                .set_attached_key_table_for_client_session_identity(
                    identity,
                    &transition_session,
                    identity.session_id(),
                    Some(table_name.to_owned()),
                    None,
                )
                .await
        }));
    }
    for transition in transitions {
        tokio::time::timeout(ATTACH_LIFECYCLE_TIMEOUT, transition)
            .await
            .expect("stress transition completes")
            .expect("stress transition task joins")
            .expect("stress transition succeeds");
    }

    let state = handler.state.lock().await;
    let active_attach = handler.active_attach.lock().await;
    let active = active_attach
        .by_pid
        .get(&attach_pid)
        .expect("attach remains registered after stress");
    assert_eq!(
        active.key_table_generation,
        initial_generation.wrapping_add(TRANSITIONS)
    );
    let active_table = active
        .key_table_name
        .as_deref()
        .expect("one stress table remains active");
    let inactive_table = if active_table == "stress-a" {
        "stress-b"
    } else {
        assert_eq!(active_table, "stress-b");
        "stress-a"
    };
    assert_eq!(
        state
            .key_bindings
            .table(active_table)
            .expect("active stress table remains allocated")
            .references(),
        1
    );
    assert!(state.key_bindings.table(inactive_table).is_none());
}
