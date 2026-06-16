use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use rmux_proto::{Response, CAPABILITY_WEB_SHARE};

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
        upgrade::DaemonFreshness::Current => {
            ensure_required_web_capability_or_restart(connection, socket_path, binary_path, config)
        }
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

fn ensure_required_web_capability_or_restart(
    mut connection: Connection,
    socket_path: &Path,
    binary_path: &Path,
    config: &AutoStartConfig,
) -> Result<Connection, AutoStartError> {
    if !config.web_required || connection.supports_capability(CAPABILITY_WEB_SHARE)? {
        return Ok(connection);
    }

    let Response::DaemonStatus(status) =
        connection.daemon_status().map_err(AutoStartError::Client)?
    else {
        return Err(AutoStartError::IncompatibleDaemon {
            socket_path: socket_path.to_path_buf(),
            message: "running daemon did not report status for web-share upgrade".to_owned(),
        });
    };

    if status.session_count > 0 || status.client_count > 0 {
        return Err(AutoStartError::IncompatibleDaemon {
            socket_path: socket_path.to_path_buf(),
            message: "running daemon was built without web-share support and still owns sessions or clients".to_owned(),
        });
    }

    let Response::ShutdownIfIdle(shutdown) = connection
        .shutdown_if_idle()
        .map_err(AutoStartError::Client)?
    else {
        return Err(AutoStartError::IncompatibleDaemon {
            socket_path: socket_path.to_path_buf(),
            message: "running daemon cannot be restarted for web-share support".to_owned(),
        });
    };

    if !shutdown.shutdown {
        return Err(AutoStartError::IncompatibleDaemon {
            socket_path: socket_path.to_path_buf(),
            message: "running daemon became active before web-share upgrade".to_owned(),
        });
    }

    drop(connection);
    if wait_for_server_absent(socket_path)?.is_some() {
        return Err(AutoStartError::IncompatibleDaemon {
            socket_path: socket_path.to_path_buf(),
            message: format!(
                "running daemon was built without web-share support and is still active with {} sessions and {} clients",
                status.session_count, status.client_count
            ),
        });
    }

    spawn_hidden_daemon_for(binary_path, socket_path, config).map_err(|error| {
        AutoStartError::Launch {
            path: binary_path.to_path_buf(),
            error,
        }
    })?;
    wait_for_connected_server(socket_path, config)
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
                return probe_connected_server(connection, config, socket_path);
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
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    use std::path::Path;
    use std::thread;
    use std::time::Duration;

    use rmux_proto::{
        encode_frame, DaemonStatusRequest, DaemonStatusResponse, FrameDecoder, HandshakeRequest,
        HandshakeResponse, Request, Response, CAPABILITY_DAEMON_SHUTDOWN_IF_IDLE,
        CAPABILITY_DAEMON_STATUS, RMUX_WIRE_VERSION,
    };

    use crate::{ClientError, ConnectResult, Connection};

    use super::{ensure_required_web_capability_or_restart, wait_for_server_absent_with};
    use crate::auto_start::{AutoStartConfig, AutoStartError};

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

    #[test]
    fn web_required_does_not_replace_active_no_web_daemon() {
        let (connection, server_thread) = connection_with_script(vec![
            (
                Request::Handshake(HandshakeRequest::current()),
                Response::Handshake(HandshakeResponse {
                    wire_version: RMUX_WIRE_VERSION,
                    capabilities: vec![
                        CAPABILITY_DAEMON_STATUS.to_owned(),
                        CAPABILITY_DAEMON_SHUTDOWN_IF_IDLE.to_owned(),
                    ],
                }),
            ),
            (
                Request::DaemonStatus(DaemonStatusRequest),
                Response::DaemonStatus(DaemonStatusResponse {
                    rmux_version: env!("CARGO_PKG_VERSION").to_owned(),
                    wire_version: RMUX_WIRE_VERSION,
                    session_count: 1,
                    client_count: 0,
                    config_loading: false,
                }),
            ),
        ]);
        let config = AutoStartConfig::disabled().with_web_required();

        let error = ensure_required_web_capability_or_restart(
            connection,
            Path::new("/tmp/rmux-web-active.sock"),
            Path::new("/bin/rmux"),
            &config,
        )
        .expect_err("active no-web daemon must not be replaced");

        assert!(matches!(error, AutoStartError::IncompatibleDaemon { .. }));
        assert!(error.to_string().contains("without web-share support"));
        server_thread.join().expect("scripted server should finish");
    }

    fn connection_with_script(
        script: Vec<(Request, Response)>,
    ) -> (Connection, thread::JoinHandle<()>) {
        let (client, mut server) = UnixStream::pair().expect("create stream pair");
        let server_thread = thread::spawn(move || {
            let mut decoder = FrameDecoder::new();
            let mut buffer = [0_u8; 1024];
            for (expected_request, response) in script {
                loop {
                    let bytes_read = server.read(&mut buffer).expect("read request");
                    assert_ne!(bytes_read, 0, "client closed before sending request");
                    decoder.push_bytes(&buffer[..bytes_read]);
                    if let Some(request) = decoder
                        .next_frame::<Request>()
                        .expect("decode request frame")
                    {
                        assert_eq!(request, expected_request);
                        let frame = encode_frame(&response).expect("encode response frame");
                        server.write_all(&frame).expect("write response frame");
                        break;
                    }
                }
            }
        });

        (
            Connection::new(client).expect("scripted connection"),
            server_thread,
        )
    }
}
