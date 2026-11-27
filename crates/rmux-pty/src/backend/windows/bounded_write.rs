use std::collections::BTreeSet;
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{Mutex, MutexGuard, TryLockError};
use std::thread;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    GetLastError, ERROR_IO_PENDING, ERROR_NOT_FOUND, ERROR_OPERATION_ABORTED, HANDLE, WAIT_FAILED,
    WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Storage::FileSystem::WriteFile;
use windows_sys::Win32::System::Threading::{
    CreateEventW, ResetEvent, WaitForSingleObject, INFINITE,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};

mod drain;

use drain::flush_with_timeout;

const WRITE_CHUNK_SIZE: usize = 4 * 1024;

#[derive(Debug, Eq, PartialEq)]
enum CancellationOutcome {
    Completed(usize),
    Cancelled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WriteChunkOutcome {
    Progress(usize),
    Stalled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WriteStallPhase {
    Primary,
    Recovery,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
enum InputOperationKind {
    Idle,
    PipeWrite,
    Drain,
}

impl InputOperationKind {
    fn load(kind: &AtomicU8) -> Self {
        match kind.load(Ordering::Acquire) {
            value if value == Self::PipeWrite as u8 => Self::PipeWrite,
            value if value == Self::Drain as u8 => Self::Drain,
            _ => Self::Idle,
        }
    }
}

#[derive(Debug)]
struct InputOperationQueue {
    // The operation mutex provides exclusion; tickets preserve the admission
    // order that `try_lock` alone cannot guarantee between queued writers.
    next: AtomicU64,
    serving: AtomicU64,
    cancelled: Mutex<BTreeSet<u64>>,
}

impl InputOperationQueue {
    fn new() -> Self {
        Self {
            next: AtomicU64::new(0),
            serving: AtomicU64::new(0),
            cancelled: Mutex::new(BTreeSet::new()),
        }
    }

    fn issue(&self) -> u64 {
        self.next.fetch_add(1, Ordering::Relaxed)
    }

    fn is_serving(&self, ticket: u64) -> bool {
        self.serving.load(Ordering::Acquire) == ticket
    }

    fn cancel(&self, ticket: u64) {
        let mut cancelled = self
            .cancelled
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.is_serving(ticket) {
            self.advance(ticket, &mut cancelled);
        } else {
            cancelled.insert(ticket);
        }
    }

    fn complete(&self, ticket: u64) {
        let mut cancelled = self
            .cancelled
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        debug_assert!(self.is_serving(ticket));
        self.advance(ticket, &mut cancelled);
    }

    fn advance(&self, ticket: u64, cancelled: &mut BTreeSet<u64>) {
        let mut next = ticket.wrapping_add(1);
        while cancelled.remove(&next) {
            next = next.wrapping_add(1);
        }
        self.serving.store(next, Ordering::Release);
    }
}

struct InputOperationGuard<'a> {
    _guard: MutexGuard<'a, ()>,
    kind: &'a AtomicU8,
    queue: &'a InputOperationQueue,
    ticket: u64,
}

impl Drop for InputOperationGuard<'_> {
    fn drop(&mut self) {
        // Publish idle before the mutex unlocks. A waiter can briefly observe
        // idle while `try_lock` still fails, but cannot enter until this guard
        // is released and the next owner publishes its own kind.
        self.kind
            .store(InputOperationKind::Idle as u8, Ordering::Release);
        self.queue.complete(self.ticket);
    }
}

#[derive(Debug)]
pub(super) struct OverlappedPipeWriter {
    handle: OwnedHandle,
    event: OwnedHandle,
    operation: Mutex<()>,
    operation_kind: AtomicU8,
    operation_queue: InputOperationQueue,
}

impl OverlappedPipeWriter {
    pub(super) fn new(handle: OwnedHandle) -> io::Result<Self> {
        let event = unsafe {
            // SAFETY: security attributes and the optional name are
            // intentionally null. The event starts nonsignaled.
            CreateEventW(std::ptr::null(), 1, 0, std::ptr::null())
        };
        if event.is_null() {
            return Err(last_os_error());
        }
        // SAFETY: `CreateEventW` returned a uniquely owned live handle.
        let event = unsafe { OwnedHandle::from_raw_handle(event as _) };
        Ok(Self {
            handle,
            event,
            operation: Mutex::new(()),
            operation_kind: AtomicU8::new(InputOperationKind::Idle as u8),
            operation_queue: InputOperationQueue::new(),
        })
    }

    pub(super) fn write_all_with_timeout(&self, bytes: &[u8], timeout: Duration) -> io::Result<()> {
        self.write_all_with_stall_recovery_observed(bytes, timeout, Duration::ZERO, || {})
    }

    pub(super) fn write_all_with_stall_recovery(
        &self,
        bytes: &[u8],
        timeout: Duration,
        recovery_grace: Duration,
    ) -> io::Result<()> {
        self.write_all_with_stall_recovery_observed(bytes, timeout, recovery_grace, || {})
    }

