#![deny(missing_docs)]

//! Local IPC boundary for RMUX.
//!
//! This crate owns endpoint naming and local transport handles. It deliberately
//! transports bytes only; the RMUX request/response protocol stays in
//! `rmux-proto`.

mod endpoint;
mod listener;
mod stream;

pub use endpoint::{default_endpoint, endpoint_for_label, resolve_endpoint, LocalEndpoint};
pub use listener::LocalListener;
pub use stream::{
    connect_blocking, wait_for_peer_close, BlockingLocalStream, LocalStream, PeerIdentity,
};
