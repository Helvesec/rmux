use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::Arc;

use rmux_proto::{
    encode_attach_message, AttachFrameDecoder, AttachMessage, AttachedKeystroke, RmuxError,
    TerminalSize,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

use crate::ClientError;

use super::action::{AttachAction, AttachActionOutcome};
use super::metrics::AttachMetricsRecorder;
use super::screen::{
    contains_subslice, AttachScreenTracker, AttachStopDetector, ALT_SCREEN_EXIT_FALLBACK,
    DETACHED_BANNER_PREFIX, EXITED_BANNER,
};

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
    Output: Write,
{
    let mut metrics = AttachMetricsRecorder::from_env();
    let result = drive_async_attach_loop(
        reader,
        writer,
        initial_bytes,
        output,
        screen_tracker,
        channels,
        &mut metrics,
    )
    .await;
    metrics.flush();
    result
}

async fn drive_async_attach_loop<Reader, Writer, Output>(
    mut reader: Reader,
    mut writer: Writer,
    initial_bytes: Vec<u8>,
    mut output: Output,
    screen_tracker: AttachScreenTracker,
    channels: AttachAsyncChannels,
    metrics: &mut AttachMetricsRecorder,
) -> std::result::Result<(), ClientError>
where
    Reader: tokio::io::AsyncRead + Unpin,
    Writer: tokio::io::AsyncWrite + Unpin,
    Output: Write,
{
    let AttachAsyncChannels {
        mut input_rx,
        mut resize_rx,
        action_tx,
        mut action_completion_rx,
        locked,
    } = channels;
    let mut decoder = AttachFrameDecoder::new();
    decoder.push_bytes(&initial_bytes);
    let mut read_buffer = [0_u8; super::READ_BUFFER_SIZE];
    let mut stop_detector = AttachStopDetector::new(screen_tracker.clone());
    let mut pending_actions = 0_usize;

    loop {
        drain_attach_messages(
            &mut decoder,
            &mut output,
            DrainContext {
                screen_tracker: &screen_tracker,
                stop_detector: &mut stop_detector,
                action_tx: &action_tx,
                locked: &locked,
                pending_actions: &mut pending_actions,
                metrics,
            },
        )?;

        tokio::select! {
            bytes = input_rx.recv() => {
                let Some(bytes) = bytes else {
                    continue;
                };
                if locked.load(Ordering::SeqCst) {
                    continue;
                }
                for chunk in super::input::attach_input_chunks(&bytes) {
                    write_async_attach_message(
                        &mut writer,
                        AttachMessage::Keystroke(AttachedKeystroke::new(chunk.to_vec())),
                    ).await?;
                }
            }
            size = resize_rx.recv() => {
                let Some(size) = size else {
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
                pending_actions = pending_actions.saturating_sub(1);
                match completion {
                    Ok(AttachActionOutcome::Unlock) => {
                        let unlock_result =
                            write_async_attach_message(&mut writer, AttachMessage::Unlock).await;
                        if pending_actions == 0 {
                            locked.store(false, Ordering::SeqCst);
                        }
                        unlock_result?;
                    }
                    Ok(AttachActionOutcome::Exit) => {
                        return Ok(());
                    }
                    Err(error) => {
                        locked.store(false, Ordering::SeqCst);
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

fn drain_attach_messages<Output>(
    decoder: &mut AttachFrameDecoder,
    output: &mut Output,
    context: DrainContext<'_>,
) -> std::result::Result<(), ClientError>
where
    Output: Write,
{
    let DrainContext {
        screen_tracker,
        stop_detector,
        action_tx,
        locked,
        pending_actions,
        metrics,
    } = context;
    while let Some(message) = decoder.next_message().map_err(ClientError::from)? {
        match message {
            AttachMessage::Data(bytes) => {
                metrics.observe_data_frame(&bytes);
                if contains_subslice(&bytes, ALT_SCREEN_EXIT_FALLBACK)
                    || contains_subslice(&bytes, DETACHED_BANNER_PREFIX)
                    || contains_subslice(&bytes, EXITED_BANNER)
                {
                    screen_tracker.mark_stopped();
                }
                stop_detector.observe(&bytes);
                if locked.load(Ordering::SeqCst) {
                    continue;
                }
                output.write_all(&bytes).map_err(ClientError::Io)?;
                output.flush().map_err(ClientError::Io)?;
            }
            AttachMessage::KeyDispatched(_) => {}
            AttachMessage::DetachKill => {
                locked.store(true, Ordering::SeqCst);
                send_attach_action(action_tx, AttachAction::DetachKill)?;
                *pending_actions += 1;
            }
            AttachMessage::DetachExec(command) => {
                locked.store(true, Ordering::SeqCst);
                send_attach_action(action_tx, AttachAction::DetachExec(command))?;
                *pending_actions += 1;
            }
            AttachMessage::Lock(command) => {
                locked.store(true, Ordering::SeqCst);
                send_attach_action(action_tx, AttachAction::Lock(command))?;
                *pending_actions += 1;
            }
            AttachMessage::Suspend => {
                locked.store(true, Ordering::SeqCst);
                send_attach_action(action_tx, AttachAction::Suspend)?;
                *pending_actions += 1;
            }
            AttachMessage::Resize(_) => {
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
        }
    }

    Ok(())
}

pub(super) struct AttachAsyncChannels {
    input_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    resize_rx: mpsc::UnboundedReceiver<TerminalSize>,
    action_tx: std_mpsc::Sender<AttachAction>,
    action_completion_rx:
        mpsc::UnboundedReceiver<std::result::Result<AttachActionOutcome, ClientError>>,
    locked: Arc<AtomicBool>,
}

impl AttachAsyncChannels {
    pub(super) const fn new(
        input_rx: mpsc::UnboundedReceiver<Vec<u8>>,
        resize_rx: mpsc::UnboundedReceiver<TerminalSize>,
        action_tx: std_mpsc::Sender<AttachAction>,
        action_completion_rx: mpsc::UnboundedReceiver<
            std::result::Result<AttachActionOutcome, ClientError>,
        >,
        locked: Arc<AtomicBool>,
    ) -> Self {
        Self {
            input_rx,
            resize_rx,
            action_tx,
            action_completion_rx,
            locked,
        }
    }
}

struct DrainContext<'context> {
    screen_tracker: &'context AttachScreenTracker,
    stop_detector: &'context mut AttachStopDetector,
    action_tx: &'context std_mpsc::Sender<AttachAction>,
    locked: &'context Arc<AtomicBool>,
    pending_actions: &'context mut usize,
    metrics: &'context mut AttachMetricsRecorder,
}

fn send_attach_action(
    action_tx: &std_mpsc::Sender<AttachAction>,
    action: AttachAction,
) -> std::result::Result<(), ClientError> {
    action_tx
        .send(action)
        .map_err(|_| ClientError::Io(io::Error::other("attach action worker stopped")))
}

async fn write_async_attach_message<Writer>(
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
#[path = "stream_tests.rs"]
mod tests;
