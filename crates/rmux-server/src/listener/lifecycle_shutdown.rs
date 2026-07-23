use std::future::Future;

use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tracing::warn;

use crate::handler::RequestHandler;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LifecycleHookShutdownOutcome {
    Drained,
    Forced,
}

pub(super) async fn drain_lifecycle_hooks_before_deadline(
    handler: &RequestHandler,
    hook_shutdown: oneshot::Sender<()>,
    hook_task: &mut JoinHandle<()>,
    deadline: Instant,
) -> LifecycleHookShutdownOutcome {
    let normal_drains = async {
        // Ordinary delayed producers close with normal request admission. Lifecycle-hook
        // descendants retain their own lane until the hook consumer has drained.
        handler.close_normal_and_drain_lifecycle_producers().await;
        handler
            .close_normal_and_drain_post_commit_operations()
            .await;
        handler.close_and_wait_for_lifecycle_publications().await;
    };

    if complete_before_deadline(deadline, normal_drains)
        .await
        .is_some()
    {
        let _ = hook_shutdown.send(());
        if let Some(result) = complete_before_deadline(deadline, &mut *hook_task).await {
            if let Err(error) = result {
                warn!("lifecycle hook task failed: {error}");
            }
            return LifecycleHookShutdownOutcome::Drained;
        }
        warn!("aborting lifecycle hooks that did not drain before the shutdown deadline");
    } else {
        warn!("lifecycle publication did not drain before the hook shutdown deadline");
    }

    // Deactivation prevents later publications from entering the queue. Aborting and awaiting
    // the sole consumer then drops its receiver, which wakes senders already blocked in
    // `reserve()` on a full outbox.
    handler.deactivate_lifecycle_dispatch_for_shutdown();
    hook_task.abort();
    match (&mut *hook_task).await {
        Err(error) if error.is_cancelled() => {}
        Err(error) => warn!("lifecycle hook task failed after abort: {error}"),
        Ok(()) => {}
    }

    // These drains are deliberately unbounded: this fallback force-cancels only the outbox
    // receiver cycle. Once receiver drop has released blocked sender reservations, repeating the
    // idempotent barriers must still preserve every mutation that was already admitted.
    handler.close_normal_and_drain_lifecycle_producers().await;
    handler
        .close_normal_and_drain_post_commit_operations()
        .await;
    handler.close_and_wait_for_lifecycle_publications().await;

    LifecycleHookShutdownOutcome::Forced
}

async fn complete_before_deadline<T>(
    deadline: Instant,
    future: impl Future<Output = T>,
) -> Option<T> {
    tokio::pin!(future);
    let deadline_reached = tokio::time::sleep_until(deadline);
    tokio::pin!(deadline_reached);
    tokio::select! {
        biased;
        output = &mut future => Some(output),
        _ = &mut deadline_reached => None,
    }
}

#[cfg(test)]
#[path = "lifecycle_shutdown/tests.rs"]
mod tests;
