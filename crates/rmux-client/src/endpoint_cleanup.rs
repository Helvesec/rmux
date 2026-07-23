use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::connection::{connect_or_absent_with_timeout, ConnectResult};
use crate::ClientError;

// Shutdown may spend five seconds bounding a foreground shell, five seconds
// draining accepted lifecycle hooks, and two seconds joining background tasks.
// Leave additional margin for endpoint cleanup and scheduler contention.
const SERVER_ENDPOINT_CLEANUP_TIMEOUT: Duration = Duration::from_secs(15);
const SERVER_ENDPOINT_PROBE_TIMEOUT: Duration = Duration::from_millis(50);
const SERVER_ENDPOINT_CLEANUP_MIN_POLL: Duration = Duration::from_millis(1);
const SERVER_ENDPOINT_CLEANUP_MAX_POLL: Duration = Duration::from_millis(10);

/// Waits until a stopped RMUX server has released its local endpoint.
///
/// Filesystem Unix sockets are considered released only after their pathname
/// is removed. Linux abstract sockets and Windows named pipes are probed with
/// bounded connections. A deadline is reported as an error rather than being
/// mistaken for a successful `kill-server`.
pub fn wait_for_server_endpoint_cleanup(endpoint: &Path) -> Result<(), ClientError> {
    wait_for_server_endpoint_cleanup_with(
        endpoint,
        SERVER_ENDPOINT_CLEANUP_TIMEOUT,
        SERVER_ENDPOINT_PROBE_TIMEOUT,
    )
}

fn wait_for_server_endpoint_cleanup_with(
    endpoint: &Path,
    cleanup_timeout: Duration,
    probe_timeout: Duration,
) -> Result<(), ClientError> {
    #[cfg(unix)]
    if !endpoint.as_os_str().is_empty() {
        return wait_for_filesystem_endpoint_cleanup(endpoint, cleanup_timeout);
    }

    let deadline = Instant::now() + cleanup_timeout;
    let mut next_poll = SERVER_ENDPOINT_CLEANUP_MIN_POLL;
    loop {
        match connect_or_absent_with_timeout(endpoint, probe_timeout) {
            Ok(ConnectResult::Absent) => return Ok(()),
            Ok(ConnectResult::Connected(connection)) => drop(connection),
            Err(error) if shutdown_probe_error_is_transient(&error) => {}
            Err(error) => return Err(error),
        }

        let now = Instant::now();
        if now >= deadline {
            return Err(cleanup_timeout_error(endpoint));
        }
        std::thread::sleep(next_poll.min(deadline.saturating_duration_since(now)));
        next_poll = (next_poll + next_poll).min(SERVER_ENDPOINT_CLEANUP_MAX_POLL);
    }
}

#[cfg(unix)]
fn wait_for_filesystem_endpoint_cleanup(
    endpoint: &Path,
    cleanup_timeout: Duration,
) -> Result<(), ClientError> {
    let deadline = Instant::now() + cleanup_timeout;
    let mut next_poll = SERVER_ENDPOINT_CLEANUP_MIN_POLL;
    while endpoint.try_exists().map_err(ClientError::Io)? {
        let now = Instant::now();
        if now >= deadline {
            return Err(cleanup_timeout_error(endpoint));
        }
        std::thread::sleep(next_poll.min(deadline.saturating_duration_since(now)));
        next_poll = (next_poll + next_poll).min(SERVER_ENDPOINT_CLEANUP_MAX_POLL);
    }
    Ok(())
}

fn cleanup_timeout_error(endpoint: &Path) -> ClientError {
    ClientError::Io(io::Error::new(
        io::ErrorKind::TimedOut,
        format!(
            "kill-server timed out waiting for the daemon endpoint '{}' to close",
            endpoint.display()
        ),
    ))
}

fn shutdown_probe_error_is_transient(error: &ClientError) -> bool {
    matches!(
        error,
        ClientError::Io(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::TimedOut
                    | std::io::ErrorKind::WouldBlock
                    | std::io::ErrorKind::Interrupted
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::ConnectionReset
            )
    )
}

#[cfg(all(test, unix))]
mod tests {
    use super::{wait_for_server_endpoint_cleanup_with, SERVER_ENDPOINT_PROBE_TIMEOUT};
    use crate::ClientError;
    use std::path::PathBuf;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    #[test]
    fn filesystem_endpoint_cleanup_waits_for_path_removal() {
        let endpoint = unique_endpoint("wait");
        std::fs::write(&endpoint, b"occupied").expect("create endpoint marker");
        let removed_endpoint = endpoint.clone();
        let remover = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(40));
            std::fs::remove_file(removed_endpoint).expect("remove endpoint marker");
        });

        let started = Instant::now();
        wait_for_server_endpoint_cleanup_with(
            &endpoint,
            Duration::from_secs(1),
            SERVER_ENDPOINT_PROBE_TIMEOUT,
        )
        .expect("endpoint cleanup");
        remover.join().expect("endpoint remover");

        assert!(
            started.elapsed() >= Duration::from_millis(30),
            "cleanup returned before the endpoint path was removed"
        );
    }

    #[test]
    fn filesystem_endpoint_cleanup_timeout_is_an_error() {
        let endpoint = unique_endpoint("timeout");
        std::fs::write(&endpoint, b"occupied").expect("create endpoint marker");

        let error = wait_for_server_endpoint_cleanup_with(
            &endpoint,
            Duration::from_millis(20),
            SERVER_ENDPOINT_PROBE_TIMEOUT,
        )
        .expect_err("present endpoint must time out");
        let _ = std::fs::remove_file(&endpoint);

        assert!(
            matches!(
                error,
                ClientError::Io(ref io_error)
                    if io_error.kind() == std::io::ErrorKind::TimedOut
                        && io_error.to_string().contains(&endpoint.display().to_string())
            ),
            "unexpected cleanup error: {error}"
        );
    }

    fn unique_endpoint(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "rmux-endpoint-cleanup-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time after epoch")
                .as_nanos()
        ))
    }
}
