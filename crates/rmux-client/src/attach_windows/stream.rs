use std::collections::VecDeque;
use std::io::{self, Write};
use std::sync::mpsc as std_mpsc;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use rmux_proto::{
    encode_attach_message, AttachFrameDecoder, AttachMessage, AttachedKeystroke, RmuxError,
    TerminalSize,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

use crate::ClientError;

use super::action::{AttachAction, AttachActionOutcome};
use super::lock_state::AttachLockState;
use super::metrics::AttachMetricsRecorder;
use super::screen::{
    contains_subslice, AttachScreenTracker, AttachStopDetector, ALT_SCREEN_EXIT_FALLBACK,
    DETACHED_BANNER_PREFIX, EXITED_BANNER,
};

const ATTACH_OUTPUT_QUEUE_CAPACITY: usize = 64;
const ATTACH_OUTPUT_PENDING_MAX_BYTES: usize = 4 * 1024 * 1024;
const ATTACH_OUTPUT_BACKPRESSURE_RETRY: Duration = Duration::from_millis(5);
const ATTACH_OUTPUT_SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(250);

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
    output: Output,
    screen_tracker: AttachScreenTracker,
    channels: AttachAsyncChannels,
    metrics: &mut AttachMetricsRecorder,
) -> std::result::Result<(), ClientError>
where
    Reader: tokio::io::AsyncRead + Unpin,
    Writer: tokio::io::AsyncWrite + Unpin,
    Output: Write + Send + 'static,
{
    let AttachAsyncChannels {
        mut input_rx,
        mut resize_rx,
        action_tx,
        mut action_completion_rx,
        locked,
        windows_console_key_enabled,
    } = channels;
    let mut decoder = AttachFrameDecoder::new();
    decoder.push_bytes(&initial_bytes);
    let mut read_buffer = [0_u8; super::READ_BUFFER_SIZE];
    let mut stop_detector = AttachStopDetector::new(screen_tracker.clone());
    let mut mouse_tracker = WindowsConsoleMouseTracker::default();
    let mut pending_actions = 0_usize;
    let mut input_open = true;
    let mut resize_open = true;
    let mut output = AttachOutputQueue::spawn(output);
    let mut output_failure_rx = output.take_failure_notifications();

    loop {
        output.flush_pending()?;
        if !output.is_backpressured() {
            drain_attach_messages(
                &mut decoder,
                &mut output,
                DrainContext {
                    screen_tracker: &screen_tracker,
                    stop_detector: &mut stop_detector,
                    mouse_tracker: &mut mouse_tracker,
                    action_tx: &action_tx,
                    locked: &locked,
                    pending_actions: &mut pending_actions,
                    metrics,
                },
            )?;
        }
        output.check_failure()?;
        let retry_output = output.is_backpressured();

        tokio::select! {
            _ = tokio::time::sleep(ATTACH_OUTPUT_BACKPRESSURE_RETRY), if retry_output => {}
            failure = output_failure_rx.recv() => {
                if failure.is_none() {
                    return Err(ClientError::Io(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "attach output writer stopped",
                    )));
                }
                output.check_failure()?;
            }
            input = input_rx.recv(), if input_open => {
                let Some(input) = input else {
                    input_open = false;
                    continue;
                };
                if locked.is_locked() {
                    continue;
                }
                let input_bytes = input.payload();
                let windows_console_key = if windows_console_key_enabled {
                    input.windows_console_key()
                } else {
                    None
                };
                for chunk in super::input::attach_input_chunks(input_bytes) {
                    let mut keystroke = AttachedKeystroke::new(chunk.to_vec());
                    if chunk.len() == input_bytes.len() {
                        if let Some(key) = windows_console_key {
                            keystroke = keystroke.with_windows_console_key(key);
                        }
                    }
                    write_async_attach_message(
                        &mut writer,
                        AttachMessage::Keystroke(keystroke),
                    ).await?;
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
                        pending_actions = pending_actions.saturating_sub(1);
                        let unlock_result =
                            write_async_attach_message(&mut writer, AttachMessage::Unlock).await;
                        if pending_actions == 0 {
                            locked.unlock();
                        }
                        unlock_result?;
                    }
                    Ok(AttachActionOutcome::Continue) => {}
                    Ok(AttachActionOutcome::Exit) => {
                        return Ok(());
                    }
                    Err(error) => {
                        locked.unlock();
                        return Err(error);
                    }
                }
            }
            read = reader.read(&mut read_buffer), if !output.is_backpressured() => {
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

fn drain_attach_messages(
    decoder: &mut AttachFrameDecoder,
    output: &mut AttachOutputQueue,
    context: DrainContext<'_>,
) -> std::result::Result<(), ClientError> {
    let DrainContext {
        screen_tracker,
        stop_detector,
        mouse_tracker,
        action_tx,
        locked,
        pending_actions,
        metrics,
    } = context;
    while let Some(message) = decoder.next_message().map_err(ClientError::from)? {
        match message {
            AttachMessage::Data(bytes) | AttachMessage::Render(bytes) => {
                metrics.observe_data_frame(&bytes);
                if contains_subslice(&bytes, ALT_SCREEN_EXIT_FALLBACK)
                    || contains_subslice(&bytes, DETACHED_BANNER_PREFIX)
                    || contains_subslice(&bytes, EXITED_BANNER)
                {
                    screen_tracker.mark_stopped();
                }
                stop_detector.observe(&bytes);
                if let Some(enabled) = mouse_tracker.observe(&bytes) {
                    send_attach_action(action_tx, AttachAction::MouseInputEnabled(enabled))?;
                }
                if locked.is_locked() {
                    continue;
                }
                output.write_frame(bytes)?;
                if output.is_backpressured() {
                    break;
                }
            }
            AttachMessage::KeyDispatched(_) => {}
            AttachMessage::DetachKill => {
                locked.lock();
                send_attach_action(action_tx, AttachAction::DetachKill)?;
                *pending_actions += 1;
            }
            AttachMessage::DetachExec(command) => {
                locked.lock();
                send_attach_action(action_tx, AttachAction::LegacyDetachExec(command))?;
                *pending_actions += 1;
            }
            AttachMessage::DetachExecShellCommand(command) => {
                locked.lock();
                send_attach_action(action_tx, AttachAction::DetachExec(command))?;
                *pending_actions += 1;
            }
            AttachMessage::Lock(command) => {
                locked.lock();
                send_attach_action(action_tx, AttachAction::LegacyLock(command))?;
                *pending_actions += 1;
            }
            AttachMessage::LockShellCommand(command) => {
                locked.lock();
                send_attach_action(action_tx, AttachAction::Lock(command))?;
                *pending_actions += 1;
            }
            AttachMessage::Suspend => {
                locked.lock();
                send_attach_action(action_tx, AttachAction::Suspend)?;
                *pending_actions += 1;
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
        }
    }

    Ok(())
}

struct AttachOutputQueue {
    command_tx: Option<std_mpsc::SyncSender<Vec<u8>>>,
    failure_rx: std_mpsc::Receiver<io::Error>,
    failure_wake_rx: Option<mpsc::UnboundedReceiver<()>>,
    done_rx: std_mpsc::Receiver<()>,
    worker: Option<thread::JoinHandle<()>>,
    pending: VecDeque<Vec<u8>>,
    pending_bytes: usize,
}

impl AttachOutputQueue {
    fn spawn<Output>(mut output: Output) -> Self
    where
        Output: Write + Send + 'static,
    {
        let (command_tx, command_rx) =
            std_mpsc::sync_channel::<Vec<u8>>(ATTACH_OUTPUT_QUEUE_CAPACITY);
        let (failure_tx, failure_rx) = std_mpsc::channel();
        let (failure_wake_tx, failure_wake_rx) = mpsc::unbounded_channel();
        let (done_tx, done_rx) = std_mpsc::channel();
        let worker = thread::spawn(move || {
            while let Ok(bytes) = command_rx.recv() {
                if let Err(error) = output.write_all(&bytes).and_then(|()| output.flush()) {
                    let _ = failure_tx.send(error);
                    let _ = failure_wake_tx.send(());
                    break;
                }
            }
            let _ = done_tx.send(());
        });

        Self {
            command_tx: Some(command_tx),
            failure_rx,
            failure_wake_rx: Some(failure_wake_rx),
            done_rx,
            worker: Some(worker),
            pending: VecDeque::new(),
            pending_bytes: 0,
        }
    }

    fn write_frame(&mut self, bytes: Vec<u8>) -> std::result::Result<(), ClientError> {
        self.check_failure()?;
        let next_pending_bytes = self.pending_bytes.saturating_add(bytes.len());
        if next_pending_bytes > ATTACH_OUTPUT_PENDING_MAX_BYTES {
            return Err(ClientError::Io(io::Error::other(format!(
                "attach output writer is blocked and queued more than {ATTACH_OUTPUT_PENDING_MAX_BYTES} bytes"
            ))));
        }
        self.pending_bytes = next_pending_bytes;
        self.pending.push_back(bytes);
        self.flush_pending()
    }

    fn flush_pending(&mut self) -> std::result::Result<(), ClientError> {
        self.check_failure()?;
        let Some(command_tx) = self.command_tx.as_ref() else {
            return Err(ClientError::Io(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "attach output writer stopped",
            )));
        };

        while let Some(bytes) = self.pending.pop_front() {
            let len = bytes.len();
            match command_tx.try_send(bytes) {
                Ok(()) => {
                    self.pending_bytes = self.pending_bytes.saturating_sub(len);
                }
                Err(std_mpsc::TrySendError::Full(bytes)) => {
                    self.pending.push_front(bytes);
                    break;
                }
                Err(std_mpsc::TrySendError::Disconnected(_)) => {
                    return Err(ClientError::Io(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "attach output writer stopped",
                    )));
                }
            }
        }

        self.check_failure()
    }

    fn is_backpressured(&self) -> bool {
        !self.pending.is_empty()
    }

    fn check_failure(&mut self) -> std::result::Result<(), ClientError> {
        match self.failure_rx.try_recv() {
            Ok(error) => Err(ClientError::Io(error)),
            Err(std_mpsc::TryRecvError::Empty) => Ok(()),
            Err(std_mpsc::TryRecvError::Disconnected) => Ok(()),
        }
    }

    fn take_failure_notifications(&mut self) -> mpsc::UnboundedReceiver<()> {
        self.failure_wake_rx
            .take()
            .expect("attach output failure notifications should only be taken once")
    }
}

impl Drop for AttachOutputQueue {
    fn drop(&mut self) {
        drop(self.command_tx.take());
        if self
            .done_rx
            .recv_timeout(ATTACH_OUTPUT_SHUTDOWN_TIMEOUT)
            .is_ok()
        {
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }
}

pub(super) struct AttachAsyncChannels {
    input_rx: mpsc::Receiver<super::input::AttachInput>,
    resize_rx: mpsc::UnboundedReceiver<TerminalSize>,
    action_tx: std_mpsc::Sender<AttachAction>,
    action_completion_rx:
        mpsc::UnboundedReceiver<std::result::Result<AttachActionOutcome, ClientError>>,
    locked: Arc<AttachLockState>,
    windows_console_key_enabled: bool,
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
            resize_rx,
            action_tx,
            action_completion_rx,
            locked,
            windows_console_key_enabled,
        }
    }
}

