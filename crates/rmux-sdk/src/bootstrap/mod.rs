//! Runtime bootstrap helpers for SDK daemon discovery.

pub(crate) mod deadline;
pub mod discovery;
#[cfg(unix)]
pub mod startup_unix;
#[cfg(windows)]
pub mod startup_windows;
