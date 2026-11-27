use std::collections::hash_map::DefaultHasher;
use std::error::Error;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use rmux_client::INTERNAL_DAEMON_FLAG;

use crate::common::{
    default_socket_path_in, shutdown_rmux_server, terminate_child, tmux_shim_directories,
    unique_tmpdir, wait_for_socket, write_hidden_launcher, AutoStartCleanup, BINARY_OVERRIDE_ENV,
    BINARY_OVERRIDE_TEST_OPT_IN_ENV, DAEMON_SHUTDOWN_GRACE,
};

const TEST_SHELL_STARTUP: &str = "export PS1='tester@RMUXHOST:~$ '\nexport PROMPT=\"$PS1\"\n";

type CliHarnessLock = MutexGuard<'static, ()>;

/// How long a CLI command keeps the cross-process command lock before it is
/// treated as a long wait. Startup, including any daemon the command
/// auto-starts, stays serialized; a wait on a running daemon does not.
pub(crate) const CLI_COMMAND_LOCK_LONG_WAIT: Duration = Duration::from_millis(250);

#[path = "cli_command_window.rs"]
mod command_window;

#[cfg(target_os = "linux")]
#[path = "empty_socket_lock.rs"]
mod empty_socket_lock;
#[cfg(target_os = "linux")]
pub(crate) use empty_socket_lock::{acquire_empty_socket_path_lock, EmptySocketPathLock};

pub(crate) struct CliHarness {
    _harness_lock: CliHarnessLock,
    tmpdir: PathBuf,
    runtime_dir: PathBuf,
    socket_path: PathBuf,
    launcher_path: PathBuf,
    pid_path: PathBuf,
}

#[cfg(not(target_os = "linux"))]
pub(crate) struct EmptySocketPathLock {
    path: PathBuf,
}

