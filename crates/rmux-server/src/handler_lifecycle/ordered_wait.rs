use std::sync::Arc;
use std::time::Duration;

use tokio::sync::oneshot;
use tracing::warn;

use super::LifecycleDispatchItem;
use crate::handler::lifecycle_dispatch_queue::BoundedDispatchQueue;

const ORDERED_LIFECYCLE_CALLER_WAIT: Duration = Duration::from_millis(500);

pub(super) async fn dispatch_without_unbounded_caller_wait(
    dispatch: Arc<BoundedDispatchQueue<LifecycleDispatchItem>>,
    item: LifecycleDispatchItem,
    completion: oneshot::Receiver<()>,
) {
    let deadline = tokio::time::Instant::now() + ORDERED_LIFECYCLE_CALLER_WAIT;
    match tokio::time::timeout_at(deadline, dispatch.send_if_active(item)).await {
        Ok(Ok(true)) => {
            // The queue owns the accepted event, so dropping this receiver at
            // the caller deadline cannot cancel the hook. It only stops the
            // latency-sensitive caller from waiting for completion.
            let _ = tokio::time::timeout_at(deadline, completion).await;
        }
        Ok(Ok(false)) => {}
        Ok(Err(_)) => warn!("lifecycle dispatch queue closed before ordered hook completed"),
        Err(_) => {
            warn!("lifecycle dispatch queue remained saturated; dropping ordered hook at admission")
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use rmux_core::LifecycleEvent;
    use rmux_proto::{
        HookLifecycle, HookName, NewSessionRequest, Request, Response, ScopeSelector, SessionName,
        SetHookMutationRequest, TerminalSize,
    };
    use tokio::sync::oneshot;

    use super::super::prepare_lifecycle_event;
    use super::super::LifecycleDispatchItem;
    use crate::handler::lifecycle_dispatch_queue::BoundedDispatchQueue;
    use crate::handler::RequestHandler;

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
        assert!(matches!(response, Response::NewSession(_)), "{response:?}");
        session
    }

    async fn set_focus_hook(handler: &RequestHandler, command: &str) {
        let response = handler
            .handle(Request::SetHookMutation(SetHookMutationRequest {
                scope: ScopeSelector::Global,
                hook: HookName::ClientFocusIn,
                command: Some(command.to_owned()),
                lifecycle: HookLifecycle::Persistent,
                append: false,
                unset: false,
                run_immediately: false,
                index: None,
            }))
            .await;
        assert!(matches!(response, Response::SetHook(_)), "{response:?}");
    }

    async fn prepared_focus_event(
        handler: &RequestHandler,
        session_name: SessionName,
    ) -> super::super::QueuedLifecycleEvent {
        let mut state = handler.state.lock().await;
        prepare_lifecycle_event(
            &mut state,
            &LifecycleEvent::ClientFocusIn {
                session_name,
                client_name: Some("ordered-lifecycle-test".to_owned()),
            },
        )
    }

    async fn spawn_lifecycle_consumer(
        handler: &RequestHandler,
    ) -> (oneshot::Sender<()>, tokio::task::JoinHandle<()>) {
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

    async fn stop_lifecycle_consumer(
        handler: &RequestHandler,
        shutdown: oneshot::Sender<()>,
        consumer: tokio::task::JoinHandle<()>,
    ) {
        let _ = shutdown.send(());
        handler.shutdown_wait_for();
        tokio::time::timeout(Duration::from_secs(2), consumer)
            .await
            .expect("lifecycle consumer stops after draining shutdown")
            .expect("lifecycle consumer task joins");
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

    #[tokio::test]
    async fn ordered_lifecycle_wait_observes_a_fast_mutating_hook_before_returning() {
        let handler = RequestHandler::new();
        let session = create_session(&handler, "ordered-fast-hook").await;
        set_focus_hook(&handler, "set-buffer -b ordered-lifecycle-fast completed").await;
        let (shutdown, consumer) = spawn_lifecycle_consumer(&handler).await;
        let event = prepared_focus_event(&handler, session).await;

        handler.emit_prepared_and_wait(event).await;

        let state = handler.state.lock().await;
        let (_, content) = state
            .buffers
            .show(Some("ordered-lifecycle-fast"))
            .expect("fast hook mutation is visible before the caller resumes");
        assert_eq!(content, b"completed");
        drop(state);
        stop_lifecycle_consumer(&handler, shutdown, consumer).await;
    }

    #[tokio::test]
    async fn ordered_lifecycle_wait_releases_the_caller_but_drains_the_blocked_hook() {
        let handler = RequestHandler::new();
        let session = create_session(&handler, "ordered-blocked-hook").await;
        set_focus_hook(&handler, "wait-for ordered-lifecycle-block").await;
        let (shutdown, consumer) = spawn_lifecycle_consumer(&handler).await;
        let event = prepared_focus_event(&handler, session).await;

        tokio::time::timeout(
            Duration::from_secs(2),
            handler.emit_prepared_and_wait(event),
        )
        .await
        .expect("a blocked hook must not keep the latency-sensitive caller indefinitely");
        wait_for_hook_block(&handler, "ordered-lifecycle-block").await;

        stop_lifecycle_consumer(&handler, shutdown, consumer).await;
        assert_eq!(
            handler.wait_for_counts("ordered-lifecycle-block"),
            (0, 0, false),
            "shutdown drains the queued hook and its completion"
        );
    }

    #[tokio::test]
    async fn saturated_ordered_dispatch_drops_at_admission_without_a_detached_sender() {
        let handler = RequestHandler::new();
        let session = create_session(&handler, "ordered-saturated-hook").await;
        let dispatch = Arc::new(BoundedDispatchQueue::new(1));
        let mut receiver = dispatch.activate().expect("test owns queue receiver");

        dispatch
            .send_if_active(LifecycleDispatchItem {
                event: prepared_focus_event(&handler, session.clone()).await,
                completion: None,
            })
            .await
            .expect("fill bounded dispatch queue");

        let (completion_tx, completion_rx) = oneshot::channel();
        let started = tokio::time::Instant::now();
        super::dispatch_without_unbounded_caller_wait(
            Arc::clone(&dispatch),
            LifecycleDispatchItem {
                event: prepared_focus_event(&handler, session).await,
                completion: Some(completion_tx),
            },
            completion_rx,
        )
        .await;

        let elapsed = started.elapsed();
        assert!(
            elapsed >= super::ORDERED_LIFECYCLE_CALLER_WAIT && elapsed < Duration::from_secs(2),
            "queue admission must respect the bounded caller budget: {elapsed:?}"
        );
        assert!(receiver.recv().await.is_some(), "filler remains queued");
        assert!(
            tokio::time::timeout(Duration::from_millis(1), receiver.recv())
                .await
                .is_err(),
            "timed-out admission must not survive in a detached sender task"
        );
    }
}
