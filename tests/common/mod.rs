#![allow(dead_code)]
#![allow(unused_imports)]

mod attach;
mod cli;
mod tmux_compat;
mod workflow_fixture;

use std::collections::BTreeSet;
use std::error::Error;
use std::fs;
use std::fs::OpenOptions;
use std::io::{self, Read};
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use rmux_client::{
    connect_or_absent, default_socket_path, ClientError, ConnectResult, Connection,
    INTERNAL_DAEMON_FLAG,
};
use rmux_proto::{ListSessionsRequest, Response, TerminalSize};
use rustix::event::{poll, PollFd, PollFlags, Timespec};
use rustix::termios::{tcgetattr, tcgetwinsize, tcsetattr, OptionalActions, SpecialCodeIndex};

pub(crate) use attach::{
    drain_attach_output, drain_attach_output_bytes, read_until_contains, read_until_contains_all,
    AttachedSession,
};
pub(crate) use cli::{
    acquire_empty_socket_path_lock, assert_clap_failure, assert_success, stderr, stdout,
    CliHarness, DaemonGuard,
};
pub(crate) use tmux_compat::{
    CapturedCommand, EnvironmentOverrides, FrozenTmuxBinary, TmuxCompatHarness, TmuxCompatRun,
    TmuxCompatRunConfig, DEFAULT_FROZEN_TMUX_PATH, DEFAULT_TMUX_COMPAT_TERM, FROZEN_TMUX_ENV,
    FROZEN_TMUX_REFERENCE_REL_PATH, PTY_SERIALIZATION_NOTE, TMUX_COMPAT_PREREQUISITES_NOTE,
};
pub(crate) use workflow_fixture::{
    verify_fixture_coherence, WorkflowStep, CANONICAL_SESSION_WORKFLOW, EXPECTED_LABELS,
    WORKFLOW_COLORTERM_PRINT_COMMAND,
};

pub(crate) const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
pub(crate) const BINARY_OVERRIDE_ENV: &str = "RMUX_INTERNAL_BINARY_PATH";
pub(crate) const BINARY_OVERRIDE_TEST_OPT_IN_ENV: &str = "RMUX_ALLOW_INTERNAL_BINARY_OVERRIDE";

static UNIQUE_ID: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn unique_socket_path(label: &str) -> PathBuf {
    let unique_id = UNIQUE_ID.fetch_add(1, Ordering::Relaxed);
    PathBuf::from("/tmp")
        .join(format!(
            "rx-{}-{}-{unique_id}",
            compact_label(label),
            std::process::id()
        ))
        .join("s.sock")
}

pub(crate) fn unique_tmpdir(label: &str) -> PathBuf {
    let unique_id = UNIQUE_ID.fetch_add(1, Ordering::Relaxed);
    PathBuf::from("/tmp").join(format!(
        "rx-{}-{}-{unique_id}",
        compact_label(label),
        std::process::id()
    ))
}

pub(crate) fn default_socket_path_in(tmpdir: &Path) -> Result<PathBuf, Box<dyn Error>> {
    let _guard = env_lock().lock().expect("env lock");
    let previous = std::env::var_os("RMUX_TMPDIR");
    let _restore = EnvVarGuard::new("RMUX_TMPDIR", previous);
    std::env::set_var("RMUX_TMPDIR", tmpdir);
    Ok(default_socket_path()?)
}

pub(crate) fn wait_for_socket(socket_path: &Path, child: &mut Child) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + STARTUP_TIMEOUT;

    loop {
        match connect_or_absent(socket_path)? {
            ConnectResult::Connected(mut connection) => {
                match probe_hidden_daemon_readiness(&mut connection) {
                    Ok(()) => return Ok(()),
                    Err(error) if is_transient_hidden_daemon_readiness_error(&error) => {}
                    Err(error) => return Err(error.into()),
                }
            }
            ConnectResult::Absent => {}
        }

        if let Some(status) = child.try_wait()? {
            return Err(
                format!("hidden daemon exited before readiness with status {status}").into(),
            );
        }

        if Instant::now() >= deadline {
            return Err(format!(
                "timed out waiting for hidden daemon socket '{}'",
                socket_path.display()
            )
            .into());
        }

        std::thread::sleep(Duration::from_millis(50));
    }
}

