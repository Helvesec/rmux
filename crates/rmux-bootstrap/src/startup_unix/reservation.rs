//! Endpoint reservation held by an exiting daemon after it releases its socket.

use std::path::Path;
use std::time::Duration;

use rmux_os::identity::real_user_id;

use super::lock::StartupLock;
use super::{prepare_parent_for_filesystem_socket, StartupError};
use crate::deadline::StartupDeadline;

const RESERVATION_POLL_INTERVAL: Duration = Duration::from_millis(1);

/// Holds the startup lock of a filesystem endpoint, so that no caller of
/// [`super::connect_or_start`] launches a daemon on it until this value drops.
///
/// An exiting daemon takes it before it unlinks its socket: a caller then meets
/// an absent endpoint, waits for the lock, and starts the next daemon only
/// once the exiting one has finished its shutdown.
#[derive(Debug)]
pub struct EndpointReservation {
    _lock: StartupLock,
}

impl EndpointReservation {
    /// Takes the startup lock that [`super::connect_or_start`] uses for
    /// `socket_path`, waiting at most `wait` for a current holder.
    ///
    /// # Errors
    ///
    /// Returns the [`StartupError`] that `connect_or_start` would meet on the
    /// same path, including [`StartupError::Lock`] once `wait` elapses.
    pub async fn acquire(socket_path: &Path, wait: Duration) -> Result<Self, StartupError> {
        if socket_path.as_os_str().is_empty() {
            return Err(StartupError::InvalidPath {
                reason: "an unnamed endpoint has no startup lock".to_owned(),
                path: socket_path.to_path_buf(),
            });
        }
        let owner_uid = real_user_id();
        let prepared = prepare_parent_for_filesystem_socket(socket_path, owner_uid)?;
        let lock = StartupLock::acquire(
            &prepared.lock_path,
            owner_uid,
            StartupDeadline::from_timeout(Some(wait)),
            RESERVATION_POLL_INTERVAL,
        )
        .await?;
        Ok(Self { _lock: lock })
    }
}
