//! Polling startup path for targets without a startup serializer.
//!
//! Unix and Windows serialize daemon startup through `rmux_bootstrap`. Other
//! targets launch the hidden daemon and poll the socket until it answers; Unix
//! unit tests drive the same polling loop with injected connect and launch
//! closures.

use std::path::Path;
use std::time::{Duration, Instant};

use super::{
    is_transient_connect_error, AutoStartError, EnsuredServerConnection, ServerConnectionProvenance,
};
#[cfg(not(any(unix, windows)))]
use super::{launch_hidden_daemon, probe_server_readiness, AutoStartConfig};
use crate::{ClientError, ConnectResult, Connection};

#[cfg(not(any(unix, windows)))]
const AUTO_START_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(not(any(unix, windows)))]
const POLL_INTERVAL: Duration = Duration::from_millis(50);

#[cfg(not(any(unix, windows)))]
pub(super) fn ensure_server_running_polling(
    socket_path: &Path,
    config: AutoStartConfig,
) -> Result<EnsuredServerConnection, AutoStartError> {
    if config.loads_startup_config() {
        return ensure_server_running_with_probe_outcome(
            socket_path,
            AUTO_START_TIMEOUT,
            POLL_INTERVAL,
            || crate::connect_or_absent(socket_path),
            || launch_hidden_daemon(socket_path, &config),
            |_| Ok(()),
        );
    }

    ensure_server_running_with_probe_outcome(
        socket_path,
        AUTO_START_TIMEOUT,
        POLL_INTERVAL,
        || crate::connect_or_absent(socket_path),
        || launch_hidden_daemon(socket_path, &config),
        probe_server_readiness,
    )
}

#[cfg(not(any(unix, windows)))]
fn ensure_server_running_with<ConnectFn, LaunchFn>(
    socket_path: &Path,
    timeout: Duration,
    poll_interval: Duration,
    connect: ConnectFn,
    launch: LaunchFn,
) -> Result<Connection, AutoStartError>
where
    ConnectFn: FnMut() -> Result<ConnectResult, ClientError>,
    LaunchFn: FnMut() -> Result<(), AutoStartError>,
{
    ensure_server_running_with_probe(
        socket_path,
        timeout,
        poll_interval,
        connect,
        launch,
        probe_server_readiness,
    )
}

#[cfg(any(all(test, unix), not(any(unix, windows))))]
pub(super) fn ensure_server_running_with_probe<ConnectFn, LaunchFn, ProbeFn>(
    socket_path: &Path,
    timeout: Duration,
    poll_interval: Duration,
    connect: ConnectFn,
    launch: LaunchFn,
    probe: ProbeFn,
) -> Result<Connection, AutoStartError>
where
    ConnectFn: FnMut() -> Result<ConnectResult, ClientError>,
    LaunchFn: FnMut() -> Result<(), AutoStartError>,
    ProbeFn: FnMut(&mut Connection) -> Result<(), ClientError>,
{
    ensure_server_running_with_probe_outcome(
        socket_path,
        timeout,
        poll_interval,
        connect,
        launch,
        probe,
    )
    .map(EnsuredServerConnection::into_connection)
}

#[cfg(any(all(test, unix), not(any(unix, windows))))]
fn ensure_server_running_with_probe_outcome<ConnectFn, LaunchFn, ProbeFn>(
    socket_path: &Path,
    timeout: Duration,
    poll_interval: Duration,
    mut connect: ConnectFn,
    mut launch: LaunchFn,
    mut probe: ProbeFn,
) -> Result<EnsuredServerConnection, AutoStartError>
where
    ConnectFn: FnMut() -> Result<ConnectResult, ClientError>,
    LaunchFn: FnMut() -> Result<(), AutoStartError>,
    ProbeFn: FnMut(&mut Connection) -> Result<(), ClientError>,
{
    match connect().map_err(AutoStartError::Client)? {
        ConnectResult::Connected(mut connection) => {
            probe(&mut connection).map_err(AutoStartError::Client)?;
            return Ok(EnsuredServerConnection::new(
                connection,
                ServerConnectionProvenance::JoinedExisting,
                socket_path.to_path_buf(),
            ));
        }
        ConnectResult::Absent => {}
    }

    launch()?;
    let connection = wait_for_server(
        socket_path,
        timeout,
        poll_interval,
        &mut connect,
        &mut probe,
    )?;
    Ok(EnsuredServerConnection::new(
        connection,
        ServerConnectionProvenance::StartedByCaller,
        socket_path.to_path_buf(),
    ))
}

#[cfg(any(all(test, unix), not(any(unix, windows))))]
fn wait_for_server<ConnectFn, ProbeFn>(
    socket_path: &Path,
    timeout: Duration,
    poll_interval: Duration,
    connect: &mut ConnectFn,
    probe: &mut ProbeFn,
) -> Result<Connection, AutoStartError>
where
    ConnectFn: FnMut() -> Result<ConnectResult, ClientError>,
    ProbeFn: FnMut(&mut Connection) -> Result<(), ClientError>,
{
    let start = Instant::now();
    let deadline = start + timeout;

    loop {
        match connect() {
            Ok(crate::ConnectResult::Connected(mut connection)) => match probe(&mut connection) {
                Ok(()) => return Ok(connection),
                Err(error) if is_transient_connect_error(&error) => {}
                Err(error) => return Err(AutoStartError::Client(error)),
            },
            Ok(crate::ConnectResult::Absent) => {}
            Err(error) if is_transient_connect_error(&error) => {}
            Err(error) => return Err(AutoStartError::Client(error)),
        }

        let now = Instant::now();
        if now >= deadline {
            return Err(AutoStartError::TimedOut {
                socket_path: socket_path.to_path_buf(),
                waited: timeout,
            });
        }

        std::thread::sleep(poll_interval.min(deadline.saturating_duration_since(now)));
    }
}
