#[cfg(unix)]
use std::fs::File;
#[cfg(all(target_os = "linux", target_env = "gnu"))]
use std::os::fd::AsFd;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::process::ExitStatus;
#[cfg(unix)]
use std::process::{Child, Command, Stdio};

#[cfg(all(not(unix), not(windows)))]
use crate::unsupported_op;
#[cfg(all(not(unix), not(windows)))]
use crate::PtyError;
#[cfg(any(unix, windows))]
use crate::{backend, PtyPair};
use crate::{ChildCommand, ProcessId, PtyChild, Result, RmuxShell, ShellChild, Signal, SpawnedPty};

/// The default backend: an operating-system process, forked and executed on
/// Unix and launched into a ConPTY on Windows.
#[derive(Clone, Copy, Debug, Default)]
pub struct ProcessShell;

impl RmuxShell for ProcessShell {
    fn spawn(&self, command: ChildCommand) -> Result<SpawnedPty> {
        spawn_child(command)
    }
}

/// The [`ShellChild`] implementation backing [`ProcessShell`].
#[derive(Debug)]
#[cfg_attr(all(not(unix), not(windows)), allow(dead_code))]
struct ProcessChild {
    #[cfg(unix)]
    child: UnixChild,
    #[cfg(windows)]
    child: backend::WindowsChild,
    pid: ProcessId,
}

/// A Unix pane process: launched by `posix_spawn` where the platform allows
/// it, otherwise by `std::process::Command` after `fork`.
#[cfg(unix)]
#[derive(Debug)]
enum UnixChild {
    Command(Child),
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    Spawned(backend::SpawnedProcess),
}

#[cfg(unix)]
impl UnixChild {
    fn wait(&mut self) -> std::io::Result<ExitStatus> {
        match self {
            Self::Command(child) => child.wait(),
            #[cfg(all(target_os = "linux", target_env = "gnu"))]
            Self::Spawned(child) => child.wait(),
        }
    }

    fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
        match self {
            Self::Command(child) => child.try_wait(),
            #[cfg(all(target_os = "linux", target_env = "gnu"))]
            Self::Spawned(child) => child.try_wait(),
        }
    }
}

impl ShellChild for ProcessChild {
    fn pid(&self) -> ProcessId {
        self.pid
    }

    fn wait(&mut self) -> Result<ExitStatus> {
        #[cfg(unix)]
        {
            Ok(self.child.wait()?)
        }

        #[cfg(not(unix))]
        {
            #[cfg(windows)]
            {
                backend::wait_child(&mut self.child)
            }

            #[cfg(not(windows))]
            {
                Err(PtyError::Unsupported(unsupported_op::WAIT_FOR_PTY_CHILD))
            }
        }
    }

    fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
        #[cfg(unix)]
        {
            Ok(self.child.try_wait()?)
        }

