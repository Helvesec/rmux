use std::io::{self, Write};
use std::os::windows::io::AsRawHandle;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use tokio::sync::mpsc;
use windows_sys::Win32::Foundation::{GetLastError, ERROR_NOT_FOUND, HANDLE};
use windows_sys::Win32::System::IO::CancelSynchronousIo;

use super::stream::AttachOutputFence;

pub(super) const ATTACH_OUTPUT_QUEUE_CAPACITY: usize = 64;
const ATTACH_OUTPUT_CANCEL_RETRY: Duration = Duration::from_millis(10);

pub(super) enum AttachOutputTrySendError<T> {
    Full(T),
    Closed(T),
}

enum AttachOutputCommand {
    Frame(Vec<u8>),
    Fence(AttachOutputFence),
}

pub(super) struct AttachOutputWorker {
    command_tx: Option<std_mpsc::SyncSender<AttachOutputCommand>>,
    completed_frame_rx: std_mpsc::Receiver<()>,
    failure_rx: std_mpsc::Receiver<io::Error>,
    failure_wake_rx: Option<mpsc::UnboundedReceiver<()>>,
    fence_wake_rx: Option<mpsc::UnboundedReceiver<AttachOutputFence>>,
    done_rx: std_mpsc::Receiver<()>,
    worker: Option<thread::JoinHandle<()>>,
    cancellation_requested: Arc<AtomicBool>,
}

impl AttachOutputWorker {
    #[cfg(test)]
    pub(super) fn spawn<Output>(output: Output) -> Self
    where
        Output: Write + Send + 'static,
    {
        Self::spawn_with_fence_flush(output, Write::flush)
    }

    pub(super) fn spawn_with_fence_flush<Output, FenceFlush>(
        mut output: Output,
        mut fence_flush: FenceFlush,
    ) -> Self
    where
        Output: Write + Send + 'static,
        FenceFlush: FnMut(&mut Output) -> io::Result<()> + Send + 'static,
    {
        let (command_tx, command_rx) =
            std_mpsc::sync_channel::<AttachOutputCommand>(ATTACH_OUTPUT_QUEUE_CAPACITY);
        let (completed_frame_tx, completed_frame_rx) = std_mpsc::channel();
        let (failure_tx, failure_rx) = std_mpsc::channel();
        let (failure_wake_tx, failure_wake_rx) = mpsc::unbounded_channel();
        let (fence_wake_tx, fence_wake_rx) = mpsc::unbounded_channel();
        let (done_tx, done_rx) = std_mpsc::channel();
        let cancellation_requested = Arc::new(AtomicBool::new(false));
        let worker_cancellation = Arc::clone(&cancellation_requested);
        let worker = thread::spawn(move || {
            while let Ok(command) = command_rx.recv() {
                if worker_cancellation.load(Ordering::SeqCst) {
                    break;
                }
                match command {
                    AttachOutputCommand::Frame(bytes) => {
                        if let Err(error) = output.write_all(&bytes).and_then(|()| output.flush()) {
                            let _ = failure_tx.send(error);
                            let _ = failure_wake_tx.send(());
                            break;
                        }
                        let _ = completed_frame_tx.send(());
                    }
                    AttachOutputCommand::Fence(fence) => {
                        if let Err(error) = fence_flush(&mut output) {
                            let _ = failure_tx.send(error);
                            let _ = failure_wake_tx.send(());
                            break;
                        }
                        if fence_wake_tx.send(fence).is_err() {
                            break;
                        }
                    }
                }
            }
            // Some production writers perform final synchronous console I/O
            // from Drop. Keep the cancellation loop active until that I/O and
            // all owned handles are gone, then publish worker completion.
            drop(output);
            let _ = done_tx.send(());
        });

        Self {
            command_tx: Some(command_tx),
            completed_frame_rx,
            failure_rx,
            failure_wake_rx: Some(failure_wake_rx),
            fence_wake_rx: Some(fence_wake_rx),
            done_rx,
            worker: Some(worker),
            cancellation_requested,
        }
    }

    pub(super) fn try_send_frame(
        &self,
        bytes: Vec<u8>,
    ) -> Result<(), AttachOutputTrySendError<Vec<u8>>> {
        let Some(command_tx) = self.command_tx.as_ref() else {
            return Err(AttachOutputTrySendError::Closed(bytes));
        };
        match command_tx.try_send(AttachOutputCommand::Frame(bytes)) {
            Ok(()) => Ok(()),
            Err(std_mpsc::TrySendError::Full(AttachOutputCommand::Frame(bytes))) => {
                Err(AttachOutputTrySendError::Full(bytes))
            }
            Err(std_mpsc::TrySendError::Disconnected(AttachOutputCommand::Frame(bytes))) => {
                Err(AttachOutputTrySendError::Closed(bytes))
            }
            Err(
                std_mpsc::TrySendError::Full(AttachOutputCommand::Fence(_))
                | std_mpsc::TrySendError::Disconnected(AttachOutputCommand::Fence(_)),
            ) => unreachable!("frame enqueue must return the submitted frame"),
        }
    }

