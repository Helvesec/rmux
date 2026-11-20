use std::collections::VecDeque;
use std::io::{self, Write};
use std::sync::mpsc as std_mpsc;
use std::time::Duration;

use tokio::sync::mpsc;

use crate::ClientError;

use super::super::output_worker::{AttachOutputTrySendError, AttachOutputWorker};
use super::spool::{cleanup_orphaned_attach_output_spools, spool_error, AttachOutputSpool};
use super::AttachOutputFence;

pub(super) const ATTACH_OUTPUT_PENDING_MAX_BYTES: usize = 4 * 1024 * 1024;
pub(super) const ATTACH_OUTPUT_BACKPRESSURE_RETRY: Duration = Duration::from_millis(5);
const ATTACH_OUTPUT_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
pub(super) const ATTACH_RENDER_MAX_PENDING: Duration = Duration::from_millis(8);

pub(super) struct AttachOutputQueue {
    worker: AttachOutputWorker,
    pub(super) pending: VecDeque<AttachOutputFrame>,
    pub(super) pending_bytes: usize,
    pub(super) spool: AttachOutputSpool,
    pending_fences: VecDeque<AttachOutputFence>,
    next_fence_sequence: u64,
    pub(super) queued_frames: usize,
    pub(super) pending_render_started_at: Option<std::time::Instant>,
    pub(super) painted_frame: bool,
}

impl AttachOutputQueue {
    #[cfg(test)]
    pub(super) fn spawn<Output>(output: Output) -> Self
    where
        Output: Write + Send + 'static,
    {
        Self::spawn_with_fence_flush(output, Write::flush)
    }

    pub(super) fn spawn_with_fence_flush<Output, FenceFlush>(
        output: Output,
        output_fence_flush: FenceFlush,
    ) -> Self
    where
        Output: Write + Send + 'static,
        FenceFlush: FnMut(&mut Output) -> io::Result<()> + Send + 'static,
    {
        cleanup_orphaned_attach_output_spools();
        Self {
            worker: AttachOutputWorker::spawn_with_fence_flush(output, output_fence_flush),
            pending: VecDeque::new(),
            pending_bytes: 0,
            spool: AttachOutputSpool::default(),
            pending_fences: VecDeque::new(),
            next_fence_sequence: 0,
            queued_frames: 0,
            pending_render_started_at: None,
            painted_frame: false,
        }
    }

    pub(super) fn write_frame(&mut self, bytes: Vec<u8>) -> std::result::Result<(), ClientError> {
        self.write_output_frame(AttachOutputFrame::strict(bytes))
    }

    pub(super) fn write_render(&mut self, bytes: Vec<u8>) -> std::result::Result<(), ClientError> {
        self.write_output_frame(AttachOutputFrame::render(bytes))
    }

    pub(super) fn request_fence(&mut self) -> std::result::Result<AttachOutputFence, ClientError> {
        let fence = AttachOutputFence(self.next_fence_sequence);
        self.next_fence_sequence = self.next_fence_sequence.checked_add(1).ok_or_else(|| {
            ClientError::Io(io::Error::other("attach output fence sequence overflowed"))
        })?;
        self.pending_fences.push_back(fence);
        self.flush_pending()?;
        Ok(fence)
    }

    fn write_output_frame(
        &mut self,
        frame: AttachOutputFrame,
    ) -> std::result::Result<(), ClientError> {
        self.check_failure()?;
        self.push_pending(frame)?;
        self.flush_pending()
    }