    /// Runs a non-pipe input operation after ConPTY consumes prior pipe bytes.
    ///
    /// Some Windows console input must use records rather than the ConPTY
    /// pipe, but it still belongs to the pane's single input stream. Sharing
    /// this mutex prevents such an operation from overtaking an admitted pipe
    /// write. Flushing is also required after a completed `WriteFile`: that
    /// completion only means the pipe accepted the bytes, while ConPTY may not
    /// yet have converted them into console input records. An admitted pipe
    /// writer already owns the established inactivity/recovery deadline, so
    /// its healthy progress must not consume this operation's drain budget.
    /// Waiting behind another drain is bounded, and this operation's own drain
    /// is bounded after admission. A timed-out drain is cancelled on the exact
    /// thread performing it before the mutex is released, so its callback
    /// cannot run later. A successful flush also needs a live-reader check:
    /// closing the reader can discard buffered bytes and complete the flush
    /// successfully without consuming them.
    pub(super) fn run_after_pipe_drain<T>(
        &self,
        timeout: Duration,
        operation: impl FnOnce() -> io::Result<T>,
    ) -> io::Result<T> {
        let _operation = lock_input_operation(
            &self.operation,
            &self.operation_kind,
            &self.operation_queue,
            InputOperationKind::Drain,
            timeout,
        )
        .map_err(|error| contextual_timeout(error, || drain_timeout(timeout)))?;
        let started = Instant::now();
        flush_with_timeout(self.handle.as_raw_handle() as HANDLE, timeout).map_err(|error| {
            match error.kind() {
                io::ErrorKind::TimedOut => drain_timeout(timeout),
                _ => error,
            }
        })?;
        let remaining = timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(drain_timeout(timeout));
        }
        // A flush completion alone does not prove the reader is still open.
        // Issue a real zero-byte WriteFile on this byte-mode pipe: it checks
        // the connection without adding input. Do not use write_all here,
        // which skips empty slices (and would reacquire the operation lock).
        if self.write_chunk(&[], remaining)? == WriteChunkOutcome::Stalled
            || started.elapsed() >= timeout
        {
            return Err(drain_timeout(timeout));
        }
        operation()
    }

