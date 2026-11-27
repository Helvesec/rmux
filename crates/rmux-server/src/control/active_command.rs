use super::{ControlCommandOrigin, ControlCommandResult, ControlQueueEofCancellation, JoinHandle};
use std::future::{poll_fn, Future};
use tokio::sync::oneshot;

#[cfg(any(unix, windows))]
pub(super) fn spawn_control_command_with_first_poll(
    future: impl Future<Output = ControlCommandResult> + Send + 'static,
) -> (JoinHandle<ControlCommandResult>, oneshot::Receiver<bool>) {
    let (first_poll_tx, first_poll_rx) = oneshot::channel();
    let task = tokio::spawn(async move {
        let mut future = std::pin::pin!(future);
        let mut first_poll_tx = Some(first_poll_tx);
        poll_fn(|cx| {
            let result = future.as_mut().poll(cx);
            if let Some(sender) = first_poll_tx.take() {
                let _ = sender.send(result.is_ready());
            }
            result
        })
        .await
    });
    (task, first_poll_rx)
}

#[derive(Debug)]
#[cfg(any(unix, windows))]
pub(super) struct ActiveControlCommand {
    pub(super) timestamp: i64,
    pub(super) command_number: u64,
    pub(super) guard_flag: u8,
    pub(super) origin: ControlCommandOrigin,
    /// Whether the command's own guard ended in `%error`, recorded by its
    /// first terminal: the owner guard always closes before any child frame
    /// opens, so later terminals belong to inserted children.
    pub(super) owner_failed: Option<bool>,
    pub(super) eof_cancellation: ControlQueueEofCancellation,
    pub(super) task: Option<JoinHandle<ControlCommandResult>>,
}

#[cfg(any(unix, windows))]
impl Drop for ActiveControlCommand {
    fn drop(&mut self) {
        if let Some(task) = self.task.as_ref() {
            if !task.is_finished() {
                task.abort();
            }
        }
    }
}
