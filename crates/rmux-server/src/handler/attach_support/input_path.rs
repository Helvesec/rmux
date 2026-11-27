//! Whether work runs on an attached client's input path, and releasing that path from a binding.
//!
//! The attached input path awaits a binding's commands to keep their order, so that client's next
//! input frames - including the keys and mouse reports meant for an overlay it shows - wait
//! behind the binding. A popup that its caller waits for needs those frames to ever close. When
//! one is shown on a client, the client's input path stops awaiting its bindings and lets them
//! finish in the background; the caller keeps waiting for the popup and the binding's own
//! commands still run in order. As tmux decides where a key goes when it arrives - an overlay or
//! a prompt takes it at once, anything else waits behind the running command - input that
//! arrives while such a binding runs and nothing takes the client's input is queued as it came,
//! then replayed in order once the binding has finished; a popup shown in between never receives
//! it. Focus reports go on at once, as tmux applies them. Work that runs inline on the input path
//! cannot be released that way, so a popup shown from there must not wait for its own client.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::sync::{Arc, Mutex as StdMutex, PoisonError};

use rmux_core::kitty_keyboard::Event;
use rmux_proto::AttachedWindowsConsoleKey;
use tokio::sync::Notify;
use tokio::task::{AbortHandle, JoinError, JoinHandle};

use super::super::RequestHandler;

tokio::task_local! {
    static ATTACHED_INPUT_PATH: u32;
}

mod queued_input;
#[cfg(test)]
pub(in crate::handler) use queued_input::test_barrier;
pub(in crate::handler) use queued_input::{
    check_queued_input_target, replays_queued_input, FrameCursor, FramePart, InputArrival,
    OrderedInput,
};
use queued_input::{PasteScan, QueuedInputIdentity};

/// Runs `future` as the input path of the client `attach_pid`.
pub(in crate::handler) fn on_attached_input_path<F>(
    attach_pid: u32,
    future: F,
) -> impl Future<Output = F::Output>
where
    F: Future,
{
    ATTACHED_INPUT_PATH.scope(attach_pid, future)
}

/// Whether the current task is the input path of the client `attach_pid` itself.
pub(in crate::handler) fn runs_on_attached_input_path(attach_pid: u32) -> bool {
    ATTACHED_INPUT_PATH
        .try_with(|input_path| *input_path == attach_pid)
        .unwrap_or(false)
}

#[derive(Debug, Default)]
struct RegisteredWaits {
    next_id: u64,
    by_client: HashMap<u32, Vec<(u64, Arc<Notify>)>>,
    /// Per client, the released bindings that have not finished yet.
    released_work: HashMap<u32, usize>,
    /// Per client, ordinary input queued behind its released bindings; present while any is
    /// queued or being replayed.
    queued_input: HashMap<u32, QueuedInput>,
}

#[derive(Debug, Default)]
struct QueuedInput {
    frames: VecDeque<QueuedFrame>,
    replaying: bool,
    /// The bracketed paste the queued input leaves open, if any, where the next input continues.
    paste: PasteScan,
}

/// One unit of a client's input queued behind its released bindings, as it arrived.
#[derive(Debug)]
struct QueuedFrame {
    identity: QueuedInputIdentity,
    unit: QueuedUnit,
}

#[derive(Debug)]
enum QueuedUnit {
    /// A live frame, with the undecoded input retained before it.
    Live {
        bytes: Vec<u8>,
        windows_console_key: Option<AttachedWindowsConsoleKey>,
    },
    /// Retained input the escape time finalised: nothing after it may complete it, so it is
    /// replayed complete before the next unit.
    EscapeFlushed(Vec<u8>),
    /// A keyboard event decoded from a frame whose rest waits; it has no bytes of its own.
    KeyboardEvent(Event),
}

/// Per attach pid, the bindings its input path is currently awaiting.
#[derive(Debug, Default)]
pub(in crate::handler) struct AttachedInputWaits(Arc<StdMutex<RegisteredWaits>>);

impl AttachedInputWaits {
    /// Registers `attach_pid`'s input path as awaiting a binding until the wait is dropped.
    pub(in crate::handler) fn register(&self, attach_pid: u32) -> AttachedInputWait {
        let release = Arc::new(Notify::new());
        let mut waits = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let id = waits.next_id;
        waits.next_id += 1;
        waits
            .by_client
            .entry(attach_pid)
            .or_default()
            .push((id, Arc::clone(&release)));
        AttachedInputWait {
            waits: Arc::clone(&self.0),
            attach_pid,
            id,
            release,
        }
    }