fn probe_hidden_daemon_readiness(connection: &mut Connection) -> Result<(), ClientError> {
    let response = connection.list_sessions(ListSessionsRequest {
        format: None,
        filter: None,
        sort_order: None,
        reversed: false,
    })?;
    match response {
        Response::ListSessions(_) => Ok(()),
        other => Err(ClientError::Protocol(rmux_proto::RmuxError::Server(
            format!("unexpected readiness response: {other:?}"),
        ))),
    }
}

fn is_transient_hidden_daemon_readiness_error(error: &ClientError) -> bool {
    matches!(
        error,
        ClientError::Io(io_error)
            if matches!(
                io_error.kind(),
                io::ErrorKind::WouldBlock
                    | io::ErrorKind::Interrupted
                    | io::ErrorKind::TimedOut
            )
    )
}

pub(crate) fn terminate_child(child: &mut Child) -> Result<(), Box<dyn Error>> {
    match child.try_wait()? {
        Some(_) => Ok(()),
        None => {
            if let Some(socket_path) = internal_daemon_socket_path(child) {
                let _ = shutdown_rmux_server(&socket_path);
                if wait_for_child_exit(child, Duration::from_secs(2))? {
                    return Ok(());
                }
            }

            child.kill()?;
            let _ = child.wait()?;
            Ok(())
        }
    }
}

pub(crate) fn shutdown_rmux_server(socket_path: &Path) -> Result<(), Box<dyn Error>> {
    match connect_or_absent(socket_path) {
        Ok(ConnectResult::Absent) => {}
        Err(_) => {}
        Ok(ConnectResult::Connected(mut connection)) => {
            let _ = connection.kill_server();
            let _ = wait_for_socket_cleanup(socket_path, Duration::from_secs(2));
        }
    }
    if wait_for_daemon_process_exit(socket_path, Duration::from_secs(2)).is_err() {
        let _ = kill_daemon_processes(socket_path);
        let _ = wait_for_daemon_process_exit(socket_path, Duration::from_secs(2));
    }
    Ok(())
}

pub(crate) fn write_hidden_launcher(
    launcher_path: &Path,
    pid_path: &Path,
) -> Result<(), Box<dyn Error>> {
    let temp_launcher_path = launcher_path.with_extension("tmp");
    let _ = fs::remove_file(&temp_launcher_path);
    fs::write(
        &temp_launcher_path,
        format!(
            "#!/bin/sh\n'{}' \"$@\" >/dev/null 2>&1 &\necho $! > '{}'\nexit 0\n",
            env!("CARGO_BIN_EXE_rmux"),
            pid_path.display()
        ),
    )?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = fs::metadata(&temp_launcher_path)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&temp_launcher_path, permissions)?;
    }

    fs::rename(&temp_launcher_path, launcher_path)?;
    Ok(())
}

pub(crate) fn terminate_recorded_pid(pid_path: &Path) -> Result<(), Box<dyn Error>> {
    let pid = fs::read_to_string(pid_path)?
        .trim()
        .parse::<u32>()
        .map_err(|error| format!("invalid daemon pid in '{}': {error}", pid_path.display()))?;

    let status = Command::new("kill").arg(pid.to_string()).status()?;
    if !status.success() {
        return Err(format!("failed to terminate daemon pid {pid}: {status}").into());
    }

    Ok(())
}

