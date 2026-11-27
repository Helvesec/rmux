//! An idle daemon releases its endpoint as soon as it stops accepting.
//!
//! A bound socket that no longer accepts lets the kernel queue connections
//! that the end of the drain then resets (Linux) or closes (macOS) unanswered.
//! The exiting daemon instead takes the endpoint's startup lock, unlinks its
//! socket and closes its listener: a client meets an absent endpoint, waits
//! for the lock, and starts the next daemon only once this one has drained its
//! hooks and reaped its panes, which still cannot reach a newer generation.

use std::path::Path;
use std::time::Duration;

use rmux_bootstrap::startup_unix::EndpointReservation;
use tracing::{debug, warn};

use crate::daemon::ShutdownHandle;
use crate::handler::RequestHandler;
use crate::socket_cleanup::SocketCleanup;

/// A starting client holds the lock only between its connect attempts.
const RESERVATION_WAIT: Duration = Duration::from_millis(250);

/// Reserves and unlinks the endpoint of an idle shutdown. `None` keeps the
/// listener bound until the end of the drain, as for every other shutdown.
pub(super) async fn release_idle_endpoint(
    shutdown_handle: &ShutdownHandle,
    handler: &RequestHandler,
    socket_path: &Path,
    cleanup: &SocketCleanup,
) -> Option<EndpointReservation> {
    if !shutdown_handle.releases_endpoint_early() || cleanup.socket_identity().is_none() {
        return None;
    }
    let reservation = match EndpointReservation::acquire(socket_path, RESERVATION_WAIT).await {
        Ok(reservation) => reservation,
        Err(error) => {
            debug!(
                path = %socket_path.display(),
                "keeping the endpoint bound through shutdown: {error}"
            );
            return None;
        }
    };
    // Restore the owner-only modes while the socket file still names them.
    if let Err(error) = handler.restore_owner_only_unix_transport().await {
        warn!(
            path = %socket_path.display(),
            "failed to restore owner-only Unix socket permissions during shutdown: {error}"
        );
        return None;
    }
    match cleanup.unlink_socket_now() {
        Ok(true) => Some(reservation),
        Ok(false) => None,
        Err(error) => {
            warn!(
                path = %socket_path.display(),
                "keeping the endpoint bound through shutdown: {error}"
            );
            None
        }
    }
}
