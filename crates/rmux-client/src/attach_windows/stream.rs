use std::collections::VecDeque;
use std::fs::{read_dir, remove_file, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::Duration;

use rmux_proto::{
    encode_attach_message, AttachFrameDecoder, AttachMessage, AttachedKeystroke, RmuxError,
    TerminalSize,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tracing::warn;
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_INVALID_PARAMETER, STILL_ACTIVE,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_TEMPORARY, FILE_FLAG_DELETE_ON_CLOSE,
};
use windows_sys::Win32::System::Threading::{
    GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};

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
const ATTACH_OUTPUT_SPOOL_MAX_BYTES: u64 = 64 * 1024 * 1024;
const ATTACH_OUTPUT_SPOOL_PREFIX: &str = "rmux-attach-output-";
const ATTACH_OUTPUT_SPOOL_SUFFIX: &str = ".spool";
const ATTACH_OUTPUT_BACKPRESSURE_RETRY: Duration = Duration::from_millis(5);
const ATTACH_OUTPUT_SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(250);
const ATTACH_RENDER_MAX_PENDING: Duration = Duration::from_millis(8);

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
        output.check_failure()?;
        let retry_output_delay = output.backpressure_retry_delay();

        tokio::select! {
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
            AttachMessage::Data(bytes) => {
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
            }
            AttachMessage::Render(bytes) => {
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
                output.write_render(bytes)?;
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
    completed_rx: std_mpsc::Receiver<()>,
    failure_rx: std_mpsc::Receiver<io::Error>,
    failure_wake_rx: Option<mpsc::UnboundedReceiver<()>>,
    done_rx: std_mpsc::Receiver<()>,
    worker: Option<thread::JoinHandle<()>>,
    pending: VecDeque<AttachOutputFrame>,
    pending_bytes: usize,
    spool: AttachOutputSpool,
    queued_frames: usize,
    pending_render_started_at: Option<std::time::Instant>,
    painted_frame: bool,
}

impl AttachOutputQueue {
    fn spawn<Output>(mut output: Output) -> Self
    where
        Output: Write + Send + 'static,
    {
        cleanup_orphaned_attach_output_spools();
        let (command_tx, command_rx) =
            std_mpsc::sync_channel::<Vec<u8>>(ATTACH_OUTPUT_QUEUE_CAPACITY);
        let (completed_tx, completed_rx) = std_mpsc::channel();
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
                let _ = completed_tx.send(());
            }
            let _ = done_tx.send(());
        });

        Self {
            command_tx: Some(command_tx),
            completed_rx,
            failure_rx,
            failure_wake_rx: Some(failure_wake_rx),
            done_rx,
            worker: Some(worker),
            pending: VecDeque::new(),
            pending_bytes: 0,
            spool: AttachOutputSpool::default(),
            queued_frames: 0,
            pending_render_started_at: None,
            painted_frame: false,
        }
    }

    fn write_frame(&mut self, bytes: Vec<u8>) -> std::result::Result<(), ClientError> {
        self.write_output_frame(AttachOutputFrame::strict(bytes))
    }

    fn write_render(&mut self, bytes: Vec<u8>) -> std::result::Result<(), ClientError> {
        self.write_output_frame(AttachOutputFrame::render(bytes))
    }

    fn write_output_frame(
        &mut self,
        frame: AttachOutputFrame,
    ) -> std::result::Result<(), ClientError> {
        self.check_failure()?;
        self.push_pending(frame)?;
        self.flush_pending()
    }

    fn push_pending(&mut self, frame: AttachOutputFrame) -> std::result::Result<(), ClientError> {
        if !self.spool.is_empty() {
            self.spool.push(frame).map_err(spool_error)?;
            return Ok(());
        }

        let replace_tail_len = self.pending_tail_render_len_if_replacing(frame.kind);
        let pending_bytes_after_replace = self
            .pending_bytes
            .saturating_sub(replace_tail_len.unwrap_or(0));
        if pending_bytes_after_replace.saturating_add(frame.len()) > ATTACH_OUTPUT_PENDING_MAX_BYTES
        {
            self.spool.push(frame).map_err(spool_error)?;
            if replace_tail_len.is_some() {
                self.remove_pending_tail_render();
            }
            return Ok(());
        }
        if replace_tail_len.is_some() {
            self.remove_pending_tail_render();
        }
        self.push_pending_memory(frame);
        Ok(())
    }

    fn pending_tail_render_len_if_replacing(
        &self,
        frame_kind: AttachOutputFrameKind,
    ) -> Option<usize> {
        if self.should_replace_tail_render(frame_kind) {
            return self.pending.back().map(AttachOutputFrame::len);
        }
        None
    }

    fn remove_pending_tail_render(&mut self) {
        if let Some(replaced) = self.pending.pop_back() {
            self.pending_bytes = self.pending_bytes.saturating_sub(replaced.len());
        }
    }

    fn push_pending_memory(&mut self, frame: AttachOutputFrame) {
        if frame.kind == AttachOutputFrameKind::Render && self.pending_render_started_at.is_none() {
            self.pending_render_started_at = Some(std::time::Instant::now());
        }
        self.pending_bytes = self.pending_bytes.saturating_add(frame.len());
        self.pending.push_back(frame);
    }

    fn should_replace_tail_render(&self, frame_kind: AttachOutputFrameKind) -> bool {
        frame_kind == AttachOutputFrameKind::Render
            && self
                .pending
                .back()
                .is_some_and(AttachOutputFrame::is_render)
    }

    fn flush_pending(&mut self) -> std::result::Result<(), ClientError> {
        self.drain_completed_writes();
        self.check_failure()?;
        let Some(command_tx) = self.command_tx.as_ref().cloned() else {
            return Err(ClientError::Io(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "attach output writer stopped",
            )));
        };

        loop {
            self.refill_pending_from_spool()?;
            let Some(frame) = self.pending.pop_front() else {
                break;
            };
            let strict_waiting = self.pending_has_waiting_strict();
            if frame.kind == AttachOutputFrameKind::Render
                && ((self.queued_frames != 0 && !strict_waiting)
                    || self.should_coalesce_front_render())
            {
                self.pending.push_front(frame);
                break;
            }

            let len = frame.len();
            let kind = frame.kind;
            match command_tx.try_send(frame.bytes) {
                Ok(()) => {
                    self.queued_frames = self.queued_frames.saturating_add(1);
                    self.pending_bytes = self.pending_bytes.saturating_sub(len);
                    self.painted_frame = true;
                    if kind == AttachOutputFrameKind::Render {
                        self.pending_render_started_at = None;
                        self.rearm_pending_render_timer();
                    }
                }
                Err(std_mpsc::TrySendError::Full(bytes)) => {
                    self.pending.push_front(AttachOutputFrame::new(kind, bytes));
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

    fn refill_pending_from_spool(&mut self) -> std::result::Result<(), ClientError> {
        if !self.pending.is_empty() {
            return Ok(());
        }
        let Some(frame) = self.spool.pop().map_err(spool_error)? else {
            return Ok(());
        };
        self.push_pending_memory(frame);
        Ok(())
    }

    fn should_coalesce_front_render(&self) -> bool {
        self.painted_frame && !self.pending_render_expired() && !self.pending_has_waiting_strict()
    }

    fn rearm_pending_render_timer(&mut self) {
        if self.pending_render_started_at.is_none()
            && (self.pending.iter().any(AttachOutputFrame::is_render)
                || self.spool.contains_render())
        {
            self.pending_render_started_at = Some(std::time::Instant::now());
        }
    }

    fn pending_render_expired(&self) -> bool {
        self.pending_render_started_at
            .is_some_and(|started_at| started_at.elapsed() >= ATTACH_RENDER_MAX_PENDING)
    }

    fn pending_has_waiting_strict(&self) -> bool {
        self.pending.iter().any(AttachOutputFrame::is_strict) || self.spool.contains_strict()
    }

    fn drain_completed_writes(&mut self) {
        while self.completed_rx.try_recv().is_ok() {
            self.queued_frames = self.queued_frames.saturating_sub(1);
        }
    }

    fn is_backpressured(&self) -> bool {
        !self.pending.is_empty() || !self.spool.is_empty()
    }

    #[cfg(test)]
    fn should_pause_server_reads(&self) -> bool {
        false
    }

    fn backpressure_retry_delay(&self) -> Option<Duration> {
        if !self.is_backpressured() {
            return None;
        }
        if self.pending_has_waiting_strict() {
            return Some(ATTACH_OUTPUT_BACKPRESSURE_RETRY);
        }
        let Some(started_at) = self.pending_render_started_at else {
            return Some(ATTACH_OUTPUT_BACKPRESSURE_RETRY);
        };
        let elapsed = started_at.elapsed();
        if elapsed >= ATTACH_RENDER_MAX_PENDING {
            return Some(ATTACH_OUTPUT_BACKPRESSURE_RETRY);
        }
        Some(ATTACH_RENDER_MAX_PENDING - elapsed)
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

#[derive(Debug)]
struct AttachOutputFrame {
    kind: AttachOutputFrameKind,
    bytes: Vec<u8>,
}

impl AttachOutputFrame {
    fn strict(bytes: Vec<u8>) -> Self {
        Self::new(AttachOutputFrameKind::Strict, bytes)
    }

    fn render(bytes: Vec<u8>) -> Self {
        Self::new(AttachOutputFrameKind::Render, bytes)
    }

    fn new(kind: AttachOutputFrameKind, bytes: Vec<u8>) -> Self {
        Self { kind, bytes }
    }

    fn len(&self) -> usize {
        self.bytes.len()
    }

    fn is_strict(&self) -> bool {
        self.kind == AttachOutputFrameKind::Strict
    }

    fn is_render(&self) -> bool {
        self.kind == AttachOutputFrameKind::Render
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AttachOutputFrameKind {
    Strict,
    Render,
}

#[derive(Default)]
struct AttachOutputSpool {
    file: Option<File>,
    path: Option<PathBuf>,
    frames: VecDeque<AttachOutputSpoolFrame>,
    end_offset: u64,
    outstanding_bytes: u64,
    strict_frames: usize,
    render_frames: usize,
}

#[derive(Debug)]
struct AttachOutputSpoolFrame {
    kind: AttachOutputFrameKind,
    offset: u64,
    len: usize,
}

impl AttachOutputSpool {
    fn push(&mut self, frame: AttachOutputFrame) -> io::Result<()> {
        self.push_with_limit(frame, ATTACH_OUTPUT_SPOOL_MAX_BYTES)
    }

    fn push_with_limit(&mut self, frame: AttachOutputFrame, max_bytes: u64) -> io::Result<()> {
        let replaced_tail_len = self.tail_render_len_if_replacing(frame.kind);
        let outstanding_after_replace = self
            .outstanding_bytes
            .saturating_sub(replaced_tail_len.unwrap_or(0) as u64);
        let next_outstanding = checked_spool_end_offset(outstanding_after_replace, frame.len())?;
        if next_outstanding > max_bytes {
            return Err(io::Error::other(format!(
                "attach output spool exceeded {max_bytes} bytes"
            )));
        }
        self.replace_tail_render_if_needed(frame.kind)?;
        let offset = self.next_write_offset(frame.kind);
        let len = frame.len();
        let next_end_offset = checked_spool_end_offset(offset, len)?;
        if self.should_compact_for_write(next_end_offset, max_bytes) {
            self.compact()?;
        }
        let offset = self.next_write_offset(frame.kind);
        let next_end_offset = checked_spool_end_offset(offset, len)?;
        let physical_limit = spool_physical_limit(max_bytes);
        if next_end_offset > physical_limit {
            return Err(io::Error::other(format!(
                "attach output spool exceeded {physical_limit} bytes"
            )));
        }
        let file = self.file()?;
        file.seek(SeekFrom::Start(offset))?;
        file.write_all(&frame.bytes)?;
        self.end_offset = next_end_offset;
        self.outstanding_bytes = next_outstanding;
        self.add_frame_kind(frame.kind);
        self.frames.push_back(AttachOutputSpoolFrame {
            kind: frame.kind,
            offset,
            len,
        });
        Ok(())
    }

    fn pop(&mut self) -> io::Result<Option<AttachOutputFrame>> {
        let Some(frame) = self.frames.pop_front() else {
            self.cleanup()?;
            return Ok(None);
        };
        let file = self.file()?;
        file.seek(SeekFrom::Start(frame.offset))?;
        let mut bytes = vec![0_u8; frame.len];
        file.read_exact(&mut bytes)?;
        self.outstanding_bytes = self.outstanding_bytes.saturating_sub(frame.len as u64);
        self.remove_frame_kind(frame.kind);
        if self.frames.is_empty() {
            self.cleanup()?;
        }
        Ok(Some(AttachOutputFrame::new(frame.kind, bytes)))
    }

    fn contains_strict(&self) -> bool {
        self.strict_frames > 0
    }

    fn contains_render(&self) -> bool {
        self.render_frames > 0
    }

    fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    fn next_write_offset(&self, kind: AttachOutputFrameKind) -> u64 {
        if kind == AttachOutputFrameKind::Render {
            if let Some(frame) = self.frames.back().filter(|frame| frame.is_render()) {
                return frame.offset;
            }
        }
        self.end_offset
    }

    fn should_compact_for_write(&self, next_end_offset: u64, max_bytes: u64) -> bool {
        if next_end_offset <= max_bytes {
            return false;
        }
        next_end_offset > spool_physical_limit(max_bytes)
            || self.reclaimable_bytes() >= spool_compact_threshold(max_bytes)
    }

    fn reclaimable_bytes(&self) -> u64 {
        self.frames.front().map_or(0, |frame| frame.offset)
    }

    fn tail_render_len_if_replacing(&self, kind: AttachOutputFrameKind) -> Option<usize> {
        if kind != AttachOutputFrameKind::Render {
            return None;
        }
        self.frames
            .back()
            .filter(|frame| frame.is_render())
            .map(|frame| frame.len)
    }

    fn replace_tail_render_if_needed(&mut self, kind: AttachOutputFrameKind) -> io::Result<()> {
        if kind != AttachOutputFrameKind::Render
            || !self
                .frames
                .back()
                .is_some_and(|frame| frame.kind == AttachOutputFrameKind::Render)
        {
            return Ok(());
        }

        let Some(frame) = self.frames.pop_back() else {
            return Ok(());
        };
        self.outstanding_bytes = self.outstanding_bytes.saturating_sub(frame.len as u64);
        self.remove_frame_kind(frame.kind);
        self.end_offset = frame.offset;
        if let Some(file) = self.file.as_mut() {
            file.set_len(self.end_offset)?;
        }
        Ok(())
    }

    fn compact(&mut self) -> io::Result<()> {
        if self.frames.is_empty() {
            self.cleanup()?;
            return Ok(());
        }
        let file = self.file.as_mut().ok_or_else(|| {
            io::Error::other("attach output spool has frames without a backing file")
        })?;
        let mut next_offset = 0_u64;
        for frame in &mut self.frames {
            if frame.offset != next_offset {
                file.seek(SeekFrom::Start(frame.offset))?;
                let mut bytes = vec![0_u8; frame.len];
                file.read_exact(&mut bytes)?;
                file.seek(SeekFrom::Start(next_offset))?;
                file.write_all(&bytes)?;
                frame.offset = next_offset;
            }
            next_offset = checked_spool_end_offset(next_offset, frame.len)?;
        }
        file.set_len(next_offset)?;
        self.end_offset = next_offset;
        Ok(())
    }

    fn file(&mut self) -> io::Result<&mut File> {
        if self.file.is_none() {
            let path = attach_output_spool_path();
            let file = open_attach_output_spool_file(&path)?;
            self.path = Some(path);
            self.file = Some(file);
            self.end_offset = 0;
        }
        Ok(self
            .file
            .as_mut()
            .expect("attach output spool file exists after initialization"))
    }

    fn cleanup(&mut self) -> io::Result<()> {
        self.frames.clear();
        self.end_offset = 0;
        self.outstanding_bytes = 0;
        self.strict_frames = 0;
        self.render_frames = 0;
        drop(self.file.take());
        if let Some(path) = self.path.take() {
            match remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    fn add_frame_kind(&mut self, kind: AttachOutputFrameKind) {
        match kind {
            AttachOutputFrameKind::Strict => {
                self.strict_frames = self.strict_frames.saturating_add(1);
            }
            AttachOutputFrameKind::Render => {
                self.render_frames = self.render_frames.saturating_add(1);
            }
        }
    }

    fn remove_frame_kind(&mut self, kind: AttachOutputFrameKind) {
        match kind {
            AttachOutputFrameKind::Strict => {
                self.strict_frames = self.strict_frames.saturating_sub(1);
            }
            AttachOutputFrameKind::Render => {
                self.render_frames = self.render_frames.saturating_sub(1);
            }
        }
    }
}

impl AttachOutputSpoolFrame {
    fn is_render(&self) -> bool {
        self.kind == AttachOutputFrameKind::Render
    }
}

impl Drop for AttachOutputSpool {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

fn attach_output_spool_path() -> PathBuf {
    static SPOOL_COUNTER: AtomicU64 = AtomicU64::new(0);
    let sequence = SPOOL_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(attach_output_spool_file_name(std::process::id(), sequence))
}

fn open_attach_output_spool_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options
        .create_new(true)
        .read(true)
        .write(true)
        .share_mode(0)
        .custom_flags(FILE_ATTRIBUTE_TEMPORARY | FILE_FLAG_DELETE_ON_CLOSE);
    options.open(path)
}

fn checked_spool_end_offset(offset: u64, len: usize) -> io::Result<u64> {
    let len = u64::try_from(len)
        .map_err(|_| io::Error::other("attach output frame is too large to spool"))?;
    offset
        .checked_add(len)
        .ok_or_else(|| io::Error::other("attach output spool offset overflowed"))
}

fn spool_compact_threshold(max_bytes: u64) -> u64 {
    (max_bytes / 4).max(1)
}

fn spool_physical_limit(max_bytes: u64) -> u64 {
    max_bytes.saturating_add(spool_compact_threshold(max_bytes))
}

fn cleanup_orphaned_attach_output_spools() {
    static CLEANED: OnceLock<()> = OnceLock::new();
    CLEANED.get_or_init(cleanup_orphaned_attach_output_spools_now);
}

fn cleanup_orphaned_attach_output_spools_now() {
    let Ok(entries) = read_dir(std::env::temp_dir()) else {
        return;
    };
    let current_pid = std::process::id();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(pid) = attach_output_spool_pid(file_name) else {
            continue;
        };
        if pid == current_pid || attach_output_spool_owner_is_running(pid) {
            continue;
        }
        if let Err(error) = remove_file(&path) {
            if error.kind() != io::ErrorKind::NotFound {
                warn!(
                    path = %path.display(),
                    "failed to remove orphaned attach output spool: {error}"
                );
            }
        }
    }
}

fn attach_output_spool_pid(file_name: &str) -> Option<u32> {
    let rest = file_name.strip_prefix(ATTACH_OUTPUT_SPOOL_PREFIX)?;
    let rest = rest.strip_suffix(ATTACH_OUTPUT_SPOOL_SUFFIX)?;
    let (pid, sequence) = rest.split_once('-')?;
    sequence.parse::<u64>().ok()?;
    pid.parse().ok()
}

fn attach_output_spool_owner_is_running(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    // SAFETY: OpenProcess is called with a plain PID and no inherited handle;
    // the returned handle is checked for null before use, passed only to
    // GetExitCodeProcess with a valid out-pointer to a local, and always
    // released with CloseHandle on the non-null path.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return GetLastError() != ERROR_INVALID_PARAMETER;
        }
        let mut exit_code = 0;
        let running =
            GetExitCodeProcess(handle, &mut exit_code) != 0 && exit_code == STILL_ACTIVE as u32;
        let _ = CloseHandle(handle);
        running
    }
}

fn attach_output_spool_file_name(pid: u32, sequence: u64) -> String {
    format!("{ATTACH_OUTPUT_SPOOL_PREFIX}{pid}-{sequence}{ATTACH_OUTPUT_SPOOL_SUFFIX}")
}

fn spool_error(error: io::Error) -> ClientError {
    ClientError::Io(io::Error::new(
        error.kind(),
        format!("failed to spool blocked attach output: {error}"),
    ))
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
    normal_tracking: bool,
    button_tracking: bool,
    any_tracking: bool,
    tail: Vec<u8>,
}

impl WindowsConsoleMouseTracker {
    fn observe(&mut self, bytes: &[u8]) -> Option<bool> {
        const TAIL_LEN: usize = 7;

        if bytes.is_empty() {
            return None;
        }

        let mut combined = Vec::with_capacity(self.tail.len() + bytes.len());
        combined.extend_from_slice(&self.tail);
        combined.extend_from_slice(bytes);

        let mut observed = None;
        for index in 0..combined.len() {
            let tail = &combined[index..];
            if tail.starts_with(b"\x1b[?1000h") {
                self.normal_tracking = true;
                observed = Some(self.mouse_input_enabled());
            } else if tail.starts_with(b"\x1b[?1000l") {
                self.normal_tracking = false;
                observed = Some(self.mouse_input_enabled());
            } else if tail.starts_with(b"\x1b[?1002h") {
                self.button_tracking = true;
                observed = Some(self.mouse_input_enabled());
            } else if tail.starts_with(b"\x1b[?1002l") {
                self.button_tracking = false;
                observed = Some(self.mouse_input_enabled());
            } else if tail.starts_with(b"\x1b[?1003h") {
                self.any_tracking = true;
                observed = Some(self.mouse_input_enabled());
            } else if tail.starts_with(b"\x1b[?1003l") {
                self.any_tracking = false;
                observed = Some(self.mouse_input_enabled());
            } else if tail.starts_with(b"\x1b[?1006h") {
                // SGR mouse encoding is independent from DECSET 1000/1002/1003 tracking.
            } else if tail.starts_with(b"\x1b[?1006l") {
                // Disabling SGR encoding must not disable Windows console mouse input.
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

    const fn mouse_input_enabled(&self) -> bool {
        self.normal_tracking || self.button_tracking || self.any_tracking
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
