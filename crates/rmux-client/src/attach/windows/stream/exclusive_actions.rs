use std::collections::VecDeque;
use std::io;
use std::sync::mpsc as std_mpsc;
use std::sync::Arc;
use std::time::Duration;

use crate::attach_lock_state::AttachLockState;
use crate::ClientError;

use super::super::action::AttachAction;
use super::super::terminal;
use super::{AttachOutputFence, AttachOutputQueue};

struct PendingAttachAction {
    action: AttachAction,
    fence: Option<PendingAttachActionFence>,
}

struct PendingAttachActionFence {
    id: AttachOutputFence,
    deadline: tokio::time::Instant,
}

#[derive(Default)]
pub(super) struct PendingAttachActions {
    actions: VecDeque<PendingAttachAction>,
    completed_fences: VecDeque<AttachOutputFence>,
}

impl PendingAttachActions {
    pub(super) fn queue_immediate(&mut self, action: AttachAction) {
        self.actions.push_back(PendingAttachAction {
            action,
            fence: None,
        });
    }

    pub(super) fn queue_exclusive(
        &mut self,
        output: &mut AttachOutputQueue,
        locked: &Arc<AttachLockState>,
        pending_exclusive_count: &mut usize,
        output_fence_timeout: Duration,
        action: AttachAction,
    ) -> Result<(), ClientError> {
        // Suppression is process-wide because Win32 console-control callbacks
        // are process-wide. Arm it before the input lock so a racing callback
        // is either consumed by the current input read or discarded at the
        // boundary, never retained until the terminal action completes.
        terminal::suppress_ctrl_c_input();
        locked.lock();
        let fence = output.request_fence()?;
        self.actions.push_back(PendingAttachAction {
            action,
            fence: Some(PendingAttachActionFence {
                id: fence,
                deadline: tokio::time::Instant::now() + output_fence_timeout,
            }),
        });
        *pending_exclusive_count += 1;
        Ok(())
    }

    pub(super) fn complete_fence(&mut self, fence: AttachOutputFence) {
        self.completed_fences.push_back(fence);
    }

    pub(super) fn dispatch_ready(
        &mut self,
        action_tx: &std_mpsc::Sender<AttachAction>,
    ) -> Result<(), ClientError> {
        loop {
            let Some(pending) = self.actions.front() else {
                return Ok(());
            };
            if let Some(fence) = pending.fence.as_ref() {
                let Some(completed) = self.completed_fences.front().copied() else {
                    return Ok(());
                };
                if completed != fence.id {
                    return Err(ClientError::Io(io::Error::other(format!(
                        "attach output fence completed out of order: expected {:?}, received {:?}",
                        fence.id, completed
                    ))));
                }
                self.completed_fences.pop_front();
            }
            let pending = self
                .actions
                .pop_front()
                .expect("guarded pending attach action remains queued");
            action_tx
                .send(pending.action)
                .map_err(|_| ClientError::Io(io::Error::other("attach action worker stopped")))?;
        }
    }

    pub(super) fn next_fence_deadline(&self) -> Option<tokio::time::Instant> {
        self.actions
            .iter()
            .find_map(|pending| pending.fence.as_ref().map(|fence| fence.deadline))
    }
}

pub(super) async fn wait_for_output_fence_deadline(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}
