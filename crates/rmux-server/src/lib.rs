#![forbid(unsafe_code)]
#![deny(missing_docs)]

//! Tokio-based detached RPC server for RMUX.

#[cfg(unix)]
mod clock_mode;
#[cfg(unix)]
mod control;
#[cfg(any(unix, windows))]
#[cfg_attr(windows, allow(dead_code))]
mod control_mode;
#[cfg(unix)]
mod control_notifications;
#[cfg(unix)]
mod copy_mode;
mod daemon;
#[cfg(unix)]
mod format_runtime;
#[cfg(unix)]
mod handler;
#[cfg(unix)]
mod handler_support;
#[cfg(unix)]
mod hook_compat;
#[cfg(unix)]
mod hook_runtime;
#[cfg(unix)]
mod input_keys;
#[cfg(unix)]
mod key_table;
#[cfg(unix)]
mod keys;
#[cfg(unix)]
mod listener;
#[cfg(unix)]
mod mouse;
#[cfg(any(unix, windows))]
#[cfg_attr(windows, allow(dead_code))]
mod outer_terminal;
#[cfg(unix)]
mod pane_io;
#[cfg(any(unix, windows))]
#[cfg_attr(windows, allow(dead_code))]
mod pane_screen_state;
#[cfg(unix)]
mod pane_terminal_lookup;
#[cfg(unix)]
mod pane_terminal_process;
#[cfg(unix)]
mod pane_terminals;
#[cfg(unix)]
mod pane_transcript;
#[cfg(unix)]
mod renderer;
#[cfg(any(unix, windows))]
#[cfg_attr(windows, allow(dead_code))]
mod server_access;
#[cfg(unix)]
mod terminal;
#[cfg(unix)]
mod wait_for;
#[cfg(windows)]
mod windows_runtime;

pub use daemon::{
    default_socket_path, ConfigFileSelection, ConfigLoadOptions, DaemonConfig, ServerDaemon,
    ServerHandle,
};
