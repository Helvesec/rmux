#![deny(missing_docs)]

//! Small OS-boundary helpers for RMUX.
//!
//! This crate is intentionally narrow. Add modules only when a real migrated
//! call site consumes them in the same change.

#[cfg(windows)]
pub mod command;
pub mod daemon;
pub mod host;
pub mod identity;
pub mod memory;
pub mod path;
#[cfg(windows)]
pub mod pipe;
pub mod process;
pub mod process_tree;
#[cfg(windows)]
pub mod shell;
pub mod signals;
pub mod terminal;