    /// Releases every binding wait of `attach_pid`'s input path, including one not yet polled.
    pub(in crate::handler) fn release(&self, attach_pid: u32) {
        let waits = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        for (_, release) in waits.by_client.get(&attach_pid).into_iter().flatten() {
            release.notify_one();
        }
    }
}

/// Where a client's queued input is replayed once its last released binding has finished.
pub(in crate::handler) struct ReplayContext {
    handler: RequestHandler,
    runtime: tokio::runtime::Handle,
}

/// A released binding of one client that has not finished; that client's ordinary input queues
/// until it is dropped, and is then replayed.
pub(in crate::handler) struct ReleasedInputWork {
    waits: Arc<StdMutex<RegisteredWaits>>,
    attach_pid: u32,
    replay: Option<ReplayContext>,
}

impl Drop for ReleasedInputWork {
    fn drop(&mut self) {
        let mut waits = self.waits.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(count) = waits.released_work.get_mut(&self.attach_pid) else {
            return;
        };
        *count -= 1;
        if *count > 0 {
            return;
        }
        waits.released_work.remove(&self.attach_pid);
        let Some(queued) = waits
            .queued_input
            .get_mut(&self.attach_pid)
            .filter(|queued| !queued.replaying && !queued.frames.is_empty())
        else {
            return;
        };
        let Some(ReplayContext { handler, runtime }) = self.replay.take() else {
            return;
        };
        queued.replaying = true;
        let attach_pid = self.attach_pid;
        // The input path runs on the server's runtime; so does its replay, and whatever the
        // replayed keys start.
        runtime.spawn(async move { handler.replay_queued_input(attach_pid).await });
    }
}

/// One binding awaited by an attached client's input path; unregisters itself when dropped.
#[derive(Debug)]
pub(in crate::handler) struct AttachedInputWait {
    waits: Arc<StdMutex<RegisteredWaits>>,
    attach_pid: u32,
    id: u64,
    release: Arc<Notify>,
}

impl AttachedInputWait {
    /// Resolves once the input path should stop awaiting this binding.
    pub(in crate::handler) async fn released(&self) {
        self.release.notified().await;
    }

    /// Marks the binding this wait was released from as running on without the input path.
    fn begin_released_work(&self, replay: ReplayContext) -> ReleasedInputWork {
        *self
            .waits
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .released_work
            .entry(self.attach_pid)
            .or_default() += 1;
        ReleasedInputWork {
            waits: Arc::clone(&self.waits),
            attach_pid: self.attach_pid,
            replay: Some(replay),
        }
    }
}

impl Drop for AttachedInputWait {
    fn drop(&mut self) {
        let mut waits = self.waits.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(client_waits) = waits.by_client.get_mut(&self.attach_pid) {
            client_waits.retain(|(id, _)| *id != self.id);
            if client_waits.is_empty() {
                waits.by_client.remove(&self.attach_pid);
            }
        }
    }
}

/// Aborts a task when dropped, unless disarmed once the task has been joined.
struct AbortOnDrop(Option<AbortHandle>);

impl AbortOnDrop {
    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        if let Some(abort_handle) = self.0.take() {
            abort_handle.abort();
        }
    }
}

/// How work started for an attached client (a binding's commands) ended for its caller.
pub(in crate::handler) enum InputPathWork<T> {
    /// The work finished while its caller awaited it.
    Finished(Result<T, JoinError>),
    /// The client's input path was released; the work goes on without it.
    Released(ReleasedWork<T>),
}

/// Work its client's input path no longer awaits.
pub(in crate::handler) struct ReleasedWork<T> {
    task: JoinHandle<T>,
    abort: AbortOnDrop,
    /// Boxed: this guard is several hundred bytes, which every `InputPathWork` would carry.
    outstanding: Box<ReleasedInputWork>,
}

impl<T> ReleasedWork<T> {
    /// Waits for the work; dropping this future still aborts it. The client's ordinary input
    /// keeps waiting until the returned guard is dropped, once the work's result is handled.
    pub(in crate::handler) async fn join(self) -> (Result<T, JoinError>, ReleasedInputWork) {
        let Self {
            task,
            mut abort,
            outstanding,
        } = self;
        let joined = task.await;
        abort.disarm();
        (joined, *outstanding)
    }
}

