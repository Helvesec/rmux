use std::future::Future;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::{Instant, Interval, MissedTickBehavior};
use tracing::warn;

use crate::handler::{LifecycleDispatchItem, RequestHandler};
use crate::socket_cleanup::SocketCleanup;

/// How long the shutdown drain waits for the next accepted lifecycle hook to finish.
const LIFECYCLE_HOOK_SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// The sole lifecycle-hook consumer task, with the handles its shutdown drain needs.
pub(super) struct HookConsumer {
    task: JoinHandle<()>,
    shutdown: oneshot::Sender<()>,
    progress: watch::Receiver<()>,
}

impl HookConsumer {
    /// Spawns the consumer, which reports progress after each dispatched lifecycle event.
    pub(super) fn spawn(
        handler: &Arc<RequestHandler>,
        events: mpsc::Receiver<LifecycleDispatchItem>,
    ) -> Self {
        let (hook_shutdown, hook_shutdown_rx) = oneshot::channel();
        let (progress, progress_rx) = watch::channel(());
        let handler = Arc::clone(handler);
        let task = tokio::spawn(async move {
            handler
                .consume_lifecycle_hooks_reporting_progress(events, hook_shutdown_rx, &progress)
                .await;
        });
        Self {
            task,
            shutdown: hook_shutdown,
            progress: progress_rx,
        }
    }

    /// Drains the accepted hooks while they keep progressing, then forces what remains.
    pub(super) async fn drain(
        self,
        handler: &RequestHandler,
        show_progress: impl FnMut() -> bool,
    ) -> LifecycleHookShutdownOutcome {
        let Self {
            mut task,
            shutdown,
            progress,
        } = self;
        let mut bound = HookDrainBound::new(LIFECYCLE_HOOK_SHUTDOWN_GRACE, progress, show_progress);
        drain_lifecycle_hooks_while_progressing(handler, shutdown, &mut task, &mut bound).await
    }
}

/// Makes drain progress visible to a waiting `kill-server`, when this server still holds its
/// endpoint as a socket file it owns; otherwise reports that no client can see it.
#[cfg(unix)]
pub(super) fn kill_server_progress<'a>(
    socket_path: &'a Path,
    endpoint_held: bool,
    cleanup: &SocketCleanup,
) -> impl FnMut() -> bool + 'a {
    let owned = cleanup.socket_identity().filter(|_| endpoint_held);
    move || show_progress_on_socket_file(socket_path, owned)
}

/// Windows named pipes carry no timestamp a waiting client could observe.
#[cfg(not(unix))]
pub(super) fn kill_server_progress<'a>(
    _socket_path: &'a Path,
    _endpoint_held: bool,
    _cleanup: &SocketCleanup,
) -> impl FnMut() -> bool + 'a {
    || false
}

/// The visible grace is counted in this many ticks the server actually observes.
const GRACE_TICKS: u32 = 50;

/// Bounds the lifecycle-hook drain by the absence of progress rather than by its total length.
///
/// When `show_progress` can make progress visible to a waiting `kill-server`, every dispatched
/// lifecycle event restarts the grace, and the grace is counted in ticks the server observes:
/// a stretch in which the whole server could not run (stopped, starved) counts as one tick, not
/// as hook inactivity. Otherwise the client would give up on an endpoint the drain still holds,
/// so the drain keeps a single wall-clock grace. A hook that never ends is forced either way.
pub(super) struct HookDrainBound<S> {
    progress: watch::Receiver<()>,
    progress_open: bool,
    show_progress: S,
    pace: DrainPace,
}

enum DrainPace {
    /// No waiting client can see progress: one wall-clock grace for the whole drain.
    Fixed { deadline: Instant },
    /// Visible progress: the grace is `GRACE_TICKS` observed ticks without a dispatched event.
    Visible { ticks: Interval, idle_ticks: u32 },
}

impl DrainPace {
    fn new(grace: Duration, visible: bool) -> Self {
        let now = Instant::now();
        if !visible {
            return Self::Fixed {
                deadline: now + grace,
            };
        }
        let tick = (grace / GRACE_TICKS).max(Duration::from_millis(1));
        let mut ticks = tokio::time::interval_at(now + tick, tick);
        ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
        Self::Visible {
            ticks,
            idle_ticks: 0,
        }
    }

    async fn exhausted(&mut self) {
        match self {
            Self::Fixed { deadline } => tokio::time::sleep_until(*deadline).await,
            Self::Visible { ticks, idle_ticks } => {
                while *idle_ticks < GRACE_TICKS {
                    ticks.tick().await;
                    *idle_ticks += 1;
                }
            }
        }
    }

