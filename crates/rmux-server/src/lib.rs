#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![deny(clippy::await_holding_lock)]

//! Tokio-based detached RPC server for RMUX.

#[cfg(any(unix, windows))]
#[cfg_attr(windows, allow(dead_code))]
mod automatic_rename;
#[cfg(any(unix, windows))]
#[cfg_attr(windows, allow(dead_code))]
mod client_flags;
#[cfg(any(unix, windows))]
#[cfg_attr(windows, allow(dead_code))]
mod clock_mode;
#[cfg(any(unix, windows))]
#[cfg_attr(windows, allow(dead_code))]
mod control;
#[cfg(any(unix, windows))]
#[cfg_attr(windows, allow(dead_code))]
mod control_mode;
#[cfg(any(unix, windows))]
#[cfg_attr(windows, allow(dead_code))]
mod control_notifications;
#[cfg(any(unix, windows))]
#[cfg_attr(windows, allow(dead_code))]
mod copy_mode;
mod daemon;
#[cfg(any(unix, windows))]
mod diagnostic_log;
#[cfg(any(unix, windows))]
#[cfg_attr(windows, allow(dead_code))]
mod format_runtime;
#[cfg(any(unix, windows))]
#[cfg_attr(windows, allow(dead_code))]
mod handler;
#[cfg(any(unix, windows))]
#[cfg_attr(windows, allow(dead_code))]
mod handler_support;
#[cfg(any(unix, windows))]
#[cfg_attr(windows, allow(dead_code))]
mod hook_compat;
#[cfg(any(unix, windows))]
#[cfg_attr(windows, allow(dead_code))]
mod hook_runtime;
#[cfg(any(unix, windows))]
mod host_name;
#[cfg(any(unix, windows))]
mod input_keys;
#[cfg(any(unix, windows))]
#[cfg_attr(windows, allow(dead_code))]
mod key_table;
#[cfg(any(unix, windows))]
#[cfg_attr(windows, allow(dead_code))]
mod keys;
#[cfg(any(unix, windows))]
#[cfg_attr(windows, allow(dead_code))]
mod legacy_command;
#[cfg(any(unix, windows))]
mod limits;
#[cfg(any(unix, windows))]
#[cfg_attr(windows, allow(dead_code))]
mod listener;
#[cfg(any(unix, windows))]
#[cfg_attr(windows, allow(dead_code))]
mod listener_options;
#[cfg(any(unix, windows))]
#[cfg_attr(windows, allow(dead_code))]
mod listener_signals;
#[cfg(any(unix, windows))]
mod mouse;
#[cfg(any(unix, windows))]
#[cfg_attr(windows, allow(dead_code))]
mod outer_terminal;
#[cfg(any(unix, windows))]
#[cfg_attr(windows, allow(dead_code))]
mod pane_indices;
#[cfg(any(unix, windows))]
#[cfg_attr(windows, allow(dead_code))]
mod pane_io;
#[cfg(unix)]
mod pane_reader_runtime;
#[cfg(any(unix, windows))]
#[cfg_attr(windows, allow(dead_code))]
mod pane_screen_state;
#[cfg(any(unix, windows))]
#[cfg_attr(windows, allow(dead_code))]
mod pane_terminal_lookup;
#[cfg(any(unix, windows))]
#[cfg_attr(windows, allow(dead_code))]
mod pane_terminal_process;
#[cfg(any(unix, windows))]
#[cfg_attr(windows, allow(dead_code))]
mod pane_terminals;
#[cfg(any(unix, windows))]
#[cfg_attr(windows, allow(dead_code))]
mod pane_transcript;
#[cfg(any(unix, windows))]
#[cfg_attr(windows, allow(dead_code))]
mod pane_visible_geometry;
#[cfg(any(unix, windows))]
mod perf_instrument;
#[cfg(any(unix, windows))]
#[cfg_attr(windows, allow(dead_code))]
mod renderer;
#[cfg(any(unix, windows))]
#[cfg_attr(windows, allow(dead_code))]
mod server_access;
mod signals;
#[cfg(any(unix, windows))]
#[cfg_attr(windows, allow(dead_code))]
mod socket_cleanup;
#[cfg(any(unix, windows))]
mod status_lines;
#[cfg(any(unix, windows))]
#[cfg_attr(windows, allow(dead_code))]
mod status_ranges;
#[cfg(any(unix, windows))]
#[cfg_attr(windows, allow(dead_code))]
mod terminal;
#[cfg(test)]
mod test_env;
#[cfg(test)]
mod test_shell;
#[cfg(any(unix, windows))]
mod tmux_shim;
#[cfg(unix)]
mod unix_socket;
#[cfg(any(unix, windows))]
#[cfg_attr(windows, allow(dead_code))]
mod wait_for;
#[cfg(all(any(unix, windows), feature = "web"))]
mod web;

/// Fuzzing entry points for protocol parsers.
#[cfg(all(any(unix, windows), feature = "web", feature = "fuzzing"))]
#[doc(hidden)]
pub mod fuzzing {
    /// Feeds arbitrary bytes into the server-side share client-frame parser.
    pub fn websocket_client_frame(data: &[u8]) {
        crate::web::fuzz_client_frame(data);
    }
}

pub use daemon::{
    default_socket_path, ConfigFileSelection, ConfigLoadOptions, DaemonConfig, ServerDaemon,
    ServerHandle,
};
