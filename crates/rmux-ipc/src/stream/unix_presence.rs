//! Tells a busy Unix socket from an abandoned one without queueing on it.
//!
//! A stream `connect` cannot: Darwin refuses it with ECONNREFUSED both when
//! nobody holds the socket any more and when the listener's accept queue is
//! full, and Linux blocks or rejects it with EAGAIN on a full queue. A datagram
//! `connect` never reaches the accept queue. Both kernels resolve the path to
//! its bound socket first and refuse a type mismatch with EPROTOTYPE, so only a
//! socket file that no live socket holds yields ECONNREFUSED.

use std::io;

use rustix::net::{connect, socket_with, AddressFamily, SocketType};

use super::{configure_socket_for_connect, socket_creation_flags};
use crate::LocalEndpoint;

/// What a local Unix endpoint holds, learned without joining its accept queue.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SocketPresence {
    /// A live socket is bound to the endpoint. Its server may still be too
    /// busy to accept, so a refused stream connect does not prove it absent.
    Bound,
    /// The endpoint names a socket file that no live socket holds.
    Unbound,
    /// Nothing exists at the endpoint's path.
    Missing,
}

/// Probes whether a live socket is bound to `endpoint`.
///
/// The probe never blocks and never consumes a slot of the accept queue, so it
/// answers the same for an idle, a saturated and a not-yet-listening server.
///
/// # Errors
///
/// Returns the underlying error when the probe socket cannot be created or the
/// path cannot be resolved, for example `PermissionDenied`.
pub fn probe_socket_presence(endpoint: &LocalEndpoint) -> io::Result<SocketPresence> {
    let address = endpoint.socket_addr_unix()?;
    let probe = socket_with(
        AddressFamily::UNIX,
        SocketType::DGRAM,
        socket_creation_flags(),
        None,
    )?;
    configure_socket_for_connect(&probe)?;

    match connect(&probe, &address) {
        Ok(()) | Err(rustix::io::Errno::PROTOTYPE) => Ok(SocketPresence::Bound),
        Err(rustix::io::Errno::CONNREFUSED) => Ok(SocketPresence::Unbound),
        Err(rustix::io::Errno::NOENT) => Ok(SocketPresence::Missing),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::fd::OwnedFd;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;
    use std::path::{Path, PathBuf};

    use rustix::net::{connect, socket, AddressFamily, SocketAddrUnix, SocketType};

    use super::{probe_socket_presence, SocketPresence};
    use crate::LocalEndpoint;

    #[test]
    fn the_probe_tells_a_saturated_listener_from_an_abandoned_socket() {
        let root = PathBuf::from("/tmp").join(format!("rip-{}", std::process::id()));
        fs::create_dir_all(&root).expect("create test root");
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).expect("secure root");
        let socket_path = root.join("s");
        let endpoint = LocalEndpoint::from_path(socket_path.clone());
        assert_eq!(probe(&endpoint), SocketPresence::Missing);

        let listener = UnixListener::bind(&socket_path).expect("bind listener");
        rustix::net::listen(&listener, 1).expect("shrink backlog");
        let queued = fill_accept_queue(&socket_path);
        assert_eq!(probe(&endpoint), SocketPresence::Bound);
        assert_eq!(
            probe(&endpoint),
            SocketPresence::Bound,
            "the probe must not take a slot of the accept queue"
        );

        drop(listener);
        drop(queued);
        assert!(socket_path.exists(), "closing a listener leaves its file");
        assert_eq!(probe(&endpoint), SocketPresence::Unbound);
        let _ = fs::remove_dir_all(root);
    }

    fn probe(endpoint: &LocalEndpoint) -> SocketPresence {
        probe_socket_presence(endpoint).expect("presence probe")
    }

    /// Fills the queue until Darwin refuses (ECONNREFUSED) or Linux rejects
    /// (EAGAIN) a non-blocking connect.
    fn fill_accept_queue(socket_path: &Path) -> Vec<OwnedFd> {
        let address = SocketAddrUnix::new(socket_path).expect("socket address");
        let mut queued = Vec::new();
        loop {
            let client = socket(AddressFamily::UNIX, SocketType::STREAM, None).expect("socket");
            rustix::io::ioctl_fionbio(&client, true).expect("non-blocking client");
            match connect(&client, &address) {
                Ok(()) => queued.push(client),
                Err(rustix::io::Errno::CONNREFUSED | rustix::io::Errno::AGAIN) => return queued,
                Err(error) => panic!("unexpected connect error: {error}"),
            }
            assert!(queued.len() < 64, "the accept queue never filled");
        }
    }
}