        #[cfg(not(unix))]
        {
            #[cfg(windows)]
            {
                backend::try_wait_child(&mut self.child)
            }

            #[cfg(not(windows))]
            {
                Err(PtyError::Unsupported(
                    unsupported_op::TRY_WAIT_FOR_PTY_CHILD,
                ))
            }
        }
    }

    fn kill(&self, signal: Signal) -> Result<()> {
        #[cfg(unix)]
        {
            backend::kill_foreground_process_group(self.pid, signal)
        }

        #[cfg(not(unix))]
        {
            #[cfg(windows)]
            {
                backend::kill_child(&self.child, signal)
            }

            #[cfg(not(windows))]
            {
                let _ = signal;
                Err(PtyError::Unsupported(unsupported_op::SIGNAL_PTY_FOREGROUND))
            }
        }
    }

    fn kill_session_leader(&self, signal: Signal) -> Result<()> {
        #[cfg(unix)]
        {
            backend::kill_process(self.pid, signal)?;
            Ok(())
        }

        #[cfg(not(unix))]
        {
            #[cfg(windows)]
            {
                backend::kill_child(&self.child, signal)
            }

            #[cfg(not(windows))]
            {
                let _ = signal;
                Err(PtyError::Unsupported(
                    unsupported_op::SIGNAL_PTY_SESSION_LEADER,
                ))
            }
        }
    }

    #[cfg(unix)]
    fn continue_if_stopped(&self) -> Result<bool> {
        let Some(stop_signal) = backend::stopped_signal(self.pid)? else {
            return Ok(false);
        };
        if stop_signal == libc::SIGTTIN || stop_signal == libc::SIGTTOU {
            return Ok(false);
        }

        backend::kill_foreground_process_group(self.pid, Signal::CONT)
            .or_else(|_| backend::kill_process(self.pid, Signal::CONT))?;
        Ok(true)
    }

    #[cfg(windows)]
    fn wait_for_process_tree_exit(&mut self) -> Result<ExitStatus> {
        backend::wait_child_process_tree(&mut self.child)
    }

    #[cfg(windows)]
    fn try_wait_for_process_tree_exit(&mut self) -> Result<Option<ExitStatus>> {
        backend::try_wait_child_process_tree(&mut self.child)
    }

    #[cfg(windows)]
    fn try_clone_for_wait(&self) -> Result<Box<dyn ShellChild>> {
        Ok(Box::new(Self {
            child: backend::try_clone_child_for_wait(&self.child)?,
            pid: self.pid,
        }))
    }

    #[cfg(windows)]
    fn try_clone_for_exit_teardown(&self) -> Result<Box<dyn ShellChild>> {
        Ok(Box::new(Self {
            child: backend::try_clone_child_for_exit_teardown(&self.child)?,
            pid: self.pid,
        }))
    }

    #[cfg(windows)]
    fn close_pseudoconsole(&self) {
        backend::close_child_pseudoconsole(&self.child);
    }
}

