use std::io;
use std::os::fd::RawFd;

use super::inherited_fds::{clear_close_on_exec, inherited_fd_scan_limit_with};
use super::{mark_inherited_fds_close_on_exec_fallback_except, StartupReadyNotification};

#[cfg(test)]
mod fd_tests;

impl StartupReadyNotification {
    /// Selects an inherited Linux eventfd without taking ownership of it.
    ///
    /// The caller must keep the descriptor valid until signaling completes.
    #[must_use]
    pub const fn from_inherited_fd(ready_fd: RawFd) -> Self {
        Self {
            ready_fd: Some(ready_fd),
        }
    }

    /// Signals the configured transport, or does nothing when disabled.
    pub fn signal(&self) -> io::Result<()> {
        match self.ready_fd {
            Some(fd) => signal_startup_ready_fd(fd),
            None => Ok(()),
        }
    }
}

/// Writes a single readiness notification to an inherited Linux eventfd.
pub fn signal_startup_ready_fd(ready_fd: RawFd) -> io::Result<()> {
    if ready_fd < 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "startup readiness fd must be non-negative",
        ));
    }

    let bytes = 1_u64.to_ne_bytes();
    // SAFETY: `ready_fd` is supplied by the parent rmux client as an inherited
    // eventfd. Borrowing it does not transfer ownership, and `write` only
    // touches the kernel object referenced by this descriptor.
    let fd = unsafe { std::os::fd::BorrowedFd::borrow_raw(ready_fd) };
    let written = rustix::io::write(fd, &bytes).map_err(io::Error::from)?;
    if written == bytes.len() {
        return Ok(());
    }

    Err(io::Error::new(
        io::ErrorKind::WriteZero,
        "short eventfd readiness write",
    ))
}

/// Linux bounds the fallback scan with `fs.nr_open` and enumerates the
/// descriptors already open through `/proc/self/fd`.
pub(super) fn inherited_fd_scan_limit() -> io::Result<RawFd> {
    inherited_fd_scan_limit_with(kernel_fd_limit()?, "/proc/self/fd")
}

fn kernel_fd_limit() -> io::Result<RawFd> {
    let value = std::fs::read_to_string("/proc/sys/fs/nr_open")?;
    match value.trim().parse::<RawFd>() {
        Ok(limit) if limit >= 3 => Ok(limit),
        _ => Err(io::Error::from_raw_os_error(libc::EINVAL)),
    }
}

pub(super) fn mark_inherited_fds_close_on_exec_except(
    preserved_fds: &[RawFd],
    scan_limit: Result<RawFd, i32>,
) -> io::Result<()> {
    mark_inherited_fds_with(preserved_fds, scan_limit, close_range_cloexec)
}

pub(super) fn mark_inherited_fds_with(
    preserved_fds: &[RawFd],
    scan_limit: Result<RawFd, i32>,
    close_range: fn(u32, u32) -> io::Result<bool>,
) -> io::Result<()> {
    if mark_inherited_fd_ranges_close_on_exec(preserved_fds, close_range)? {
        return Ok(());
    }

    mark_inherited_fds_close_on_exec_fallback_except(preserved_fds, scan_limit)
}

fn mark_inherited_fd_ranges_close_on_exec(
    preserved_fds: &[RawFd],
    close_range: fn(u32, u32) -> io::Result<bool>,
) -> io::Result<bool> {
    let mut start = 3_u32;

    for &fd in preserved_fds {
        if fd < 3 {
            continue;
        }
        let Ok(fd) = u32::try_from(fd) else {
            continue;
        };
        if start < fd && !close_range(start, fd - 1)? {
            return Ok(false);
        }
        clear_close_on_exec(fd as libc::c_int)?;
        start = fd.saturating_add(1);
    }

    if !close_range(start, u32::MAX)? {
        return Ok(false);
    }
    Ok(true)
}

fn close_range_cloexec(first: u32, last: u32) -> io::Result<bool> {
    if first > last {
        return Ok(true);
    }

    // SAFETY: `close_range` with `CLOSE_RANGE_CLOEXEC` does not close file
    // descriptors or dereference Rust memory; it only asks the kernel to mark
    // inherited descriptors in the supplied range close-on-exec in the child
    // process.
    let result = unsafe {
        libc::syscall(
            libc::SYS_close_range,
            first,
            last,
            libc::CLOSE_RANGE_CLOEXEC,
        )
    };
    if result == 0 {
        return Ok(true);
    }

    close_range_error(io::Error::last_os_error())
}

pub(super) fn close_range_error(error: io::Error) -> io::Result<bool> {
    match error.raw_os_error() {
        Some(libc::ENOSYS | libc::EINVAL) => Ok(false),
        _ => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    #[test]
    fn readiness_notification_borrows_the_eventfd_and_clones_without_signaling() {
        // SAFETY: eventfd creates a new descriptor without pointer arguments.
        let raw = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        assert!(raw >= 0);
        // SAFETY: raw is the fresh descriptor returned above, owned only here.
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        let mut bytes = [0_u8; 8];
        {
            let notification = StartupReadyNotification::from_inherited_fd(fd.as_raw_fd());
            let cloned = notification.clone();
            assert_eq!(
                rustix::io::read(&fd, &mut bytes),
                Err(rustix::io::Errno::AGAIN)
            );
            notification.signal().unwrap();
            assert_eq!(rustix::io::read(&fd, &mut bytes).unwrap(), 8);
            assert_eq!(u64::from_ne_bytes(bytes), 1);
            cloned.signal().unwrap();
            assert_eq!(rustix::io::read(&fd, &mut bytes).unwrap(), 8);
            assert_eq!(u64::from_ne_bytes(bytes), 1);
        }
        signal_startup_ready_fd(fd.as_raw_fd()).unwrap();
        assert_eq!(rustix::io::read(&fd, &mut bytes).unwrap(), 8);
        assert_eq!(u64::from_ne_bytes(bytes), 1);
    }

    #[test]
    fn readiness_notification_preserves_transport_errors() {
        assert_eq!(
            StartupReadyNotification::from_inherited_fd(-1)
                .signal()
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        let read_only = std::fs::File::open("/dev/null").unwrap();
        let expected = signal_startup_ready_fd(read_only.as_raw_fd()).unwrap_err();
        let actual = StartupReadyNotification::from_inherited_fd(read_only.as_raw_fd())
            .signal()
            .unwrap_err();
        assert_eq!(actual.raw_os_error(), expected.raw_os_error());
    }
    #[test]
    fn inherited_fd_configuration_preserves_only_the_requested_descriptor() {
        let open_counter = |flags| {
            // SAFETY: eventfd creates a new descriptor without pointer arguments.
            let raw = unsafe { libc::eventfd(0, flags) };
            assert!(raw >= 0);
            // SAFETY: raw is a fresh, uniquely owned descriptor.
            unsafe { OwnedFd::from_raw_fd(raw) }
        };
        let kept = open_counter(libc::EFD_CLOEXEC);
        let discarded = open_counter(0);
        let mut command = std::process::Command::new("/bin/sh");
        command
            .args([
                "-c",
                "test -e /proc/self/fd/$1 && test ! -e /proc/self/fd/$2",
                "rmux-fd-proof",
            ])
            .arg(kept.as_raw_fd().to_string())
            .arg(discarded.as_raw_fd().to_string());
        super::super::configure_hidden_daemon_command_preserving_fds(
            &mut command,
            false,
            &[kept.as_raw_fd()],
        );
        assert!(command.status().unwrap().success());
    }
}
