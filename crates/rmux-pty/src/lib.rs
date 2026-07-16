#![deny(missing_docs)]
#![deny(unsafe_op_in_unsafe_fn)]

//! PTY allocation, sizing, and child-process management for RMUX.
//!
//! This crate confines PTY and terminal-control boundaries behind a small,
//! documented API that exposes:
//! - PTY allocation,
//! - terminal size query and resize on PTY file descriptors,
//! - child spawning into a controlling terminal-backed PTY, and
//! - child signaling and reaping.

mod backend;
mod child;
mod pty;
mod size;
#[cfg(windows)]
mod windows_console_input;

#[cfg(any(test, all(not(unix), not(windows))))]
pub(crate) mod unsupported_op {
    //! Canonical operation tokens carried by `PtyError::Unsupported` arms.
    //!
    //! The Tier-3 (`cfg(all(not(unix), not(windows)))`) call sites in `pty.rs`
    //! and `child.rs` reference these constants by name, and the
    //! platform-agnostic inventory test in this crate's `tests` module reads
    //! the same `ALL` slice.
    pub(crate) const OPEN_PTY_PAIR: &str = "open pty pair";
    pub(crate) const SPAWN_PTY_CHILD: &str = "spawn pty child";
    pub(crate) const WAIT_FOR_PTY_CHILD: &str = "wait for pty child";
    pub(crate) const TRY_WAIT_FOR_PTY_CHILD: &str = "try wait for pty child";
    pub(crate) const SIGNAL_PTY_FOREGROUND: &str = "signal pty foreground process group";
    pub(crate) const SIGNAL_PTY_SESSION_LEADER: &str = "signal pty session leader";
    pub(crate) const QUERY_PTY_SIZE: &str = "query pty size";
    pub(crate) const RESIZE_PTY: &str = "resize pty";
    pub(crate) const CLONE_PTY_IO: &str = "clone pty io";

    pub(crate) const ALL: &[&str] = &[
        OPEN_PTY_PAIR,
        SPAWN_PTY_CHILD,
        WAIT_FOR_PTY_CHILD,
        TRY_WAIT_FOR_PTY_CHILD,
        SIGNAL_PTY_FOREGROUND,
        SIGNAL_PTY_SESSION_LEADER,
        QUERY_PTY_SIZE,
        RESIZE_PTY,
        CLONE_PTY_IO,
    ];
}

use std::error::Error as StdError;
use std::ffi::NulError;
use std::fmt;

pub use child::{ChildCommand, PtyChild, SpawnedPty};
#[cfg(unix)]
pub use pty::PtySlave;
pub use pty::{PtyIo, PtyMaster, PtyPair};
pub use size::{TerminalGeometry, TerminalPixels, TerminalSize};
#[cfg(windows)]
pub use windows_console_input::{
    send_windows_console_interrupt, write_windows_console_key, write_windows_console_key_batch,
    write_windows_console_key_then_interrupt_if_processed, write_windows_console_mouse_drag,
    WindowsConsoleKeyEvent,
};

/// A crate-local result type for PTY operations.
pub type Result<T> = std::result::Result<T, PtyError>;

/// A platform-neutral process identifier for PTY children.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct ProcessId(u32);

impl ProcessId {
    /// Creates a process identifier from the operating-system value.
    pub fn new(raw: u32) -> Result<Self> {
        if raw == 0 {
            return Err(PtyError::InvalidPid(raw));
        }
        Ok(Self(raw))
    }

    /// Returns the raw operating-system process id.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self.0
    }

    #[cfg(unix)]
    pub(crate) fn as_rustix_pid(self) -> Result<rustix::process::Pid> {
        let raw = i32::try_from(self.0).map_err(|_| PtyError::InvalidPid(self.0))?;
        rustix::process::Pid::from_raw(raw).ok_or(PtyError::InvalidPid(self.0))
    }
}

/// A high-level child process termination request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PtySignal {
    /// Ask the foreground program to interrupt its current operation.
    Interrupt,
    /// Ask the process group to terminate gracefully.
    Terminate,
    /// Forcefully stop the process group.
    Kill,
    /// Hang up the terminal session.
    Hangup,
    /// Continue a stopped foreground process group.
    Continue,
}

/// Compatibility signal names used by existing RMUX call sites.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Signal(PtySignal);

impl Signal {
    /// Interrupt request.
    pub const INT: Self = Self(PtySignal::Interrupt);
    /// Termination request.
    pub const TERM: Self = Self(PtySignal::Terminate);
    /// Force-kill request.
    pub const KILL: Self = Self(PtySignal::Kill);
    /// Hangup request.
    pub const HUP: Self = Self(PtySignal::Hangup);
    /// Continue request.
    pub const CONT: Self = Self(PtySignal::Continue);