#[cfg(not(target_os = "linux"))]
impl Drop for EmptySocketPathLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(cli_command_lock_owner_path(&self.path));
        let _ = fs::remove_dir(&self.path);
    }
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn acquire_empty_socket_path_lock() -> Result<EmptySocketPathLock, Box<dyn Error>> {
    let path = empty_socket_path_lock_path();
    let deadline = Instant::now() + Duration::from_secs(120);

    loop {
        match fs::create_dir(&path) {
            Ok(()) => {
                if let Err(error) = record_cli_command_lock_owner(&path) {
                    if error.kind() == io::ErrorKind::NotFound {
                        let _ = fs::remove_dir(&path);
                        continue;
                    }
                    let _ = fs::remove_dir(&path);
                    return Err(format!(
                        "failed to record empty -S lock owner '{}': {error}",
                        path.display()
                    )
                    .into());
                }
                return Ok(EmptySocketPathLock { path });
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if clear_stale_cli_command_lock(&path)? {
                    continue;
                }
                if Instant::now() >= deadline {
                    return Err(format!(
                        "timed out waiting for empty -S lock '{}'",
                        path.display()
                    )
                    .into());
                }
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => {
                return Err(format!(
                    "failed to acquire empty -S lock '{}': {error}",
                    path.display()
                )
                .into());
            }
        }
    }
}

impl CliHarness {
    pub(crate) fn new(label: &str) -> Result<Self, Box<dyn Error>> {
        let harness_lock = acquire_cli_harness_lock();
        let tmpdir = unique_tmpdir(label);
        fs::create_dir_all(&tmpdir)?;
        #[cfg(unix)]
        fs::set_permissions(&tmpdir, fs::Permissions::from_mode(0o711))?;
        write_test_shell_startup_files(&tmpdir.join("home"))?;
        // Every daemon of this harness publishes its tmux shim here instead of
        // the host's XDG_RUNTIME_DIR, so Drop can see a shim the daemon left.
        let runtime_dir = tmpdir.join("runtime");
        fs::create_dir(&runtime_dir)?;
        #[cfg(unix)]
        fs::set_permissions(&runtime_dir, fs::Permissions::from_mode(0o700))?;
        let socket_path = default_socket_path_in(&tmpdir)?;
        let launcher_path = tmpdir.join("rmux-launcher.sh");
        let pid_path = tmpdir.join("rmux.pid");

        Ok(Self {
            _harness_lock: harness_lock,
            tmpdir,
            runtime_dir,
            socket_path,
            launcher_path,
            pid_path,
        })
    }

    pub(crate) fn run(&self, args: &[&str]) -> Result<Output, Box<dyn Error>> {
        self.run_with(args, |_| {})
    }

    pub(crate) fn run_with<F>(&self, args: &[&str], configure: F) -> Result<Output, Box<dyn Error>>
    where
        F: FnOnce(&mut Command),
    {
        let mut command = self.base_command();
        command.args(args);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        configure(&mut command);
        Ok(output_under_cli_command_lock(command, &self.socket_path)?)
    }

    pub(crate) async fn run_async(&self, args: &[&str]) -> Result<Output, Box<dyn Error>> {
        self.run_async_with(args, |_| {}).await
    }

    pub(crate) async fn run_async_with<F>(
        &self,
        args: &[&str],
        configure: F,
    ) -> Result<Output, Box<dyn Error>>
    where
        F: FnOnce(&mut Command),
    {
        // Build the identity-guarded command while the harness still belongs
        // to the runtime thread; only owned blocking work crosses this boundary.
        let mut command = self.base_command();
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        configure(&mut command);
        let socket_path = self.socket_path.clone();
        Ok(tokio::task::spawn_blocking(move || {
            output_under_cli_command_lock(command, &socket_path)
        })
        .await??)
    }

    pub(crate) fn start_hidden_daemon(&self) -> Result<DaemonGuard, Box<dyn Error>> {
        let _lock = acquire_cli_command_lock()?;
        let mut child = self.hidden_daemon_command().spawn()?;

        wait_for_socket(&self.socket_path, &mut child)?;
        Ok(DaemonGuard { child })
    }

    /// Builds the hidden daemon of this harness, bound to its private socket.
    ///
    /// The path is passed as the auto-start client passes it. A daemon left
    /// to resolve `RMUX_TMPDIR` itself falls back to the user's default socket
    /// under `/tmp` once this harness has removed its directory.
    pub(crate) fn hidden_daemon_command(&self) -> Command {
        let mut command = self.base_command();
        command
            .arg(INTERNAL_DAEMON_FLAG)
            .arg(&self.socket_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command
    }

    pub(crate) fn auto_start_cleanup(&self) -> Result<AutoStartCleanup, Box<dyn Error>> {
        write_hidden_launcher(&self.launcher_path, &self.pid_path)?;
        Ok(AutoStartCleanup::new(
            self.socket_path.clone(),
            self.pid_path.clone(),
        ))
    }

    pub(crate) fn base_command(&self) -> Command {
        let binary =
            rmux_os::build_identity::resolve(std::ffi::OsStr::new(env!("CARGO_BIN_EXE_rmux")))
                .expect("candidate executable identity");
        let mut command = Command::new(binary);
        command.env("RMUX_TMPDIR", &self.tmpdir);
        command.env("HOME", self.tmpdir.join("home"));
        command.env("XDG_CONFIG_HOME", self.tmpdir.join("xdg"));
        command.env("XDG_RUNTIME_DIR", &self.runtime_dir);
        command.env("RMUX_DISABLE_TMUX_FALLBACK", "1");
        command.env(BINARY_OVERRIDE_TEST_OPT_IN_ENV, "1");
        command.env_remove(BINARY_OVERRIDE_ENV);
        command.env_remove("RMUX");
        command.env_remove("TMUX");
        command.env_remove("RMUX_PANE");
        command.env_remove("TMUX_PANE");
        command
    }

    pub(crate) fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    pub(crate) fn pid_path(&self) -> &Path {
        &self.pid_path
    }

    pub(crate) fn launcher_path(&self) -> &Path {
        &self.launcher_path
    }

    pub(crate) fn tmpdir(&self) -> &Path {
        &self.tmpdir
    }

    pub(crate) fn runtime_dir(&self) -> &Path {
        &self.runtime_dir
    }
}

fn acquire_cli_harness_lock() -> CliHarnessLock {
    static CLI_HARNESS_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    CLI_HARNESS_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn write_test_shell_startup_files(home: &Path) -> Result<(), Box<dyn Error>> {
    fs::create_dir_all(home)?;
    fs::write(home.join(".hushlogin"), "")?;
    for file_name in [
        ".bash_profile",
        ".bashrc",
        ".profile",
        ".zprofile",
        ".zshrc",
    ] {
        fs::write(home.join(file_name), TEST_SHELL_STARTUP)?;
    }
    Ok(())
}

impl Drop for CliHarness {
    fn drop(&mut self) {
        let _ = shutdown_rmux_server(&self.socket_path);
        let _ = fs::remove_file(&self.socket_path);
        // Only the daemon's own shutdown removes its shim, and it does so after
        // unlinking the socket: allow that tail to finish, then fail the test.
        let leaked_shims = remaining_tmux_shims(&self.runtime_dir, DAEMON_SHUTDOWN_GRACE);
        let _ = fs::remove_dir_all(&self.tmpdir);
        if !leaked_shims.is_empty() && !thread::panicking() {
            panic!(
                "a daemon of this CliHarness exited without removing its tmux shim, left {leaked_shims:?} in '{}'",
                self.runtime_dir.display()
            );
        }
    }
}

fn remaining_tmux_shims(runtime_dir: &Path, timeout: Duration) -> Vec<String> {
    let deadline = Instant::now() + timeout;
    loop {
        let shims = tmux_shim_directories(runtime_dir).unwrap_or_default();
        if shims.is_empty() || Instant::now() >= deadline {
            return shims;
        }
        thread::sleep(Duration::from_millis(20));
    }
}

pub(crate) struct DaemonGuard {
    child: Child,
}

impl DaemonGuard {
    pub(crate) fn child_mut(&mut self) -> &mut Child {
        &mut self.child
    }

    pub(crate) fn pid(&self) -> u32 {
        self.child.id()
    }
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = terminate_child(&mut self.child);
    }
}

struct CliCommandLock {
    path: PathBuf,
}

impl Drop for CliCommandLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(cli_command_lock_owner_path(&self.path));
        let _ = fs::remove_dir(&self.path);
    }
}

