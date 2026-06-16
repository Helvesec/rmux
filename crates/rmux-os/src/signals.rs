//! Signal disposition helpers.

use std::io;
#[cfg(unix)]
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
#[cfg(unix)]
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};

#[cfg(unix)]
const NO_TERMINATION_SIGNAL: usize = 0;
#[cfg(unix)]
const HANDLED_SIGNALS: [i32; 7] = [
    libc::SIGINT,
    libc::SIGTERM,
    libc::SIGCHLD,
    libc::SIGUSR1,
    libc::SIGHUP,
    libc::SIGQUIT,
    libc::SIGUSR2,
];

#[cfg(unix)]
static TERMINATION_SIGNAL: AtomicUsize = AtomicUsize::new(NO_TERMINATION_SIGNAL);
#[cfg(unix)]
static CHILD_CHANGED: AtomicBool = AtomicBool::new(false);
#[cfg(unix)]
static RECREATE_SOCKET: AtomicBool = AtomicBool::new(false);
#[cfg(unix)]
static IGNORED_SIGNAL: AtomicBool = AtomicBool::new(false);
#[cfg(unix)]
static SERVER_SIGNALS_INSTALLED: AtomicBool = AtomicBool::new(false);
#[cfg(unix)]
static SIGNAL_WAKE_READ_FD: AtomicI32 = AtomicI32::new(-1);
#[cfg(unix)]
static SIGNAL_WAKE_WRITE_FD: AtomicI32 = AtomicI32::new(-1);

/// Read side of the daemon signal wake pipe.
#[cfg(unix)]
#[derive(Debug)]
pub struct ServerSignalWake {
    read_fd: SignalWakeReadFd,
}

/// Process-global read side of the signal wake pipe.
///
/// The matching write fd is stored as a raw descriptor for use from signal
/// handlers, so this wrapper intentionally does not close the descriptor on
/// drop. Both sides live until daemon process exit.
#[cfg(unix)]
#[derive(Debug)]
pub struct SignalWakeReadFd {
    fd: RawFd,
}

#[cfg(unix)]
impl AsRawFd for SignalWakeReadFd {
    fn as_raw_fd(&self) -> RawFd {
        self.fd
    }
}

#[cfg(unix)]
impl AsFd for SignalWakeReadFd {
    fn as_fd(&self) -> BorrowedFd<'_> {
        unsafe {
            // SAFETY: `SignalWakeReadFd` wraps the process-global read side of
            // the daemon signal wake pipe. The descriptor is intentionally kept
            // open until process exit.
            BorrowedFd::borrow_raw(self.fd)
        }
    }
}

#[cfg(unix)]
impl ServerSignalWake {
    /// Returns the read descriptor used to integrate signal wakeups into an
    /// async event loop.
    #[must_use]
    pub fn into_read_fd(self) -> SignalWakeReadFd {
        self.read_fd
    }
}

/// One server signal observed by the process-global RMUX signal handlers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolledServerSignal {
    /// SIGINT was received.
    Interrupt,
    /// SIGTERM was received.
    Terminate,
    /// SIGCHLD was received.
    ChildChanged,
    /// SIGUSR1 was received.
    RecreateSocket,
    /// A non-actionable daemon signal was received.
    Ignored,
}

/// Installs RMUX daemon signal handlers and returns a wake descriptor.
///
/// The handlers set process-global atomics and write one byte to a nonblocking
/// wake pipe. Callers should wait for the returned descriptor and then drain
/// pending signals with [`poll_server_signal_flags`].
#[cfg(unix)]
pub fn install_server_signal_flags() -> io::Result<ServerSignalWake> {
    install_server_signal_flags_impl()
}

/// Installs RMUX daemon signal handlers.
#[cfg(not(unix))]
pub fn install_server_signal_flags() -> io::Result<()> {
    install_server_signal_flags_impl()
}

/// Polls and clears one pending RMUX daemon signal.
#[must_use]
pub fn poll_server_signal_flags() -> Option<PolledServerSignal> {
    poll_server_signal_flags_impl()
}

#[cfg(unix)]
fn install_server_signal_flags_impl() -> io::Result<ServerSignalWake> {
    if SERVER_SIGNALS_INSTALLED
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        if signal_wake_pipe_installed() {
            return duplicate_signal_wake_reader();
        }
        SERVER_SIGNALS_INSTALLED.store(false, Ordering::SeqCst);
        return install_server_signal_flags_impl();
    }

    let (read_fd, write_fd) = create_signal_wake_pipe()?;
    SIGNAL_WAKE_READ_FD.store(read_fd.as_raw_fd(), Ordering::SeqCst);
    SIGNAL_WAKE_WRITE_FD.store(write_fd.as_raw_fd(), Ordering::SeqCst);

    for signal in HANDLED_SIGNALS {
        if let Err(error) = install_signal_handler(signal) {
            SERVER_SIGNALS_INSTALLED.store(false, Ordering::SeqCst);
            SIGNAL_WAKE_READ_FD.store(-1, Ordering::SeqCst);
            SIGNAL_WAKE_WRITE_FD.store(-1, Ordering::SeqCst);
            return Err(error);
        }
    }
    let watcher = Ok(ServerSignalWake {
        read_fd: SignalWakeReadFd {
            fd: read_fd.as_raw_fd(),
        },
    });
    std::mem::forget(read_fd);
    std::mem::forget(write_fd);
    watcher
}

