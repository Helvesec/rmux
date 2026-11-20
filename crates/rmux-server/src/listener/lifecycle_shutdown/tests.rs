use std::time::Duration;

use rmux_core::LifecycleEvent;
use rmux_proto::{
    HookLifecycle, HookName, NewSessionRequest, Request, Response, ScopeSelector, SessionName,
    SetHookMutationRequest, ShowBufferRequest, TerminalSize, WaitForMode, WaitForRequest,
};
use tokio::sync::{oneshot, watch};

use super::*;

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

async fn create_session(handler: &RequestHandler, name: &str) -> SessionName {
    let session = session_name(name);
    let response = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session.clone(),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))
        .await;
    crate::session_fixture::assert_new_session(&response);
    session
}

async fn set_focus_hook(
    handler: &RequestHandler,
    command: &str,
    lifecycle: HookLifecycle,
    append: bool,
) {
    let response = handler
        .handle(Request::SetHookMutation(SetHookMutationRequest {
            scope: ScopeSelector::Global,
            hook: HookName::ClientFocusIn,
            command: Some(command.to_owned()),
            lifecycle,
            append,
            unset: false,
            run_immediately: false,
            index: None,
        }))
        .await;
    assert!(matches!(response, Response::SetHook(_)), "{response:?}");
}

fn spawn_lifecycle_consumer(handler: &RequestHandler) -> (oneshot::Sender<()>, JoinHandle<()>) {
    let events = handler
        .take_lifecycle_dispatch_receiver()
        .expect("test activates the lifecycle dispatch receiver once");
    let (shutdown, shutdown_rx) = oneshot::channel();
    let consumer_handler = handler.clone();
    let consumer = tokio::spawn(async move {
        consumer_handler
            .consume_lifecycle_hooks(events, shutdown_rx)
            .await;
    });
    (shutdown, consumer)
}

async fn emit_focus(handler: &RequestHandler, session_name: &SessionName, client_name: &str) {
    handler
        .emit_lifecycle_event_for_test(LifecycleEvent::ClientFocusIn {
            session_name: session_name.clone(),
            client_name: Some(client_name.to_owned()),
        })
        .await;
}

async fn wait_for_hook_block(handler: &RequestHandler, channel: &str) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if handler.wait_for_counts(channel) == (1, 0, false) {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("hook reaches its deterministic wait-for seam");
}

async fn signal_wait_for(handler: &RequestHandler, channel: &str) {
    let response = handler
        .handle(Request::WaitFor(WaitForRequest {
            channel: channel.to_owned(),
            mode: WaitForMode::Signal,
        }))
        .await;
    assert!(matches!(response, Response::WaitFor(_)), "{response:?}");
}

async fn seal_final_lifecycle_boundaries(handler: &RequestHandler) {
    handler.close_and_drain_lifecycle_producers().await;
    handler.close_and_drain_post_commit_operations().await;
    handler.seal_and_wait_for_lifecycle_publications().await;
}

