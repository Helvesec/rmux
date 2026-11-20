use std::collections::VecDeque;
use std::io::{self, Write};
use std::sync::mpsc as std_mpsc;
use std::sync::Arc;
use std::time::Duration;

use rmux_proto::{encode_attach_message, AttachFrameDecoder, AttachMessage, TerminalSize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};

use crate::ClientError;

use super::action::{AttachAction, AttachActionOutcome};
use super::metrics::AttachMetricsRecorder;
#[cfg(test)]
use super::output_worker::ATTACH_OUTPUT_QUEUE_CAPACITY;
use super::screen::{AttachScreenTracker, AttachStopDetector};
#[cfg(test)]
use super::screen::{
    AttachStopGeneration, ALT_SCREEN_EXIT_FALLBACK, DETACHED_BANNER_PREFIX, EXITED_BANNER,
};
use super::terminal;
use crate::attach_lock_state::AttachLockState;

mod exclusive_actions;
mod input_forwarding;
mod message_drain;
mod mouse_tracker;
mod output_queue;
mod spool;

use exclusive_actions::{wait_for_output_fence_deadline, PendingAttachActions};
use input_forwarding::{
    await_input_worker_completion, input_worker_stopped_error, take_ready_input_worker_completion,
    write_attach_input_once, PendingRepeatedAttachInput,
};
use message_drain::{drain_attach_messages, DrainContext};
use mouse_tracker::WindowsConsoleMouseTracker;
use output_queue::{finish_attach_output, AttachOutputQueue, ATTACH_OUTPUT_BACKPRESSURE_RETRY};
#[cfg(test)]
use output_queue::{
    AttachOutputFrame, AttachOutputFrameKind, ATTACH_OUTPUT_PENDING_MAX_BYTES,
    ATTACH_RENDER_MAX_PENDING,
};
#[cfg(test)]
use spool::{
    attach_output_spool_file_name, attach_output_spool_pid,
    cleanup_orphaned_attach_output_spools_now, AttachOutputSpool, ATTACH_OUTPUT_SPOOL_MAX_BYTES,
};

const ATTACH_OUTPUT_FENCE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct AttachOutputFence(u64);

struct AttachOutputTarget<Output, FenceFlush> {
    output: Output,
    fence_flush: FenceFlush,
}

#[cfg(test)]
pub(super) async fn drive_async_attach<Reader, Writer, Output>(
    reader: Reader,
    writer: Writer,
    initial_bytes: Vec<u8>,
    output: Output,
    screen_tracker: AttachScreenTracker,
    channels: AttachAsyncChannels,
) -> std::result::Result<(), ClientError>
where
    Reader: tokio::io::AsyncRead + Unpin,
    Writer: tokio::io::AsyncWrite + Unpin,
    Output: Write + Send + 'static,
{
    drive_async_attach_with_output_fence(
        reader,
        writer,
        initial_bytes,
        output,
        screen_tracker,
        channels,
        Write::flush,
    )
    .await
}

pub(super) async fn drive_async_attach_with_output_fence<Reader, Writer, Output, FenceFlush>(
    reader: Reader,
    writer: Writer,
    initial_bytes: Vec<u8>,
    output: Output,
    screen_tracker: AttachScreenTracker,
    channels: AttachAsyncChannels,
    output_fence_flush: FenceFlush,
) -> std::result::Result<(), ClientError>
where
    Reader: tokio::io::AsyncRead + Unpin,
    Writer: tokio::io::AsyncWrite + Unpin,
    Output: Write + Send + 'static,
    FenceFlush: FnMut(&mut Output) -> io::Result<()> + Send + 'static,
{
    let mut metrics = AttachMetricsRecorder::from_env();
    let result = drive_async_attach_loop(
        reader,
        writer,
        initial_bytes,
        AttachOutputTarget {
            output,
            fence_flush: output_fence_flush,
        },
        screen_tracker,
        channels,
        &mut metrics,
    )
    .await;
    metrics.flush();
    result
}

