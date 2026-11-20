//! Raw terminal lifecycle and attach-stream helpers for attach-mode clients.

mod screen;
mod terminal_cleanup;
#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
pub use unix::*;
#[cfg(windows)]
pub use windows::*;