impl RequestHandler {
    /// Runs `work` for the client `attach_pid` as a separate task and awaits it, aborting it if
    /// this wait is dropped. Only the client's own input path can be released from the wait, by a
    /// popup shown on that client; any other caller, such as `send-keys -K`, awaits the work and
    /// gets its result.
    pub(in crate::handler) async fn run_input_path_work<F>(
        &self,
        attach_pid: u32,
        work: F,
    ) -> InputPathWork<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.run_input_path_work_inner(attach_pid, work, false)
            .await
    }

    /// A menu has queued its replacement surface before starting this work.
    /// If the command suspends, free the input path so its attach loop can paint
    /// that surface. Ordinary input still queues behind the released work.
    /// Callers outside the client's input path keep awaiting the command.
    pub(in crate::handler) async fn run_input_path_work_for_repaint<F>(
        &self,
        attach_pid: u32,
        work: F,
    ) -> InputPathWork<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.run_input_path_work_inner(attach_pid, work, true).await
    }

    async fn run_input_path_work_inner<F>(
        &self,
        attach_pid: u32,
        work: F,
        release_on_pending: bool,
    ) -> InputPathWork<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        // Registered before the work starts, so a popup it shows at once still releases the path.
        let input_wait = if runs_on_attached_input_path(attach_pid) {
            Some(
                self.active_attach
                    .lock()
                    .await
                    .attached_input_waits
                    .register(attach_pid),
            )
        } else {
            None
        };
        let mut repaint_release = input_wait
            .as_ref()
            .filter(|_| release_on_pending)
            .map(|wait| Arc::clone(&wait.release));
        let mut task = tokio::spawn(async move {
            let mut work = std::pin::pin!(work);
            std::future::poll_fn(|cx| {
                let polled = work.as_mut().poll(cx);
                // Poll once before releasing: commands which finish immediately
                // retain their synchronous result. Notify only this wait, never
                // another binding or a same-PID replacement's wait.
                if polled.is_pending() {
                    if let Some(release) = repaint_release.take() {
                        release.notify_one();
                    }
                }
                polled
            })
            .await
        });
        let mut abort = AbortOnDrop(Some(task.abort_handle()));
        let released = async {
            match &input_wait {
                Some(input_wait) => input_wait.released().await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            biased;
            joined = &mut task => {
                abort.disarm();
                InputPathWork::Finished(joined)
            }
            () = released => {
                let outstanding = input_wait
                    .as_ref()
                    .expect("only a registered wait is released")
                    .begin_released_work(ReplayContext {
                        handler: self.clone(),
                        runtime: tokio::runtime::Handle::current(),
                    });
                InputPathWork::Released(ReleasedWork {
                    task,
                    abort,
                    outstanding: Box::new(outstanding),
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{on_attached_input_path, runs_on_attached_input_path, AttachedInputWaits};

    #[tokio::test]
    async fn releasing_a_client_frees_only_its_own_binding_waits() {
        let waits = AttachedInputWaits::default();
        let first = waits.register(7);
        let second = waits.register(7);
        let other_client = waits.register(8);

        waits.release(7);
        tokio::time::timeout(Duration::from_secs(1), first.released())
            .await
            .expect("a wait registered before the release is freed");
        tokio::time::timeout(Duration::from_secs(1), second.released())
            .await
            .expect("every wait of the released client is freed");
        assert!(
            tokio::time::timeout(Duration::from_millis(50), other_client.released())
                .await
                .is_err(),
            "another client's binding keeps its input path"
        );

        drop(first);
        drop(second);
        waits.release(7);
        let later = waits.register(7);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), later.released())
                .await
                .is_err(),
            "a release does not free a binding registered after it"
        );
    }

    #[tokio::test]
    async fn only_the_scoped_client_runs_on_its_input_path() {
        assert!(!runs_on_attached_input_path(7));
        on_attached_input_path(7, async {
            assert!(runs_on_attached_input_path(7));
            assert!(!runs_on_attached_input_path(8));
            let spawned = tokio::spawn(async { runs_on_attached_input_path(7) });
            assert!(
                !spawned.await.expect("task joins"),
                "work spawned off the input path is not on it"
            );
        })
        .await;
    }
}
