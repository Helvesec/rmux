//! Passthrough-mode mount of `handler_attach_tests.rs`.
//!
//! Sister of `handler_attach_tests_normal_mount.rs`.  Defining
//! `FORCE_PASSTHROUGH = true` here causes every helper that creates a
//! session to use `NewSessionExtRequest { passthrough: true, ... }`,
//! so the entire test suite runs against a passthrough session.
//!
//! Gated by the `passthrough-global-tests` feature so the default
//! `cargo test` only runs normal mode.

pub(super) const FORCE_PASSTHROUGH: bool = true;

#[path = "handler_attach_tests.rs"]
mod inner;