    pub(super) fn push_pending(
        &mut self,
        frame: AttachOutputFrame,
    ) -> std::result::Result<(), ClientError> {
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

    pub(super) fn flush_pending(&mut self) -> std::result::Result<(), ClientError> {
        self.drain_completed_writes();
        self.check_failure()?;

        loop {
            self.refill_pending_from_spool()?;
            let Some(frame) = self.pending.pop_front() else {
                break;
            };
            let strict_waiting =
                self.pending_has_waiting_strict() || !self.pending_fences.is_empty();
            if frame.kind == AttachOutputFrameKind::Render
                && ((self.queued_frames != 0 && !strict_waiting)
                    || self.should_coalesce_front_render())
            {
                self.pending.push_front(frame);
                break;
            }

            let len = frame.len();
            let kind = frame.kind;
            match self.worker.try_send_frame(frame.bytes) {
                Ok(()) => {
                    self.queued_frames = self.queued_frames.saturating_add(1);
                    self.pending_bytes = self.pending_bytes.saturating_sub(len);
                    self.painted_frame = true;
                    if kind == AttachOutputFrameKind::Render {
                        self.pending_render_started_at = None;
                        self.rearm_pending_render_timer();
                    }
                }
                Err(AttachOutputTrySendError::Full(bytes)) => {
                    self.pending.push_front(AttachOutputFrame::new(kind, bytes));
                    break;
                }
                Err(AttachOutputTrySendError::Closed(_)) => {
                    return Err(ClientError::Io(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "attach output writer stopped",
                    )));
                }
            }
        }

        if self.pending.is_empty() && self.spool.is_empty() {
            while let Some(fence) = self.pending_fences.pop_front() {
                match self.worker.try_send_fence(fence) {
                    Ok(()) => {}
                    Err(AttachOutputTrySendError::Full(fence)) => {
                        self.pending_fences.push_front(fence);
                        break;
                    }
                    Err(AttachOutputTrySendError::Closed(_)) => {
                        return Err(ClientError::Io(io::Error::new(
                            io::ErrorKind::BrokenPipe,
                            "attach output writer stopped",
                        )));
                    }
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
        self.painted_frame
            && !self.pending_render_expired()
            && !self.pending_has_waiting_strict()
            && self.pending_fences.is_empty()
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

    pub(super) fn drain_completed_writes(&mut self) {
        for _ in 0..self.worker.drain_completed_frames() {
            self.queued_frames = self.queued_frames.saturating_sub(1);
        }
    }

    fn is_backpressured(&self) -> bool {
        !self.pending.is_empty() || !self.spool.is_empty() || !self.pending_fences.is_empty()
    }

    #[cfg(test)]
    pub(super) fn should_pause_server_reads(&self) -> bool {
        false
    }

    pub(super) fn backpressure_retry_delay(&self) -> Option<Duration> {
        if !self.is_backpressured() {
            return None;
        }
        if self.pending_has_waiting_strict() || !self.pending_fences.is_empty() {
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

    pub(super) fn check_failure(&mut self) -> std::result::Result<(), ClientError> {
        self.worker.check_failure().map_err(ClientError::Io)
    }

    pub(super) fn take_failure_notifications(&mut self) -> mpsc::UnboundedReceiver<()> {
        self.worker.take_failure_notifications()
    }

    pub(super) fn take_fence_notifications(
        &mut self,
    ) -> mpsc::UnboundedReceiver<AttachOutputFence> {
        self.worker.take_fence_notifications()
    }

    pub(super) fn cancel_and_join(&mut self) -> std::result::Result<(), ClientError> {
        self.pending.clear();
        self.pending_bytes = 0;
        self.pending_fences.clear();
        let worker_result = self.worker.cancel_and_join().map_err(ClientError::Io);
        let spool_result = self.spool.cleanup().map_err(spool_error);
        worker_result?;
        spool_result
    }

    pub(super) fn is_stopped(&self) -> bool {
        self.worker.is_stopped()
    }

    pub(super) fn finish(&mut self) -> std::result::Result<(), ClientError> {
        if self.is_stopped() {
            return Ok(());
        }
        self.finish_with_timeout(ATTACH_OUTPUT_DRAIN_TIMEOUT)
    }

    fn finish_with_timeout(
        &mut self,
        drain_timeout: Duration,
    ) -> std::result::Result<(), ClientError> {
        let deadline = std::time::Instant::now() + drain_timeout;
        loop {
            self.flush_pending()?;
            self.drain_completed_writes();
            self.check_failure()?;
            if self.pending.is_empty()
                && self.spool.is_empty()
                && self.pending_fences.is_empty()
                && self.queued_frames == 0
            {
                return self.worker.join_after_drain().map_err(ClientError::Io);
            }

            let now = std::time::Instant::now();
            if now >= deadline {
                let timeout_error = ClientError::Io(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "timed out draining strict attach output before shutdown",
                ));
                self.cancel_and_join()?;
                return Err(timeout_error);
            }
            let wait = (deadline - now).min(ATTACH_OUTPUT_BACKPRESSURE_RETRY);
            match self.worker.recv_completed_frame_timeout(wait) {
                Ok(()) => {
                    self.queued_frames = self.queued_frames.saturating_sub(1);
                }
                Err(std_mpsc::RecvTimeoutError::Timeout) => {}
                Err(std_mpsc::RecvTimeoutError::Disconnected) => {
                    self.check_failure()?;
                    return Err(ClientError::Io(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "attach output writer stopped before pending output drained",
                    )));
                }
            }
        }
    }
}

#[derive(Debug)]
pub(super) struct AttachOutputFrame {
    pub(super) kind: AttachOutputFrameKind,
    pub(super) bytes: Vec<u8>,
}

impl AttachOutputFrame {
    pub(super) fn strict(bytes: Vec<u8>) -> Self {
        Self::new(AttachOutputFrameKind::Strict, bytes)
    }

    pub(super) fn render(bytes: Vec<u8>) -> Self {
        Self::new(AttachOutputFrameKind::Render, bytes)
    }

    pub(super) fn new(kind: AttachOutputFrameKind, bytes: Vec<u8>) -> Self {
        Self { kind, bytes }
    }

    pub(super) fn len(&self) -> usize {
        self.bytes.len()
    }

    pub(super) fn is_strict(&self) -> bool {
        self.kind == AttachOutputFrameKind::Strict
    }

    pub(super) fn is_render(&self) -> bool {
        self.kind == AttachOutputFrameKind::Render
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AttachOutputFrameKind {
    Strict,
    Render,
}

pub(super) async fn finish_attach_output(
    mut output: AttachOutputQueue,
) -> std::result::Result<(), ClientError> {
    tokio::task::spawn_blocking(move || output.finish())
        .await
        .map_err(|error| {
            ClientError::Io(io::Error::other(format!(
                "attach output drain task failed: {error}"
            )))
        })?
}
