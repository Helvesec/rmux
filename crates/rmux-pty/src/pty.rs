use std::io;

#[cfg(unix)]
use std::ffi::OsString;
#[cfg(unix)]
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd, RawFd};
#[cfg(unix)]
use std::os::unix::ffi::OsStringExt;
#[cfg(unix)]
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::backend;
#[cfg(all(not(unix), not(windows)))]
use crate::unsupported_op;
#[cfg(windows)]
use crate::ProcessId;
#[cfg(all(not(unix), not(windows)))]
use crate::PtyError;
use crate::{Result, TerminalGeometry, TerminalSize};
#[cfg(unix)]
use rustix::termios::{tcgetattr, ttyname, LocalModes};

mod io_endpoint;
pub use io_endpoint::PtyIo;
#[cfg(unix)]
pub(crate) use io_endpoint::SessionLeaderExit;

#[cfg(unix)]
/// The slave endpoint of a Unix pseudoterminal pair.
#[derive(Debug)]
pub struct PtySlave {
    fd: OwnedFd,
}

#[cfg(unix)]
impl PtySlave {
    /// Duplicates the slave terminal endpoint.
    pub fn try_clone(&self) -> Result<Self> {
        Ok(Self {
            fd: self.fd.try_clone()?,
        })
    }

    /// Consumes the slave endpoint and returns the owned file descriptor.
    #[must_use]
    pub fn into_owned_fd(self) -> OwnedFd {
        self.fd
    }
}

#[cfg(unix)]
impl AsFd for PtySlave {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

#[cfg(unix)]
impl AsRawFd for PtySlave {
    fn as_raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }
}

/// The master handle of a pseudoterminal.
#[derive(Debug)]
pub struct PtyMaster {
    io: PtyIo,
    #[cfg(unix)]
    startup_slave: Option<OwnedFd>,
    /// The name of the slave of the pair this master was allocated with,
    /// resolved once from the slave descriptor this process owned at that
    /// moment. It is a name, never a retained descriptor, so it costs the pair
    /// no lifetime: nothing here keeps a slave open.
    #[cfg(unix)]
    slave_path: Option<PathBuf>,
}

impl PtyMaster {
    #[cfg(unix)]
    pub(crate) fn new(fd: OwnedFd) -> Self {
        Self {
            io: PtyIo::new(fd),
            startup_slave: None,
            slave_path: None,
        }
    }

    #[cfg(unix)]
    pub(crate) fn with_startup_slave(mut self, slave: OwnedFd) -> Self {
        self.startup_slave = Some(slave);
        self
    }

    #[cfg(unix)]
    fn with_slave_path(mut self, slave_path: Option<PathBuf>) -> Self {
        self.slave_path = slave_path;
        self
    }

    /// Returns the name of the slave of the PTY this master was allocated with.
    ///
    /// This describes the terminal the pair owns. It is fixed at allocation and
    /// does not follow anything the child later does to its own descriptors, so
    /// a child that replaces its standard input does not change it. `None` when
    /// the name could not be resolved at allocation; there is no fallback and no
    /// guess.
    #[cfg(unix)]
    #[must_use]
    pub fn slave_path(&self) -> Option<&Path> {
        self.slave_path.as_deref()
    }

    #[cfg(windows)]
    pub(crate) fn new(pty: backend::WindowsPty) -> Self {
        Self {
            io: PtyIo::new(Arc::new(pty)),
        }
    }

    /// Queries the current terminal geometry for this PTY.
    pub fn size(&self) -> Result<TerminalSize> {
        self.io.size()
    }

    /// Resizes this PTY.
    pub fn resize(&self, size: TerminalSize) -> Result<()> {
        self.io.resize(size)
    }

    /// Resizes this PTY, preserving optional pixel geometry where supported.
    pub fn resize_geometry(&self, geometry: TerminalGeometry) -> Result<()> {
        self.io.resize_geometry(geometry)
    }

    /// Clones the master handle.
    pub fn try_clone(&self) -> Result<Self> {
        Ok(Self {
            io: self.io.try_clone()?,
            #[cfg(unix)]
            startup_slave: None,
            #[cfg(unix)]
            slave_path: self.slave_path.clone(),
        })
    }

