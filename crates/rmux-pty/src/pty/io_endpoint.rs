//! The shared I/O endpoint of a pseudoterminal master.

use std::io;

#[cfg(unix)]
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd, RawFd};
#[cfg(unix)]
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
#[cfg(unix)]
use std::sync::{Mutex, OnceLock};
#[cfg(any(unix, windows))]
use std::time::Duration;

use crate::backend;
#[cfg(all(not(unix), not(windows)))]
use crate::unsupported_op;
#[cfg(unix)]
use crate::ProcessId;
#[cfg(all(not(unix), not(windows)))]
use crate::PtyError;
use crate::{Result, TerminalGeometry, TerminalSize};

#[cfg(any(unix, windows))]
const DEFAULT_PTY_WRITE_TIMEOUT: Duration = Duration::from_secs(2);

/// The I/O endpoint for a pseudoterminal.
#[derive(Debug)]
pub struct PtyIo {
    #[cfg(unix)]
    pub(super) fd: Arc<OwnedFd>,
    #[cfg(unix)]
    pub(super) startup_slave: Mutex<Option<OwnedFd>>,
    /// The process spawned on the slave, shared by every clone of this
    /// endpoint and by its child handle. Recorded once by
    /// [`crate::SpawnedPty::new`].
    #[cfg(unix)]
    session_leader: Arc<SessionLeaderExit>,
    /// The ordered input backlog of this PTY, shared by every clone.
    #[cfg(unix)]
    input: Arc<backend::PtyInputQueue>,
    #[cfg(windows)]
    pub(super) pty: Arc<backend::WindowsPty>,
}

impl PtyIo {
    #[cfg(unix)]
    pub(crate) fn new(fd: OwnedFd) -> Self {
        Self {
            fd: Arc::new(fd),
            startup_slave: Mutex::new(None),
            session_leader: Arc::default(),
            input: Arc::default(),
        }
    }

    #[cfg(windows)]
    pub(crate) fn new(pty: Arc<backend::WindowsPty>) -> Self {
        Self { pty }
    }

    /// Queries the current terminal geometry for this PTY endpoint.
    pub fn size(&self) -> Result<TerminalSize> {
        #[cfg(unix)]
        {
            backend::query_size(self.fd.as_ref().as_fd())
        }

        #[cfg(not(unix))]
        {
            #[cfg(windows)]
            {
                backend::query_size(&self.pty)
            }

            #[cfg(not(windows))]
            {
                Err(PtyError::Unsupported(unsupported_op::QUERY_PTY_SIZE))
            }
        }
    }

    /// Resizes this PTY endpoint.
    pub fn resize(&self, size: TerminalSize) -> Result<()> {
        #[cfg(unix)]
        {
            backend::apply_size(self.fd.as_ref().as_fd(), size)
        }

        #[cfg(not(unix))]
        {
            #[cfg(windows)]
            {
                backend::apply_size(&self.pty, size)
            }

            #[cfg(not(windows))]
            {
                let _ = size;
                Err(PtyError::Unsupported(unsupported_op::RESIZE_PTY))
            }
        }
    }

    /// Resizes this PTY endpoint, preserving optional pixel geometry where supported.
    pub fn resize_geometry(&self, geometry: TerminalGeometry) -> Result<()> {
        #[cfg(unix)]
        {
            backend::apply_geometry(self.fd.as_ref().as_fd(), geometry)
        }

        #[cfg(not(unix))]
        {
            #[cfg(windows)]
            {
                backend::apply_geometry(&self.pty, geometry)
            }

            #[cfg(not(windows))]
            {
                let _ = geometry;
                Err(PtyError::Unsupported(unsupported_op::RESIZE_PTY))
            }
        }
    }

    /// Clones this PTY I/O endpoint.
    pub fn try_clone(&self) -> Result<Self> {
        #[cfg(unix)]
        {
            Ok(Self {
                fd: Arc::clone(&self.fd),
                startup_slave: Mutex::new(None),
                session_leader: Arc::clone(&self.session_leader),
                input: Arc::clone(&self.input),
            })
        }

        #[cfg(not(unix))]
        {
            #[cfg(windows)]
            {
                Ok(Self {
                    pty: Arc::clone(&self.pty),
                })
            }

            #[cfg(not(windows))]
            {
                Err(PtyError::Unsupported(unsupported_op::CLONE_PTY_IO))
            }
        }
    }

