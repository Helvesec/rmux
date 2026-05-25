#![allow(dead_code)]

mod backlog;
mod leases;
mod origin;
mod registry;
mod server;
mod websocket;

pub(crate) use registry::{WebShareAccess, WebShareRegistry};
pub(crate) use server::spawn;

#[cfg(test)]
mod tests;
