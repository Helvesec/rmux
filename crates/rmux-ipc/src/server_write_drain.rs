//! Graceful server-write finalization for local streams.

use std::io;
#[cfg(windows)]
use std::os::windows::io::{AsHandle, AsRawHandle, FromRawHandle, OwnedHandle};
#[cfg(windows)]
use std::sync::{Arc, Mutex};
#[cfg(windows)]
use std::time::Duration;

use crate::LocalStream;

#[cfg(any(windows, test))]
mod operation;

/// How long a graceful drain may wait for the control peer to consume queued
/// output before the transport is finalized without it.
///
/// The wait exists so a peer that is reading normally cannot lose the terminal
/// records, so the budget has to cover a slow but live reader — it matches the
/// listener's `DETACHED_RESPONSE_WRITE_TIMEOUT`. It is only an upper bound: a
/// peer that reads promptly is not delayed.
#[cfg(windows)]
const SERVER_WRITE_DRAIN_DEADLINE: Duration = Duration::from_secs(5);

/// A transport-specific lease used to preserve server output during a graceful close.
///
/// Unix stream sockets retain bytes already accepted by the kernel when the
/// server closes. Windows named pipes instead discard unread bytes when the
/// server disconnects, so their lease owns a duplicate handle that can wait
/// for the client to consume all queued output before the stream is dropped.
pub struct ServerWriteDrain {
    #[cfg(windows)]
    handle: OwnedHandle,
}

impl ServerWriteDrain {
    /// Captures the state needed to finalize writes after the stream is split.
    pub fn capture(stream: &LocalStream) -> io::Result<Self> {
        #[cfg(unix)]
        {
            let _ = stream;
            Ok(Self {})
        }

        #[cfg(windows)]
        {
            Ok(Self {
                handle: stream.as_handle().try_clone_to_owned()?,
            })
        }
    }

    /// Waits until graceful server output is safe from transport close.
    ///
    /// This is a no-op for Unix stream sockets. On Windows it performs the
    /// named-pipe server drain under a strict deadline, because
    /// `FlushFileBuffers` returns only once the client has read all buffered
    /// bytes — a condition the client, not the server, controls.
    pub async fn flush_to_peer(self) -> io::Result<()> {
        #[cfg(unix)]
        {
            Ok(())
        }

        #[cfg(windows)]
        {
            self.flush_to_peer_within(SERVER_WRITE_DRAIN_DEADLINE).await
        }
    }

    #[cfg(windows)]
    async fn flush_to_peer_within(self, timeout: Duration) -> io::Result<()> {
        let target = Arc::new(NamedPipeDrain::new(self.handle));
        match operation::drain_with_deadline(target, timeout).await? {
            operation::BoundedDrainOutcome::Completed(Ok(())) => Ok(()),
            operation::BoundedDrainOutcome::Completed(Err(error))
                if crate::is_peer_disconnect(&error) =>
            {
                Ok(())
            }
            operation::BoundedDrainOutcome::Completed(Err(error)) => Err(error),
            operation::BoundedDrainOutcome::TimedOut => Err(drain_timeout_error(timeout)),
        }
    }
}

#[cfg(windows)]
fn drain_timeout_error(timeout: Duration) -> io::Error {
    io::Error::new(
        io::ErrorKind::TimedOut,
        format!(
            "the control client did not read queued output within {timeout:?}; \
             peer-read drain cancellation was requested and the transport will be finalized"
        ),
    )
}

/// The connected server end of a named pipe, as a drain target.
#[cfg(windows)]
struct NamedPipeDrain {
    handle: OwnedHandle,
    worker_thread: Mutex<Option<OwnedHandle>>,
}

#[cfg(windows)]
impl NamedPipeDrain {
    fn new(handle: OwnedHandle) -> Self {
        Self {
            handle,
            worker_thread: Mutex::new(None),
        }
    }
}

