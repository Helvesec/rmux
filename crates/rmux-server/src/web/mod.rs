#![allow(dead_code)]

mod backlog;
mod leases;
mod origin;
mod protocol;
mod registry;
mod server;
mod websocket;

pub(crate) use registry::{
    WebShareAccess, WebShareConnectRole, WebShareRegistry, WebShareRevokeReason, WebShareSettings,
};
pub(crate) use server::spawn;

#[cfg(test)]
mod tests;
