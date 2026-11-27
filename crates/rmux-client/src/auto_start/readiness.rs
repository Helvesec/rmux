//! Readiness probing of a connected daemon during auto-start.

use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

#[cfg(windows)]
use rmux_proto::DaemonStatusResponse;
use rmux_proto::{Response, RmuxError};

use crate::{upgrade, ClientError, Connection};

use super::{AutoStartConfig, AutoStartError, DEFAULT_STARTUP_DEADLINE, STARTUP_POLL_INTERVAL};

#[cfg(windows)]
pub(super) fn probe_connected_server_windows(
    mut connection: Connection,
    _config: &AutoStartConfig,
    socket_path: &Path,
) -> Result<(Connection, Option<DaemonStatusResponse>), AutoStartError> {
    let deadline = Instant::now() + DEFAULT_STARTUP_DEADLINE;
    let mut poll_attempt = 0_u32;
    loop {
        match probe_server_readiness_status(&mut connection) {
            Ok(status) => return Ok((connection, status)),
            Err(ClientError::Protocol(RmuxError::UnsupportedWireVersion { got, .. })) => {
                return Err(AutoStartError::IncompatibleDaemon {
                    socket_path: socket_path.to_path_buf(),
                    message: upgrade::incompatible_daemon_message(&upgrade::IncompatibleDaemon {
                        daemon_version: None,
                        daemon_wire_version: Some(got),
                    }),
                });
            }
            Err(error) if is_transient_connect_error(&error) && Instant::now() < deadline => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                std::thread::sleep(startup_readiness_poll_sleep(&mut poll_attempt, remaining));
            }
            Err(error) => return Err(AutoStartError::Client(error)),
        }
    }
}

#[cfg(windows)]
fn probe_server_readiness_status(
    connection: &mut Connection,
) -> Result<Option<DaemonStatusResponse>, ClientError> {
    let response = connection.daemon_status()?;
    match response {
        Response::DaemonStatus(status) if status.config_loading => {
            Err(ClientError::Io(io::Error::new(
                io::ErrorKind::WouldBlock,
                "daemon is still loading startup config",
            )))
        }
        Response::DaemonStatus(status) => Ok(Some(status)),
        Response::Error(_) => Ok(None),
        other => Err(ClientError::Protocol(rmux_proto::RmuxError::Server(
            format!("unexpected readiness response: {other:?}"),
        ))),
    }
}

pub(super) fn probe_connected_server(
    mut connection: Connection,
    _config: &AutoStartConfig,
    socket_path: &Path,
) -> Result<Connection, AutoStartError> {
    let deadline = Instant::now() + DEFAULT_STARTUP_DEADLINE;
    let mut poll_attempt = 0_u32;
    loop {
        match probe_server_readiness(&mut connection) {
            Ok(()) => return Ok(connection),
            Err(ClientError::Protocol(RmuxError::UnsupportedWireVersion { got, .. })) => {
                return Err(AutoStartError::IncompatibleDaemon {
                    socket_path: socket_path.to_path_buf(),
                    message: upgrade::incompatible_daemon_message(&upgrade::IncompatibleDaemon {
                        daemon_version: None,
                        daemon_wire_version: Some(got),
                    }),
                });
            }
            Err(error) if is_transient_connect_error(&error) && Instant::now() < deadline => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                std::thread::sleep(startup_readiness_poll_sleep(&mut poll_attempt, remaining));
            }
            Err(error) => return Err(AutoStartError::Client(error)),
        }
    }
}

pub(super) fn startup_readiness_poll_sleep(
    poll_attempt: &mut u32,
    remaining: Duration,
) -> Duration {
    #[cfg(windows)]
    {
        const INITIAL_POLL_MILLIS: u64 = 1;

        let shift = (*poll_attempt).min(6);
        *poll_attempt = (*poll_attempt).saturating_add(1);
        let millis = INITIAL_POLL_MILLIS
            .checked_shl(shift)
            .unwrap_or(u64::MAX)
            .min(STARTUP_POLL_INTERVAL.as_millis() as u64);
        Duration::from_millis(millis).min(remaining)
    }

    #[cfg(not(windows))]
    {
        const INITIAL_POLL_MILLIS: u64 = 1;

        let shift = (*poll_attempt).min(6);
        *poll_attempt = (*poll_attempt).saturating_add(1);
        let millis = INITIAL_POLL_MILLIS
            .checked_shl(shift)
            .unwrap_or(u64::MAX)
            .min(STARTUP_POLL_INTERVAL.as_millis() as u64);
        Duration::from_millis(millis).min(remaining)
    }
}

pub(super) fn is_transient_connect_error(error: &ClientError) -> bool {
    matches!(
        error,
        ClientError::Io(io_error)
            if matches!(
                io_error.kind(),
                io::ErrorKind::WouldBlock
                    | io::ErrorKind::Interrupted
                    | io::ErrorKind::TimedOut
            )
    )
}

pub(super) fn probe_server_readiness(connection: &mut Connection) -> Result<(), ClientError> {
    let response = connection.daemon_status()?;
    match response {
        Response::DaemonStatus(status) if status.config_loading => {
            Err(ClientError::Io(io::Error::new(
                io::ErrorKind::WouldBlock,
                "daemon is still loading startup config",
            )))
        }
        Response::DaemonStatus(_) => Ok(()),
        Response::Error(_) => Ok(()),
        other => Err(ClientError::Protocol(rmux_proto::RmuxError::Server(
            format!("unexpected readiness response: {other:?}"),
        ))),
    }
}
