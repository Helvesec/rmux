use std::os::fd::{BorrowedFd, OwnedFd, RawFd};

use std::io;
use std::mem::MaybeUninit;
use std::time::Duration;

use rustix::fs::{fcntl_getfl, fcntl_setfl, open, Mode, OFlags};
use rustix::process::{
    getpid, ioctl_tiocsctty, kill_process as rustix_kill_process, kill_process_group, setsid,
};
use rustix::pty::{grantpt, ioctl_tiocgptpeer, openpt, ptsname, unlockpt, OpenptFlags};
use rustix::termios::{tcgetwinsize, tcsetpgrp, tcsetwinsize};

use super::unix_io;
use crate::{size, ProcessId, Result, Signal, TerminalGeometry, TerminalSize};

pub(crate) fn open_pty_pair() -> Result<(OwnedFd, OwnedFd)> {
    let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC)?;
    grantpt(&master)?;
    unlockpt(&master)?;

    let slave = open_slave(&master)?;

    Ok((master, slave))
}

fn open_slave(master: &OwnedFd) -> Result<OwnedFd> {
    match ioctl_tiocgptpeer(
        master,
        OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC,
    ) {
        Ok(slave) => Ok(slave),
        Err(peer_error) => open_slave_by_name(master).map_err(|_| peer_error.into()),
    }
}

fn open_slave_by_name(master: &OwnedFd) -> Result<OwnedFd> {
    let slave_name = ptsname(master, Vec::new())?;
    Ok(open(
        slave_name.as_c_str(),
        OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC,
        Mode::empty(),
    )?)
}

pub(crate) fn query_size(fd: BorrowedFd<'_>) -> Result<TerminalSize> {
    Ok(size::from_winsize(tcgetwinsize(fd)?))
}

pub(crate) fn apply_size(fd: BorrowedFd<'_>, size: TerminalSize) -> Result<()> {
    tcsetwinsize(fd, size::into_winsize(size))?;
    Ok(())
}

pub(crate) fn apply_geometry(fd: BorrowedFd<'_>, geometry: TerminalGeometry) -> Result<()> {
    tcsetwinsize(fd, size::into_winsize_geometry(geometry))?;
    Ok(())
}

pub(crate) fn setup_child_controlling_terminal(raw_master_fd: RawFd) -> std::io::Result<()> {
    // SAFETY: This closes only the child process' inherited copy of the PTY
    // master fd. The parent still owns its separate descriptor.
    unsafe { rustix::io::close(raw_master_fd) };

    setsid().map_err(std::io::Error::from)?;

    // SAFETY: `stdin` has already been wired to the PTY slave by `Command`, so
    // fd 0 is a valid borrowed descriptor for the rest of the pre-exec setup.
    let slave_stdin = unsafe { BorrowedFd::borrow_raw(0) };
    ioctl_tiocsctty(slave_stdin).map_err(std::io::Error::from)?;
    tcsetpgrp(slave_stdin, getpid()).map_err(std::io::Error::from)?;

    Ok(())
}

pub(crate) fn kill_foreground_process_group(pid: ProcessId, signal: Signal) -> Result<()> {
    kill_process_group(pid.as_rustix_pid()?, signal.as_rustix_signal())?;
    Ok(())
}

pub(crate) fn kill_process(pid: ProcessId, signal: Signal) -> Result<()> {
    rustix_kill_process(pid.as_rustix_pid()?, signal.as_rustix_signal())?;
    Ok(())
}

pub(crate) fn stopped_signal(pid: ProcessId) -> Result<Option<i32>> {
    let mut info = MaybeUninit::<libc::siginfo_t>::zeroed();
    // SAFETY: `info` points to writable storage for one siginfo_t. WNOWAIT
    // observes the stopped status without consuming the child's eventual exit
    // status, which remains owned by `std::process::Child`.
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            pid.as_u32() as libc::id_t,
            info.as_mut_ptr(),
            libc::WSTOPPED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if result == -1 {
        let errno = last_errno();
        if errno == rustix::io::Errno::CHILD {
            return Ok(None);
        }
        return Err(errno.into());
    }

    // SAFETY: `info` was zero-initialized before the call and `waitid`
    // returned success, so reading the initialized siginfo_t is valid.
    let info = unsafe { info.assume_init() };
    // SAFETY: `waitid` with WSTOPPED populates `si_pid` when a stopped child
    // is available. A zero pid means WNOHANG had no event.
    let si_pid = unsafe { info.si_pid() };
    if si_pid == 0 {
        Ok(None)
    } else {
        // SAFETY: a non-zero `si_pid` means this siginfo_t carries a real
        // child status for the requested child.
        Ok(Some(unsafe { info.si_status() }))
    }
}

