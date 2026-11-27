//! Descriptor isolation after fork, before the PTY child's exec.

use std::io;

/// Only stdin/stdout/stderr survive exec. Mark rather than close: Rust's
/// private exec-error pipe must remain writable if the later exec fails.
#[cfg(target_os = "linux")]
pub(crate) fn mark_child_fds_close_on_exec() -> io::Result<()> {
    // SAFETY: this syscall only updates flags in the forked child's fd table.
    let result = unsafe {
        libc::syscall(
            libc::SYS_close_range,
            3_u32,
            u32::MAX,
            libc::CLOSE_RANGE_CLOEXEC,
        )
    };
    if result == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::ENOSYS | libc::EINVAL) => mark_open_fds_close_on_exec(),
        _ => Err(error),
    }
}

#[cfg(target_os = "linux")]
fn mark_open_fds_close_on_exec() -> io::Result<()> {
    use rustix::fs::{open, Mode, OFlags, RawDir};
    use std::mem::MaybeUninit;

    // RawDir uses getdents into caller-owned storage, without allocation or
    // libc directory locks after fork. Enumerating open descriptors also
    // handles descriptors above a subsequently lowered RLIMIT_NOFILE.
    let directory = open(
        c"/proc/self/fd",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    let mut buffer = [MaybeUninit::uninit(); 4096];
    let mut entries = RawDir::new(directory, &mut buffer);
    while let Some(entry) = entries.next() {
        let entry = match entry {
            Err(rustix::io::Errno::INTR) => continue,
            other => other?,
        };
        let fd = entry
            .file_name()
            .to_bytes()
            .iter()
            .try_fold(0_i32, |fd, byte| {
                if byte.is_ascii_digit() {
                    fd.checked_mul(10)?.checked_add(i32::from(byte - b'0'))
                } else {
                    None
                }
            });
        if let Some(fd) = fd.filter(|fd| *fd >= 3) {
            mark_close_on_exec(fd)?;
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
pub(crate) fn mark_child_fds_close_on_exec() -> io::Result<()> {
    // Darwin's null-buffer PROC_PIDLISTFDS query returns storage for the
    // allocated descriptor table (fd_nfiles plus slack), not just its occupied
    // slots. It therefore bounds the highest fd even after lowering rlimits.
    // Query in the child, where no other thread can grow the table.
    // SAFETY: this libproc wrapper is a direct syscall with no allocation;
    // null buffer and zero length request only the required storage size.
    let bytes = loop {
        let bytes = unsafe {
            libc::proc_pidinfo(
                libc::getpid(),
                libc::PROC_PIDLISTFDS,
                0,
                std::ptr::null_mut(),
                0,
            )
        };
        if bytes > 0 {
            break bytes;
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    };
    for fd in 3..bytes / libc::PROC_PIDLISTFD_SIZE {
        mark_close_on_exec(fd)?;
    }
    Ok(())
}

fn mark_close_on_exec(fd: i32) -> io::Result<()> {
    let flags = loop {
        // SAFETY: F_GETFD only queries this descriptor in the child.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        if flags >= 0 {
            break flags;
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::EBADF) => return Ok(()),
            Some(libc::EINTR) => continue,
            _ => return Err(error),
        }
    };
    if flags & libc::FD_CLOEXEC != 0 {
        return Ok(());
    }
    loop {
        // SAFETY: the fd is open; no other thread runs in this forked child.
        if unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}
