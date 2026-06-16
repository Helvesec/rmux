//! Local stream handles.

use std::io;
#[cfg(all(unix, not(target_os = "linux")))]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::path::Path;
#[cfg(unix)]
use std::time::Duration;
#[cfg(unix)]
use std::time::Instant;

#[cfg(unix)]
use crate::LocalEndpoint;
use rmux_os::identity::UserIdentity;

#[cfg(unix)]
use rustix::event::{poll, PollFd, PollFlags, Timespec};
#[cfg(unix)]
use rustix::net::RecvFlags;
#[cfg(windows)]
#[path = "stream_windows.rs"]
mod windows;

#[cfg(windows)]
pub use windows::{connect_blocking, BlockingLocalStream, LocalStream};

/// Identity of a connected local peer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerIdentity {
    /// Peer process id.
    pub pid: u32,
    /// Peer Unix user id.
    pub uid: u32,
    /// Platform user identity for the peer.
    pub user: UserIdentity,
}

/// Async local byte stream used by the server runtime.
#[cfg(unix)]
pub type LocalStream = tokio::net::UnixStream;

/// Blocking local byte stream used by the CLI.
#[cfg(unix)]
pub type BlockingLocalStream = std::os::unix::net::UnixStream;

/// Waits for the connected local peer to disappear without consuming protocol bytes.
pub async fn wait_for_peer_close(stream: &LocalStream) -> io::Result<()> {
    wait_for_peer_close_impl(stream).await
}

/// Returns whether an I/O error represents a normal local peer disconnect.
#[must_use]
pub fn is_peer_disconnect(error: &io::Error) -> bool {
    is_peer_disconnect_impl(error)
}

#[cfg(unix)]
async fn wait_for_peer_close_impl(stream: &LocalStream) -> io::Result<()> {
    loop {
        if let Err(error) = stream.readable().await {
            if is_peer_disconnect_impl(&error) {
                return Ok(());
            }
            return Err(error);
        }
        let mut probe = [0_u8; 1];

        match rustix::net::recv(stream, &mut probe, RecvFlags::PEEK) {
            Ok((_initialized, 0)) => return Ok(()),
            Ok((_initialized, _available)) => {
                if peer_close_is_pollable(stream)? {
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            Err(rustix::io::Errno::INTR | rustix::io::Errno::AGAIN) => continue,
            Err(rustix::io::Errno::PIPE | rustix::io::Errno::CONNRESET) => return Ok(()),
            Err(error) => return Err(io::Error::from(error)),
        }
    }
}

#[cfg(unix)]
fn peer_close_is_pollable(stream: &LocalStream) -> io::Result<bool> {
    let timeout = Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    let mut fds = [PollFd::new(stream, peer_close_interest_flags())];
    match poll(&mut fds, Some(&timeout)) {
        Ok(0) => Ok(false),
        Ok(_) => Ok(fds[0].revents().intersects(peer_close_ready_flags())),
        Err(rustix::io::Errno::INTR) => Ok(false),
        Err(error) => Err(error.into()),
    }
}

#[cfg(unix)]
fn peer_close_interest_flags() -> PollFlags {
    PollFlags::IN | peer_close_ready_flags()
}

#[cfg(unix)]
fn peer_close_ready_flags() -> PollFlags {
    let flags = PollFlags::ERR | PollFlags::HUP;
    #[cfg(any(
        target_os = "freebsd",
        target_os = "illumos",
        all(
            target_os = "linux",
            not(any(target_arch = "sparc", target_arch = "sparc64"))
        )
    ))]
    {
        flags | PollFlags::RDHUP
    }
    #[cfg(not(any(
        target_os = "freebsd",
        target_os = "illumos",
        all(
            target_os = "linux",
            not(any(target_arch = "sparc", target_arch = "sparc64"))
        )
    )))]
    {
        flags
    }
}

#[cfg(windows)]
async fn wait_for_peer_close_impl(stream: &LocalStream) -> io::Result<()> {
    windows::wait_for_peer_close_impl(stream).await
}

#[cfg(unix)]
fn is_peer_disconnect_impl(error: &io::Error) -> bool {
    if matches!(
        error.kind(),
        io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset
    ) {
        return true;
    }
    false
}

#[cfg(windows)]
fn is_peer_disconnect_impl(error: &io::Error) -> bool {
    windows::is_peer_disconnect(error)
}

#[cfg(unix)]
impl PeerIdentity {
    pub(crate) fn from_unix_stream(stream: &LocalStream) -> io::Result<Self> {
        let credentials = stream.peer_cred()?;
        let pid = credentials
            .pid()
            .ok_or_else(|| io::Error::other("unix peer credentials did not include a pid"))?;
        let uid = credentials.uid();
        let pid = u32::try_from(pid)
            .map_err(|_| io::Error::other(format!("invalid unix peer pid {pid}")))?;
        Ok(Self {
            pid,
            uid,
            user: UserIdentity::Uid(uid),
        })
    }
}

