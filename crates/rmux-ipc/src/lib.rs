#![deny(missing_docs)]

//! Local IPC boundary for RMUX.
//!
//! This crate owns endpoint naming and local transport handles. It deliberately
//! transports bytes only; the RMUX request/response protocol stays in
//! `rmux-proto`.

mod endpoint;
mod listener;
#[cfg(unix)]
mod managed_socket_unix;
mod stream;
#[cfg(windows)]
mod windows_mutex;

pub use endpoint::{
    default_endpoint, endpoint_for_label, resolve_endpoint, resolve_tmux_compatible_endpoint,
    LocalEndpoint,
};
pub use listener::LocalListener;
pub use stream::{
    connect_blocking, is_peer_disconnect, wait_for_peer_close, BlockingLocalStream, LocalStream,
    PeerIdentity,
};
#[cfg(windows)]
pub use stream::{connect_windows_pipe, WindowsPipeClient};
#[cfg(windows)]
pub use windows_mutex::{
    acquire_named_mutex, NamedMutexAcquire, NamedMutexError, NamedMutexGuard, MAX_NAMED_MUTEX_LEN,
};