    pub(super) fn try_send_fence(
        &self,
        fence: AttachOutputFence,
    ) -> Result<(), AttachOutputTrySendError<AttachOutputFence>> {
        let Some(command_tx) = self.command_tx.as_ref() else {
            return Err(AttachOutputTrySendError::Closed(fence));
        };
        match command_tx.try_send(AttachOutputCommand::Fence(fence)) {
            Ok(()) => Ok(()),
            Err(std_mpsc::TrySendError::Full(AttachOutputCommand::Fence(fence))) => {
                Err(AttachOutputTrySendError::Full(fence))
            }
            Err(std_mpsc::TrySendError::Disconnected(AttachOutputCommand::Fence(fence))) => {
                Err(AttachOutputTrySendError::Closed(fence))
            }
            Err(
                std_mpsc::TrySendError::Full(AttachOutputCommand::Frame(_))
                | std_mpsc::TrySendError::Disconnected(AttachOutputCommand::Frame(_)),
            ) => unreachable!("fence enqueue must return the submitted fence"),
        }
    }

    pub(super) fn drain_completed_frames(&self) -> usize {
        let mut completed = 0_usize;
        while self.completed_frame_rx.try_recv().is_ok() {
            completed = completed.saturating_add(1);
        }
        completed
    }

    pub(super) fn recv_completed_frame_timeout(
        &self,
        timeout: Duration,
    ) -> Result<(), std_mpsc::RecvTimeoutError> {
        self.completed_frame_rx.recv_timeout(timeout)
    }

    pub(super) fn check_failure(&self) -> io::Result<()> {
        match self.failure_rx.try_recv() {
            Ok(error) => Err(error),
            Err(std_mpsc::TryRecvError::Empty | std_mpsc::TryRecvError::Disconnected) => Ok(()),
        }
    }

    pub(super) fn take_failure_notifications(&mut self) -> mpsc::UnboundedReceiver<()> {
        self.failure_wake_rx
            .take()
            .expect("attach output failure notifications should only be taken once")
    }

    pub(super) fn take_fence_notifications(
        &mut self,
    ) -> mpsc::UnboundedReceiver<AttachOutputFence> {
        self.fence_wake_rx
            .take()
            .expect("attach output fence notifications should only be taken once")
    }

    pub(super) fn is_stopped(&self) -> bool {
        self.worker.is_none()
    }

    pub(super) fn join_after_drain(&mut self) -> io::Result<()> {
        self.command_tx.take();
        self.wait_for_worker_exit(false)
    }

    pub(super) fn cancel_and_join(&mut self) -> io::Result<()> {
        self.cancellation_requested.store(true, Ordering::SeqCst);
        self.command_tx.take();
        self.wait_for_worker_exit(true)
    }

    fn wait_for_worker_exit(&mut self, cancel: bool) -> io::Result<()> {
        let Some(worker) = self.worker.take() else {
            return Ok(());
        };
        let thread_handle = worker.as_raw_handle() as HANDLE;
        let mut cancellation_error = None;

        loop {
            if cancel {
                let cancelled = unsafe {
                    // SAFETY: the raw handle is borrowed from the live output
                    // worker JoinHandle. It remains valid until the worker is
                    // joined below, and CancelSynchronousIo does not take
                    // ownership of it.
                    CancelSynchronousIo(thread_handle)
                };
                if cancelled == 0 {
                    let error_code = unsafe {
                        // SAFETY: GetLastError has no preconditions and is read
                        // immediately after the failed Win32 call above.
                        GetLastError()
                    };
                    if error_code != ERROR_NOT_FOUND && cancellation_error.is_none() {
                        cancellation_error = Some(io::Error::from_raw_os_error(error_code as i32));
                    }
                }
            }

            match self.done_rx.recv_timeout(ATTACH_OUTPUT_CANCEL_RETRY) {
                Ok(()) | Err(std_mpsc::RecvTimeoutError::Disconnected) => break,
                Err(std_mpsc::RecvTimeoutError::Timeout) if cancel => continue,
                Err(std_mpsc::RecvTimeoutError::Timeout) => continue,
            }
        }

        worker
            .join()
            .map_err(|_| io::Error::other("attach output writer thread panicked"))?;
        if let Some(error) = cancellation_error {
            return Err(io::Error::new(
                error.kind(),
                format!("failed to cancel attach output writer: {error}"),
            ));
        }
        Ok(())
    }
}

impl Drop for AttachOutputWorker {
    fn drop(&mut self) {
        let _ = self.cancel_and_join();
    }
}