/// Runs `command` like [`Command::output`] while holding the cross-process
/// CLI command lock through its startup. Once the command has run for
/// [`CLI_COMMAND_LOCK_LONG_WAIT`] and `daemon_socket` exists, it is only
/// waiting on that daemon, so the lock is released for the rest of the wait.
/// A command whose daemon socket never appears keeps the lock until it exits.
pub(crate) fn output_under_cli_command_lock(
    mut command: Command,
    daemon_socket: &Path,
) -> io::Result<Output> {
    let lock = acquire_cli_command_lock().map_err(|error| io::Error::other(error.to_string()))?;
    let child = command.spawn()?;
    let (exited, exit_signal) = mpsc::channel::<()>();
    let daemon_socket = daemon_socket.to_owned();
    let releaser = thread::spawn(move || {
        hold_cli_command_lock_until_long_wait(lock, &exit_signal, &daemon_socket)
    });
    let output = child.wait_with_output();
    drop(exited);
    releaser
        .join()
        .map_err(|_| io::Error::other("CLI command lock releaser panicked"))?;
    output
}

fn hold_cli_command_lock_until_long_wait(
    lock: CliCommandLock,
    exit_signal: &Receiver<()>,
    daemon_socket: &Path,
) {
    let mut timeout = CLI_COMMAND_LOCK_LONG_WAIT;
    loop {
        match exit_signal.recv_timeout(timeout) {
            Err(RecvTimeoutError::Timeout) if daemon_socket.exists() => break,
            Err(RecvTimeoutError::Timeout) => timeout = Duration::from_millis(20),
            Ok(()) | Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    drop(lock);
}

/// The process currently holding the cross-process CLI command lock, if any.
pub(crate) fn cli_command_lock_owner() -> Option<u32> {
    let owner_path = cli_command_lock_owner_path(&cli_command_lock_path());
    fs::read_to_string(owner_path).ok()?.trim().parse().ok()
}

/// Waits longer than this are reported on stderr: a test that is slow but
/// green otherwise gives no hint that the shared lock ate its seconds.
const CLI_COMMAND_LOCK_REPORTED_WAIT: Duration = Duration::from_millis(500);

fn acquire_cli_command_lock() -> Result<CliCommandLock, Box<dyn Error>> {
    let started = Instant::now();
    let holder_at_start = cli_command_lock_owner();
    let lock = wait_for_cli_command_lock()?;
    let waited = started.elapsed();
    if waited > CLI_COMMAND_LOCK_REPORTED_WAIT {
        eprintln!(
            "STEP CLI command lock waited {:.3}s (held at start by pid {holder_at_start:?})",
            waited.as_secs_f64()
        );
    }
    Ok(lock)
}

fn wait_for_cli_command_lock() -> Result<CliCommandLock, Box<dyn Error>> {
    let path = cli_command_lock_path();
    let deadline = Instant::now() + Duration::from_secs(120);

    loop {
        match fs::create_dir(&path) {
            Ok(()) => {
                if let Err(error) = record_cli_command_lock_owner(&path) {
                    if error.kind() == io::ErrorKind::NotFound {
                        let _ = fs::remove_dir(&path);
                        continue;
                    }
                    let _ = fs::remove_dir(&path);
                    return Err(format!(
                        "failed to record CLI command lock owner '{}': {error}",
                        path.display()
                    )
                    .into());
                }
                return Ok(CliCommandLock { path });
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if clear_stale_cli_command_lock(&path)? {
                    continue;
                }
                if Instant::now() >= deadline {
                    return Err(format!(
                        "timed out waiting for CLI command lock '{}'",
                        path.display()
                    )
                    .into());
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => {
                return Err(format!(
                    "failed to acquire CLI command lock '{}': {error}",
                    path.display()
                )
                .into());
            }
        }
    }
}

fn cli_command_lock_path() -> PathBuf {
    let scope = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("rmux"));
    let scope = scope.canonicalize().unwrap_or(scope);
    let mut hasher = DefaultHasher::new();
    scope.hash(&mut hasher);
    std::env::temp_dir().join(format!("rmux-cli-command-{:016x}.lock", hasher.finish()))
}

#[cfg(not(target_os = "linux"))]
fn empty_socket_path_lock_path() -> PathBuf {
    let scope = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("rmux"));
    let scope = scope.canonicalize().unwrap_or(scope);
    let mut hasher = DefaultHasher::new();
    scope.hash(&mut hasher);
    std::env::temp_dir().join(format!("rmux-empty-socket-{:016x}.lock", hasher.finish()))
}

fn record_cli_command_lock_owner(path: &Path) -> io::Result<()> {
    let owner_path = cli_command_lock_owner_path(path);
    let owner = std::process::id().to_string();
    let mut last_error = None;
    for _ in 0..5 {
        match fs::write(&owner_path, &owner) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Err(error),
            Err(error) => {
                last_error = Some(error);
                thread::sleep(Duration::from_millis(5));
            }
        }
    }
    Err(last_error.unwrap_or_else(|| io::Error::other("failed to write lock owner")))
}

fn cli_command_lock_owner_path(path: &Path) -> PathBuf {
    path.join("owner.pid")
}

fn clear_stale_cli_command_lock(path: &Path) -> Result<bool, Box<dyn Error>> {
    let owner_path = cli_command_lock_owner_path(path);
    let owner_pid = match fs::read_to_string(&owner_path) {
        Ok(owner_pid) => Some(owner_pid),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(format!(
                "failed to inspect CLI command lock owner '{}': {error}",
                owner_path.display()
            )
            .into())
        }
    };

    match owner_pid {
        Some(owner_pid) => {
            let owner_pid = owner_pid.trim();
            let parsed = owner_pid.parse::<u32>().ok();
            if let Some(owner_pid) = parsed {
                if process_id_exists(owner_pid) {
                    return Ok(false);
                }
            } else if !lock_dir_is_stale(path)? {
                return Ok(false);
            }

            let _ = fs::remove_file(&owner_path);
            match fs::remove_dir(path) {
                Ok(()) => Ok(true),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(true),
                Err(error) if error.kind() == io::ErrorKind::DirectoryNotEmpty => Ok(false),
                Err(error) => Err(format!(
                    "failed to clear stale CLI command lock '{}': {error}",
                    path.display()
                )
                .into()),
            }
        }
        None => {
            if !lock_dir_is_stale(path)? {
                return Ok(false);
            }

            match fs::remove_dir(path) {
                Ok(()) => Ok(true),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(true),
                Err(error) if error.kind() == io::ErrorKind::DirectoryNotEmpty => Ok(false),
                Err(error) => Err(format!(
                    "failed to clear stale CLI command lock '{}': {error}",
                    path.display()
                )
                .into()),
            }
        }
    }
}

