//! Internal daemon startup coordination shared by the SDK and CLI client.
//!
//! The platform modules own startup locking, endpoint reservations, probes and
//! readiness deadlines. Callers supply launchers and retain their discovery,
//! configuration, upgrade and reconnect policies. These low-level helpers are
//! runtime implementation details; SDK applications use
//! `rmux_sdk::Rmux::builder().connect_or_start()` to obtain an SDK handle.
//!
//! This crate is packaged so published RMUX crates can share one implementation.
//! It is not part of the public SDK facade.

#![forbid(unsafe_code)]

pub mod deadline;
pub mod hidden_daemon;
#[cfg(unix)]
pub mod startup_unix;
#[cfg(windows)]
pub mod startup_windows;
