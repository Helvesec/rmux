use std::io;

#[cfg(unix)]
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd, RawFd};
use std::sync::Arc;
#[cfg(unix)]
use std::sync::Mutex;

use crate::backend;
#[cfg(all(not(unix), not(windows)))]
use crate::unsupported_op;
#[cfg(all(not(unix), not(windows)))]
use crate::PtyError;
use crate::{Result, TerminalGeometry, TerminalSize};
#[cfg(unix)]
use rustix::termios::{tcgetattr, LocalModes};

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

/// The I/O endpoint for a pseudoterminal.
#[derive(Debug)]
pub struct PtyIo {
    #[cfg(unix)]
    fd: Arc<OwnedFd>,
    #[cfg(unix)]
    startup_slave: Mutex<Option<OwnedFd>>,
    #[cfg(windows)]
    pty: Arc<backend::WindowsPty>,
}

impl PtyIo {
    #[cfg(unix)]
    pub(crate) fn new(fd: OwnedFd) -> Self {
        Self {
            fd: Arc::new(fd),
            startup_slave: Mutex::new(None),
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

    /// Reads bytes from this PTY endpoint.
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

    /// Writes all bytes to this PTY endpoint.
    pub fn write_all(&self, bytes: &[u8]) -> io::Result<()> {
        #[cfg(unix)]
        {
            backend::write_all(self.fd.as_ref().as_fd(), bytes)
        }

        #[cfg(not(unix))]
        {
            #[cfg(windows)]
            {
                self.pty.write_all(bytes)
            }

            #[cfg(not(windows))]
            {
                let _ = bytes;
                Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "pty I/O is unsupported on this platform",
                ))
            }
        }
    }

    /// Makes the PTY endpoint nonblocking.
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

    /// Returns a borrowed Unix descriptor for integration points that still
    /// require `AsyncFd`.
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

/// The master handle of a pseudoterminal.
#[derive(Debug)]
pub struct PtyMaster {
    io: PtyIo,
    #[cfg(unix)]
    startup_slave: Option<OwnedFd>,
}

impl PtyMaster {
    #[cfg(unix)]
    pub(crate) fn new(fd: OwnedFd) -> Self {
        Self {
            io: PtyIo::new(fd),
            startup_slave: None,
        }
    }

    #[cfg(unix)]
    pub(crate) fn with_startup_slave(mut self, slave: OwnedFd) -> Self {
        self.startup_slave = Some(slave);
        self
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
        })
    }

    /// Clones the master handle for the pane output reader and transfers the
    /// one-shot Unix startup slave guard, if present.
    pub fn try_clone_for_startup_reader(&mut self) -> Result<Self> {
        Ok(Self {
            io: self.io.try_clone()?,
            #[cfg(unix)]
            startup_slave: self.startup_slave.take(),
        })
    }

    /// Duplicates the master handle as an I/O endpoint.
    pub fn try_clone_io(&self) -> Result<PtyIo> {
        self.io.try_clone()
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

    /// Consumes this Unix PTY master and returns the owned file descriptor.
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

    /// Writes all bytes to the PTY master.
    pub fn write_all(&self, bytes: &[u8]) -> io::Result<()> {
        self.io.write_all(bytes)
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
    pub fn open() -> Result<Self> {
        #[cfg(unix)]
        {
            let (master, slave) = backend::open_pty_pair()?;

            Ok(Self {
                master: PtyMaster::new(master),
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