pub(crate) fn read(fd: BorrowedFd<'_>, buffer: &mut [u8]) -> io::Result<usize> {
    unix_io::read(fd, buffer)
}

pub(crate) fn try_read(fd: BorrowedFd<'_>, buffer: &mut [u8]) -> io::Result<usize> {
    unix_io::try_read(fd, buffer)
}

pub(crate) fn write_all_with_timeout(
    fd: BorrowedFd<'_>,
    buffer: &[u8],
    timeout: Duration,
) -> io::Result<()> {
    unix_io::write_all_with_timeout(fd, buffer, timeout)
}

pub(crate) fn try_write_immediate(fd: BorrowedFd<'_>, buffer: &[u8]) -> io::Result<usize> {
    unix_io::try_write_immediate(fd, buffer)
}

pub(crate) fn set_nonblocking(fd: BorrowedFd<'_>) -> io::Result<()> {
    let flags = fcntl_getfl(fd).map_err(io::Error::other)?;
    fcntl_setfl(fd, flags | OFlags::NONBLOCK).map_err(io::Error::other)?;
    Ok(())
}

fn last_errno() -> rustix::io::Errno {
    let raw = std::io::Error::last_os_error()
        .raw_os_error()
        .unwrap_or(libc::EIO);
    rustix::io::Errno::from_raw_os_error(raw)
}

#[cfg(all(test, target_env = "gnu"))]
pub(crate) use fork_free::{resolve_program, DEFAULT_EXEC_SEARCH_PATH};
#[cfg(target_env = "gnu")]
pub(crate) use fork_free::{spawn_pty_child_without_fork, SpawnedProcess};

#[cfg(target_env = "gnu")]
mod fork_free {
    //! Pane launch without `fork()`. `Command` forks once a `pre_exec` hook
    //! is set: fork copies the daemon page tables, every page the daemon
    //! writes before the child execs takes a copy-on-write fault, and the
    //! daemon waits for that exec, so a new pane costs more as the daemon
    //! grows. glibc `posix_spawn` (`CLONE_VM | CLONE_VFORK`) expresses the
    //! whole PTY launch. Whatever it cannot reproduce exactly returns `None`
    //! and the caller keeps the fork launch, which also reports errors.

    use std::collections::BTreeMap;
    use std::ffi::{CStr, CString, OsStr, OsString};
    use std::io;
    use std::mem::MaybeUninit;
    use std::os::fd::{AsRawFd, BorrowedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::process::ExitStatusExt;
    use std::path::{Path, PathBuf};
    use std::process::ExitStatus;
    use std::sync::OnceLock;

    use crate::ChildCommand;

    /// `execvp` searches this list when the child environment has no `PATH`.
    pub(crate) const DEFAULT_EXEC_SEARCH_PATH: &str = "/bin:/usr/bin";

    /// Signals the `fork` launch resets before `exec`: Rust's `SIGPIPE` reset
    /// plus `rmux_os::signals::reset_child_signal_dispositions`.
    const DEFAULT_DISPOSITION_SIGNALS: [libc::c_int; 7] = [
        libc::SIGPIPE,
        libc::SIGHUP,
        libc::SIGINT,
        libc::SIGQUIT,
        libc::SIGTERM,
        libc::SIGUSR1,
        libc::SIGUSR2,
    ];

    type AddChdirFn = unsafe extern "C" fn(
        *mut libc::posix_spawn_file_actions_t,
        *const libc::c_char,
    ) -> libc::c_int;
    type AddClosefromFn =
        unsafe extern "C" fn(*mut libc::posix_spawn_file_actions_t, libc::c_int) -> libc::c_int;

    /// `addchdir_np` (glibc 2.29) and `addclosefrom_np` (2.34), resolved at
    /// run time so an older glibc keeps the fork launch instead of not loading.
    struct ExtendedFileActions {
        add_chdir: AddChdirFn,
        add_closefrom: AddClosefromFn,
    }

    fn extended_file_actions() -> Option<&'static ExtendedFileActions> {
        static ACTIONS: OnceLock<Option<ExtendedFileActions>> = OnceLock::new();
        ACTIONS
            .get_or_init(|| {
                let add_chdir = lookup_symbol(c"posix_spawn_file_actions_addchdir_np")?;
                let add_closefrom = lookup_symbol(c"posix_spawn_file_actions_addclosefrom_np")?;
                // SAFETY: glibc exports both symbols with exactly these C
                // signatures; a non-null `dlsym` result is the function address.
                Some(unsafe {
                    ExtendedFileActions {
                        add_chdir: std::mem::transmute::<*mut libc::c_void, AddChdirFn>(add_chdir),
                        add_closefrom: std::mem::transmute::<*mut libc::c_void, AddClosefromFn>(
                            add_closefrom,
                        ),
                    }
                })
            })
            .as_ref()
    }