fn process_id_exists(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };

    // SAFETY: `kill(pid, 0)` does not send a signal; it only asks the kernel
    // whether the process exists and whether this process may signal it.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }

    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

fn lock_dir_is_stale(path: &Path) -> Result<bool, Box<dyn Error>> {
    let modified = match fs::metadata(path).and_then(|metadata| metadata.modified()) {
        Ok(modified) => modified,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(true),
        Err(error) => {
            return Err(format!(
                "failed to inspect CLI command lock '{}': {error}",
                path.display()
            )
            .into())
        }
    };
    Ok(modified.elapsed().unwrap_or_default() >= Duration::from_secs(2))
}

/// Stderr fragments that mean a CLI command failed only because the daemon
/// did not answer within the client's detached RPC deadline.
const DAEMON_STALL_MARKERS: [&str; 3] = [
    "waiting for the rmux server response",
    "sending a request to the rmux server",
    // Raw EAGAIN text, for any Unix path that still reports it unnamed.
    "Resource temporarily unavailable",
];

/// Whether `stderr` reports a daemon that stalled past the client's RPC
/// deadline, a transient condition of loaded test hosts worth retrying.
pub(crate) fn is_transient_daemon_stall(stderr: &str) -> bool {
    DAEMON_STALL_MARKERS
        .iter()
        .any(|marker| stderr.contains(marker))
}

