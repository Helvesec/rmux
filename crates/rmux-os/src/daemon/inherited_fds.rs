use std::io;
use std::os::fd::RawFd;

/// Prepare the complete fallback range in the parent, never after fork.
/// The kernel's descriptor ceiling covers descriptors opened between configure
/// and spawn. Enumerating existing descriptors also covers descriptors retained
/// above a subsequently lowered resource or kernel limit. Neither limit is
/// truncated to a fixed performance cap or to RLIMIT_NOFILE's soft limit.
///
/// The platform supplies its kernel ceiling and descriptor directory; Linux
/// does so from `daemon/linux.rs`.
pub(super) fn inherited_fd_scan_limit_with(
    kernel_fd_limit: RawFd,
    fd_directory: &str,
) -> io::Result<RawFd> {
    let mut limit = kernel_fd_limit;
    for entry in std::fs::read_dir(fd_directory)? {
        let name = entry?.file_name();
        if let Some(fd) = name.to_str().and_then(|name| name.parse::<RawFd>().ok()) {
            let end = fd
                .checked_add(1)
                .ok_or_else(|| io::Error::from_raw_os_error(libc::EOVERFLOW))?;
            limit = limit.max(end);
        }
    }
    Ok(limit)
}

#[cfg(not(target_os = "linux"))]
pub(super) fn inherited_fd_scan_limit() -> io::Result<RawFd> {
    inherited_fd_scan_limit_with(kernel_fd_limit()?, "/dev/fd")
}

#[cfg(target_os = "macos")]
fn kernel_fd_limit() -> io::Result<RawFd> {
    let mut mib = [libc::CTL_KERN, libc::KERN_MAXFILESPERPROC];
    let mut limit: libc::c_int = 0;
    let mut size = std::mem::size_of_val(&limit);
    // SAFETY: both buffers have the sizes passed to sysctl; no value is written.
    let result = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            2,
            (&mut limit as *mut libc::c_int).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    if size != std::mem::size_of_val(&limit) || limit < 3 {
        return Err(io::Error::from_raw_os_error(libc::EINVAL));
    }
    Ok(limit)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn kernel_fd_limit() -> io::Result<RawFd> {
    // Fail closed when no finite kernel bound can be established on this Unix.
    let mut limit = std::mem::MaybeUninit::<libc::rlimit>::uninit();
    // SAFETY: getrlimit writes one rlimit through a valid, writable pointer;
    // the value is read only after the call reports success.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, limit.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let limit = unsafe { limit.assume_init() }.rlim_max;
    RawFd::try_from(limit)
        .ok()
        .filter(|limit| *limit >= 3)
        .ok_or_else(|| io::Error::from_raw_os_error(libc::EOVERFLOW))
}

pub(super) fn mark_inherited_fds_close_on_exec_fallback_except(
    preserved_fds: &[RawFd],
    scan_limit: Result<RawFd, i32>,
) -> io::Result<()> {
    let max_fd = scan_limit.map_err(io::Error::from_raw_os_error)?;
    for &fd in preserved_fds {
        if fd >= 3 {
            clear_close_on_exec(fd)?;
        }
    }

    for fd in 3..max_fd {
        if preserved_fds.contains(&fd) {
            continue;
        }

        // SAFETY: `fcntl(F_GETFD)` observes descriptor flags for an integer fd.
        // Invalid descriptors are reported as EBADF and handled below.
        let flags = fcntl_getfd_retry(fd)?;
        if flags == -1 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EBADF) {
                continue;
            }
            return Err(error);
        }

        // SAFETY: `fd` was accepted by `F_GETFD`; `F_SETFD` only updates that
        // descriptor's close-on-exec flag and reports races via EBADF.
        let result = fcntl_setfd_retry(fd, flags | libc::FD_CLOEXEC)?;
        if result == -1 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EBADF) {
                continue;
            }
            return Err(error);
        }
    }

    Ok(())
}

pub(super) fn clear_close_on_exec(fd: libc::c_int) -> io::Result<()> {
    let flags = fcntl_getfd_retry(fd)?;
    if flags == -1 {
        return Err(io::Error::last_os_error());
    }
    let result = fcntl_setfd_retry(fd, flags & !libc::FD_CLOEXEC)?;
    if result == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn fcntl_getfd_retry(fd: libc::c_int) -> io::Result<libc::c_int> {
    loop {
        // SAFETY: `fcntl(F_GETFD)` observes descriptor flags for an integer fd.
        let result = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        if result != -1 || io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
            return Ok(result);
        }
    }
}

fn fcntl_setfd_retry(fd: libc::c_int, flags: libc::c_int) -> io::Result<libc::c_int> {
    loop {
        // SAFETY: `fcntl(F_SETFD)` mutates only fd flags for the supplied fd.
        let result = unsafe { libc::fcntl(fd, libc::F_SETFD, flags) };
        if result != -1 || io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
            return Ok(result);
        }
    }
}
