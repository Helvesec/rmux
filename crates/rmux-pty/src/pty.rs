use std::os::fd::{AsFd, BorrowedFd, OwnedFd};

use rustix::pty::{grantpt, ioctl_tiocgptpeer, openpt, unlockpt, OpenptFlags};
use rustix::termios::{tcgetwinsize, tcsetwinsize};

use crate::{Result, TerminalSize};

/// The master endpoint of a Linux pseudoterminal.
#[derive(Debug)]
pub struct PtyMaster {
    fd: OwnedFd,
}

impl PtyMaster {
    /// Queries the current terminal geometry for this PTY.
    pub fn size(&self) -> Result<TerminalSize> {
        query_size(self.as_fd())
    }

    /// Resizes this PTY.
    pub fn resize(&self, size: TerminalSize) -> Result<()> {
        apply_size(self.as_fd(), size)
    }

    /// Duplicates the master file descriptor.
    pub fn try_clone(&self) -> Result<Self> {
        Ok(Self {
            fd: self.fd.try_clone()?,
        })
    }

    /// Consumes the wrapper and returns the owned file descriptor.
    #[must_use]
    pub fn into_owned_fd(self) -> OwnedFd {
        self.fd
    }
}

impl AsFd for PtyMaster {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

/// The slave endpoint of a Linux pseudoterminal.
#[derive(Debug)]
pub struct PtySlave {
    fd: OwnedFd,
}

impl PtySlave {
    /// Queries the current terminal geometry for this PTY.
    pub fn size(&self) -> Result<TerminalSize> {
        query_size(self.as_fd())
    }

    /// Duplicates the slave file descriptor.
    pub fn try_clone(&self) -> Result<Self> {
        Ok(Self {
            fd: self.fd.try_clone()?,
        })
    }

    /// Consumes the wrapper and returns the owned file descriptor.
    #[must_use]
    pub fn into_owned_fd(self) -> OwnedFd {
        self.fd
    }
}

impl AsFd for PtySlave {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

/// A freshly allocated PTY master/slave pair.
#[derive(Debug)]
pub struct PtyPair {
    master: PtyMaster,
    slave: PtySlave,
}

impl PtyPair {
    /// Allocates a PTY pair using the Linux `ptmx` allocator.
    pub fn open() -> Result<Self> {
        let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC)?;
        grantpt(&master)?;
        unlockpt(&master)?;

        let slave = ioctl_tiocgptpeer(
            &master,
            OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC,
        )?;

        Ok(Self {
            master: PtyMaster { fd: master },
            slave: PtySlave { fd: slave },
        })
    }

    /// Allocates a PTY pair and applies an initial window size.
    pub fn open_with_size(size: TerminalSize) -> Result<Self> {
        let pair = Self::open()?;
        pair.master.resize(size)?;
        Ok(pair)
    }

    /// Returns the master endpoint.
    #[must_use]
    pub fn master(&self) -> &PtyMaster {
        &self.master
    }

    /// Returns the slave endpoint.
    #[must_use]
    pub fn slave(&self) -> &PtySlave {
        &self.slave
    }

    /// Consumes the pair and returns the two endpoints.
    #[must_use]
    pub fn into_split(self) -> (PtyMaster, PtySlave) {
        (self.master, self.slave)
    }
}

fn query_size(fd: BorrowedFd<'_>) -> Result<TerminalSize> {
    Ok(TerminalSize::from_winsize(tcgetwinsize(fd)?))
}

fn apply_size(fd: BorrowedFd<'_>, size: TerminalSize) -> Result<()> {
    tcsetwinsize(fd, size.into_winsize())?;
    Ok(())
}