#[cfg(unix)]
fn unblock_child_signals() -> std::io::Result<()> {
    // SAFETY: Called only in the single-threaded post-fork child. Both signal
    // operations are async-signal-safe and use initialized stack storage.
    unsafe {
        let mut mask = std::mem::zeroed::<libc::sigset_t>();
        if libc::sigemptyset(&mut mask) != 0
            || libc::sigprocmask(libc::SIG_SETMASK, &mask, std::ptr::null_mut()) != 0
        {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(unix)]
fn spawn_child(command: ChildCommand) -> Result<SpawnedPty> {
    let pair = match command.size {
        Some(size) => PtyPair::open_with_size(size)?,
        None => PtyPair::open()?,
    };
    let (master, slave) = pair.into_split();
    let raw_master_fd = master.raw_fd();
    let startup_slave = slave.try_clone()?.into_owned_fd();
    let master = master.with_startup_slave(startup_slave);

    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    if let Some(raw_pid) = backend::spawn_pty_child_without_fork(&command, slave.as_fd()) {
        let child = UnixChild::Spawned(backend::SpawnedProcess::new(raw_pid));
        let pid = ProcessId::new(raw_pid.unsigned_abs())?;
        return Ok(SpawnedPty::new(
            master,
            PtyChild::new(ProcessChild { child, pid }),
        ));
    }

    let stdin = File::from(slave.try_clone()?.into_owned_fd());
    let stdout = File::from(slave.try_clone()?.into_owned_fd());
    let stderr = File::from(slave.into_owned_fd());

    let mut std_command = Command::new(&command.program);
    if let Some(arg0) = &command.arg0 {
        std_command.arg0(arg0);
    }
    std_command.args(&command.args);
    std_command.stdin(Stdio::from(stdin));
    std_command.stdout(Stdio::from(stdout));
    std_command.stderr(Stdio::from(stderr));
    if command.clear_env {
        std_command.env_clear();
    }
    if let Some(current_dir) = &command.current_dir {
        std_command.current_dir(current_dir);
    }

    for (key, value) in &command.env {
        std_command.env(key, value);
    }

    let pre_exec = move || {
        rmux_os::signals::reset_child_signal_dispositions()?;
        backend::setup_child_controlling_terminal(raw_master_fd)?;
        backend::mark_child_fds_close_on_exec()?;
        // Exec preserves the spawning thread's mask. A new terminal job must
        // receive its signals even when the daemon thread had them blocked.
        unblock_child_signals()
    };

    // SAFETY: The closure only performs post-fork child setup required for PTY
    // correctness: it closes the child's inherited master fd copy, creates a
    // new session, installs the slave as the controlling terminal, sets the
    // foreground process group, marks other descriptors close-on-exec without
    // closing the exec-error pipe early, and unblocks signals. No allocation
    // or parent-owned locks after fork.
    unsafe {
        std_command.pre_exec(pre_exec);
    }

    let child = std_command.spawn()?;
    let pid = ProcessId::new(child.id())?;
    let child = UnixChild::Command(child);

    Ok(SpawnedPty::new(
        master,
        PtyChild::new(ProcessChild { child, pid }),
    ))
}

#[cfg(not(unix))]
fn spawn_child(_command: ChildCommand) -> Result<SpawnedPty> {
    #[cfg(windows)]
    {
        let pair = match _command.size {
            Some(size) => PtyPair::open_with_size(size)?,
            None => PtyPair::open()?,
        };
        let master = pair.into_master();
        let child = backend::spawn_child(_command, master.windows_pty())?;
        let pid = child.pid();
        Ok(SpawnedPty::new(
            master,
            PtyChild::new(ProcessChild { child, pid }),
        ))
    }

    #[cfg(not(windows))]
    {
        Err(PtyError::Unsupported(unsupported_op::SPAWN_PTY_CHILD))
    }
}

/// The Linux glibc pane launch that avoids `fork`, checked against the
/// contract of the `fork` launch it replaces.
#[cfg(all(test, target_os = "linux", target_env = "gnu"))]
mod fork_free_launch_tests {
    use std::ffi::OsStr;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    use crate::backend::{
        open_pty_pair, resolve_program, spawn_pty_child_without_fork, SpawnedProcess,
        DEFAULT_EXEC_SEARCH_PATH,
    };
    use crate::ChildCommand;

    fn scratch_directory(name: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("rmux-pty-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&directory).expect("create scratch directory");
        directory
            .canonicalize()
            .expect("canonical scratch directory")
    }

    fn write_program(directory: &Path, name: &str, mode: u32) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = directory.join(name);
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").expect("write program");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).expect("chmod");
        path
    }

    fn read_until_marker(master: &OwnedFd, marker: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut output = Vec::new();
        while !String::from_utf8_lossy(&output).contains(marker) {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(
                !remaining.is_zero(),
                "no {marker} in {:?}",
                String::from_utf8_lossy(&output)
            );
            let mut poll = libc::pollfd {
                fd: master.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: one initialized pollfd for a descriptor owned by the test.
            let ready =
                unsafe { libc::poll(&mut poll, 1, remaining.as_millis().min(1_000) as i32) };
            if ready <= 0 {
                continue;
            }
            let mut buffer = [0_u8; 1024];
            // SAFETY: the buffer is writable for its full length.
            let read =
                unsafe { libc::read(master.as_raw_fd(), buffer.as_mut_ptr().cast(), buffer.len()) };
            assert!(read > 0, "master read: {}", std::io::Error::last_os_error());
            output.extend_from_slice(&buffer[..read as usize]);
        }
        String::from_utf8_lossy(&output).into_owned()
    }

    fn field<'a>(report: &'a str, key: &str) -> &'a str {
        report
            .split_whitespace()
            .find_map(|word| word.strip_prefix(key))
            .unwrap_or_else(|| panic!("{key} missing from {report:?}"))
    }

    #[test]
    fn program_search_settles_execvp_before_the_child_exists() {
        let directory = scratch_directory("search");
        let executable = write_program(&directory, "s2093-probe", 0o755);
        write_program(&directory, "s2093-plain", 0o644);
        let search = format!("/nonexistent-rmux-dir:{}", directory.display());

        assert_eq!(
            resolve_program(Path::new("s2093-probe"), Some(OsStr::new(&search))),
            Some(executable)
        );
        assert_eq!(
            resolve_program(Path::new("s2093-plain"), Some(OsStr::new(&search))),
            None
        );
        assert_eq!(
            resolve_program(Path::new("./relative/tool"), None),
            Some(PathBuf::from("./relative/tool"))
        );
        assert_eq!(
            resolve_program(Path::new("sh"), Some(OsStr::new(":/bin"))),
            None,
            "empty entry is the child's cwd"
        );
        assert_eq!(
            resolve_program(Path::new("sh"), Some(OsStr::new("relative:/bin"))),
            None
        );
        assert_eq!(resolve_program(Path::new(""), None), None);
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn program_search_without_path_uses_the_glibc_execvp_default() {
        // glibc execvp searches confstr(_CS_PATH) when PATH is unset, in order.
        let mut buffer = [0_u8; 256];
        // SAFETY: the buffer is writable for its full length.
        let length =
            unsafe { libc::confstr(libc::_CS_PATH, buffer.as_mut_ptr().cast(), buffer.len()) };
        assert!(
            (2..=buffer.len()).contains(&length),
            "confstr(_CS_PATH) = {length}"
        );
        let glibc_default = std::str::from_utf8(&buffer[..length - 1]).expect("_CS_PATH");
        assert_eq!(DEFAULT_EXEC_SEARCH_PATH, glibc_default);
        assert_eq!(
            resolve_program(Path::new("sh"), None),
            resolve_program(Path::new("sh"), Some(OsStr::new(glibc_default)))
        );
    }

    #[test]
    fn fork_free_launch_matches_the_fork_launch_contract() {
        let directory = scratch_directory("launch");
        let (master, slave) = open_pty_pair().expect("pty pair");
        let mut pipe = [0; 2];
        // SAFETY: pipe writes two new descriptors, deliberately without CLOEXEC,
        // so only the launch's own isolation can keep them out of the child.
        assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
        // SAFETY: both descriptors were just created and are owned here.
        let (pipe_read, pipe_write) =
            unsafe { (OwnedFd::from_raw_fd(pipe[0]), OwnedFd::from_raw_fd(pipe[1])) };
        // Builtins only up to the report, so the shell's own state is read
        // before any command substitution forks.
        let script = format!(
            "read -r stat < /proc/$$/stat; set -- ${{stat##*) }}; \
             while read -r key value; do case $key in \
             SigBlk:) blocked=$value;; SigIgn:) ignored=$value;; esac; done < /proc/$$/status; \
             if [ -e /proc/$$/fd/{leak} ]; then leak=open; else leak=closed; fi; \
             stdio=other; [ -t 0 ] && [ /proc/$$/fd/1 -ef /proc/$$/fd/0 ] \
             && [ /proc/$$/fd/2 -ef /proc/$$/fd/0 ] && stdio=pty; \
             printf 'pid=%s pgrp=%s sid=%s tpgid=%s fd=%s stdio=%s SigBlk:%s SigIgn:%s cwd=%s DONE\\n' \
             \"$$\" \"$3\" \"$4\" \"$6\" \"$leak\" \"$stdio\" \"$blocked\" \"$ignored\" \"$(pwd -P)\"",
            leak = pipe_read.as_raw_fd()
        );
        let command = ChildCommand::new("sh")
            .arg("-c")
            .arg(script)
            .env("PATH", "/usr/bin:/bin")
            .current_dir(&directory);

        let mut blocked = std::mem::MaybeUninit::<libc::sigset_t>::uninit();
        let mut previous = std::mem::MaybeUninit::<libc::sigset_t>::uninit();
        // SAFETY: the sets are initialized before use; only this test thread's
        // mask changes, and it is restored right after the launch.
        unsafe {
            libc::sigemptyset(blocked.as_mut_ptr());
            libc::sigaddset(blocked.as_mut_ptr(), libc::SIGUSR2);
            libc::pthread_sigmask(libc::SIG_BLOCK, blocked.as_ptr(), previous.as_mut_ptr());
        }
        let pid = spawn_pty_child_without_fork(&command, std::os::fd::AsFd::as_fd(&slave));
        // SAFETY: restores the mask saved above.
        unsafe {
            libc::pthread_sigmask(libc::SIG_SETMASK, previous.as_ptr(), std::ptr::null_mut())
        };
        let pid = pid.expect("glibc posix_spawn launch");

        let report = read_until_marker(&master, "DONE");
        let status = SpawnedProcess::new(pid).wait().expect("wait");
        drop((pipe_read, pipe_write));
        let _ = std::fs::remove_dir_all(&directory);

        assert!(status.success(), "{status:?} {report:?}");
        let pid = pid.to_string();
        assert_eq!(field(&report, "pid="), pid, "{report}");
        assert_eq!(field(&report, "pgrp="), pid, "own process group: {report}");
        assert_eq!(field(&report, "sid="), pid, "session leader: {report}");
        assert_eq!(
            field(&report, "tpgid="),
            pid,
            "foreground group of its controlling PTY: {report}"
        );
        assert_eq!(Path::new(field(&report, "cwd=")), directory, "{report}");
        assert_eq!(
            field(&report, "fd="),
            "closed",
            "inherited descriptor leaked: {report}"
        );
        assert_eq!(
            field(&report, "stdio="),
            "pty",
            "fds 0, 1 and 2 must all be the PTY slave: {report}"
        );
        assert_eq!(field(&report, "SigBlk:"), "0000000000000000", "{report}");
        let ignored = u64::from_str_radix(field(&report, "SigIgn:"), 16).expect("SigIgn hex");
        assert_eq!(
            ignored & (1 << (libc::SIGPIPE - 1)),
            0,
            "SIGPIPE reset to default: {report}"
        );
    }

    #[test]
    fn fork_free_launch_resets_signals_the_daemon_ignores() {
        use libc::{SIGHUP, SIGINT, SIGPIPE, SIGQUIT, SIGTERM, SIGUSR1, SIGUSR2};
        const ISOLATED: &str = "RMUX_TEST_FORK_FREE_IGNORED_SIGNALS";
        const TEST: &str =
            "process_shell::fork_free_launch_tests::fork_free_launch_resets_signals_the_daemon_ignores";
        if std::env::var_os(ISOLATED).is_none() {
            // Dispositions are process-wide: ignore them only in a test process
            // that runs this test alone.
            crate::test_rerun::rerun_exact(TEST, |command| {
                command.env(ISOLATED, "1");
            });
            return;
        }
        // Rust's SIGPIPE reset plus rmux_os::signals::reset_child_signal_dispositions.
        let reset = [SIGPIPE, SIGHUP, SIGINT, SIGQUIT, SIGTERM, SIGUSR1, SIGUSR2];
        for signal in reset {
            // SAFETY: SIG_IGN is valid for these catchable signals, and this
            // process runs no other test.
            assert_ne!(
                unsafe { libc::signal(signal, libc::SIG_IGN) },
                libc::SIG_ERR
            );
        }
        let (master, slave) = open_pty_pair().expect("pty pair");
        let command = ChildCommand::new("/bin/sh").arg("-c").arg(
            "while read -r key value; do case $key in \
             SigIgn:) printf 'SigIgn:%s DONE\\n' \"$value\";; esac; done < /proc/$$/status",
        );
        let pid = spawn_pty_child_without_fork(&command, std::os::fd::AsFd::as_fd(&slave))
            .expect("launch");
        let report = read_until_marker(&master, "DONE");
        SpawnedProcess::new(pid).wait().expect("wait");
        let ignored = u64::from_str_radix(field(&report, "SigIgn:"), 16).expect("SigIgn hex");
        let inherited: Vec<_> = reset
            .into_iter()
            .filter(|signal| ignored & (1 << (signal - 1)) != 0)
            .collect();
        assert!(
            inherited.is_empty(),
            "still ignored {inherited:?}: {report}"
        );
    }

    #[test]
    fn spawned_process_caches_its_exit_status_like_std_child() {
        let (_master, slave) = open_pty_pair().expect("pty pair");
        let command = ChildCommand::new("/bin/sh").arg("-c").arg("exit 7");
        let pid = spawn_pty_child_without_fork(&command, std::os::fd::AsFd::as_fd(&slave))
            .expect("launch");
        let mut process = SpawnedProcess::new(pid);
        let deadline = Instant::now() + Duration::from_secs(5);
        let status = loop {
            if let Some(status) = process.try_wait().expect("try_wait") {
                break status;
            }
            assert!(Instant::now() < deadline, "child did not exit");
            std::thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(status.code(), Some(7));
        assert_eq!(process.wait().expect("cached wait").code(), Some(7));
        assert_eq!(
            process
                .try_wait()
                .expect("cached try_wait")
                .map(|s| s.code()),
            Some(Some(7))
        );
    }
}