#[cfg(unix)]
fn signal_wake_pipe_installed() -> bool {
    SIGNAL_WAKE_READ_FD.load(Ordering::SeqCst) >= 0
        && SIGNAL_WAKE_WRITE_FD.load(Ordering::SeqCst) >= 0
}

#[cfg(unix)]
fn poll_server_signal_flags_impl() -> Option<PolledServerSignal> {
    match TERMINATION_SIGNAL.swap(NO_TERMINATION_SIGNAL, Ordering::SeqCst) as i32 {
        libc::SIGINT => return Some(PolledServerSignal::Interrupt),
        libc::SIGTERM => return Some(PolledServerSignal::Terminate),
        _ => {}
    }
    if CHILD_CHANGED.swap(false, Ordering::SeqCst) {
        return Some(PolledServerSignal::ChildChanged);
    }
    if RECREATE_SOCKET.swap(false, Ordering::SeqCst) {
        return Some(PolledServerSignal::RecreateSocket);
    }
    if IGNORED_SIGNAL.swap(false, Ordering::SeqCst) {
        return Some(PolledServerSignal::Ignored);
    }
    None
}

#[cfg(not(unix))]
fn install_server_signal_flags_impl() -> io::Result<()> {
    Ok(())
}

#[cfg(not(unix))]
fn poll_server_signal_flags_impl() -> Option<PolledServerSignal> {
    None
}