struct DrainContext<'context> {
    screen_tracker: &'context AttachScreenTracker,
    stop_detector: &'context mut AttachStopDetector,
    mouse_tracker: &'context mut WindowsConsoleMouseTracker,
    action_tx: &'context std_mpsc::Sender<AttachAction>,
    locked: &'context Arc<AttachLockState>,
    pending_actions: &'context mut usize,
    metrics: &'context mut AttachMetricsRecorder,
}

#[derive(Debug, Default)]
struct WindowsConsoleMouseTracker {
    enabled: bool,
    tail: Vec<u8>,
}

impl WindowsConsoleMouseTracker {
    fn observe(&mut self, bytes: &[u8]) -> Option<bool> {
        const ENABLE: [&[u8]; 4] = [
            b"\x1b[?1000h",
            b"\x1b[?1002h",
            b"\x1b[?1003h",
            b"\x1b[?1006h",
        ];
        const DISABLE: [&[u8]; 4] = [
            b"\x1b[?1000l",
            b"\x1b[?1002l",
            b"\x1b[?1003l",
            b"\x1b[?1006l",
        ];
        const TAIL_LEN: usize = 7;

        if bytes.is_empty() {
            return None;
        }

        let mut combined = Vec::with_capacity(self.tail.len() + bytes.len());
        combined.extend_from_slice(&self.tail);
        combined.extend_from_slice(bytes);

        let mut observed = None;
        for index in 0..combined.len() {
            if ENABLE
                .iter()
                .any(|sequence| combined[index..].starts_with(sequence))
            {
                observed = Some(true);
            } else if DISABLE
                .iter()
                .any(|sequence| combined[index..].starts_with(sequence))
            {
                observed = Some(false);
            }
        }

        self.tail.clear();
        self.tail
            .extend_from_slice(&combined[combined.len().saturating_sub(TAIL_LEN)..]);

        let enabled = observed?;
        if self.enabled == enabled {
            return None;
        }
        self.enabled = enabled;
        Some(enabled)
    }
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