#[tokio::test]
async fn full_outbox_and_stuck_hook_are_forced_at_the_shared_deadline() {
    let handler = RequestHandler::with_lifecycle_dispatch_capacity_for_test(1);
    let session = create_session(&handler, "shutdown-full-outbox").await;
    set_focus_hook(
        &handler,
        "wait-for shutdown-full-outbox-hook",
        HookLifecycle::Persistent,
        false,
    )
    .await;
    let (hook_shutdown, mut hook_task) = spawn_lifecycle_consumer(&handler);

    emit_focus(&handler, &session, "accepted").await;
    wait_for_hook_block(&handler, "shutdown-full-outbox-hook").await;
    emit_focus(&handler, &session, "queued").await;

    let mut observed = handler.subscribe_lifecycle_events();
    let blocked_handler = handler.clone();
    let blocked_session = session.clone();
    let blocked_publication = tokio::spawn(async move {
        emit_focus(&blocked_handler, &blocked_session, "blocked").await;
    });
    tokio::time::timeout(Duration::from_secs(2), observed.recv())
        .await
        .expect("blocked publication reaches the saturated outbox")
        .expect("lifecycle broadcast remains active");
    tokio::task::yield_now().await;
    assert!(
        !blocked_publication.is_finished(),
        "the third publication must wait for outbox capacity"
    );
    let following_event = handler
        .prepare_lifecycle_event_for_test(LifecycleEvent::ClientFocusIn {
            session_name: session.clone(),
            client_name: Some("ordered-behind".to_owned()),
        })
        .await;
    let following_handler = handler.clone();
    let following_publication = tokio::spawn(async move {
        following_handler
            .emit_prepared_lifecycle_event_for_test(following_event)
            .await;
    });
    tokio::task::yield_now().await;
    assert!(
        !following_publication.is_finished(),
        "the synchronously reserved fourth ticket waits behind the blocked commit turn"
    );

    let (_progress, progress_rx) = watch::channel(());
    let deadline = Instant::now() + Duration::from_millis(50);
    let mut bound = HookDrainBound::new(Duration::from_millis(50), progress_rx, || false);
    let outcome = drain_lifecycle_hooks_while_progressing(
        &handler,
        hook_shutdown,
        &mut hook_task,
        &mut bound,
    )
    .await;

    assert_eq!(outcome, LifecycleHookShutdownOutcome::Forced);
    assert!(
        Instant::now() >= deadline,
        "fallback waits for its deadline"
    );
    blocked_publication
        .await
        .expect("blocked publication is released when the receiver drops");
    following_publication
        .await
        .expect("the following commit ticket advances after receiver drop");
    assert!(hook_task.is_finished());
    assert_eq!(
        handler.wait_for_counts("shutdown-full-outbox-hook"),
        (0, 0, false),
        "aborting the accepted hook removes its wait-for waiter"
    );

    // The forced path already repeated these barriers. A second repetition proves that the
    // shutdown recovery remains idempotent after the queue receiver is gone.
    handler.close_normal_and_drain_lifecycle_producers().await;
    handler
        .close_normal_and_drain_post_commit_operations()
        .await;
    handler.close_and_wait_for_lifecycle_publications().await;
    seal_final_lifecycle_boundaries(&handler).await;
}

#[tokio::test]
async fn normal_shutdown_drains_already_accepted_and_queued_hooks() {
    let handler = RequestHandler::with_lifecycle_dispatch_capacity_for_test(1);
    let session = create_session(&handler, "shutdown-normal-outbox").await;
    set_focus_hook(
        &handler,
        "wait-for shutdown-normal-outbox-hook",
        HookLifecycle::OneShot,
        false,
    )
    .await;
    set_focus_hook(
        &handler,
        "set-buffer -b shutdown-normal-outbox drained",
        HookLifecycle::Persistent,
        true,
    )
    .await;
    let (hook_shutdown, mut hook_task) = spawn_lifecycle_consumer(&handler);

    emit_focus(&handler, &session, "accepted").await;
    wait_for_hook_block(&handler, "shutdown-normal-outbox-hook").await;
    emit_focus(&handler, &session, "queued").await;
    signal_wait_for(&handler, "shutdown-normal-outbox-hook").await;

    let (_progress, progress_rx) = watch::channel(());
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut bound = HookDrainBound::new(Duration::from_secs(2), progress_rx, || false);
    let outcome = drain_lifecycle_hooks_while_progressing(
        &handler,
        hook_shutdown,
        &mut hook_task,
        &mut bound,
    )
    .await;

    assert_eq!(outcome, LifecycleHookShutdownOutcome::Drained);
    assert!(Instant::now() < deadline);
    assert!(hook_task.is_finished());
    let response = handler
        .handle(Request::ShowBuffer(ShowBufferRequest {
            name: Some("shutdown-normal-outbox".to_owned()),
        }))
        .await;
    assert_eq!(
        response
            .command_output()
            .expect("drained hook creates its buffer")
            .stdout(),
        b"drained"
    );
    seal_final_lifecycle_boundaries(&handler).await;
}

