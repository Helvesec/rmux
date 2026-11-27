#![deny(missing_docs)]

//! Small OS-boundary helpers for RMUX.
//!
//! This crate is intentionally narrow. Add modules only when a real migrated
//! call site consumes them in the same change.

#[doc(hidden)]
pub mod build_identity;
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
#[cfg(all(feature = "test-seams", target_os = "linux"))]
#[doc(hidden)]
pub mod process_tree_test_support;
#[cfg(windows)]
pub mod shell;
pub mod signals;
pub mod terminal;
#[cfg(all(test, unix))]
#[path = "../../../tests/support/rerun_exact.rs"]
mod test_rerun;

/// Whether an environment variable is private to RMUX processes.
pub fn is_internal_environment_name(name: &std::ffi::OsStr) -> bool {
    let prefix = b"RMUX_INTERNAL_";
    let bytes = name.as_encoded_bytes();
    if cfg!(windows) {
        bytes
            .get(..prefix.len())
            .is_some_and(|name| name.eq_ignore_ascii_case(prefix))
    } else {
        bytes.starts_with(prefix)
    }
}
