#![deny(missing_docs)]

//! Small OS-boundary helpers for RMUX.
//!
//! This crate is intentionally narrow. Add modules only when a real migrated
//! call site consumes them in the same change.

pub mod identity;
pub mod process;
pub mod terminal;