    fn write_all_with_stall_recovery_observed(
        &self,
        mut bytes: &[u8],
        timeout: Duration,
        recovery_grace: Duration,
        mut observe_stall: impl FnMut(),
    ) -> io::Result<()> {
        let total_timeout = timeout.saturating_add(recovery_grace);
        let _operation = lock_input_operation(
            &self.operation,
            &self.operation_kind,
            &self.operation_queue,
            InputOperationKind::PipeWrite,
            total_timeout,
        )
        .map_err(|error| contextual_timeout(error, || write_timeout(timeout, recovery_grace)))?;
        let mut last_progress = Instant::now();
        let mut phase = WriteStallPhase::Primary;
        while !bytes.is_empty() {
            let phase_timeout = match phase {
                WriteStallPhase::Primary => timeout,
                WriteStallPhase::Recovery => recovery_grace,
            };
            let remaining = phase_timeout.saturating_sub(last_progress.elapsed());
            if remaining.is_zero() {
                if begin_stall_recovery(&mut phase, recovery_grace) {
                    last_progress = Instant::now();
                    observe_stall();
                    continue;
                }
                return Err(write_timeout(timeout, recovery_grace));
            }
            let chunk_len = bytes.len().min(WRITE_CHUNK_SIZE);
            match self.write_chunk(&bytes[..chunk_len], remaining)? {
                WriteChunkOutcome::Progress(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "ConPTY input pipe accepted zero bytes",
                    ));
                }
                WriteChunkOutcome::Progress(bytes_written) => {
                    bytes = &bytes[bytes_written..];
                    last_progress = Instant::now();
                    phase = WriteStallPhase::Primary;
                }
                WriteChunkOutcome::Stalled => {
                    if !begin_stall_recovery(&mut phase, recovery_grace) {
                        return Err(write_timeout(timeout, recovery_grace));
                    }
                    last_progress = Instant::now();
                    observe_stall();
                }
            }
        }
        Ok(())
    }

    fn write_chunk(&self, bytes: &[u8], remaining: Duration) -> io::Result<WriteChunkOutcome> {
        let reset = unsafe {
            // SAFETY: `event` is a live manual-reset event and writes are
            // serialized by `operation`.
            ResetEvent(self.event.as_raw_handle() as HANDLE)
        };
        if reset == 0 {
            return Err(last_os_error());
        }

        let mut overlapped = OVERLAPPED {
            hEvent: self.event.as_raw_handle() as HANDLE,
            ..OVERLAPPED::default()
        };
        let started = unsafe {
            // SAFETY: the writer handle was opened with
            // `FILE_FLAG_OVERLAPPED`; `bytes` and `overlapped` remain alive
            // until synchronous completion or until cancellation is drained
            // below.
            WriteFile(
                self.handle.as_raw_handle() as HANDLE,
                bytes.as_ptr().cast(),
                u32::try_from(bytes.len()).expect("write chunk fits in DWORD"),
                std::ptr::null_mut(),
                &mut overlapped,
            )
        };
        if started != 0 {
            return self
                .completed_transfer(&overlapped)
                .map(WriteChunkOutcome::Progress);
        }

        let error = last_error_code();
        if error != ERROR_IO_PENDING {
            return Err(io::Error::from_raw_os_error(error as i32));
        }

        match unsafe {
            // SAFETY: the event belongs to the pending `overlapped`
            // operation and remains live for the duration of this wait.
            WaitForSingleObject(
                self.event.as_raw_handle() as HANDLE,
                wait_timeout_millis(remaining),
            )
        } {
            WAIT_OBJECT_0 => self
                .completed_transfer(&overlapped)
                .map(WriteChunkOutcome::Progress),
            WAIT_TIMEOUT => match self.cancel_and_drain(&overlapped)? {
                CancellationOutcome::Completed(transferred) => {
                    Ok(WriteChunkOutcome::Progress(transferred))
                }
                CancellationOutcome::Cancelled => Ok(WriteChunkOutcome::Stalled),
            },
            WAIT_FAILED => {
                let wait_error = last_os_error();
                self.cancel_and_drain(&overlapped)?;
                Err(wait_error)
            }
            status => {
                self.cancel_and_drain(&overlapped)?;
                Err(io::Error::other(format!(
                    "unexpected ConPTY write wait status {status}"
                )))
            }
        }
    }

    fn completed_transfer(&self, overlapped: &OVERLAPPED) -> io::Result<usize> {
        let mut transferred = 0_u32;
        let completed = unsafe {
            // SAFETY: `overlapped` identifies an operation that has already
            // completed, either synchronously or by signaling its event.
            GetOverlappedResult(
                self.handle.as_raw_handle() as HANDLE,
                overlapped,
                &mut transferred,
                0,
            )
        };
        if completed == 0 {
            return Err(last_os_error());
        }
        Ok(transferred as usize)
    }

    fn cancel_and_drain(&self, overlapped: &OVERLAPPED) -> io::Result<CancellationOutcome> {
        let cancelled = unsafe {
            // SAFETY: `overlapped` identifies the live operation issued on
            // `handle`; cancellation is scoped to that operation.
            CancelIoEx(self.handle.as_raw_handle() as HANDLE, overlapped)
        };
        let cancel_error = if cancelled == 0 {
            let code = last_error_code();
            (code != ERROR_NOT_FOUND).then_some(io::Error::from_raw_os_error(code as i32))
        } else {
            None
        };

        let mut transferred = 0_u32;
        let completed = unsafe {
            // SAFETY: cancellation does not make the caller-owned buffer or
            // `OVERLAPPED` reusable immediately. Waiting here guarantees the
            // operation has reached a terminal state before either is
            // dropped.
            GetOverlappedResult(
                self.handle.as_raw_handle() as HANDLE,
                overlapped,
                &mut transferred,
                1,
            )
        };
        if completed == 0 {
            let code = last_error_code();
            if code == ERROR_OPERATION_ABORTED {
                if let Some(error) = cancel_error {
                    return Err(io::Error::new(
                        error.kind(),
                        format!("failed to cancel timed-out ConPTY write: {error}"),
                    ));
                }
                return Ok(CancellationOutcome::Cancelled);
            } else {
                return Err(io::Error::from_raw_os_error(code as i32));
            }
        }
        Ok(CancellationOutcome::Completed(transferred as usize))
    }
}

fn lock_input_operation<'a>(
    operation: &'a Mutex<()>,
    active_kind: &'a AtomicU8,
    queue: &'a InputOperationQueue,
    requested_kind: InputOperationKind,
    drain_wait_timeout: Duration,
) -> io::Result<InputOperationGuard<'a>> {
    let ticket = queue.issue();
    let mut drain_wait_started = None;
    loop {
        if queue.is_serving(ticket) {
            match operation.try_lock() {
                Ok(guard) => {
                    active_kind.store(requested_kind as u8, Ordering::Release);
                    return Ok(InputOperationGuard {
                        _guard: guard,
                        kind: active_kind,
                        queue,
                        ticket,
                    });
                }
                Err(TryLockError::Poisoned(_)) => {
                    queue.cancel(ticket);
                    return Err(io::Error::other("ConPTY input writer mutex poisoned"));
                }
                Err(TryLockError::WouldBlock) => {}
            }
        }

        match InputOperationKind::load(active_kind) {
            InputOperationKind::Drain => {
                let started = drain_wait_started.get_or_insert_with(Instant::now);
                let remaining = drain_wait_timeout.saturating_sub(started.elapsed());
                if remaining.is_zero() {
                    queue.cancel(ticket);
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "ConPTY input operation mutex wait timed out",
                    ));
                }
                thread::sleep(remaining.min(Duration::from_millis(1)));
            }
            InputOperationKind::PipeWrite => {
                // Pipe writes are independently bounded by consecutive
                // inactivity. Queue time behind one is not inactivity in the
                // operation that has not yet been admitted.
                drain_wait_started = None;
                thread::sleep(Duration::from_millis(1));
            }
            InputOperationKind::Idle => thread::yield_now(),
        }
    }
}

fn contextual_timeout(error: io::Error, timeout: impl FnOnce() -> io::Error) -> io::Error {
    if error.kind() == io::ErrorKind::TimedOut {
        timeout()
    } else {
        error
    }
}

