use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use crate::{connect_or_absent, upgrade, ConnectResult, Connection};
use tracing::debug;

use super::{
    is_transient_connect_error, probe_connected_server, spawn_hidden_daemon_for, AutoStartConfig,
    AutoStartError,
};

const SEAMLESS_RESTART_TIMEOUT: Duration = Duration::from_secs(5);
const SEAMLESS_RESTART_POLL_INTERVAL: Duration = Duration::from_millis(50);

pub(super) fn ensure_daemon_fresh_or_restart(
    mut connection: Connection,
    socket_path: &Path,
    binary_path: &Path,
    config: &AutoStartConfig,
) -> Result<Connection, AutoStartError> {
    let freshness = match upgrade::inspect_daemon(&mut connection) {
        Ok(freshness) => freshness,
        Err(error) => {
            debug!(
                error = ?error,
                "daemon freshness inspection failed; assuming current daemon"
            );
            return Ok(connection);
        }
    };

    match freshness {
        upgrade::DaemonFreshness::Current => Ok(connection),
        upgrade::DaemonFreshness::StaleActive(stale) => {
            upgrade::warn_stale_active_daemon(&stale, socket_path);
            Ok(connection)
        }
        upgrade::DaemonFreshness::Incompatible(incompatible) => {
            Err(AutoStartError::IncompatibleDaemon {
                socket_path: socket_path.to_path_buf(),
                message: upgrade::incompatible_daemon_message(&incompatible),
            })
        }
        upgrade::DaemonFreshness::StaleIdle(stale) => {
            if !upgrade::request_idle_shutdown(&mut connection, &stale)
                .map_err(AutoStartError::Client)?
            {
                upgrade::warn_stale_active_daemon(&stale, socket_path);
                return Ok(connection);
            }
            drop(connection);
            if let Some(connection) = wait_for_server_absent(socket_path)? {
                upgrade::warn_stale_active_daemon(&stale, socket_path);
                return Ok(connection);
            }
            spawn_hidden_daemon_for(binary_path, socket_path, config).map_err(|error| {
                AutoStartError::Launch {
                    path: binary_path.to_path_buf(),
                    error,
                }
            })?;
            wait_for_connected_server(socket_path, config)
        }
    }
}

fn wait_for_server_absent(socket_path: &Path) -> Result<Option<Connection>, AutoStartError> {
    wait_for_server_absent_with(
        socket_path,
        SEAMLESS_RESTART_TIMEOUT,
        SEAMLESS_RESTART_POLL_INTERVAL,
        || connect_or_absent(socket_path),
    )
}

fn wait_for_server_absent_with<ConnectFn>(
    socket_path: &Path,
    timeout: Duration,
    poll_interval: Duration,
    mut connect: ConnectFn,
) -> Result<Option<Connection>, AutoStartError>
where
    ConnectFn: FnMut() -> Result<ConnectResult, crate::ClientError>,
{
    let deadline = Instant::now() + timeout;
    loop {
        match connect() {
            Ok(ConnectResult::Absent) => return Ok(None),
            Ok(ConnectResult::Connected(connection)) if Instant::now() >= deadline => {
                return Ok(Some(connection));
            }
            Ok(ConnectResult::Connected(_connection)) => {}
            Err(error) if is_transient_connect_error(&error) => {}
            Err(error) => return Err(AutoStartError::Client(error)),
        }

        let now = Instant::now();
        if now >= deadline {
            match connect() {
                Ok(ConnectResult::Connected(connection)) => return Ok(Some(connection)),
                Ok(ConnectResult::Absent) => return Ok(None),
                Err(error) if is_transient_connect_error(&error) => {}
                Err(error) => return Err(AutoStartError::Client(error)),
            }
            return Err(AutoStartError::TimedOut {
                socket_path: socket_path.to_path_buf(),
                waited: timeout,
            });
        }
        thread::sleep(poll_interval.min(deadline.saturating_duration_since(now)));
    }
}

fn wait_for_connected_server(
    socket_path: &Path,
    config: &AutoStartConfig,
) -> Result<Connection, AutoStartError> {
    let deadline = Instant::now() + SEAMLESS_RESTART_TIMEOUT;
    loop {
        match connect_or_absent(socket_path) {
            Ok(ConnectResult::Connected(connection)) => {
                return probe_connected_server(connection, config);
            }
            Ok(ConnectResult::Absent) => {}
            Err(error) if is_transient_connect_error(&error) => {}
            Err(error) => return Err(AutoStartError::Client(error)),
        }

        let now = Instant::now();
        if now >= deadline {
            return Err(AutoStartError::TimedOut {
                socket_path: socket_path.to_path_buf(),
                waited: SEAMLESS_RESTART_TIMEOUT,
            });
        }
        thread::sleep(SEAMLESS_RESTART_POLL_INTERVAL.min(deadline.saturating_duration_since(now)));
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::net::UnixStream;
    use std::path::Path;
    use std::time::Duration;

    use crate::{ClientError, ConnectResult, Connection};

    use super::wait_for_server_absent_with;

    #[test]
    fn wait_for_server_absent_returns_existing_connection_after_timeout() {
        let result = wait_for_server_absent_with(
            Path::new("/tmp/rmux-upgrade-timeout.sock"),
            Duration::from_millis(0),
            Duration::from_millis(1),
            || {
                let (client, _server) = UnixStream::pair().expect("create stream pair");
                Ok(ConnectResult::Connected(
                    Connection::new(client).expect("connection with timeout"),
                ))
            },
        )
        .expect("timeout with reachable server should reconnect");

        assert!(
            result.is_some(),
            "shutdown cancellation should gracefully fall back to the surviving daemon"
        );
    }

    #[test]
    fn wait_for_server_absent_returns_none_when_socket_disappears() {
        let result = wait_for_server_absent_with(
            Path::new("/tmp/rmux-upgrade-absent.sock"),
            Duration::from_millis(10),
            Duration::from_millis(1),
            || Ok(ConnectResult::Absent),
        )
        .expect("absent socket succeeds");

        assert!(result.is_none());
    }

    #[test]
    fn wait_for_server_absent_still_times_out_on_transient_errors() {
        let error = wait_for_server_absent_with(
            Path::new("/tmp/rmux-upgrade-transient.sock"),
            Duration::from_millis(0),
            Duration::from_millis(1),
            || Err(ClientError::Io(std::io::ErrorKind::WouldBlock.into())),
        )
        .expect_err("transient-only state should still time out");

        assert!(matches!(error, super::AutoStartError::TimedOut { .. }));
    }
}