    #[cfg(unix)]
    pub(crate) const fn as_rustix_signal(self) -> rustix::process::Signal {
        match self.0 {
            PtySignal::Interrupt => rustix::process::Signal::INT,
            PtySignal::Terminate => rustix::process::Signal::TERM,
            PtySignal::Kill => rustix::process::Signal::KILL,
            PtySignal::Hangup => rustix::process::Signal::HUP,
            PtySignal::Continue => rustix::process::Signal::CONT,
        }
    }
}

/// Errors produced by PTY allocation, resize, and child-process operations.
#[derive(Debug)]
pub enum PtyError {
    /// A syscall-backed PTY or terminal-control error.
    #[cfg(unix)]
    Os(rustix::io::Errno),
    /// A child-process spawn or wait error from the standard library.
    Spawn(std::io::Error),
    /// A command path, argument, or environment value contained an interior NUL.
    Nul(NulError),
    /// `std::process` returned a PID that could not be represented by RMUX.
    InvalidPid(u32),
    /// The requested PTY operation is not implemented by this platform backend yet.
    Unsupported(&'static str),
}

impl fmt::Display for PtyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            #[cfg(unix)]
            Self::Os(errno) => write!(formatter, "pty syscall failed: {errno}"),
            Self::Spawn(error) => write!(formatter, "child process operation failed: {error}"),
            Self::Nul(error) => write!(
                formatter,
                "interior NUL byte in process configuration: {error}"
            ),
            Self::InvalidPid(pid) => {
                write!(formatter, "child process returned an invalid pid: {pid}")
            }
            Self::Unsupported(operation) => {
                write!(
                    formatter,
                    "pty operation is unsupported on this platform: {operation}"
                )
            }
        }
    }
}

impl StdError for PtyError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            #[cfg(unix)]
            Self::Os(errno) => Some(errno),
            Self::Spawn(error) => Some(error),
            Self::Nul(error) => Some(error),
            Self::InvalidPid(_) => None,
            Self::Unsupported(_) => None,
        }
    }
}

#[cfg(unix)]
impl From<rustix::io::Errno> for PtyError {
    fn from(value: rustix::io::Errno) -> Self {
        Self::Os(value)
    }
}

impl From<std::io::Error> for PtyError {
    fn from(value: std::io::Error) -> Self {
        Self::Spawn(value)
    }
}

impl From<NulError> for PtyError {
    fn from(value: NulError) -> Self {
        Self::Nul(value)
    }
}

#[cfg(test)]
mod tests {
    use super::{unsupported_op, PtyError};

    #[test]
    fn pty_error_unsupported_display_is_stable_for_documented_operations() {
        for operation in unsupported_op::ALL {
            let formatted = format!("{}", PtyError::Unsupported(operation));
            assert_eq!(
                formatted,
                format!("pty operation is unsupported on this platform: {operation}")
            );
        }
    }

    #[test]
    fn pty_error_unsupported_inventory_matches_documented_count() {
        assert_eq!(unsupported_op::ALL.len(), 9);
    }

    #[test]
    fn pty_error_unsupported_inventory_entries_are_unique_and_non_empty() {
        use std::collections::BTreeSet;

        let unique: BTreeSet<&&str> = unsupported_op::ALL.iter().collect();
        assert_eq!(unique.len(), unsupported_op::ALL.len());
        for operation in unsupported_op::ALL {
            assert!(!operation.is_empty());
            assert!(!operation.contains(':'));
        }
    }

    #[test]
    fn pty_error_unsupported_carries_no_source() {
        use std::error::Error as _;

        let err = PtyError::Unsupported(unsupported_op::QUERY_PTY_SIZE);
        assert!(err.source().is_none());
    }

    #[cfg(all(not(unix), not(windows)))]
    #[test]
    fn unsupported_backend_returns_explicit_errors() {
        use std::io;

        use super::{ChildCommand, PtyPair};

        let open_pair =
            PtyPair::open().expect_err("non-Unix non-Windows targets have no PTY backend");
        assert!(matches!(
            open_pair,
            PtyError::Unsupported(op) if op == unsupported_op::OPEN_PTY_PAIR
        ));

        let spawn = ChildCommand::new("cmd.exe")
            .spawn()
            .expect_err("non-Unix non-Windows targets have no PTY backend");
        assert!(matches!(
            spawn,
            PtyError::Unsupported(op) if op == unsupported_op::SPAWN_PTY_CHILD
        ));

        // The Tier-3 read/write/set_nonblocking arms in `pty.rs` return a typed
        // `io::Error` with `ErrorKind::Unsupported`. They cannot be exercised
        // here without a `PtyIo` instance (no constructor on Tier-3), so the
        // Tier-3 contract for those call sites is enforced at compile time by
        // the `cfg(not(windows))` arm in `pty.rs` together with the shared
        // `unsupported_op` constant module referenced both here and at every
        // Tier-3 producer.
        let _ = io::ErrorKind::Unsupported;
    }
}
