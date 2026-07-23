use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::sync::Mutex;
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

const WRITE_CHUNK_SIZE: usize = 4 * 1024;

#[derive(Debug, Eq, PartialEq)]
enum CancellationOutcome {
    Completed(usize),
    Cancelled,
}

#[derive(Debug)]
pub(super) struct OverlappedPipeWriter {
    handle: OwnedHandle,
    event: OwnedHandle,
    operation: Mutex<()>,
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
        })
    }

    pub(super) fn write_all_with_timeout(
        &self,
        mut bytes: &[u8],
        timeout: Duration,
    ) -> io::Result<()> {
        let _operation = self
            .operation
            .lock()
            .map_err(|_| io::Error::other("ConPTY input writer mutex poisoned"))?;
        let mut last_progress = Instant::now();
        while !bytes.is_empty() {
            let remaining = timeout.saturating_sub(last_progress.elapsed());
            if remaining.is_zero() {
                return Err(write_timeout(timeout));
            }
            let chunk_len = bytes.len().min(WRITE_CHUNK_SIZE);
            let bytes_written = self.write_chunk(&bytes[..chunk_len], remaining, timeout)?;
            if bytes_written == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "ConPTY input pipe accepted zero bytes",
                ));
            }
            bytes = &bytes[bytes_written..];
            last_progress = Instant::now();
        }
        Ok(())
    }

    fn write_chunk(
        &self,
        bytes: &[u8],
        remaining: Duration,
        timeout: Duration,
    ) -> io::Result<usize> {
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
            return self.completed_transfer(&overlapped);
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
            WAIT_OBJECT_0 => self.completed_transfer(&overlapped),
            WAIT_TIMEOUT => match self.cancel_and_drain(&overlapped)? {
                CancellationOutcome::Completed(transferred) => Ok(transferred),
                CancellationOutcome::Cancelled => Err(write_timeout(timeout)),
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

fn write_timeout(timeout: Duration) -> io::Error {
    io::Error::new(
        io::ErrorKind::TimedOut,
        format!("PTY write made no progress for {} ms", timeout.as_millis()),
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
    fn bounded_write_times_out_on_non_draining_pipe() {
        let (writer, mut read) = writer_and_reader(4096);
        let payload = vec![b'x'; 16 * 1024 * 1024];
        let started = std::time::Instant::now();

        let error = writer
            .write_all_with_timeout(&payload, Duration::from_millis(50))
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
    fn timed_out_write_is_drained_before_the_writer_is_reused() {
        let (writer, mut read) = writer_and_reader(4096);
        let stalled_payload = vec![b'x'; 64 * 1024];

        let error = writer
            .write_all_with_timeout(&stalled_payload, Duration::from_millis(20))
            .expect_err("full pipe should time out");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);

        let reader = thread::spawn(move || {
            let mut received = Vec::new();
            read.read_to_end(&mut received)
                .expect("read accepted prefix and sentinel");
            received
        });
        let sentinel = b"RMUX-WRITER-REUSED";
        writer
            .write_all_with_timeout(sentinel, Duration::from_secs(2))
            .expect("writer should remain usable after cancellation");
        drop(writer);

        let received = reader.join().expect("reader");
        assert!(received.ends_with(sentinel));
        assert!(
            received[..received.len() - sentinel.len()]
                .iter()
                .all(|byte| *byte == b'x'),
            "a timed-out write may leave only its accepted prefix"
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
    fn bounded_write_preserves_broken_pipe_error() {
        let pipe = create_conpty_input_pipe(4096).expect("pipe");
        let writer = OverlappedPipeWriter::new(pipe.write).expect("writer");
        drop(pipe.read);

        let error = writer
            .write_all_with_timeout(b"x", Duration::from_secs(1))
            .expect_err("closed pipe should fail");

        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
    }
}