#[cfg(unix)]
fn install_signal_handler(signal: i32) -> io::Result<()> {
    let mut action = unsafe {
        // SAFETY: `sigaction` is a plain C struct. Zero initialization covers
        // platform-specific fields such as Linux `sa_restorer` before we fill
        // the portable fields below.
        std::mem::zeroed::<libc::sigaction>()
    };
    action.sa_sigaction = handle_signal as *const () as usize;
    action.sa_flags = signal_action_flags(signal);
    let empty_mask = unsafe {
        // SAFETY: `action.sa_mask` points to initialized writable storage.
        libc::sigemptyset(&mut action.sa_mask)
    };
    if empty_mask != 0 {
        return Err(io::Error::last_os_error());
    }
    let result = unsafe {
        // SAFETY: `signal` comes from libc signal constants and `action`
        // points to a fully initialized sigaction structure for this call.
        libc::sigaction(signal, &action, std::ptr::null_mut())
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(unix)]
fn signal_action_flags(_signal: i32) -> i32 {
    // SIGCHLD must fire for stopped children too. The daemon uses that event
    // to resume foreground pane jobs that accidentally stop themselves.
    libc::SA_RESTART
}

#[cfg(unix)]
extern "C" fn handle_signal(signal: i32) {
    match signal {
        libc::SIGINT | libc::SIGTERM => {
            TERMINATION_SIGNAL.store(signal as usize, Ordering::SeqCst);
        }
        libc::SIGCHLD => {
            CHILD_CHANGED.store(true, Ordering::SeqCst);
        }
        libc::SIGUSR1 => {
            RECREATE_SOCKET.store(true, Ordering::SeqCst);
        }
        libc::SIGHUP | libc::SIGQUIT | libc::SIGUSR2 => {
            IGNORED_SIGNAL.store(true, Ordering::SeqCst);
        }
        _ => {}
    }
    wake_signal_listener();
}

#[cfg(unix)]
fn create_signal_wake_pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [-1; 2];
    #[cfg(any(target_os = "linux", target_os = "android"))]
    let result = unsafe {
        // SAFETY: `fds` points to two writable c_int slots for `pipe2`.
        libc::pipe2(fds.as_mut_ptr(), libc::O_NONBLOCK | libc::O_CLOEXEC)
    };
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    let result = unsafe {
        // SAFETY: `fds` points to two writable c_int slots for `pipe`.
        libc::pipe(fds.as_mut_ptr())
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        set_fd_flags(fds[0], libc::O_NONBLOCK)?;
        set_fd_flags(fds[1], libc::O_NONBLOCK)?;
        set_close_on_exec(fds[0])?;
        set_close_on_exec(fds[1])?;
    }
    let read_fd = unsafe {
        // SAFETY: `pipe` initialized `fds[0]` and ownership is transferred here.
        OwnedFd::from_raw_fd(fds[0])
    };
    let write_fd = unsafe {
        // SAFETY: `pipe` initialized `fds[1]` and ownership is transferred here.
        OwnedFd::from_raw_fd(fds[1])
    };
    Ok((read_fd, write_fd))
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
fn set_fd_flags(fd: libc::c_int, flags: libc::c_int) -> io::Result<()> {
    let current = unsafe {
        // SAFETY: `fcntl(F_GETFL)` observes flags for a valid pipe descriptor.
        libc::fcntl(fd, libc::F_GETFL)
    };
    if current == -1 {
        return Err(io::Error::last_os_error());
    }
    let result = unsafe {
        // SAFETY: `fcntl(F_SETFL)` updates flags for a valid pipe descriptor.
        libc::fcntl(fd, libc::F_SETFL, current | flags)
    };
    if result == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
fn set_close_on_exec(fd: libc::c_int) -> io::Result<()> {
    let current = unsafe {
        // SAFETY: `fcntl(F_GETFD)` observes descriptor flags for a valid fd.
        libc::fcntl(fd, libc::F_GETFD)
    };
    if current == -1 {
        return Err(io::Error::last_os_error());
    }
    let result = unsafe {
        // SAFETY: `fcntl(F_SETFD)` updates descriptor flags for a valid fd.
        libc::fcntl(fd, libc::F_SETFD, current | libc::FD_CLOEXEC)
    };
    if result == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(unix)]
fn duplicate_signal_wake_reader() -> io::Result<ServerSignalWake> {
    let read_fd = SIGNAL_WAKE_READ_FD.load(Ordering::SeqCst);
    if read_fd < 0 {
        return Err(io::Error::other("server signal wake pipe is not installed"));
    }
    let duplicated = unsafe {
        // SAFETY: `read_fd` is the process-global read side of the wake pipe.
        libc::dup(read_fd)
    };
    if duplicated < 0 {
        return Err(io::Error::last_os_error());
    }
    set_close_on_exec_if_available(duplicated)?;
    Ok(ServerSignalWake {
        read_fd: SignalWakeReadFd { fd: duplicated },
    })
}

#[cfg(unix)]
fn set_close_on_exec_if_available(fd: libc::c_int) -> io::Result<()> {
    let current = unsafe {
        // SAFETY: `fcntl(F_GETFD)` observes descriptor flags for a valid fd.
        libc::fcntl(fd, libc::F_GETFD)
    };
    if current == -1 {
        return Err(io::Error::last_os_error());
    }
    let result = unsafe {
        // SAFETY: `fcntl(F_SETFD)` updates descriptor flags for a valid fd.
        libc::fcntl(fd, libc::F_SETFD, current | libc::FD_CLOEXEC)
    };
    if result == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(unix)]
fn wake_signal_listener() {
    let write_fd = SIGNAL_WAKE_WRITE_FD.load(Ordering::Relaxed);
    if write_fd < 0 {
        return;
    }
    let byte = [1_u8];
    let _ = unsafe {
        // SAFETY: `write_fd` is a nonblocking pipe fd installed before the signal
        // handlers. `write` is async-signal-safe and the byte buffer is stack
        // allocated.
        libc::write(write_fd, byte.as_ptr().cast(), 1)
    };
}

/// Resets signal dispositions that RMUX may handle in its daemon process back
/// to their default behavior before executing a pane child.
///
/// This is a no-op on non-Unix platforms.
pub fn reset_child_signal_dispositions() -> io::Result<()> {
    reset_child_signal_dispositions_impl()
}

#[cfg(unix)]
fn reset_child_signal_dispositions_impl() -> io::Result<()> {
    for signal in [
        libc::SIGHUP,
        libc::SIGINT,
        libc::SIGQUIT,
        libc::SIGTERM,
        libc::SIGUSR1,
        libc::SIGUSR2,
    ] {
        reset_signal(signal)?;
    }
    Ok(())
}

#[cfg(unix)]
fn reset_signal(signal: libc::c_int) -> io::Result<()> {
    let mut action = unsafe {
        // SAFETY: `sigaction` is a plain C struct. Zero initialization covers
        // platform-specific fields such as Linux `sa_restorer` before we fill
        // the portable fields below.
        std::mem::zeroed::<libc::sigaction>()
    };
    action.sa_sigaction = libc::SIG_DFL;
    action.sa_flags = 0;
    let empty_mask = unsafe {
        // SAFETY: `action.sa_mask` points to initialized writable storage.
        libc::sigemptyset(&mut action.sa_mask)
    };
    if empty_mask != 0 {
        return Err(io::Error::last_os_error());
    }
    let result = unsafe {
        // SAFETY: `signal` comes from libc signal constants and `action`
        // points to a fully initialized sigaction structure for this call.
        libc::sigaction(signal, &action, std::ptr::null_mut())
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(unix))]
fn reset_child_signal_dispositions_impl() -> io::Result<()> {
    Ok(())
}