fn begin_stall_recovery(phase: &mut WriteStallPhase, recovery_grace: Duration) -> bool {
    if *phase != WriteStallPhase::Primary || recovery_grace.is_zero() {
        return false;
    }
    *phase = WriteStallPhase::Recovery;
    true
}

fn write_timeout(timeout: Duration, recovery_grace: Duration) -> io::Error {
    let total_stall = timeout.saturating_add(recovery_grace);
    io::Error::new(
        io::ErrorKind::TimedOut,
        format!(
            "PTY write made no progress for {} ms",
            total_stall.as_millis()
        ),
    )
}

fn drain_timeout(timeout: Duration) -> io::Error {
    io::Error::new(
        io::ErrorKind::TimedOut,
        format!(
            "ConPTY input pipe did not drain within {} ms",
            timeout.as_millis()
        ),
    )
}

fn wait_timeout_millis(timeout: Duration) -> u32 {
    let millis = timeout
        .as_millis()
        .saturating_add(u128::from(
            !timeout.subsec_nanos().is_multiple_of(1_000_000),
        ))
        .max(1);
    u32::try_from(millis)
        .unwrap_or(INFINITE - 1)
        .min(INFINITE - 1)
}

fn last_error_code() -> u32 {
    unsafe {
        // SAFETY: `GetLastError` reads the calling thread's last-error slot.
        GetLastError()
    }
}

