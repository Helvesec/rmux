use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use rmux_proto::{AttachFrameDecoder, AttachMessage, RmuxError};

use crate::attach_lock_state::AttachLockState;
use crate::ClientError;

use super::super::action::AttachAction;
use super::super::metrics::AttachMetricsRecorder;
use super::super::screen::{AttachStopDetector, AttachStopGeneration};
use super::exclusive_actions::PendingAttachActions;
use super::mouse_tracker::WindowsConsoleMouseTracker;
use super::output_queue::AttachOutputQueue;

pub(super) struct DrainContext<'context> {
    pub(super) stop_detector: &'context mut AttachStopDetector,
    pub(super) mouse_tracker: &'context mut WindowsConsoleMouseTracker,
    pub(super) locked: &'context Arc<AttachLockState>,
    pub(super) pending_actions: &'context mut usize,
    pub(super) pending_attach_actions: &'context mut PendingAttachActions,
    pub(super) pending_resume_generations: &'context mut VecDeque<Option<AttachStopGeneration>>,
    pub(super) output_fence_timeout: Duration,
    pub(super) metrics: &'context mut AttachMetricsRecorder,
}

pub(super) fn drain_attach_messages(
    decoder: &mut AttachFrameDecoder,
    output: &mut AttachOutputQueue,
    context: DrainContext<'_>,
) -> std::result::Result<(), ClientError> {
    let DrainContext {
        stop_detector,
        mouse_tracker,
        locked,
        pending_actions,
        pending_attach_actions,
        pending_resume_generations,
        output_fence_timeout,
        metrics,
    } = context;
    while let Some(message) = decoder.next_message().map_err(ClientError::from)? {
        match message {
            AttachMessage::Data(bytes) => {
                metrics.observe_data_frame(&bytes);
                stop_detector.observe(&bytes);
                if let Some(enabled) = mouse_tracker.observe(&bytes) {
                    pending_attach_actions
                        .queue_immediate(AttachAction::MouseInputEnabled(enabled));
                }
                if locked.is_locked() {
                    continue;
                }
                output.write_frame(bytes)?;
            }
            AttachMessage::Render(bytes) => {
                metrics.observe_data_frame(&bytes);
                stop_detector.observe(&bytes);
                if let Some(enabled) = mouse_tracker.observe(&bytes) {
                    pending_attach_actions
                        .queue_immediate(AttachAction::MouseInputEnabled(enabled));
                }
                if locked.is_locked() {
                    continue;
                }
                output.write_render(bytes)?;
            }
            AttachMessage::KeyDispatched(_) => {}
            AttachMessage::DetachKill => {
                pending_attach_actions.queue_exclusive(
                    output,
                    locked,
                    pending_actions,
                    output_fence_timeout,
                    AttachAction::DetachKill,
                )?;
            }
            AttachMessage::DetachExec(command) => {
                pending_attach_actions.queue_exclusive(
                    output,
                    locked,
                    pending_actions,
                    output_fence_timeout,
                    AttachAction::LegacyDetachExec(command),
                )?;
            }
            AttachMessage::DetachExecShellCommand(command) => {
                pending_attach_actions.queue_exclusive(
                    output,
                    locked,
                    pending_actions,
                    output_fence_timeout,
                    AttachAction::DetachExec(command),
                )?;
            }
            AttachMessage::Lock(command) => {
                let stop_generation = stop_detector.current_stop_generation();
                pending_attach_actions.queue_exclusive(
                    output,
                    locked,
                    pending_actions,
                    output_fence_timeout,
                    AttachAction::LegacyLock(command),
                )?;
                pending_resume_generations.push_back(stop_generation);
            }
            AttachMessage::LockShellCommand(command) => {
                let stop_generation = stop_detector.current_stop_generation();
                pending_attach_actions.queue_exclusive(
                    output,
                    locked,
                    pending_actions,
                    output_fence_timeout,
                    AttachAction::Lock(command),
                )?;
                pending_resume_generations.push_back(stop_generation);
            }
            AttachMessage::Suspend => {
                let stop_generation = stop_detector.current_stop_generation();
                pending_attach_actions.queue_exclusive(
                    output,
                    locked,
                    pending_actions,
                    output_fence_timeout,
                    AttachAction::Suspend,
                )?;
                pending_resume_generations.push_back(stop_generation);
            }
            AttachMessage::Resize(_) | AttachMessage::ResizeGeometry(_) => {
                return Err(ClientError::Protocol(RmuxError::Decode(
                    "received unexpected resize message from attach stream".to_owned(),
                )));
            }
            AttachMessage::Unlock => {
                return Err(ClientError::Protocol(RmuxError::Decode(
                    "received unexpected unlock message from attach stream".to_owned(),
                )));
            }
            AttachMessage::Keystroke(_) => {
                return Err(ClientError::Protocol(RmuxError::Decode(
                    "received unexpected keystroke message from attach stream".to_owned(),
                )));
            }
            // The Windows client does not opt into `stream.attach.server_exited`.
            AttachMessage::ServerExited => {
                return Err(ClientError::Protocol(RmuxError::Decode(
                    "received unexpected server-exited message from attach stream".to_owned(),
                )));
            }
        }
    }

    Ok(())
}
