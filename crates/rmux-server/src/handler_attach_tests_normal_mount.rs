//! Normal-mode mount of `handler_attach_tests.rs`.
//!
//! Both this file and `handler_attach_tests_passthrough_mount.rs` exist
//! solely to declare `FORCE_PASSTHROUGH` *before* including the shared
//! source.  The single mounted file at `handler_attach_tests.rs` reads
//! `super::FORCE_PASSTHROUGH` to decide whether to create sessions via
//! `NewSessionRequest` (normal) or `NewSessionExtRequest { passthrough: true, ... }`.
//!
//! See [`mod inner`] for the actual test bodies.

pub(super) const FORCE_PASSTHROUGH: bool = false;

#[path = "handler_attach_tests.rs"]
mod inner;
