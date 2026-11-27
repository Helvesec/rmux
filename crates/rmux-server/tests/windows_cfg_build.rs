//! Compile regression for Windows production code that unit-test reexports can hide.
//!
//! Check with `cargo check -p rmux-server --test windows_cfg_build
//! --target x86_64-pc-windows-msvc`. Cargo builds the real server dependency without
//! `cfg(test)`, so Windows resize planning must resolve its lifecycle types there.
//! This target needs no Windows linker or native process execution.

#![cfg(windows)]

use rmux_server::ServerDaemon;

const _: Option<ServerDaemon> = None;
