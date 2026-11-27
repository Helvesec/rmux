//! Completion of the command that opened a popup.
//!
//! Job exit under `-E`, `display-popup -C`, replacement and teardown share [`PopupCompletion`].
//! The last state copy releases the command: tmux 3.7b reports the job's status or 128 + SIGHUP.

use std::sync::{Arc, Mutex as StdMutex, PoisonError};

use tokio::sync::oneshot;

use super::super::scripting_support::QueueCommandAction;
use super::super::RequestHandler;
use super::state::PopupOverlayState;

/// The status tmux reports when a popup is freed before its job has exited.
const POPUP_FREED_BEFORE_JOB_EXIT_STATUS: i32 = 128 + 1;

/// Arms `popup` so its opening command can wait for it, unless that command runs inline on the
/// input path of the popup's own client: the popup's keys and mouse reports come through that
/// path, so waiting there would keep the popup from ever closing.
pub(super) fn arm_popup_completion(
    popup: &mut PopupOverlayState,
    runs_on_own_input_path: bool,
) -> Option<oneshot::Receiver<i32>> {
    if runs_on_own_input_path {
        return None;
    }
    let (completion, receiver) = PopupCompletion::new();
    popup.completion = Some(completion);
    Some(receiver)
}

impl RequestHandler {
    /// The opening command's result once its popup, installed on `attach_pid`, has left.
    pub(super) async fn wait_for_popup_completion(
        &self,
        attach_pid: u32,
        receiver: oneshot::Receiver<i32>,
    ) -> QueueCommandAction {
        // The popup's input must not wait behind a binding its client's input path awaits,
        // whether that binding leads to this command or not.
        self.active_attach
            .lock()
            .await
            .attached_input_waits
            .release(attach_pid);
        let status = match crate::listener::shutdown_answer::popup_wait_ends_on_shutdown() {
            Some(shutdown_wait) => {
                let mut closing = self.normal_request_shutdown_receiver();
                let status = tokio::select! {
                    // Shutdown frees the popup as well; as in tmux, the request then answers with
                    // what it has acquired and runs nothing after the popup.
                    biased;
                    _ = closing.wait_for(|closing| *closing) => None,
                    status = receiver => Some(status.unwrap_or(POPUP_FREED_BEFORE_JOB_EXIT_STATUS)),
                };
                #[cfg(test)]
                if status.is_some() {
                    crate::listener::shutdown_answer::pause_popup_completion_for_test().await;
                }
                // Either shutdown notification may arrive before popup completion.
                if status.is_none()
                    || *closing.borrow()
                    || crate::listener::shutdown_answer::request_stopping_for_shutdown()
                {
                    shutdown_wait.stop_request();
                    None
                } else {
                    status
                }
            }
            None => Some(receiver.await.unwrap_or(POPUP_FREED_BEFORE_JOB_EXIT_STATUS)),
        };
        QueueCommandAction::Normal {
            output: None,
            error: None,
            source_file_error: None,
            exit_status: status,
        }
    }
}

/// Shared by every copy of one popup's state; see the module documentation.
#[derive(Debug, Clone)]
pub(in crate::handler) struct PopupCompletion(Arc<CompletionState>);

#[derive(Debug)]
struct CompletionState {
    job_status: StdMutex<Option<i32>>,
    waiter: StdMutex<Option<oneshot::Sender<i32>>>,
}

impl PopupCompletion {
    /// A completion and the receiver the opening command waits on.
    fn new() -> (Self, oneshot::Receiver<i32>) {
        let (sender, receiver) = oneshot::channel();
        let state = CompletionState {
            job_status: StdMutex::new(None),
            waiter: StdMutex::new(Some(sender)),
        };
        (Self(Arc::new(state)), receiver)
    }

    /// Records the popup job's exit status. The popup itself may stay open (`-EE`, no `-E`);
    /// the status is reported once the popup is freed.
    pub(super) fn record_job_exit(&self, status: i32) {
        *self
            .0
            .job_status
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(status);
    }
}

impl Drop for CompletionState {
    fn drop(&mut self) {
        let status = self
            .job_status
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
            .unwrap_or(POPUP_FREED_BEFORE_JOB_EXIT_STATUS);
        if let Some(waiter) = self
            .waiter
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            let _ = waiter.send(status);
        }
    }
}
