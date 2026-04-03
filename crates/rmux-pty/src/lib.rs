#![deny(missing_docs)]
#![deny(unsafe_op_in_unsafe_fn)]
#![cfg_attr(not(target_os = "linux"), allow(unused_imports, unused_variables))]

//! Linux PTY allocation, sizing, and child-process management for RMUX.
//!
//! This crate confines PTY and terminal-control boundaries behind a small,
//! documented API that exposes:
//! - PTY master/slave allocation,
//! - terminal size query and resize on PTY file descriptors,
//! - child spawning into a controlling terminal-backed PTY, and
//! - child signaling and reaping.

#[cfg(not(target_os = "linux"))]
compile_error!("rmux-pty only supports Linux");

mod child;
mod pty;
mod size;

use std::error::Error as StdError;
use std::ffi::NulError;
use std::fmt;

pub use child::{ChildCommand, PtyChild, SpawnedPty};
pub use pty::{PtyMaster, PtyPair, PtySlave};
pub use rustix::process::Signal;
pub use size::TerminalSize;

/// A crate-local result type for PTY operations.
pub type Result<T> = std::result::Result<T, PtyError>;

/// Errors produced by PTY allocation, resize, and child-process operations.
#[derive(Debug)]
pub enum PtyError {
    /// A syscall-backed PTY or terminal-control error.
    Os(rustix::io::Errno),
    /// A child-process spawn or wait error from the standard library.
    Spawn(std::io::Error),
    /// A command path, argument, or environment value contained an interior NUL.
    Nul(NulError),
    /// `std::process` returned a PID that could not be represented as a `rustix` PID.
    InvalidPid(u32),
}

impl fmt::Display for PtyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Os(errno) => write!(formatter, "pty syscall failed: {errno}"),
            Self::Spawn(error) => write!(formatter, "child process operation failed: {error}"),
            Self::Nul(error) => write!(
                formatter,
                "interior NUL byte in process configuration: {error}"
            ),
            Self::InvalidPid(pid) => {
                write!(formatter, "child process returned an invalid pid: {pid}")
            }
        }
    }
}

impl StdError for PtyError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Os(errno) => Some(errno),
            Self::Spawn(error) => Some(error),
            Self::Nul(error) => Some(error),
            Self::InvalidPid(_) => None,
        }
    }
}

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