    /// Reads bytes from this PTY endpoint, waiting for Unix readiness when needed.
    pub fn read(&self, buffer: &mut [u8]) -> io::Result<usize> {
        #[cfg(unix)]
        {
            backend::read(self.fd.as_ref().as_fd(), buffer)
        }

        #[cfg(not(unix))]
        {
            #[cfg(windows)]
            {
                self.pty.read(buffer)
            }

            #[cfg(not(windows))]
            {
                let _ = buffer;
                Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "pty I/O is unsupported on this platform",
                ))
            }
        }
    }

    /// Attempts one nonblocking read from this Unix PTY endpoint.
    #[cfg(unix)]
    pub fn try_read(&self, buffer: &mut [u8]) -> io::Result<usize> {
        backend::try_read(self.fd.as_ref().as_fd(), buffer)
    }

    /// Writes all bytes to this PTY endpoint with the default two-second timeout.
    pub fn write_all(&self, bytes: &[u8]) -> io::Result<()> {
        #[cfg(any(unix, windows))]
        {
            self.write_all_with_timeout(bytes, DEFAULT_PTY_WRITE_TIMEOUT)
        }

        #[cfg(not(any(unix, windows)))]
        {
            let _ = bytes;
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "pty I/O is unsupported on this platform",
            ))
        }
    }

    /// Writes all bytes to this PTY endpoint while write progress continues.
    /// The timeout measures consecutive inactivity, so a timeout may occur
    /// after the PTY accepted a prefix of `bytes`.
    #[cfg(any(unix, windows))]
    pub fn write_all_with_timeout(&self, bytes: &[u8], timeout: Duration) -> io::Result<()> {
        #[cfg(unix)]
        {
            backend::write_all_with_timeout(self.fd.as_ref().as_fd(), bytes, timeout)
        }

        #[cfg(windows)]
        {
            self.pty.write_all_with_timeout(bytes, timeout)
        }
    }

    /// Queues `bytes` as input for the program on the slave and returns
    /// without waiting for it to read them.
    ///
    /// Queued input of one PTY, from any clone of its endpoint, reaches the
    /// slave in queue order. What the slave cannot take yet is kept and
    /// written as room appears, with no inactivity deadline, until the PTY can
    /// no longer be written or its last endpoint is closed. Bytes written with
    /// [`Self::write_all`] or [`Self::write_all_with_timeout`] bypass this
    /// order. Fails only when the PTY already refuses input, for example
    /// because its slave is gone; nothing of `bytes` is kept then.
    #[cfg(unix)]
    pub fn queue_input(&self, bytes: &[u8]) -> io::Result<()> {
        self.input.push(&self.fd, bytes)
    }

    /// Ensures that the underlying PTY endpoint is nonblocking.
    /// Unix PTY masters are configured this way before they are exposed.
    pub fn set_nonblocking(&self) -> io::Result<()> {
        #[cfg(unix)]
        {
            backend::set_nonblocking(self.fd.as_ref().as_fd())
        }

        #[cfg(not(unix))]
        {
            #[cfg(windows)]
            {
                Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "set_nonblocking is not applicable to ConPTY pipe handles; \
                     async readiness is provided by the Tokio named-pipe driver",
                ))
            }

            #[cfg(not(windows))]
            {
                Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "pty I/O is unsupported on this platform",
                ))
            }
        }
    }

    /// Returns the borrowed nonblocking Unix descriptor for readiness-driven
    /// integration points such as `AsyncFd`.
    #[cfg(unix)]
    #[must_use]
    pub fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_ref().as_fd()
    }

    #[cfg(unix)]
    pub(crate) fn raw_fd(&self) -> RawFd {
        self.fd.as_ref().as_raw_fd()
    }

    /// Releases the one-shot Unix startup slave guard, if this endpoint owns
    /// one for a pane output reader.
    #[cfg(unix)]
    pub fn release_startup_slave_guard(&self) {
        let Ok(mut guard) = self.startup_slave.lock() else {
            return;
        };
        let _ = guard.take();
    }

    /// Records the process spawned on this PTY and returns the exit record
    /// its child handle must update when it reaps that process.
    #[cfg(unix)]
    pub(crate) fn record_session_leader(&self, pid: ProcessId) -> Arc<SessionLeaderExit> {
        let _ = self.session_leader.pid.set(pid);
        Arc::clone(&self.session_leader)
    }

    /// Returns true once the process spawned on this PTY is known to have
    /// exited. Its exit status is left for the child handle to reap.
    ///
    /// A master read that reports EIO or EOF before any output cannot tell a
    /// slave that is still settling after spawn from one whose process is
    /// gone; this answers that question without waiting, including after the
    /// child handle has reaped the process. False when no process was recorded
    /// or its state cannot be observed.
    #[cfg(unix)]
    #[must_use]
    pub fn session_leader_has_exited(&self) -> bool {
        self.session_leader.has_exited()
    }
}

/// What a PTY endpoint knows about the exit of the process spawned on it.
///
/// `waitid(WNOWAIT)` observes an exited child only until someone reaps it.
/// The child handle reaps on any status query (`#{pane_dead}`, send-keys,
/// pipe-pane...), which can come first; it then marks the record so that the
/// endpoint keeps answering that the process exited.
#[cfg(unix)]
#[derive(Debug, Default)]
pub(crate) struct SessionLeaderExit {
    pid: OnceLock<ProcessId>,
    reaped: AtomicBool,
}

#[cfg(unix)]
impl SessionLeaderExit {
    /// Called by the child handle once it has reaped the process.
    pub(crate) fn mark_reaped(&self) {
        self.reaped.store(true, Ordering::Release);
    }

    fn has_exited(&self) -> bool {
        // Once reaped, the pid may already belong to another process: answer
        // from the mark without probing it.
        if self.reaped.load(Ordering::Acquire) {
            return true;
        }
        let Some(pid) = self.pid.get() else {
            return false;
        };
        // A reap between the load above and the probe makes the probe answer
        // false; its mark follows the reap, so look again.
        backend::child_has_exited(*pid) || self.reaped.load(Ordering::Acquire)
    }
}

#[cfg(unix)]
impl AsFd for PtyIo {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_ref().as_fd()
    }
}

#[cfg(unix)]
impl AsRawFd for PtyIo {
    fn as_raw_fd(&self) -> RawFd {
        self.fd.as_ref().as_raw_fd()
    }
}