#[cfg(unix)]
/// Connects a blocking client stream to a local endpoint.
pub fn connect_blocking(
    endpoint: &LocalEndpoint,
    timeout: Duration,
) -> io::Result<BlockingLocalStream> {
    use rustix::net::sockopt::socket_error;
    use rustix::net::{connect as socket_connect, socket_with, AddressFamily, SocketType};

    let socket_path = endpoint.as_path();
    let address = endpoint.socket_addr_unix()?;
    let socket = socket_with(
        AddressFamily::UNIX,
        SocketType::STREAM,
        socket_creation_flags(),
        None,
    )?;
    configure_socket_for_connect(&socket)?;

    match socket_connect(&socket, &address) {
        Ok(()) => {}
        Err(rustix::io::Errno::INPROGRESS | rustix::io::Errno::WOULDBLOCK) => {
            wait_for_connect_completion(socket_path, timeout, |remaining| {
                let poll_timeout = Timespec {
                    tv_sec: remaining.as_secs() as i64,
                    tv_nsec: remaining.subsec_nanos().into(),
                };
                let mut fds = [PollFd::new(
                    &socket,
                    PollFlags::OUT | PollFlags::ERR | PollFlags::HUP,
                )];

                match poll(&mut fds, Some(&poll_timeout)) {
                    Ok(0) => Ok(ConnectProgress::Pending),
                    Ok(_) => Ok(ConnectProgress::Ready),
                    Err(rustix::io::Errno::INTR) => Ok(ConnectProgress::Pending),
                    Err(error) => Err(error.into()),
                }
            })?;
        }
        Err(error) => return Err(error.into()),
    }

    match socket_error(&socket)? {
        Ok(()) => {}
        Err(error) => return Err(error.into()),
    }

    let stream = BlockingLocalStream::from(socket);
    stream.set_nonblocking(false)?;
    Ok(stream)
}

#[cfg(target_os = "linux")]
fn socket_creation_flags() -> rustix::net::SocketFlags {
    rustix::net::SocketFlags::CLOEXEC | rustix::net::SocketFlags::NONBLOCK
}

#[cfg(all(unix, not(target_os = "linux")))]
fn socket_creation_flags() -> rustix::net::SocketFlags {
    rustix::net::SocketFlags::empty()
}

#[cfg(target_os = "linux")]
fn configure_socket_for_connect<Fd>(_socket: &Fd) -> io::Result<()>
where
    Fd: std::os::fd::AsFd,
{
    Ok(())
}

#[cfg(all(unix, not(target_os = "linux")))]
fn configure_socket_for_connect<Fd>(socket: &Fd) -> io::Result<()>
where
    Fd: std::os::fd::AsFd,
{
    let raw_fd = socket.as_fd().as_raw_fd();
    set_fd_flag(raw_fd, libc::FD_CLOEXEC)?;
    set_status_flag(raw_fd, libc::O_NONBLOCK)
}

#[cfg(all(unix, not(target_os = "linux")))]
fn set_fd_flag(raw_fd: libc::c_int, flag: libc::c_int) -> io::Result<()> {
    let flags = unsafe {
        // SAFETY: `fcntl` reads descriptor flags from a valid socket fd.
        libc::fcntl(raw_fd, libc::F_GETFD)
    };
    if flags == -1 {
        return Err(io::Error::last_os_error());
    }

    let result = unsafe {
        // SAFETY: `fcntl` updates only descriptor flags for the same valid fd.
        libc::fcntl(raw_fd, libc::F_SETFD, flags | flag)
    };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }

    Ok(())
}

#[cfg(all(unix, not(target_os = "linux")))]
fn set_status_flag(raw_fd: libc::c_int, flag: libc::c_int) -> io::Result<()> {
    let flags = unsafe {
        // SAFETY: `fcntl` reads file status flags from a valid socket fd.
        libc::fcntl(raw_fd, libc::F_GETFL)
    };
    if flags == -1 {
        return Err(io::Error::last_os_error());
    }

    let result = unsafe {
        // SAFETY: `fcntl` updates only file status flags for the same valid fd.
        libc::fcntl(raw_fd, libc::F_SETFL, flags | flag)
    };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }

    Ok(())
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ConnectProgress {
    Pending,
    Ready,
}

#[cfg(unix)]
fn wait_for_connect_completion<P>(
    socket_path: &Path,
    timeout: Duration,
    mut wait_for_ready: P,
) -> io::Result<()>
where
    P: FnMut(Duration) -> io::Result<ConnectProgress>,
{
    let deadline = Instant::now() + timeout;

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "timed out after {}s connecting to '{}'",
                    timeout.as_secs_f32(),
                    socket_path.display()
                ),
            ));
        }

        if wait_for_ready(remaining)? == ConnectProgress::Ready {
            return Ok(());
        }
    }
}