    fn lookup_symbol(name: &CStr) -> Option<*mut libc::c_void> {
        // SAFETY: `name` is NUL-terminated and RTLD_DEFAULT searches the already
        // loaded objects without loading anything.
        let address = unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) };
        (!address.is_null()).then_some(address)
    }

    /// Launches `command` on the PTY `slave` without forking the daemon.
    ///
    /// Returns the child's pid, or `None` when the caller must use the `fork`
    /// launch: missing glibc support, a program the `PATH` search cannot settle
    /// before the child exists, or any `posix_spawn` failure.
    pub(crate) fn spawn_pty_child_without_fork(
        command: &ChildCommand,
        slave: BorrowedFd<'_>,
    ) -> Option<libc::pid_t> {
        let extended = extended_file_actions()?;
        let slave_fd = slave.as_raw_fd();
        if slave_fd <= libc::STDERR_FILENO {
            return None;
        }

        let environment = child_environment(command);
        let program = resolve_program(
            &command.program,
            environment.get(OsStr::new("PATH")).map(OsString::as_os_str),
        )?;
        let program = c_string(program.as_os_str())?;
        let argv0 = command
            .arg0
            .as_deref()
            .unwrap_or(command.program.as_os_str());
        let argv = std::iter::once(argv0)
            .chain(command.args.iter().map(OsString::as_os_str))
            .map(c_string)
            .collect::<Option<Vec<_>>>()?;
        let envp = environment
            .iter()
            .map(|(key, value)| {
                let mut entry = key.as_bytes().to_vec();
                entry.push(b'=');
                entry.extend_from_slice(value.as_bytes());
                CString::new(entry).ok()
            })
            .collect::<Option<Vec<_>>>()?;
        let slave_path = CString::new(format!("/proc/self/fd/{slave_fd}")).ok()?;
        let current_dir = match &command.current_dir {
            Some(directory) => Some(c_string(directory.as_os_str())?),
            None => None,
        };

        let attributes = SpawnAttributes::new()?;
        attributes.configure()?;
        let file_actions = SpawnFileActions::new()?;
        file_actions.configure(extended, &slave_path, current_dir.as_deref())?;

        let argv_pointers = null_terminated(&argv);
        let envp_pointers = null_terminated(&envp);
        let mut pid: libc::pid_t = 0;
        // SAFETY: every pointer refers to initialized storage that outlives the
        // call; argv and envp are NULL-terminated arrays of C strings.
        let status = unsafe {
            libc::posix_spawn(
                &mut pid,
                program.as_ptr(),
                file_actions.as_ptr(),
                attributes.as_ptr(),
                argv_pointers.as_ptr(),
                envp_pointers.as_ptr(),
            )
        };
        if status != 0 {
            tracing::debug!(
                error = %io::Error::from_raw_os_error(status),
                "posix_spawn PTY launch failed; using the fork launch"
            );
            return None;
        }
        Some(pid)
    }

    /// The environment `Command` would pass: the daemon's own variables unless
    /// cleared, then the command's variables, the last value of a key winning.
    fn child_environment(command: &ChildCommand) -> BTreeMap<OsString, OsString> {
        let mut environment = BTreeMap::new();
        if !command.clear_env {
            environment.extend(std::env::vars_os());
        }
        for (key, value) in &command.env {
            environment.insert(key.clone(), value.clone());
        }
        environment
    }

    /// Settles `execvp`'s search before the child exists, or returns `None` when
    /// only the child could settle it (an empty or relative `PATH` entry, which
    /// depends on the child's working directory, or an unreadable candidate).
    pub(crate) fn resolve_program(program: &Path, search_path: Option<&OsStr>) -> Option<PathBuf> {
        let name = program.as_os_str().as_bytes();
        if name.is_empty() {
            return None;
        }
        if name.contains(&b'/') {
            return Some(program.to_path_buf());
        }
        let search_path = search_path.unwrap_or(OsStr::new(DEFAULT_EXEC_SEARCH_PATH));
        for directory in search_path.as_bytes().split(|byte| *byte == b':') {
            if directory.first() != Some(&b'/') {
                return None;
            }
            let candidate = Path::new(OsStr::from_bytes(directory)).join(program);
            match std::fs::metadata(&candidate) {
                Ok(metadata) if metadata.is_file() => {
                    if is_executable(&candidate)? {
                        return Some(candidate);
                    }
                }
                Ok(_) => {}
                Err(error) if skips_search_entry(&error) => {}
                Err(_) => return None,
            }
        }
        None
    }

    /// `execvp` moves to the next `PATH` entry on these errors.
    fn skips_search_entry(error: &io::Error) -> bool {
        matches!(
            error.raw_os_error(),
            Some(libc::ENOENT | libc::ENOTDIR | libc::EACCES)
        )
    }

    fn is_executable(candidate: &Path) -> Option<bool> {
        let path = c_string(candidate.as_os_str())?;
        // SAFETY: `path` is a NUL-terminated C string.
        if unsafe { libc::access(path.as_ptr(), libc::X_OK) } == 0 {
            return Some(true);
        }
        skips_search_entry(&io::Error::last_os_error()).then_some(false)
    }

    fn c_string(value: &OsStr) -> Option<CString> {
        CString::new(value.as_bytes()).ok()
    }

    fn null_terminated(strings: &[CString]) -> Vec<*mut libc::c_char> {
        strings
            .iter()
            .map(|string| string.as_ptr().cast_mut())
            .chain(std::iter::once(std::ptr::null_mut()))
            .collect()
    }

    /// An initialized `posix_spawnattr_t`, kept at a stable heap address.
    struct SpawnAttributes(Box<MaybeUninit<libc::posix_spawnattr_t>>);

    impl SpawnAttributes {
        fn new() -> Option<Self> {
            let mut attributes = Box::new(MaybeUninit::uninit());
            // SAFETY: init writes a valid attribute object into the storage.
            (unsafe { libc::posix_spawnattr_init(attributes.as_mut_ptr()) } == 0)
                .then_some(Self(attributes))
        }

        fn as_ptr(&self) -> *const libc::posix_spawnattr_t {
            self.0.as_ptr()
        }

        fn as_mut_ptr(&self) -> *mut libc::posix_spawnattr_t {
            self.0.as_ptr().cast_mut()
        }

        fn configure(&self) -> Option<()> {
            let mut default_signals = empty_signal_set()?;
            for signal in DEFAULT_DISPOSITION_SIGNALS {
                // SAFETY: the set is initialized and the signal number is valid.
                if unsafe { libc::sigaddset(&mut default_signals, signal) } != 0 {
                    return None;
                }
            }
            let empty_mask = empty_signal_set()?;
            let flags = libc::POSIX_SPAWN_SETSID
                | libc::POSIX_SPAWN_SETSIGDEF
                | libc::POSIX_SPAWN_SETSIGMASK;
            // SAFETY: the attribute object was initialized by `new`; each setter
            // copies its argument. glibc older than 2.26 rejects SETSID here.
            let configured = unsafe {
                libc::posix_spawnattr_setsigdefault(self.as_mut_ptr(), &default_signals) == 0
                    && libc::posix_spawnattr_setsigmask(self.as_mut_ptr(), &empty_mask) == 0
                    && libc::posix_spawnattr_setflags(self.as_mut_ptr(), flags as libc::c_short)
                        == 0
            };
            configured.then_some(())
        }
    }

    impl Drop for SpawnAttributes {
        fn drop(&mut self) {
            // SAFETY: the object was initialized by `new` and is destroyed once.
            unsafe { libc::posix_spawnattr_destroy(self.as_mut_ptr()) };
        }
    }

    /// An initialized `posix_spawn_file_actions_t`, kept at a stable address.
    struct SpawnFileActions(Box<MaybeUninit<libc::posix_spawn_file_actions_t>>);

    impl SpawnFileActions {
        fn new() -> Option<Self> {
            let mut actions = Box::new(MaybeUninit::uninit());
            // SAFETY: init writes a valid, empty action list into the storage.
            (unsafe { libc::posix_spawn_file_actions_init(actions.as_mut_ptr()) } == 0)
                .then_some(Self(actions))
        }

        fn as_ptr(&self) -> *const libc::posix_spawn_file_actions_t {
            self.0.as_ptr()
        }

        fn as_mut_ptr(&self) -> *mut libc::posix_spawn_file_actions_t {
            self.0.as_ptr().cast_mut()
        }

        /// Runs in the child after `setsid`, in order: reopen the slave as fd 0
        /// without `O_NOCTTY` so the new session acquires it as controlling
        /// terminal with the child's group in the foreground, duplicate it to
        /// fds 1 and 2, enter the working directory, close every other fd.
        fn configure(
            &self,
            extended: &ExtendedFileActions,
            slave_path: &CStr,
            current_dir: Option<&CStr>,
        ) -> Option<()> {
            let actions = self.as_mut_ptr();
            // SAFETY: the action list was initialized by `new`; glibc copies the
            // path arguments, and the extended functions match their symbols.
            let configured = unsafe {
                libc::posix_spawn_file_actions_addopen(
                    actions,
                    libc::STDIN_FILENO,
                    slave_path.as_ptr(),
                    libc::O_RDWR,
                    0,
                ) == 0
                    && libc::posix_spawn_file_actions_adddup2(
                        actions,
                        libc::STDIN_FILENO,
                        libc::STDOUT_FILENO,
                    ) == 0
                    && libc::posix_spawn_file_actions_adddup2(
                        actions,
                        libc::STDIN_FILENO,
                        libc::STDERR_FILENO,
                    ) == 0
                    && current_dir.is_none_or(|directory| {
                        (extended.add_chdir)(actions, directory.as_ptr()) == 0
                    })
                    && (extended.add_closefrom)(actions, libc::STDERR_FILENO + 1) == 0
            };
            configured.then_some(())
        }
    }

    impl Drop for SpawnFileActions {
        fn drop(&mut self) {
            // SAFETY: the object was initialized by `new` and is destroyed once.
            unsafe { libc::posix_spawn_file_actions_destroy(self.as_mut_ptr()) };
        }
    }

    fn empty_signal_set() -> Option<libc::sigset_t> {
        let mut set = MaybeUninit::uninit();
        // SAFETY: sigemptyset initializes the provided storage on success.
        (unsafe { libc::sigemptyset(set.as_mut_ptr()) } == 0).then(|| {
            // SAFETY: sigemptyset succeeded.
            unsafe { set.assume_init() }
        })
    }

    /// A child launched by `posix_spawn`, reaped with the same `waitpid` calls
    /// and status caching as `std::process::Child`.
    #[derive(Debug)]
    pub(crate) struct SpawnedProcess {
        pid: libc::pid_t,
        status: Option<ExitStatus>,
    }

    impl SpawnedProcess {
        pub(crate) fn new(pid: libc::pid_t) -> Self {
            Self { pid, status: None }
        }

        pub(crate) fn wait(&mut self) -> io::Result<ExitStatus> {
            if let Some(status) = self.status {
                return Ok(status);
            }
            loop {
                match self.reap(0)? {
                    Some(status) => return Ok(status),
                    None => continue,
                }
            }
        }

        pub(crate) fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
            if let Some(status) = self.status {
                return Ok(Some(status));
            }
            self.reap(libc::WNOHANG)
        }

        /// One `waitpid`; `Ok(None)` means still running (WNOHANG) or EINTR.
        fn reap(&mut self, options: libc::c_int) -> io::Result<Option<ExitStatus>> {
            let mut raw_status = 0;
            // SAFETY: `raw_status` is writable and `pid` is this handle's child.
            let reaped = unsafe { libc::waitpid(self.pid, &mut raw_status, options) };
            if reaped == self.pid {
                let status = ExitStatus::from_raw(raw_status);
                self.status = Some(status);
                return Ok(Some(status));
            }
            if reaped == 0 {
                return Ok(None);
            }
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                return Ok(None);
            }
            Err(error)
        }
    }
}