/// Runs a silent CLI command, retrying up to three attempts while it fails
/// only with [`is_transient_daemon_stall`], and asserts its final success.
pub(crate) fn run_success_with_transient_retry(
    harness: &CliHarness,
    args: &[&str],
) -> Result<(), Box<dyn Error>> {
    let mut last_output = None;

    for _ in 0..3 {
        let output = harness.run(args)?;
        if output.status.code() == Some(0)
            && stdout(&output).is_empty()
            && stderr(&output).is_empty()
        {
            return Ok(());
        }

        let retryable = is_transient_daemon_stall(&stderr(&output));
        last_output = Some(output);
        if !retryable {
            break;
        }

        thread::sleep(Duration::from_millis(50));
    }

    let output = last_output.as_ref().expect("at least one command attempt");
    assert_eq!(
        output.status.code(),
        Some(0),
        "expected successful command {:?}, got status {:?}\nstdout:\n{}\nstderr:\n{}",
        args,
        output.status,
        stdout(output),
        stderr(output)
    );
    assert!(stdout(output).is_empty(), "stdout should be empty");
    assert!(stderr(output).is_empty(), "stderr should be empty");
    Ok(())
}

#[track_caller]
pub(crate) fn assert_success(output: &Output) {
    assert_eq!(
        output.status.code(),
        Some(0),
        "expected successful command, got status {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        stdout(output),
        stderr(output)
    );
    assert!(stdout(output).is_empty(), "stdout should be empty");
    assert!(stderr(output).is_empty(), "stderr should be empty");
}

pub(crate) fn assert_clap_failure(output: &Output) {
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stdout(output).is_empty(),
        "clap errors must not produce stdout"
    );
    assert!(
        !stderr(output).is_empty(),
        "clap errors must produce stderr"
    );
}

pub(crate) fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

pub(crate) fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}