    /// Clones the master handle for the pane output reader and transfers the
    /// one-shot Unix startup slave guard, if present.
    pub fn try_clone_for_startup_reader(&mut self) -> Result<Self> {
        Ok(Self {
            io: self.io.try_clone()?,
            #[cfg(unix)]
            startup_slave: self.startup_slave.take(),
            #[cfg(unix)]
            slave_path: self.slave_path.clone(),
        })
    }

    /// Duplicates the master handle as an I/O endpoint.
    pub fn try_clone_io(&self) -> Result<PtyIo> {
        self.io.try_clone()
    }

    /// Returns whether bracketed paste can use the Windows ConPTY input pipe.
    ///
    /// Retains the historical inbox paste compatibility selection and also
    /// accepts the companion runtime. When false, callers can inject Unicode
    /// console records to preserve paste delimiters. Despite the legacy name,
    /// this is not a guarantee for arbitrary control sequences: the system
    /// console can preserve paste while consuming OSC replies and output queries.
    #[cfg(windows)]
    #[must_use]
    pub fn preserves_verbatim_input(&self) -> bool {
        self.io.pty.uses_passthrough()
    }

    /// Consumes this master handle into its I/O endpoint.
    #[must_use]
    pub fn into_io(self) -> PtyIo {
        #[cfg(unix)]
        if let Some(slave) = self.startup_slave {
            self.io
                .startup_slave
                .lock()
                .expect("PTY startup slave guard mutex must not be poisoned")
                .replace(slave);
        }
        self.io
    }

    /// Consumes this Unix PTY master and returns its nonblocking owned file descriptor.
    #[cfg(unix)]
    pub fn into_owned_fd(self) -> io::Result<OwnedFd> {
        match Arc::try_unwrap(self.io.fd) {
            Ok(fd) => Ok(fd),
            Err(fd) => fd.as_ref().as_fd().try_clone_to_owned(),
        }
    }

    /// Returns the PTY I/O endpoint.
    #[must_use]
    pub fn io(&self) -> &PtyIo {
        &self.io
    }

    /// Writes all bytes to the PTY master with the default two-second timeout.
    pub fn write_all(&self, bytes: &[u8]) -> io::Result<()> {
        self.io.write_all(bytes)
    }

    /// Writes all bytes to the PTY master while write progress continues.
    /// The timeout measures consecutive inactivity, so a timeout may occur
    /// after the PTY accepted a prefix of `bytes`.
    #[cfg(any(unix, windows))]
    pub fn write_all_with_timeout(&self, bytes: &[u8], timeout: Duration) -> io::Result<()> {
        self.io.write_all_with_timeout(bytes, timeout)
    }

    /// Writes all bytes to the ConPTY master with one bounded recovery window
    /// after an inactivity timeout.
    ///
    /// The recovery preserves byte order across a cancelled overlapped write.
    /// A destination that remains blocked for `recovery_grace` returns a
    /// timeout instead of retrying indefinitely.
    #[cfg(windows)]
    pub fn write_all_with_stall_recovery(
        &self,
        bytes: &[u8],
        timeout: Duration,
        recovery_grace: Duration,
    ) -> io::Result<()> {
        self.io
            .pty
            .write_all_with_stall_recovery(bytes, timeout, recovery_grace)
    }

    /// Writes UTF-8 console records after earlier input-pipe operations for
    /// this Windows PTY have completed.
    #[cfg(windows)]
    pub fn write_windows_console_utf8_ordered(
        &self,
        process_id: ProcessId,
        bytes: &[u8],
        timeout: Duration,
    ) -> io::Result<()> {
        self.io
            .pty
            .write_console_utf8_ordered(process_id, bytes, timeout)
    }

    /// Queues bytes as input for the program on the slave without waiting.
    /// See [`PtyIo::queue_input`].
    #[cfg(unix)]
    pub fn queue_input(&self, bytes: &[u8]) -> io::Result<()> {
        self.io.queue_input(bytes)
    }

    /// Attempts to write bytes to a nonblocking Unix PTY master without waiting.
    ///
    /// Returns the number of bytes accepted immediately. Callers that need to
    /// preserve the full input stream must write the returned suffix through a
    /// blocking or readiness-driven path.
    #[cfg(unix)]
    pub fn try_write_immediate(&self, bytes: &[u8]) -> io::Result<usize> {
        backend::try_write_immediate(self.io.fd.as_ref().as_fd(), bytes)
    }