async fn drive_async_attach_loop<Reader, Writer, Output, FenceFlush>(
    mut reader: Reader,
    mut writer: Writer,
    initial_bytes: Vec<u8>,
    output_target: AttachOutputTarget<Output, FenceFlush>,
    screen_tracker: AttachScreenTracker,
    channels: AttachAsyncChannels,
    metrics: &mut AttachMetricsRecorder,
) -> std::result::Result<(), ClientError>
where
    Reader: tokio::io::AsyncRead + Unpin,
    Writer: tokio::io::AsyncWrite + Unpin,
    Output: Write + Send + 'static,
    FenceFlush: FnMut(&mut Output) -> io::Result<()> + Send + 'static,
{
    let AttachOutputTarget {
        output,
        fence_flush: output_fence_flush,
    } = output_target;
    let AttachAsyncChannels {
        mut input_rx,
        mut input_completion_rx,
        mut resize_rx,
        action_tx,
        mut action_completion_rx,
        locked,
        windows_console_key_enabled,
        error_cleanup,
        output_fence_timeout,
    } = channels;
    let mut decoder = AttachFrameDecoder::new();
    decoder.push_bytes(&initial_bytes);
    let mut read_buffer = [0_u8; super::READ_BUFFER_SIZE];
    let mut stop_detector = AttachStopDetector::new(screen_tracker.clone());
    let mut mouse_tracker = WindowsConsoleMouseTracker::default();
    let mut pending_actions = 0_usize;
    let mut pending_attach_actions = PendingAttachActions::default();
    let mut pending_resume_generations = VecDeque::new();
    let mut input_open = true;
    let mut resize_open = true;
    let mut pending_repeated_input: Option<PendingRepeatedAttachInput> = None;
    let mut output = AttachOutputQueue::spawn_with_fence_flush(output, output_fence_flush);
    let mut output_failure_rx = output.take_failure_notifications();
    let mut output_fence_rx = output.take_fence_notifications();

    let attach_result = async {
        loop {
        // The worker publishes completion only after dropping its input sender.
        // A fatal completion invalidates any still-buffered keystrokes, so poll
        // it before processing input instead of letting `select!` choose the
        // outcome nondeterministically. Normal EOF still drains the channel.
        if let Some(completion) = take_ready_input_worker_completion(&mut input_completion_rx) {
            completion?;
        }
        output.flush_pending()?;
        pending_attach_actions.dispatch_ready(&action_tx)?;
        drain_attach_messages(
            &mut decoder,
            &mut output,
            DrainContext {
                stop_detector: &mut stop_detector,
                mouse_tracker: &mut mouse_tracker,
                locked: &locked,
                pending_actions: &mut pending_actions,
                pending_attach_actions: &mut pending_attach_actions,
                pending_resume_generations: &mut pending_resume_generations,
                output_fence_timeout,
                metrics,
            },
        )?;
        pending_attach_actions.dispatch_ready(&action_tx)?;
        output.check_failure()?;
        let retry_output_delay = output.backpressure_retry_delay();
        let output_fence_deadline = pending_attach_actions.next_fence_deadline();

        tokio::select! {
            _ = wait_for_output_fence_deadline(output_fence_deadline) => {
                if let Ok(fence) = output_fence_rx.try_recv() {
                    pending_attach_actions.complete_fence(fence);
                    continue;
                }
                output.cancel_and_join()?;
                return Err(ClientError::Io(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "timed out draining attach output before an exclusive terminal action",
                )));
            }
            _ = tokio::time::sleep(retry_output_delay.unwrap_or(ATTACH_OUTPUT_BACKPRESSURE_RETRY)), if retry_output_delay.is_some() => {}
            failure = output_failure_rx.recv() => {
                if failure.is_none() {
                    return Err(ClientError::Io(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "attach output writer stopped",
                    )));
                }
                output.check_failure()?;
            }
            fence = output_fence_rx.recv(), if output_fence_deadline.is_some() => {
                let Some(fence) = fence else {
                    output.check_failure()?;
                    return Err(ClientError::Io(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "attach output writer stopped before acknowledging an output fence",
                    )));
                };
                pending_attach_actions.complete_fence(fence);
            }
            _ = std::future::ready(()), if pending_repeated_input.is_some() => {
                if let Some(completion) = take_ready_input_worker_completion(&mut input_completion_rx) {
                    completion?;
                }
                if locked.is_locked() {
                    pending_repeated_input = None;
                    continue;
                }
                let pending = pending_repeated_input
                    .as_mut()
                    .expect("guarded repeated attach input remains present");
                write_attach_input_once(
                    &mut writer,
                    &pending.input,
                    windows_console_key_enabled,
                )
                .await?;
                if pending.consume_one() {
                    pending_repeated_input = None;
                }
            }
            input = input_rx.recv(), if input_open && pending_repeated_input.is_none() => {
                if let Some(completion) = take_ready_input_worker_completion(&mut input_completion_rx) {
                    completion?;
                }
                let Some(input) = input else {
                    input_open = false;
                    continue;
                };
                if locked.is_locked() {
                    continue;
                }
                if input.repeat_count() > 1 {
                    pending_repeated_input = Some(PendingRepeatedAttachInput::new(input));
                } else {
                    write_attach_input_once(
                        &mut writer,
                        &input,
                        windows_console_key_enabled,
                    )
                    .await?;
                }
            }
            completion = await_input_worker_completion(&mut input_completion_rx), if input_completion_rx.is_some() => {
                input_completion_rx = None;
                match completion {
                    Ok(Ok(())) => {
                        // `input_loop` drops its sender before publishing normal
                        // completion. Keep receiving until `input_rx` has drained
                        // any final queued input and reports closure itself.
                    }
                    Ok(Err(error)) => return Err(error),
                    Err(_) => {
                        return Err(input_worker_stopped_error());
                    }
                }
            }
            size = resize_rx.recv(), if resize_open => {
                let Some(size) = size else {
                    resize_open = false;
                    continue;
                };
                write_async_attach_message(
                    &mut writer,
                    AttachMessage::Resize(size),
                ).await?;
            }
            completion = action_completion_rx.recv() => {
                let Some(completion) = completion else {
                    return Err(ClientError::Io(io::Error::other(
                        "attach action worker stopped before attach stream ended",
                    )));
                };
                match completion {
                    Ok(AttachActionOutcome::Unlock) => {
                        let stop_generation = pending_resume_generations.pop_front().ok_or_else(|| {
                            ClientError::Io(io::Error::other(
                                "attach action worker returned an unmatched unlock",
                            ))
                        })?;
                        pending_actions = pending_actions.saturating_sub(1);
                        if pending_actions == 0 {
                            // The action worker cannot complete an exclusive
                            // action until the input-loop lease is idle. Drop
                            // everything that lease queued before the lock;
                            // otherwise select scheduling could forward stale
                            // input only after the attach is unlocked.
                            pending_repeated_input = None;
                            while input_rx.try_recv().is_ok() {}
                        }
                        // Rearm only the stop published by this lock/suspend
                        // prelude. A newer stop belongs to a concurrent detach
                        // or session exit and remains authoritative.
                        if let Some(generation) = stop_generation {
                            screen_tracker.rearm_if_current(generation);
                        }
                        // Always acknowledge the completed resumable action.
                        // A newer stop remains authoritative because the
                        // generation-scoped rearm above cannot consume it, but
                        // it must not strand the server-side attach lock while
                        // a later detach frame is still in flight.
                        if let Err(error) =
                            write_async_attach_message(&mut writer, AttachMessage::Unlock).await
                        {
                            if matches!(
                                &error,
                                ClientError::Io(error)
                                    if screen_tracker.was_stopped()
                                        && matches!(
                                            error.kind(),
                                            io::ErrorKind::ConnectionReset
                                                | io::ErrorKind::BrokenPipe
                                        )
                            ) {
                                terminal::resume_ctrl_c_input();
                                locked.unlock();
                                return Ok(());
                            }
                            return Err(error);
                        }
                        if pending_actions == 0 {
                            terminal::resume_ctrl_c_input();
                            locked.unlock();
                        }
                    }
                    Ok(AttachActionOutcome::Continue) => {}
                    Ok(AttachActionOutcome::Exit) => {
                        return Ok(());
                    }
                    Err(error) => {
                        return Err(error);
                    }
                }
            }
            read = reader.read(&mut read_buffer) => {
                let bytes_read = match read {
                    Ok(0) => {
                        if screen_tracker.was_stopped() {
                            return Ok(());
                        }
                        return Err(ClientError::Io(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "attach stream closed before attach-stop sequence",
                        )));
                    }
                    Ok(bytes_read) => bytes_read,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error)
                        if screen_tracker.was_stopped()
                            && matches!(
                                error.kind(),
                                io::ErrorKind::ConnectionReset | io::ErrorKind::BrokenPipe
                            ) =>
                    {
                        return Ok(());
                    }
                    Err(error) => return Err(ClientError::Io(error)),
                };
                decoder.push_bytes(&read_buffer[..bytes_read]);
            }
        }
        }
    }
    .await;

    let cleanup_result =
        if attach_result.is_err() && !screen_tracker.was_stopped() && !output.is_stopped() {
            match error_cleanup {
                Some(bytes) => output.write_frame(bytes),
                None => Ok(()),
            }
        } else {
            Ok(())
        };
    let drain_result = finish_attach_output(output).await;
    match (attach_result, cleanup_result, drain_result) {
        (_, _, Err(error)) | (_, Err(error), Ok(())) => Err(error),
        (result, Ok(()), Ok(())) => result,
    }
}

