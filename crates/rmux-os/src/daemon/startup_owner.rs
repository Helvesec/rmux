use std::io;
use std::os::fd::{FromRawFd, RawFd};
use std::os::unix::net::UnixStream;

/// Takes the private bootstrap socket inherited by a hidden daemon.
///
/// # Safety
/// `fd` must be exclusively owned by this entrypoint, with no other Rust owner.
/// Call before starting threads that could close or reuse inherited descriptors.
pub unsafe fn take_startup_owner(fd: RawFd) -> io::Result<UnixStream> {
    if fd < 3 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "startup owner overlaps stdio",
        ));
    }
    // SAFETY: the caller owns the inherited descriptor. Reject a closed fd
    // before taking ownership, and prevent further inheritance by pane shells.
    if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the validated descriptor is transferred exactly once.
    let stream = unsafe { UnixStream::from_raw_fd(fd) };
    stream.peer_addr()?;
    Ok(stream)
}