#[cfg(windows)]
impl operation::DrainTarget for NamedPipeDrain {
    fn bind_to_current_thread(&self) -> io::Result<()> {
        use windows_sys::Win32::System::Threading::{
            GetCurrentThreadId, OpenThread, THREAD_TERMINATE,
        };

        let thread = unsafe {
            // SAFETY: the ID names this live dedicated worker. The returned
            // non-inheritable handle is retained until the operation retires.
            OpenThread(THREAD_TERMINATE, 0, GetCurrentThreadId())
        };
        if thread.is_null() {
            return Err(io::Error::last_os_error());
        }
        let thread = unsafe {
            // SAFETY: OpenThread returned a new owned handle on success.
            OwnedHandle::from_raw_handle(thread.cast())
        };
        let mut worker_thread = self
            .worker_thread
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if worker_thread.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "named-pipe drain worker was already bound",
            ));
        }
        *worker_thread = Some(thread);
        Ok(())
    }

    fn drain(&self) -> io::Result<()> {
        use windows_sys::Win32::Storage::FileSystem::FlushFileBuffers;

        let flushed = unsafe {
            // SAFETY: `handle` is a live duplicate of the connected server pipe
            // handle. The operation's dedicated worker and deadline owner keep
            // this target alive until either completion or cancellation.
            FlushFileBuffers(self.handle.as_raw_handle())
        };
        if flushed != 0 {
            return Ok(());
        }
        Err(io::Error::last_os_error())
    }

    fn cancel(&self) -> io::Result<operation::CancelAttempt> {
        use windows_sys::Win32::Foundation::ERROR_NOT_FOUND;
        use windows_sys::Win32::System::IO::CancelSynchronousIo;

        let worker_thread = self
            .worker_thread
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(worker_thread) = worker_thread.as_ref() else {
            return Ok(operation::CancelAttempt::NotPending);
        };
        let cancelled = unsafe {
            // SAFETY: this is a real handle with THREAD_TERMINATE access to the
            // dedicated thread that issues only this drain's synchronous I/O.
            // It cannot match Tokio's overlapped reads or writes, another pipe,
            // or a later blocking-pool operation.
            CancelSynchronousIo(worker_thread.as_raw_handle())
        };
        if cancelled != 0 {
            return Ok(operation::CancelAttempt::Requested);
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_NOT_FOUND as i32) {
            Ok(operation::CancelAttempt::NotPending)
        } else {
            Err(error)
        }
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use std::io;
    use std::os::windows::io::{FromRawHandle, IntoRawHandle, OwnedHandle};
    use std::ptr::{null, null_mut};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::windows::named_pipe::{NamedPipeClient, ServerOptions};
    use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_OVERLAPPED, OPEN_EXISTING, SECURITY_IDENTIFICATION,
        SECURITY_SQOS_PRESENT,
    };

    use super::{operation, NamedPipeDrain, ServerWriteDrain, SERVER_WRITE_DRAIN_DEADLINE};
    use crate::LocalStream;

    /// Hard bound for every assertion in this module's tests.
    const TEST_TIMEOUT: Duration = Duration::from_secs(20);
    const PENDING_OBSERVATION: Duration = Duration::from_millis(100);
    const STALLED_DRAIN_DEADLINE: Duration = Duration::from_millis(150);
    const TEST_OUTPUT: &[u8; 23] = b"terminal control output";

    static NEXT_PIPE: AtomicU64 = AtomicU64::new(1);

    async fn connected_pipe_with_raw_client(tag: &str) -> (LocalStream, OwnedHandle) {
        let sequence = NEXT_PIPE.fetch_add(1, Ordering::Relaxed);
        let pipe_name = format!(
            r"\\.\pipe\rmux-server-write-drain-{tag}-{}-{sequence}",
            std::process::id()
        );
        let server = ServerOptions::new()
            .first_pipe_instance(true)
            .create(&pipe_name)
            .expect("create named-pipe server");
        let client = open_raw_client(&pipe_name).expect("open raw named-pipe client");
        tokio::time::timeout(TEST_TIMEOUT, server.connect())
            .await
            .expect("named-pipe server connect must remain bounded")
            .expect("connect named-pipe server");
        (server, client)
    }

    fn open_raw_client(pipe_name: &str) -> io::Result<OwnedHandle> {
        let wide = pipe_name
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let handle = unsafe {
            // SAFETY: `wide` is a nul-terminated UTF-16 pipe name that lives
            // through the call. Security attributes and the template handle
            // are intentionally null, matching `ClientOptions::new()`.
            CreateFileW(
                wide.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                null(),
                OPEN_EXISTING,
                SECURITY_IDENTIFICATION | SECURITY_SQOS_PRESENT | FILE_FLAG_OVERLAPPED,
                null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        Ok(unsafe {
            // SAFETY: successful CreateFileW returned one new handle, and this
            // fixture gives that handle exactly one owner without registering,
            // duplicating, reopening, or issuing client I/O.
            OwnedHandle::from_raw_handle(handle)
        })
    }

    fn register_raw_client(client: OwnedHandle) -> io::Result<NamedPipeClient> {
        let handle = client.into_raw_handle();
        unsafe {
            // SAFETY: `client` was the sole owner of this live named-pipe
            // client handle, which was opened for overlapped I/O. Moving it
            // through `into_raw_handle` transfers that same ownership directly
            // to Tokio; there is no clone, duplicate, close, or reopen.
            NamedPipeClient::from_raw_handle(handle)
        }
    }

    async fn queue_test_output(server: &mut LocalStream) {
        tokio::time::timeout(TEST_TIMEOUT, server.write_all(TEST_OUTPUT))
            .await
            .expect("queueing server output must remain bounded")
            .expect("queue server output");
        finish_pending_test_write(server).await;
    }

    async fn finish_pending_test_write(server: &mut LocalStream) {
        // Production performs this barrier in `control::output_queue` before
        // entering the transport drain. Tokio's named-pipe flush is a no-op,
        // but Mio cannot accept this empty write until the preceding
        // overlapped write completion has been processed.
        let written = tokio::time::timeout(TEST_TIMEOUT, server.write(&[]))
            .await
            .expect("the overlapped-write barrier must remain bounded")
            .expect("finish overlapped write");
        assert_eq!(written, 0);
    }

    #[tokio::test]
    async fn server_drain_waits_until_the_client_reads_buffered_output() {
        let (mut server, raw_client) = connected_pipe_with_raw_client("reads").await;

        let drain = ServerWriteDrain::capture(&server).expect("capture server write drain");
        queue_test_output(&mut server).await;
        let mut drain_future = Box::pin(drain.flush_to_peer());
        assert!(
            tokio::time::timeout(PENDING_OBSERVATION, drain_future.as_mut())
                .await
                .is_err(),
            "the duplicate-handle drain must remain pending while the raw client is open and unread"
        );

        // Registration is the sole transition: ownership of the same raw
        // handle moves directly into Tokio/Mio, whose posted read may release
        // the already-pending FlushFileBuffers operation.
        let mut client = register_raw_client(raw_client).expect("register the same client handle");
        tokio::time::timeout(TEST_TIMEOUT, drain_future.as_mut())
            .await
            .expect("registration must release the pending drain within the hard bound")
            .expect("server drain succeeds after registration");
        drop(drain_future);
        drop(server);

        let mut output = [0_u8; 23];
        tokio::time::timeout(TEST_TIMEOUT, client.read_exact(&mut output))
            .await
            .expect("reading retained server output must remain bounded")
            .expect("read all queued server output");
        assert_eq!(&output, TEST_OUTPUT);

        let mut eof = [0_u8; 1];
        match tokio::time::timeout(TEST_TIMEOUT, client.read(&mut eof))
            .await
            .expect("server EOF must reach the client within the hard bound")
        {
            Ok(0) => {}
            Err(error) if crate::is_peer_disconnect(&error) => {}
            result => panic!("server close must reach the client as EOF: {result:?}"),
        }
    }

    #[tokio::test]
    async fn server_drain_is_bounded_when_the_client_never_reads() {
        let (mut server, raw_client) = connected_pipe_with_raw_client("stalled").await;

        let drain = ServerWriteDrain::capture(&server).expect("capture server write drain");
        queue_test_output(&mut server).await;

        // The sole client stays raw, connected, unregistered, and unread, so
        // only the production operation deadline can end this drain.
        let error = tokio::time::timeout(
            TEST_TIMEOUT,
            drain.flush_to_peer_within(STALLED_DRAIN_DEADLINE),
        )
        .await
        .expect("a stalled client must not extend the drain past its deadline")
        .expect_err("a discarded transcript must not be reported as success");

        assert_eq!(
            error.kind(),
            std::io::ErrorKind::TimedOut,
            "the bounded outcome must name the deadline: {error}"
        );
        drop(raw_client);
        drop(server);
    }

    #[tokio::test]
    async fn deadline_cancellation_retires_the_flush_worker_while_the_client_stays_open() {
        let (mut server, raw_client) = connected_pipe_with_raw_client("cancel-worker").await;

        let drain = ServerWriteDrain::capture(&server).expect("capture server write drain");
        queue_test_output(&mut server).await;
        let target = Arc::new(NamedPipeDrain::new(drain.handle));
        let outcome = tokio::time::timeout(
            TEST_TIMEOUT,
            operation::drain_with_deadline(Arc::clone(&target), STALLED_DRAIN_DEADLINE),
        )
        .await
        .expect("the operation deadline must remain bounded")
        .expect("the dedicated drain threads must start");
        assert!(matches!(outcome, operation::BoundedDrainOutcome::TimedOut));

        tokio::time::timeout(TEST_TIMEOUT, async {
            while Arc::strong_count(&target) != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("CancelSynchronousIo must retire the exact flush worker");

        let target = match Arc::try_unwrap(target) {
            Ok(target) => target,
            Err(_) => panic!("the operation must release every target owner"),
        };
        // This closes the operation's captured server handle and retained
        // worker-thread handle while the sole raw client is demonstrably still
        // owned by this scope and has never been registered or touched by I/O.
        drop(target);
        drop(raw_client);
        drop(server);
    }

    #[tokio::test]
    async fn server_drain_is_a_clean_no_op_when_the_client_disconnects_during_it() {
        let (mut server, raw_client) = connected_pipe_with_raw_client("disconnect").await;

        let drain = ServerWriteDrain::capture(&server).expect("capture server write drain");
        queue_test_output(&mut server).await;
        let mut drain_future = Box::pin(drain.flush_to_peer());
        assert!(
            tokio::time::timeout(PENDING_OBSERVATION, drain_future.as_mut())
                .await
                .is_err(),
            "the duplicate-handle drain must first remain pending with the sole raw client open"
        );

        // The peer vanishes mid-drain: no further delivery was possible, so
        // the existing peer-disconnect allowlist maps this to clean completion.
        drop(raw_client);
        tokio::time::timeout(TEST_TIMEOUT, drain_future.as_mut())
            .await
            .expect("a disconnected peer must release the drain")
            .expect("a peer that disconnected mid-drain is a clean no-op");
        drop(drain_future);
        drop(server);
    }

    #[test]
    fn the_drain_deadline_is_intentional() {
        assert_eq!(SERVER_WRITE_DRAIN_DEADLINE, Duration::from_secs(5));
    }
}
