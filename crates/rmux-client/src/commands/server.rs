use std::fmt;
use std::path::Path;

use rmux_proto::{
    DaemonStatusRequest, KillServerRequest, LockClientRequest, LockServerRequest,
    LockSessionRequest, Request, Response, ServerAccessRequest, SessionName, ShutdownIfIdleRequest,
};

use crate::{
    auto_start::{ensure_server_running_with_config, AutoStartConfig, AutoStartError},
    connection::{connect, Connection},
    ClientError,
};

impl Connection {
    /// Ensures the server is available, honouring top-level no-start-server behavior.
    pub fn start_server(
        socket_path: &Path,
        no_start_server: bool,
        config: AutoStartConfig,
    ) -> Result<Self, StartServerError> {
        if no_start_server {
            return connect(socket_path).map_err(StartServerError::Client);
        }

        ensure_server_running_with_config(socket_path, config).map_err(StartServerError::AutoStart)
    }

    /// Sends a `kill-server` request over the detached RPC channel.
    pub fn kill_server(&mut self) -> Result<Response, ClientError> {
        self.roundtrip(&Request::KillServer(KillServerRequest))
    }

    /// Sends a `kill-server` request and returns after the frame is written.
    ///
    /// Windows package clients use this for the tmux-style fire-and-forget
    /// shutdown path, where waiting for the daemon's final cleanup dominates
    /// the command latency and the named pipe does not need client-side socket
    /// removal.
    pub fn kill_server_after_write(&mut self) -> Result<(), ClientError> {
        self.write_request(&Request::KillServer(KillServerRequest))
    }

    /// Sends a legacy-wire `kill-server` request for stale daemon cleanup.
    pub fn kill_server_legacy_wire(&mut self, wire_version: u32) -> Result<(), ClientError> {
        self.write_legacy_wire_request(&Request::KillServer(KillServerRequest), wire_version)
    }

    /// Sends an internal daemon status request over the detached RPC channel.
    pub fn daemon_status(&mut self) -> Result<Response, ClientError> {
        self.roundtrip(&Request::DaemonStatus(DaemonStatusRequest))
    }

    /// Sends an internal idle-only daemon shutdown request.
    pub fn shutdown_if_idle(&mut self) -> Result<Response, ClientError> {
        self.roundtrip(&Request::ShutdownIfIdle(ShutdownIfIdleRequest))
    }

    /// Sends a `lock-server` request over the detached RPC channel.
    pub fn lock_server(&mut self) -> Result<Response, ClientError> {
        self.roundtrip(&Request::LockServer(LockServerRequest))
    }

    /// Sends a `lock-session` request over the detached RPC channel.
    pub fn lock_session(&mut self, target: SessionName) -> Result<Response, ClientError> {
        self.roundtrip(&Request::LockSession(LockSessionRequest { target }))
    }

    /// Sends a `lock-client` request over the detached RPC channel.
    pub fn lock_client(&mut self, target_client: String) -> Result<Response, ClientError> {
        self.roundtrip(&Request::LockClient(LockClientRequest { target_client }))
    }

    /// Sends a `server-access` request over the detached RPC channel.
    pub fn server_access(&mut self, request: ServerAccessRequest) -> Result<Response, ClientError> {
        self.roundtrip(&Request::ServerAccess(request))
    }
}

/// Client-side `start-server` failure surface.
#[derive(Debug)]
pub enum StartServerError {
    /// Connecting to an already-running server failed.
    Client(ClientError),
    /// Auto-starting the server failed.
    AutoStart(AutoStartError),
}

impl fmt::Display for StartServerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Client(error) => fmt::Display::fmt(error, formatter),
            Self::AutoStart(error) => fmt::Display::fmt(error, formatter),
        }
    }
}

impl std::error::Error for StartServerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Client(error) => Some(error),
            Self::AutoStart(error) => Some(error),
        }
    }
}