pub(crate) fn wait_for_no_child_processes(
    parent_pid: u32,
    timeout: Duration,
) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + timeout;

    while Instant::now() < deadline {
        let states = child_process_states(parent_pid)?;
        if states.is_empty() {
            return Ok(());
        }

        assert!(
            states.iter().all(|state| !state.starts_with('Z')),
            "zombie child processes remained under daemon {parent_pid}: {states:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    let states = child_process_states(parent_pid)?;
    Err(
        format!("timed out waiting for daemon {parent_pid} to reap pane children: {states:?}")
            .into(),
    )
}

pub(crate) fn child_process_states(parent_pid: u32) -> Result<Vec<String>, Box<dyn Error>> {
    let output = Command::new("ps").args(["-axo", "ppid=,stat="]).output()?;

    if !output.status.success() {
        if output.stdout.is_empty() && output.stderr.is_empty() {
            return Ok(Vec::new());
        }
        return Err(format!(
            "ps failed for daemon {parent_pid}: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }

    Ok(parse_child_process_states(
        parent_pid,
        &String::from_utf8_lossy(&output.stdout),
    ))
}

fn parse_child_process_states(parent_pid: u32, ps_output: &str) -> Vec<String> {
    ps_output
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let ppid = fields.next()?.parse::<u32>().ok()?;
            let state = fields.next()?;
            (ppid == parent_pid).then(|| state.to_owned())
        })
        .collect()
}

pub(crate) fn assert_only_default_socket(socket_path: &Path) -> Result<(), Box<dyn Error>> {
    let mut entries = socket_directory_entries(socket_path)?;
    entries.sort();
    assert_eq!(entries, vec!["default".to_owned()]);
    Ok(())
}

pub(crate) fn assert_socket_directory_empty(socket_path: &Path) -> Result<(), Box<dyn Error>> {
    let entries = socket_directory_entries(socket_path)?;
    assert!(
        entries.is_empty(),
        "expected an empty socket directory after shutdown, found {entries:?}"
    );
    Ok(())
}

pub(crate) fn pane_tty_paths() -> Result<BTreeSet<PathBuf>, Box<dyn Error>> {
    let mut paths = BTreeSet::new();

    for pid in pane_child_pids()? {
        let Some(target) = stdin_tty_path(pid)? else {
            continue;
        };
        if is_pty_device(&target) {
            paths.insert(target);
        }
    }

    Ok(paths)
}

pub(crate) fn pane_child_pids() -> Result<BTreeSet<u32>, Box<dyn Error>> {
    #[cfg(target_os = "linux")]
    {
        linux_pane_child_pids()
    }

    #[cfg(target_os = "macos")]
    {
        macos_pane_child_pids()
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Ok(BTreeSet::new())
    }
}

#[cfg(target_os = "linux")]
fn linux_pane_child_pids() -> Result<BTreeSet<u32>, Box<dyn Error>> {
    let pids = linux_thread_child_pids()?;
    if !pids.is_empty() {
        return Ok(pids);
    }

    linux_proc_ppid_child_pids(std::process::id())
}

#[cfg(target_os = "linux")]
fn linux_thread_child_pids() -> Result<BTreeSet<u32>, Box<dyn Error>> {
    let task_directory = format!("/proc/{}/task", std::process::id());
    let tasks = match fs::read_dir(task_directory) {
        Ok(tasks) => tasks,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Err(error) => return Err(error.into()),
    };

    let mut pids = BTreeSet::new();

    for task in tasks {
        let task = task?;
        let children = match fs::read_to_string(task.path().join("children")) {
            Ok(children) => children,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };

        for pid in children.split_whitespace() {
            pids.insert(pid.parse()?);
        }
    }

    Ok(pids)
}

#[cfg(target_os = "linux")]
fn linux_proc_ppid_child_pids(parent_pid: u32) -> Result<BTreeSet<u32>, Box<dyn Error>> {
    let mut pids = BTreeSet::new();

    for entry in fs::read_dir("/proc")? {
        let entry = entry?;
        let file_name = entry.file_name();
        let Some(pid) = file_name.to_string_lossy().parse::<u32>().ok() else {
            continue;
        };
        if pid == parent_pid {
            continue;
        }

        let stat = match fs::read_to_string(entry.path().join("stat")) {
            Ok(stat) => stat,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
                ) =>
            {
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        let Some(fields) = stat.rsplit_once(") ").map(|(_, fields)| fields) else {
            continue;
        };
        let mut fields = fields.split_whitespace();
        let _state = fields.next();
        let Some(ppid) = fields.next().and_then(|field| field.parse::<u32>().ok()) else {
            continue;
        };
        if ppid == parent_pid {
            pids.insert(pid);
        }
    }

    Ok(pids)
}

#[cfg(target_os = "macos")]
fn macos_pane_child_pids() -> Result<BTreeSet<u32>, Box<dyn Error>> {
    let output = Command::new("ps").args(["-axo", "pid=,ppid="]).output()?;
    if !output.status.success() {
        return Err(format!(
            "ps failed while listing pane children: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }

    let parent_pid = std::process::id();
    let mut pids = BTreeSet::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let mut fields = line.split_whitespace();
        let Some(pid) = fields.next().and_then(|field| field.parse::<u32>().ok()) else {
            continue;
        };
        let Some(ppid) = fields.next().and_then(|field| field.parse::<u32>().ok()) else {
            continue;
        };
        if ppid == parent_pid {
            pids.insert(pid);
        }
    }

    Ok(pids)
}

#[cfg(target_os = "linux")]
fn stdin_tty_path(pid: u32) -> Result<Option<PathBuf>, Box<dyn Error>> {
    match fs::read_link(format!("/proc/{pid}/fd/0")) {
        Ok(target) => Ok(Some(target)),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(error.into()),
    }
}

#[cfg(target_os = "macos")]
fn stdin_tty_path(pid: u32) -> Result<Option<PathBuf>, Box<dyn Error>> {
    let output = Command::new("lsof")
        .args(["-a", "-p", &pid.to_string(), "-d", "0", "-Fn"])
        .output()?;
    if !output.status.success() {
        return Ok(None);
    }

    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| line.strip_prefix('n').map(PathBuf::from)))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn stdin_tty_path(_pid: u32) -> Result<Option<PathBuf>, Box<dyn Error>> {
    Ok(None)
}

pub(crate) fn tty_size(path: &Path) -> Result<TerminalSize, Box<dyn Error>> {
    let file = fs::File::open(path)?;
    let winsize = tcgetwinsize(&file)?;

    Ok(TerminalSize {
        cols: winsize.ws_col,
        rows: winsize.ws_row,
    })
}

pub(crate) fn read_tty_exact(
    path: &Path,
    len: usize,
    timeout: Duration,
) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut file = OpenOptions::new().read(true).write(true).open(path)?;
    let original_termios = tcgetattr(&file)?;
    let mut raw_termios = original_termios.clone();
    raw_termios.make_raw();
    raw_termios.special_codes[SpecialCodeIndex::VMIN] = 1;
    raw_termios.special_codes[SpecialCodeIndex::VTIME] = 0;
    tcsetattr(&file, OptionalActions::Now, &raw_termios)?;

    let result: Result<Vec<u8>, Box<dyn Error>> = (|| {
        let mut fds = [PollFd::new(
            &file,
            PollFlags::IN | PollFlags::ERR | PollFlags::HUP,
        )];
        let timeout = Timespec {
            tv_sec: timeout.as_secs() as i64,
            tv_nsec: timeout.subsec_nanos() as i64,
        };
        let ready = poll(&mut fds, Some(&timeout))?;
        if ready == 0 || fds[0].revents().is_empty() {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "tty read timed out").into());
        }

        let mut buffer = vec![0; len];
        file.read_exact(&mut buffer)?;
        Ok(buffer)
    })();

    let _ = tcsetattr(&file, OptionalActions::Now, &original_termios);
    result
}

pub(crate) fn tty_has_input(path: &Path, timeout: Duration) -> Result<bool, Box<dyn Error>> {
    let file = OpenOptions::new().read(true).write(true).open(path)?;
    let original_termios = tcgetattr(&file)?;
    let mut raw_termios = original_termios.clone();
    raw_termios.make_raw();
    raw_termios.special_codes[SpecialCodeIndex::VMIN] = 1;
    raw_termios.special_codes[SpecialCodeIndex::VTIME] = 0;
    tcsetattr(&file, OptionalActions::Now, &raw_termios)?;

    let result: Result<bool, Box<dyn Error>> = (|| {
        let mut fds = [PollFd::new(
            &file,
            PollFlags::IN | PollFlags::ERR | PollFlags::HUP,
        )];
        let timeout = Timespec {
            tv_sec: timeout.as_secs() as i64,
            tv_nsec: timeout.subsec_nanos() as i64,
        };
        let ready = poll(&mut fds, Some(&timeout))?;

        Ok(ready != 0 && !fds[0].revents().is_empty())
    })();

    let _ = tcsetattr(&file, OptionalActions::Now, &original_termios);
    result
}

pub(crate) fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

pub(crate) struct AutoStartCleanup {
    socket_path: PathBuf,
    pid_path: PathBuf,
}

impl AutoStartCleanup {
    pub(crate) fn new(socket_path: PathBuf, pid_path: PathBuf) -> Self {
        Self {
            socket_path,
            pid_path,
        }
    }
}

impl Drop for AutoStartCleanup {
    fn drop(&mut self) {
        let _ = shutdown_rmux_server(&self.socket_path);
        let _ = terminate_recorded_pid(&self.pid_path);
        let _ = fs::remove_file(&self.socket_path);
        if let Some(parent) = self.socket_path.parent() {
            let _ = fs::remove_dir_all(parent);
        }
    }
}

pub(crate) struct EnvVarGuard {
    name: &'static str,
    previous_value: Option<std::ffi::OsString>,
}

impl EnvVarGuard {
    pub(crate) fn new(name: &'static str, previous_value: Option<std::ffi::OsString>) -> Self {
        Self {
            name,
            previous_value,
        }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match self.previous_value.as_ref() {
            Some(value) => std::env::set_var(self.name, value),
            None => std::env::remove_var(self.name),
        }
    }
}

fn compact_label(label: &str) -> String {
    let compact: String = label
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .take(8)
        .collect();

    if compact.is_empty() {
        "rmux".to_owned()
    } else {
        compact
    }
}

fn internal_daemon_socket_path(child: &Child) -> Option<PathBuf> {
    let cmdline = fs::read(format!("/proc/{}/cmdline", child.id())).ok()?;
    let arguments = cmdline
        .split(|byte| *byte == 0)
        .filter(|value| !value.is_empty())
        .map(|value| std::ffi::OsString::from_vec(value.to_vec()))
        .collect::<Vec<_>>();

    let mut arguments = arguments.into_iter();
    while let Some(argument) = arguments.next() {
        if argument == INTERNAL_DAEMON_FLAG {
            return arguments.next().map(PathBuf::from);
        }
    }

    None
}

fn wait_for_child_exit(child: &mut Child, timeout: Duration) -> Result<bool, Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    loop {
        if child.try_wait()?.is_some() {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn wait_for_socket_cleanup(socket_path: &Path, timeout: Duration) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    while socket_path.exists() {
        if Instant::now() >= deadline {
            return Err(format!(
                "timed out waiting for daemon socket '{}' to disappear",
                socket_path.display()
            )
            .into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

fn wait_for_daemon_process_exit(
    socket_path: &Path,
    timeout: Duration,
) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    while daemon_process_exists(socket_path)? {
        if Instant::now() >= deadline {
            return Err(format!(
                "timed out waiting for daemon process using '{}' to exit",
                socket_path.display()
            )
            .into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

fn daemon_process_exists(socket_path: &Path) -> Result<bool, Box<dyn Error>> {
    Ok(!daemon_processes(socket_path)?.is_empty())
}

fn kill_daemon_processes(socket_path: &Path) -> Result<(), Box<dyn Error>> {
    for pid in daemon_processes(socket_path)? {
        let _ = Command::new("kill").arg("-9").arg(pid.to_string()).status();
    }
    Ok(())
}

fn daemon_processes(socket_path: &Path) -> Result<Vec<u32>, Box<dyn Error>> {
    let needle = socket_path.as_os_str().as_encoded_bytes();
    let mut pids = Vec::new();
    for entry in fs::read_dir("/proc")? {
        let entry = entry?;
        let file_name = entry.file_name();
        if !file_name.as_encoded_bytes().iter().all(u8::is_ascii_digit) {
            continue;
        }
        let Some(pid) = file_name.to_string_lossy().parse::<u32>().ok() else {
            continue;
        };
        if pid == std::process::id() {
            continue;
        }
        let cmdline = match fs::read(entry.path().join("cmdline")) {
            Ok(cmdline) => cmdline,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
                ) =>
            {
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        if cmdline.split(|byte| *byte == 0).any(|arg| arg == needle) {
            pids.push(pid);
        }
    }
    Ok(pids)
}

fn socket_directory_entries(socket_path: &Path) -> Result<Vec<String>, Box<dyn Error>> {
    let parent = socket_path.parent().expect("socket parent");
    if !parent.exists() {
        return Ok(Vec::new());
    }

    fs::read_dir(parent)?
        .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn is_pty_device(path: &Path) -> bool {
    let Some(parent) = path.parent() else {
        return false;
    };
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };

    if parent == Path::new("/dev/pts") {
        return name.chars().all(|character| character.is_ascii_digit());
    }

    parent == Path::new("/dev")
        && name.strip_prefix("ttys").is_some_and(|suffix| {
            !suffix.is_empty() && suffix.chars().all(|ch| ch.is_ascii_hexdigit())
        })
}