    /// Returns whether the PTY line discipline currently echoes input bytes.
    #[cfg(unix)]
    pub fn local_echo_enabled(&self) -> io::Result<bool> {
        let termios = tcgetattr(self.io.fd.as_ref().as_fd())?;
        Ok(termios.local_modes.contains(LocalModes::ECHO))
    }

    #[cfg(unix)]
    pub(crate) fn raw_fd(&self) -> RawFd {
        self.io.raw_fd()
    }

    #[cfg(windows)]
    pub(crate) fn windows_pty(&self) -> Arc<backend::WindowsPty> {
        Arc::clone(&self.io.pty)
    }
}

/// A freshly allocated PTY pair.
#[derive(Debug)]
pub struct PtyPair {
    master: PtyMaster,
    #[cfg(unix)]
    slave: PtySlave,
}

impl PtyPair {
    /// Allocates a PTY pair using the platform backend.
    /// Unix master descriptors are made nonblocking before the pair is returned.
    pub fn open() -> Result<Self> {
        #[cfg(unix)]
        {
            let (master, slave) = backend::open_pty_pair()?;
            backend::set_nonblocking(master.as_fd())?;

            // The name of the terminal this pair owns, taken once from the
            // slave descriptor this process holds right here. Resolving it now
            // is what makes it independent of anything the child later does to
            // its own descriptors. A failure leaves it unknown rather than
            // guessed.
            let slave_path = ttyname(slave.as_fd(), Vec::new())
                .ok()
                .map(|name| PathBuf::from(OsString::from_vec(name.into_bytes())));

            Ok(Self {
                master: PtyMaster::new(master).with_slave_path(slave_path),
                slave: PtySlave { fd: slave },
            })
        }

        #[cfg(windows)]
        {
            let master = backend::open_pty_pair(TerminalSize::new(80, 24))?;
            Ok(Self {
                master: PtyMaster::new(master),
            })
        }

        #[cfg(not(unix))]
        #[cfg(not(windows))]
        {
            Err(PtyError::Unsupported(unsupported_op::OPEN_PTY_PAIR))
        }
    }

    /// Allocates a PTY pair and applies an initial window size.
    pub fn open_with_size(size: TerminalSize) -> Result<Self> {
        #[cfg(windows)]
        {
            let master = backend::open_pty_pair(size)?;
            Ok(Self {
                master: PtyMaster::new(master),
            })
        }

        #[cfg(not(windows))]
        {
            let pair = Self::open()?;
            pair.master.resize(size)?;
            Ok(pair)
        }
    }

    /// Returns the master endpoint.
    #[must_use]
    pub fn master(&self) -> &PtyMaster {
        &self.master
    }

    /// Returns the slave endpoint.
    #[cfg(unix)]
    #[must_use]
    pub fn slave(&self) -> &PtySlave {
        &self.slave
    }

    /// Consumes this Unix PTY pair into its master and slave endpoints.
    #[cfg(unix)]
    #[must_use]
    pub fn into_split(self) -> (PtyMaster, PtySlave) {
        (self.master, self.slave)
    }

    /// Consumes the pair and returns the master endpoint.
    #[must_use]
    pub fn into_master(self) -> PtyMaster {
        self.master
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::PtyPair;

    #[test]
    fn startup_slave_guard_transfers_only_to_reader_clone() {
        let pair = PtyPair::open().expect("pty pair");
        let (master, slave) = pair.into_split();
        let startup_slave = slave
            .try_clone()
            .expect("startup slave clone")
            .into_owned_fd();
        let mut master = master.with_startup_slave(startup_slave);

        let regular = master.try_clone().expect("regular master clone");
        assert!(
            regular.startup_slave.is_none(),
            "regular clones must not keep the slave side open"
        );

        let reader = master
            .try_clone_for_startup_reader()
            .expect("reader master clone");
        assert!(master.startup_slave.is_none(), "startup guard is one-shot");
        assert!(
            reader.startup_slave.is_some(),
            "reader clone should receive the startup guard"
        );

        let io = reader.into_io();
        assert!(
            io.startup_slave
                .lock()
                .expect("startup guard mutex")
                .is_some(),
            "reader io keeps the guard until its first read attempt"
        );
        io.release_startup_slave_guard();
        assert!(
            io.startup_slave
                .lock()
                .expect("startup guard mutex")
                .is_none(),
            "reader releases the guard explicitly after startup"
        );
    }
}