fn last_os_error() -> io::Error {
    io::Error::from_raw_os_error(last_error_code() as i32)
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::sync::Arc;
    use std::thread;

    use super::*;
    use crate::backend::windows::io::create_conpty_input_pipe;

    fn writer_and_reader(buffer_size: u32) -> (OverlappedPipeWriter, std::fs::File) {
        let pipe = create_conpty_input_pipe(buffer_size).expect("overlapped pipe");
        (
            OverlappedPipeWriter::new(pipe.write).expect("writer"),
            std::fs::File::from(pipe.read),
        )
    }

    #[test]
    fn bounded_write_completes_when_pipe_is_drained() {
        let (writer, mut read) = writer_and_reader(4096);
        let payload = vec![b'x'; 32 * 1024];
        let expected = payload.clone();
        let reader = thread::spawn(move || {
            let mut received = Vec::new();
            read.read_to_end(&mut received).expect("read payload");
            received
        });

        writer
            .write_all_with_timeout(&payload, Duration::from_secs(2))
            .expect("drained write");
        drop(writer);
        let received = reader.join().expect("reader thread");

        assert_eq!(received, expected);
    }

    #[test]
    fn bounded_write_resets_timeout_after_progress() {
        let (writer, mut read) = writer_and_reader(4096);
        let payload = vec![b'x'; 64 * 1024];
        let expected = payload.clone();
        let reader = thread::spawn(move || {
            let mut received = Vec::new();
            let mut chunk = [0; WRITE_CHUNK_SIZE];
            loop {
                let count = read.read(&mut chunk).expect("read payload");
                if count == 0 {
                    break;
                }
                received.extend_from_slice(&chunk[..count]);
                thread::sleep(Duration::from_millis(10));
            }
            received
        });
        let timeout = Duration::from_millis(50);
        let started = Instant::now();

        writer
            .write_all_with_timeout(&payload, timeout)
            .expect("continuous progress should keep the write alive");
        assert!(
            started.elapsed() > timeout,
            "test must outlive one inactivity timeout"
        );

        drop(writer);
        let received = reader.join().expect("reader thread");
        assert_eq!(received, expected);
    }

    #[test]
    fn stall_recovery_times_out_on_non_draining_pipe() {
        let (writer, mut read) = writer_and_reader(4096);
        let payload = vec![b'x'; 16 * 1024 * 1024];
        let started = std::time::Instant::now();

        let error = writer
            .write_all_with_stall_recovery(
                &payload,
                Duration::from_millis(25),
                Duration::from_millis(25),
            )
            .expect_err("full pipe should time out");

        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "nonblocking write should time out promptly, elapsed={:?}",
            started.elapsed()
        );

        drop(writer);
        let mut accepted_prefix = Vec::new();
        read.read_to_end(&mut accepted_prefix).expect("read prefix");
        assert!(accepted_prefix.len() < payload.len());
    }

    #[test]
    fn transient_stall_recovery_preserves_order_and_writer_lifetime() {
        let (writer, mut read) = writer_and_reader(4096);
        let payload = (0..16 * 1024 * 1024)
            .map(|index| u8::try_from(index % 251).expect("pattern byte fits"))
            .collect::<Vec<_>>();
        let mut expected = payload.clone();
        let follow_on = b"RMUX-AFTER-RECOVERY";
        expected.extend_from_slice(follow_on);
        let (stalled_tx, stalled_rx) = std::sync::mpsc::channel();
        let reader = thread::spawn(move || {
            stalled_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("writer did not report its first transient stall");
            let mut received = Vec::new();
            read.read_to_end(&mut received)
                .expect("read recovered payload");
            received
        });

        writer
            .write_all_with_stall_recovery_observed(
                &payload,
                Duration::from_millis(20),
                Duration::from_secs(2),
                || {
                    let _ = stalled_tx.send(());
                },
            )
            .expect("draining after the first stall should recover");
        writer
            .write_all_with_timeout(follow_on, Duration::from_secs(1))
            .expect("writer should remain usable after transient recovery");
        drop(writer);

        let received = reader.join().expect("reader");
        assert_eq!(received, expected);
    }

    #[test]
    fn serialized_non_pipe_input_waits_behind_an_admitted_pipe_write() {
        let (writer, mut read) = writer_and_reader(4096);
        let writer = Arc::new(writer);
        let payload = vec![b'x'; 1024 * 1024];
        let expected = payload.clone();
        let (stalled_tx, stalled_rx) = std::sync::mpsc::channel();
        let pipe_writer = Arc::clone(&writer);
        let pipe_write = thread::spawn(move || {
            pipe_writer.write_all_with_stall_recovery_observed(
                &payload,
                Duration::from_millis(20),
                Duration::from_secs(2),
                || {
                    let _ = stalled_tx.send(());
                },
            )
        });
        stalled_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("pipe write did not reach a real blocked interval");

        let ordered_writer = Arc::clone(&writer);
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let ordered = thread::spawn(move || {
            ordered_writer.run_after_pipe_drain(Duration::from_secs(2), || {
                entered_tx.send(()).expect("record serialized entry");
                Ok(())
            })
        });
        assert!(
            matches!(
                entered_rx.recv_timeout(Duration::from_millis(100)),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            ),
            "non-pipe input overtook the blocked pipe write"
        );

        let expected_len = expected.len();
        let reader = thread::spawn(move || {
            let mut received = vec![0_u8; expected_len];
            read.read_exact(&mut received).expect("read pipe payload");
            // Keep the destination alive through the ordered callback.
            (received, read)
        });
        pipe_write
            .join()
            .expect("pipe writer thread")
            .expect("pipe write after drain");
        entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("serialized input did not follow the pipe write");
        ordered
            .join()
            .expect("ordered input thread")
            .expect("serialized input operation");
        let (received, _read) = reader.join().expect("reader thread");
        assert_eq!(received, expected);
    }

    #[test]
    fn serialized_non_pipe_input_waits_until_completed_pipe_bytes_are_consumed() {
        let (writer, mut read) = writer_and_reader(4096);
        let writer = Arc::new(writer);
        let payload = b"accepted-but-not-consumed";
        writer
            .write_all_with_timeout(payload, Duration::from_secs(1))
            .expect("small write fits in the pipe buffer");

        let ordered_writer = Arc::clone(&writer);
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let ordered = thread::spawn(move || {
            ordered_writer.run_after_pipe_drain(Duration::from_secs(2), || {
                entered_tx.send(()).expect("record serialized entry");
                Ok(())
            })
        });
        assert!(
            matches!(
                entered_rx.recv_timeout(Duration::from_millis(100)),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            ),
            "non-pipe input overtook pipe bytes accepted but not consumed"
        );

        let mut received = vec![0_u8; payload.len()];
        read.read_exact(&mut received)
            .expect("consume pipe payload");
        entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("serialized input did not follow pipe consumption");
        ordered
            .join()
            .expect("ordered input thread")
            .expect("serialized input operation");
        assert_eq!(received, payload);
    }

    #[test]
    fn current_and_queued_ticket_cancellation_leave_no_stale_entries() {
        let current = InputOperationQueue::new();
        for handoff in 0..1000_u64 {
            let holder = current.issue();
            let waiter = current.issue();
            assert!(current.is_serving(holder));

            current.complete(holder);
            assert!(current.is_serving(waiter));
            current.cancel(waiter);

            assert_eq!(
                current.serving.load(Ordering::Acquire),
                (handoff + 1) * 2,
                "cancelling the current ticket must advance admission"
            );
        }
        assert_eq!(current.next.load(Ordering::Acquire), 2000);
        let retained = current
            .cancelled
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len();
        assert_eq!(
            retained, 0,
            "current ticket cancellations must not retain unreachable entries"
        );

        let queued = InputOperationQueue::new();
        let holder = queued.issue();
        let first_hole = queued.issue();
        let second_hole = queued.issue();
        let survivor = queued.issue();
        queued.cancel(second_hole);
        queued.cancel(first_hole);
        queued.complete(holder);

        assert!(queued.is_serving(survivor));
        assert!(
            queued
                .cancelled
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_empty(),
            "advancing admission must remove consecutive cancelled holes"
        );
        queued.complete(survivor);
        assert_eq!(queued.serving.load(Ordering::Acquire), 4);
    }

    #[test]
    fn progressing_pipe_predecessor_does_not_expire_queued_write_or_focus() {
        let (writer, mut read) = writer_and_reader(4096);
        let writer = Arc::new(writer);
        let payload = vec![b'p'; 512 * 1024];
        let expected_payload = payload.clone();
        let (stalled_tx, stalled_rx) = std::sync::mpsc::channel();
        let pipe_writer = Arc::clone(&writer);
        let pipe_write = thread::spawn(move || {
            pipe_writer.write_all_with_stall_recovery_observed(
                &payload,
                Duration::from_millis(100),
                Duration::from_millis(500),
                || {
                    let _ = stalled_tx.send(());
                },
            )
        });
        stalled_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("predecessor did not reach the real full-pipe boundary");

        let ordinary_writer = Arc::clone(&writer);
        let ordinary = thread::spawn(move || {
            let started = Instant::now();
            let result = ordinary_writer.write_all_with_timeout(b"N", Duration::from_millis(2500));
            (started.elapsed(), result)
        });
        let ordinary_queued = Instant::now();
        while writer.operation_queue.next.load(Ordering::Acquire) < 2 {
            assert!(
                ordinary_queued.elapsed() < Duration::from_secs(1),
                "ordinary waiter did not enter the operation queue"
            );
            thread::yield_now();
        }

        let focus_entered = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let ordinary_consumed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let focus_from_thread = Arc::clone(&focus_entered);
        let consumed_from_focus = Arc::clone(&ordinary_consumed);
        let focus_writer = Arc::clone(&writer);
        let focus = thread::spawn(move || {
            let started = Instant::now();
            let result = focus_writer.run_after_pipe_drain(Duration::from_millis(2500), || {
                if !consumed_from_focus.load(std::sync::atomic::Ordering::SeqCst) {
                    return Err(io::Error::other(
                        "focus operation overtook the earlier queued ordinary write",
                    ));
                }
                focus_from_thread.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            });
            (started.elapsed(), result)
        });
        let focus_queued = Instant::now();
        while writer.operation_queue.next.load(Ordering::Acquire) < 3 {
            assert!(
                focus_queued.elapsed() < Duration::from_secs(1),
                "focus waiter did not enter the operation queue"
            );
            thread::yield_now();
        }

        let reader_started = Instant::now();
        let expected_len = expected_payload.len() + 1;
        let consumed_from_reader = Arc::clone(&ordinary_consumed);
        let reader = thread::spawn(move || {
            let mut received = Vec::with_capacity(expected_len);
            let mut chunk = [0_u8; WRITE_CHUNK_SIZE];
            while received.len() < expected_len {
                thread::sleep(Duration::from_millis(25));
                let remaining = expected_len - received.len();
                let request_len = remaining.min(chunk.len());
                let bytes_read = read
                    .read(&mut chunk[..request_len])
                    .expect("read progressing predecessor");
                assert_ne!(bytes_read, 0, "input pipe closed before queued operations");
                received.extend_from_slice(&chunk[..bytes_read]);
            }
            consumed_from_reader.store(true, std::sync::atomic::Ordering::SeqCst);
            // Keep the destination alive through the focus callback.
            (received, read)
        });

        pipe_write
            .join()
            .expect("predecessor thread")
            .expect("progressing predecessor");
        let (ordinary_elapsed, ordinary_result) = ordinary.join().expect("ordinary waiter");
        let (focus_elapsed, focus_result) = focus.join().expect("focus waiter");
        let (received, _read) = reader.join().expect("reader thread");

        ordinary_result.expect("ordinary write must survive a progressing predecessor");
        focus_result.expect("focus write must survive a progressing predecessor");
        assert!(
            reader_started.elapsed() > Duration::from_millis(2500),
            "predecessor did not outlive the historical admission threshold"
        );
        assert!(
            ordinary_elapsed > Duration::from_millis(2500),
            "ordinary waiter did not remain queued beyond its old admission deadline"
        );
        assert!(
            focus_elapsed > Duration::from_millis(2500),
            "focus waiter did not remain queued beyond its old admission deadline"
        );
        assert!(
            focus_entered.load(std::sync::atomic::Ordering::SeqCst),
            "focus callback never ran"
        );
        assert_eq!(
            received,
            [expected_payload.as_slice(), b"N"].concat(),
            "queued ordinary input must remain behind the predecessor without loss"
        );
    }

    #[test]
    fn stalled_drain_and_following_lock_wait_both_time_out() {
        let (writer, mut read) = writer_and_reader(4096);
        let writer = Arc::new(writer);
        let payload = b"accepted-but-not-consumed";
        writer
            .write_all_with_timeout(payload, Duration::from_millis(50))
            .expect("small write fits in the pipe buffer");

        let callback_entered = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let callback_from_thread = Arc::clone(&callback_entered);
        let ordered_writer = Arc::clone(&writer);
        let ordered = thread::spawn(move || {
            ordered_writer.run_after_pipe_drain(Duration::from_millis(200), || {
                callback_from_thread.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            })
        });
        let wait_started = Instant::now();
        while !matches!(writer.operation.try_lock(), Err(TryLockError::WouldBlock)) {
            assert!(
                wait_started.elapsed() < Duration::from_secs(1),
                "drain did not acquire the input operation mutex"
            );
            thread::sleep(Duration::from_millis(1));
        }

        let later_started = Instant::now();
        let later_error = writer
            .write_all_with_timeout(b"N", Duration::from_millis(50))
            .expect_err("a following writer must not wait indefinitely for the drain lock");
        assert_eq!(later_error.kind(), io::ErrorKind::TimedOut);
        assert!(
            later_started.elapsed() < Duration::from_millis(500),
            "50 ms lock wait was not bounded: {:?}",
            later_started.elapsed()
        );

        let drain_error = ordered
            .join()
            .expect("ordered drain thread")
            .expect_err("an unread pipe must time out its drain");
        assert_eq!(drain_error.kind(), io::ErrorKind::TimedOut);
        assert!(
            !callback_entered.load(std::sync::atomic::Ordering::SeqCst),
            "a timed-out drain invoked its callback"
        );

        writer
            .write_all_with_timeout(b"N", Duration::from_millis(50))
            .expect("the operation mutex is released after drain cancellation");
        let mut received = vec![0_u8; payload.len() + 1];
        read.read_exact(&mut received)
            .expect("read preserved payload and following write");
        assert_eq!(received, [payload.as_slice(), b"N"].concat());
    }

    #[test]
    fn reader_closed_before_drain_empty_pipe_does_not_invoke_callback() {
        assert_reader_closed_before_drain_does_not_invoke_callback(b"");
    }

    #[test]
    fn reader_closed_before_drain_pending_bytes_does_not_invoke_callback() {
        assert_reader_closed_before_drain_does_not_invoke_callback(b"pending-drain");
    }

    fn assert_reader_closed_before_drain_does_not_invoke_callback(payload: &[u8]) {
        // Close without ever reading, before the drain starts; no scheduling race.
        let (writer, read) = writer_and_reader(4096);
        writer
            .write_all_with_timeout(payload, Duration::from_secs(1))
            .expect("small write fits in the pipe buffer");
        drop(read);

        let mut callback_entered = false;
        let error = writer
            .run_after_pipe_drain(Duration::from_secs(2), || {
                callback_entered = true;
                Ok(())
            })
            .expect_err("a closed reader cannot authorize the callback");
        assert!(
            matches!(error.raw_os_error(), Some(109 | 232)),
            "unexpected reader-close drain error: {error}"
        );
        assert!(!callback_entered, "a failed drain invoked its callback");
        assert!(writer.operation.try_lock().is_ok());
    }

    #[test]
    fn empty_pipe_drain_preserves_callback_result_and_following_bytes() {
        let (writer, mut read) = writer_and_reader(4096);
        let mut calls = 0;
        let value = writer
            .run_after_pipe_drain(Duration::from_secs(2), || {
                calls += 1;
                Ok(42)
            })
            .expect("an open, empty pipe permits the callback");
        assert_eq!(value, 42);
        assert_eq!(calls, 1);

        writer
            .write_all_with_timeout(b"after-drain", Duration::from_secs(1))
            .expect("write after the connection probe");
        drop(writer);
        let mut received = Vec::new();
        read.read_to_end(&mut received)
            .expect("read following input");
        assert_eq!(received, b"after-drain", "the probe must not inject input");
    }

    #[test]
    fn zero_byte_write_detects_reader_close() {
        let (writer, read) = writer_and_reader(4096);
        assert_eq!(
            writer.write_chunk(&[], Duration::from_secs(1)).unwrap(),
            WriteChunkOutcome::Progress(0)
        );
        drop(read);
        let error = writer
            .write_chunk(&[], Duration::from_secs(1))
            .expect_err("a zero-byte write must still check the connection");
        assert!(
            matches!(error.raw_os_error(), Some(109 | 232)),
            "unexpected reader-close probe error: {error}"
        );
    }

    #[test]
    fn reader_close_aborts_drain_without_invoking_callback() {
        let (writer, read) = writer_and_reader(4096);
        let writer = Arc::new(writer);
        writer
            .write_all_with_timeout(b"pending-drain", Duration::from_millis(50))
            .expect("small write fits in the pipe buffer");

        let callback_entered = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let callback_from_thread = Arc::clone(&callback_entered);
        let ordered_writer = Arc::clone(&writer);
        let ordered = thread::spawn(move || {
            ordered_writer.run_after_pipe_drain(Duration::from_secs(2), || {
                callback_from_thread.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            })
        });
        let wait_started = Instant::now();
        while !matches!(writer.operation.try_lock(), Err(TryLockError::WouldBlock)) {
            assert!(
                wait_started.elapsed() < Duration::from_secs(1),
                "drain did not acquire the input operation mutex"
            );
            thread::sleep(Duration::from_millis(1));
        }

        drop(read);
        let error = ordered
            .join()
            .expect("ordered drain thread")
            .expect_err("closing the reader must fail the drain");
        assert!(
            matches!(error.raw_os_error(), Some(109 | 232)),
            "unexpected reader-close drain error: {error}"
        );
        assert!(
            !callback_entered.load(std::sync::atomic::Ordering::SeqCst),
            "a failed drain invoked its callback"
        );
        assert!(
            writer.operation.try_lock().is_ok(),
            "reader-close failure left the operation mutex held"
        );
    }

    #[test]
    fn completed_operation_wins_the_cancellation_race() {
        let (writer, mut read) = writer_and_reader(4096);
        let payload = b"completed-before-cancel";
        let expected = *payload;
        let reader = thread::spawn(move || {
            let mut received = [0_u8; 23];
            read.read_exact(&mut received)
                .expect("read completed write");
            received
        });

        let reset = unsafe {
            // SAFETY: the test has exclusive access to the writer event.
            ResetEvent(writer.event.as_raw_handle() as HANDLE)
        };
        assert_ne!(reset, 0, "ResetEvent: {}", last_os_error());
        let mut overlapped = OVERLAPPED {
            hEvent: writer.event.as_raw_handle() as HANDLE,
            ..OVERLAPPED::default()
        };
        let started = unsafe {
            // SAFETY: the writer is overlapped, and the payload and operation
            // remain live until completion is observed and drained below.
            WriteFile(
                writer.handle.as_raw_handle() as HANDLE,
                payload.as_ptr().cast(),
                u32::try_from(payload.len()).expect("test payload fits DWORD"),
                std::ptr::null_mut(),
                &mut overlapped,
            )
        };
        if started == 0 {
            assert_eq!(last_error_code(), ERROR_IO_PENDING);
            let wait = unsafe {
                // SAFETY: the event belongs to the pending operation.
                WaitForSingleObject(writer.event.as_raw_handle() as HANDLE, 2_000)
            };
            assert_eq!(wait, WAIT_OBJECT_0, "pending write did not complete");
        }

        let outcome = writer
            .cancel_and_drain(&overlapped)
            .expect("completed operation should drain");
        assert_eq!(
            outcome,
            CancellationOutcome::Completed(payload.len()),
            "completion must win over a late timeout cancellation"
        );
        assert_eq!(reader.join().expect("reader"), expected);
    }

    #[test]
    fn completed_overlapped_write_reports_exact_transfer_count() {
        let (writer, mut read) = writer_and_reader(4096);
        let payload = b"overlapped-result";
        let expected = *payload;
        let reader = thread::spawn(move || {
            let mut received = [0_u8; 17];
            read.read_exact(&mut received)
                .expect("read completed write");
            received
        });

        let reset = unsafe {
            // SAFETY: the test has exclusive access to the writer event.
            ResetEvent(writer.event.as_raw_handle() as HANDLE)
        };
        assert_ne!(reset, 0, "ResetEvent: {}", last_os_error());
        let mut overlapped = OVERLAPPED {
            hEvent: writer.event.as_raw_handle() as HANDLE,
            ..OVERLAPPED::default()
        };
        let started = unsafe {
            // SAFETY: the writer is overlapped, the byte-count pointer is null
            // as required for asynchronous I/O, and all inputs remain live
            // until the operation result is collected below.
            WriteFile(
                writer.handle.as_raw_handle() as HANDLE,
                payload.as_ptr().cast(),
                u32::try_from(payload.len()).expect("test payload fits DWORD"),
                std::ptr::null_mut(),
                &mut overlapped,
            )
        };
        if started == 0 {
            assert_eq!(last_error_code(), ERROR_IO_PENDING);
            let wait = unsafe {
                // SAFETY: the event belongs to the pending operation.
                WaitForSingleObject(writer.event.as_raw_handle() as HANDLE, 2_000)
            };
            assert_eq!(wait, WAIT_OBJECT_0, "pending write did not complete");
        }

        assert_eq!(
            writer
                .completed_transfer(&overlapped)
                .expect("collect overlapped result"),
            payload.len()
        );
        assert_eq!(reader.join().expect("reader"), expected);
    }

    #[test]
    fn bounded_write_serializes_concurrent_payloads() {
        let (writer, mut read) = writer_and_reader(4096);
        let writer = Arc::new(writer);
        let first_writer = Arc::clone(&writer);
        let first = thread::spawn(move || {
            first_writer
                .write_all_with_timeout(&vec![b'a'; 32 * 1024], Duration::from_secs(2))
                .expect("first write");
        });
        let second_writer = Arc::clone(&writer);
        let second = thread::spawn(move || {
            second_writer
                .write_all_with_timeout(&vec![b'b'; 32 * 1024], Duration::from_secs(2))
                .expect("second write");
        });
        let reader = thread::spawn(move || {
            let mut received = Vec::new();
            read.read_to_end(&mut received).expect("read payload");
            received
        });

        first.join().expect("first writer");
        second.join().expect("second writer");
        drop(writer);
        let received = reader.join().expect("reader");
        assert_eq!(received.len(), 64 * 1024);
        assert!(
            received
                .windows(2)
                .filter(|pair| pair[0] != pair[1])
                .count()
                <= 1,
            "serialized writes must not interleave"
        );
    }

    #[test]
    fn stall_recovery_does_not_retry_a_dead_destination() {
        let pipe = create_conpty_input_pipe(4096).expect("pipe");
        let writer = OverlappedPipeWriter::new(pipe.write).expect("writer");
        drop(pipe.read);
        let started = Instant::now();

        let error = writer
            .write_all_with_stall_recovery(b"x", Duration::from_millis(25), Duration::from_secs(1))
            .expect_err("closed pipe should fail");

        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "a dead destination must bypass transient stall recovery"
        );
    }
}