pub(super) struct AttachAsyncChannels {
    input_rx: mpsc::Receiver<super::input::AttachInput>,
    input_completion_rx: Option<oneshot::Receiver<std::result::Result<(), ClientError>>>,
    resize_rx: mpsc::UnboundedReceiver<TerminalSize>,
    action_tx: std_mpsc::Sender<AttachAction>,
    action_completion_rx:
        mpsc::UnboundedReceiver<std::result::Result<AttachActionOutcome, ClientError>>,
    locked: Arc<AttachLockState>,
    windows_console_key_enabled: bool,
    error_cleanup: Option<Vec<u8>>,
    output_fence_timeout: Duration,
}

impl AttachAsyncChannels {
    pub(super) const fn new(
        input_rx: mpsc::Receiver<super::input::AttachInput>,
        resize_rx: mpsc::UnboundedReceiver<TerminalSize>,
        action_tx: std_mpsc::Sender<AttachAction>,
        action_completion_rx: mpsc::UnboundedReceiver<
            std::result::Result<AttachActionOutcome, ClientError>,
        >,
        locked: Arc<AttachLockState>,
        windows_console_key_enabled: bool,
    ) -> Self {
        Self {
            input_rx,
            input_completion_rx: None,
            resize_rx,
            action_tx,
            action_completion_rx,
            locked,
            windows_console_key_enabled,
            error_cleanup: None,
            output_fence_timeout: ATTACH_OUTPUT_FENCE_TIMEOUT,
        }
    }

    pub(super) fn with_input_completion(
        mut self,
        completion_rx: oneshot::Receiver<std::result::Result<(), ClientError>>,
    ) -> Self {
        self.input_completion_rx = Some(completion_rx);
        self
    }

    pub(super) fn with_error_cleanup(mut self, error_cleanup: Option<Vec<u8>>) -> Self {
        self.error_cleanup = error_cleanup;
        self
    }

    #[cfg(test)]
    pub(super) fn with_output_fence_timeout(mut self, timeout: Duration) -> Self {
        self.output_fence_timeout = timeout;
        self
    }
}

pub(super) async fn write_async_attach_message<Writer>(
    writer: &mut Writer,
    message: AttachMessage,
) -> std::result::Result<(), ClientError>
where
    Writer: tokio::io::AsyncWrite + Unpin,
{
    let frame = encode_attach_message(&message).map_err(ClientError::from)?;
    writer.write_all(&frame).await.map_err(ClientError::Io)
}

#[cfg(test)]
#[path = "../../attach_windows/stream_tests.rs"]
mod tests;
