#![forbid(unsafe_code)]
#![deny(missing_docs)]

//! Tokio-based detached RPC server for RMUX.

mod clock_mode;
mod control;
mod control_notifications;
mod copy_mode;
mod daemon;
mod format_runtime;
mod handler;
mod handler_support;
mod hook_compat;
mod hook_runtime;
mod input_keys;
mod key_table;
mod keys;
mod listener;
mod mouse;
mod outer_terminal;
mod pane_io;
mod pane_terminal_lookup;
mod pane_terminal_process;
mod pane_terminals;
mod pane_transcript;
mod renderer;
mod server_access;
mod terminal;
mod wait_for;

pub use daemon::{
    default_socket_path, ConfigFileSelection, ConfigLoadOptions, DaemonConfig, ServerDaemon,
    ServerHandle,
};
