//! A daemon that still holds its socket is alive, even when it cannot accept.
//!
//! A refused or timed-out connect proves nothing on its own: Darwin refuses a
//! connect to a full accept queue exactly as to an abandoned socket file. The
//! datagram probe of `rmux_ipc::probe_socket_presence` asks the kernel instead
//! whether a live socket is bound to the path.

use std::path::Path;
use std::time::Duration;

use rmux_ipc::SocketPresence;
use tokio::net::UnixStream;
use tokio::time::sleep;

use super::{try_connect_validated, StartupError};
use crate::deadline::StartupDeadline;

/// Keeps a zero poll interval from spinning on the probes.
const MIN_POLL_INTERVAL: Duration = Duration::from_millis(1);

/// Returns whether a live socket is bound to `socket_path`.
pub(super) fn socket_held_by_live_daemon(socket_path: &Path) -> Result<bool, StartupError> {
    let probe_error = |source| StartupError::Filesystem {
        operation: "probe potentially stale socket",
        path: socket_path.to_path_buf(),
        source,
    };
    let endpoint = rmux_ipc::resolve_endpoint(None, Some(socket_path)).map_err(probe_error)?;
    match rmux_ipc::probe_socket_presence(&endpoint).map_err(probe_error)? {
        SocketPresence::Bound => Ok(true),
        SocketPresence::Unbound | SocketPresence::Missing => Ok(false),
    }
}

/// Waits for a live but busy daemon to accept a connection.
///
/// The caller must not hold the startup lock: an idle daemon takes it before
/// it unlinks its socket. `Ok(None)` reports that the daemon released its
/// socket, so the caller may start the next one.
pub(super) async fn wait_for_busy_daemon(
    socket_path: &Path,
    owner_uid: u32,
    deadline: StartupDeadline,
    poll_interval: Duration,
) -> Result<Option<UnixStream>, StartupError> {
    loop {
        if let Some(stream) = try_connect_validated(socket_path, owner_uid, deadline).await? {
            return Ok(Some(stream));
        }
        if !socket_held_by_live_daemon(socket_path)? {
            return Ok(None);
        }
        if deadline.is_elapsed() {
            return Err(StartupError::StartupTimeout {
                socket_path: socket_path.to_path_buf(),
                waited: deadline.elapsed(),
            });
        }
        sleep(deadline.sleep_for(poll_interval.max(MIN_POLL_INTERVAL))).await;
    }
}