    fn restart(&mut self) {
        if let Self::Visible { idle_ticks, .. } = self {
            *idle_ticks = 0;
        }
    }
}

impl<S: FnMut() -> bool> HookDrainBound<S> {
    pub(super) fn new(
        grace: Duration,
        progress: watch::Receiver<()>,
        mut show_progress: S,
    ) -> Self {
        let visible = show_progress();
        Self {
            progress,
            progress_open: true,
            show_progress,
            pace: DrainPace::new(grace, visible),
        }
    }

    async fn complete<T>(&mut self, future: impl Future<Output = T>) -> Option<T> {
        tokio::pin!(future);
        loop {
            tokio::select! {
                biased;
                output = &mut future => return Some(output),
                changed = self.progress.changed(), if self.progress_open => {
                    self.observe_progress(changed.is_ok());
                }
                () = self.pace.exhausted() => return None,
            }
        }
    }

    /// A dispatched event restarts a visible grace; a closed progress channel is no longer polled.
    fn observe_progress(&mut self, changed: bool) {
        if !changed {
            self.progress_open = false;
        } else if matches!(self.pace, DrainPace::Visible { .. }) && (self.show_progress)() {
            self.pace.restart();
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LifecycleHookShutdownOutcome {
    Drained,
    Forced,
}

pub(super) async fn drain_lifecycle_hooks_while_progressing<S: FnMut() -> bool>(
    handler: &RequestHandler,
    hook_shutdown: oneshot::Sender<()>,
    hook_task: &mut JoinHandle<()>,
    bound: &mut HookDrainBound<S>,
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

    if bound.complete(normal_drains).await.is_some() {
        let _ = hook_shutdown.send(());
        if let Some(result) = bound.complete(&mut *hook_task).await {
            if let Err(error) = result {
                warn!("lifecycle hook task failed: {error}");
            }
            return LifecycleHookShutdownOutcome::Drained;
        }
        warn!("aborting lifecycle hooks that stopped progressing before the shutdown deadline");
    } else {
        warn!("lifecycle publication stopped progressing before the hook shutdown deadline");
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

/// Joins background work, drains the remaining producers, and reaps terminal owners.
pub(super) async fn drain_background_work_and_reap_terminals(handler: &RequestHandler) {
    // Keep shell registration open while already accepted lifecycle hooks drain. Once that
    // window closes, reject and join every background task before releasing the daemon process.
    let unfinished_background_tasks = handler
        .shutdown_background_tasks_and_shell_processes()
        .await;
    if !unfinished_background_tasks.is_empty() {
        warn!(
            tasks = ?unfinished_background_tasks,
            "background tasks did not join before the shutdown deadline"
        );
    }
    handler.close_and_drain_lifecycle_producers().await;
    handler.close_and_drain_post_commit_operations().await;
    handler.seal_and_wait_for_lifecycle_publications().await;

    // Every accepted producer has been drained, so the store now holds what the
    // session actually entered. Saving here, and only here, keeps push, clear,
    // set-option and handler drop free of file writes.
    handler.save_prompt_history_file().await;

    // Endpoint disappearance releases kill-server and replacement launchers.
    // Reap the terminal owners here, rather than in HandlerState's later Drop.
    // A failed reap must not skip the owner-only restore or endpoint release.
    if let Err(error) = handler.shutdown_terminal_processes().await {
        warn!("pane terminal processes did not shut down cleanly: {error}");
    }
}

/// Shows a waiting `kill-server` that the drain still advances, by touching the socket file the
/// stopping server still owns; the client extends its own wait when the file's mtime changes.
/// Returns false when no client can see the progress: no bound path, another inode at the path,
/// or a refused timestamp update.
#[cfg(unix)]
fn show_progress_on_socket_file(
    socket_path: &Path,
    owned: Option<crate::unix_socket::SocketFileIdentity>,
) -> bool {
    let Some(owned) = owned else {
        return false;
    };
    if crate::unix_socket::socket_file_identity(socket_path).ok() != Some(owned) {
        return false;
    }
    let now = rustix::fs::Timespec {
        tv_sec: 0,
        tv_nsec: rustix::fs::UTIME_NOW,
    };
    let omit = rustix::fs::Timespec {
        tv_sec: 0,
        tv_nsec: rustix::fs::UTIME_OMIT,
    };
    let times = rustix::fs::Timestamps {
        last_access: omit,
        last_modification: now,
    };
    rustix::fs::utimensat(
        rustix::fs::CWD,
        socket_path,
        &times,
        rustix::fs::AtFlags::empty(),
    )
    .is_ok()
}

#[cfg(test)]
#[path = "lifecycle_shutdown/tests.rs"]
mod tests;