#[tokio::test]
async fn ready_drain_wins_when_it_ties_the_deadline() {
    let (_progress, progress_rx) = watch::channel(());
    let mut bound = HookDrainBound::new(Duration::ZERO, progress_rx, || true);
    assert_eq!(bound.complete(async { 7_u8 }).await, Some(7));
}

/// Emits one hook completion every `interval`, `count` times.
fn spawn_hook_completions(interval: Duration, count: u32) -> (watch::Receiver<()>, JoinHandle<()>) {
    let (progress, progress_rx) = watch::channel(());
    let completions = tokio::spawn(async move {
        for _ in 0..count {
            tokio::time::sleep(interval).await;
            progress.send_replace(());
        }
    });
    (progress_rx, completions)
}

#[tokio::test(start_paused = true)]
async fn a_drain_that_keeps_finishing_hooks_outlives_its_grace() {
    let grace = Duration::from_millis(100);
    let (progress_rx, completions) = spawn_hook_completions(grace / 2, 20);
    let mut shown = 0_u32;
    let mut bound = HookDrainBound::new(grace, progress_rx, || {
        shown += 1;
        true
    });

    let started = Instant::now();
    let drained = bound.complete(tokio::time::sleep(grace * 8)).await;

    assert_eq!(drained, Some(()), "visible progress keeps the drain open");
    assert!(started.elapsed() >= grace * 8);
    drop(bound);
    assert!(
        shown >= 10,
        "each completion is shown to the waiting client: {shown}"
    );
    completions.await.expect("completion producer");
}

#[tokio::test(start_paused = true)]
async fn progress_no_client_can_see_keeps_the_single_grace() {
    let grace = Duration::from_millis(100);
    let (progress_rx, completions) = spawn_hook_completions(grace / 2, 20);
    let mut bound = HookDrainBound::new(grace, progress_rx, || false);

    let started = Instant::now();
    let drained = bound.complete(tokio::time::sleep(grace * 8)).await;

    assert_eq!(drained, None, "an unseen drain cannot outlast the client");
    assert_eq!(started.elapsed(), grace);
    completions.abort();
}

#[tokio::test(start_paused = true)]
async fn a_stuck_hook_is_forced_one_grace_after_the_last_completion() {
    let grace = Duration::from_millis(100);
    let (progress_rx, completions) = spawn_hook_completions(grace / 2, 3);
    let mut bound = HookDrainBound::new(grace, progress_rx, || true);

    let started = Instant::now();
    let drained = bound.complete(std::future::pending::<()>()).await;

    assert_eq!(drained, None);
    let last_completion = grace / 2 * 3;
    assert!(
        (last_completion + grace..=last_completion + grace + grace / 10)
            .contains(&started.elapsed()),
        "forced {:?} after the start",
        started.elapsed()
    );
    completions.await.expect("completion producer");
}

#[tokio::test(start_paused = true)]
async fn a_stretch_in_which_the_server_could_not_run_is_not_hook_inactivity() {
    let grace = Duration::from_millis(100);
    let (_progress, progress_rx) = watch::channel(());
    let mut bound = HookDrainBound::new(grace, progress_rx, || true);

    let started = Instant::now();
    let drained = bound
        .complete(async {
            // The bound starts counting first. Then the whole server is stopped for a hundred
            // graces, and the hook ends half a grace after the server runs again.
            tokio::task::yield_now().await;
            tokio::time::advance(grace * 100).await;
            tokio::time::sleep(grace / 2).await;
        })
        .await;

    assert_eq!(
        drained,
        Some(()),
        "a frozen server did not observe its hooks idle"
    );
    assert!(started.elapsed() >= grace * 100);
}
