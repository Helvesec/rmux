use std::fmt;
use std::path::Path;
#[cfg(windows)]
use std::path::PathBuf;

use rmux_proto::{
    DaemonStatusRequest, KillServerRequest, LockClientRequest, LockServerRequest,
    LockSessionRequest, Request, Response, ServerAccessRequest, SessionName, ShutdownIfIdleRequest,
};

use crate::{
    auto_start::{ensure_server_running_with_config, AutoStartConfig, AutoStartError},
    connection::{connect, Connection},
    ClientError,
};

/// Connects to the exact endpoint eligible for a `kill-server` request.
///
/// On Windows, a pre-rotation daemon may still own the static endpoint while
/// the current client has resolved a private managed generation. Only an
/// absent managed endpoint may fall back to the legacy endpoint authenticated
/// by the private discovery record. Ordinary commands never use this path.
#[cfg(windows)]
pub fn connect_for_server_shutdown(
    socket_path: &Path,
) -> Result<(Connection, PathBuf), ClientError> {
    let primary_error = match connect(socket_path) {
        Ok(connection) => return Ok((connection, socket_path.to_path_buf())),
        Err(error) if shutdown_endpoint_is_absent(&error) => error,
        Err(error) => return Err(error),
    };
    let Some(legacy_endpoint) = rmux_ipc::legacy_shutdown_endpoint(socket_path)? else {
        return Err(primary_error);
    };
    let legacy_path = legacy_endpoint.into_path();
    connect(&legacy_path).map(|connection| (connection, legacy_path))
}

#[cfg(windows)]
fn shutdown_endpoint_is_absent(error: &ClientError) -> bool {
    matches!(
        error,
        ClientError::Io(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            )
    )
}

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
    /// Windows package clients use this because the daemon can close its reply
    /// pipe during shutdown. Callers must then drop this connection and wait
    /// for endpoint release with [`crate::wait_for_server_endpoint_cleanup`].
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
