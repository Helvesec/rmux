use std::ffi::OsString;
use std::future::Future;
use std::path::PathBuf;
use std::process::{Command as StdCommand, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use rmux_os::process_tree::ProcessTreeChild;
use rmux_proto::{RmuxError, DEFAULT_MAX_FRAME_LENGTH};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{ChildStderr, ChildStdout};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use super::super::shell_processes::{
    ShellProcessGuard, ShellProcessRegistrationError, ShellProcessRegistry,
};
use crate::terminal::TerminalProfile;

const RUN_SHELL_TIMEOUT: Duration = Duration::from_secs(300);
const RUN_SHELL_PIPE_DRAIN_GRACE: Duration = Duration::from_millis(250);
const RUN_SHELL_PIPE_FINISH_TIMEOUT: Duration = Duration::from_secs(2);
const RUN_SHELL_TERMINATE_REAP_TIMEOUT: Duration = Duration::from_secs(2);
const RUN_SHELL_TRUNCATION_MARKER: &[u8] = b"\nrmux: run-shell output truncated\n";
const RUN_SHELL_OUTPUT_LIMIT: usize = DEFAULT_MAX_FRAME_LENGTH - 64 * 1024;
const MAX_BACKGROUND_TASKS: usize = 1024;
// Queued scripting composes task-local and recursive command futures; keep headroom beyond the
// 2 MiB default thread stack used by supported Windows targets.
const BACKGROUND_TASK_STACK_SIZE: usize = 8 * 1024 * 1024;

static BACKGROUND_TASK_LIMITER: OnceLock<BackgroundTaskLimiter> = OnceLock::new();

pub(in super::super) fn spawn_background_async<Fut, Factory>(
    thread_name: &'static str,
    factory: Factory,
) -> Result<(), RmuxError>
where
    Factory: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = ()> + 'static,
{
    let permit = background_task_limiter().try_acquire()?;
    if std::thread::Builder::new()
        .name(thread_name.to_owned())
        .stack_size(BACKGROUND_TASK_STACK_SIZE)
        .spawn(move || {
            let _permit = permit;
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            runtime.block_on(factory());
        })
        .is_err()
    {
        return Err(RmuxError::Server(format!(
            "failed to spawn background task '{thread_name}'"
        )));
    }
    Ok(())
}

fn background_task_limiter() -> &'static BackgroundTaskLimiter {
    BACKGROUND_TASK_LIMITER.get_or_init(|| BackgroundTaskLimiter::new(MAX_BACKGROUND_TASKS))
}

struct BackgroundTaskLimiter {
    semaphore: Arc<Semaphore>,
    max_tasks: usize,
}

impl BackgroundTaskLimiter {
    fn new(max_tasks: usize) -> Self {
        Self {
            semaphore: Arc::new(Semaphore::new(max_tasks)),
            max_tasks,
        }
    }

    fn try_acquire(&self) -> Result<OwnedSemaphorePermit, RmuxError> {
        self.semaphore.clone().try_acquire_owned().map_err(|_| {
            RmuxError::Server(format!(
                "too many background tasks; limit is {}",
                self.max_tasks
            ))
        })
    }
}

pub(super) async fn run_shell_foreground(
    command: String,
    profile: &TerminalProfile,
    show_stderr: bool,
    shell_processes: Option<Arc<ShellProcessRegistry>>,
) -> Result<Output, RmuxError> {
    run_shell_foreground_with_timeout(
        command,
        profile,
        show_stderr,
        RUN_SHELL_TIMEOUT,
        shell_processes,
    )
    .await
}

async fn run_shell_foreground_with_timeout(
    command: String,
    profile: &TerminalProfile,
    show_stderr: bool,
    timeout: Duration,
    shell_processes: Option<Arc<ShellProcessRegistry>>,
) -> Result<Output, RmuxError> {
    let profile = ShellTaskProfile::from_terminal_profile(profile);
    run_shell_foreground_async(command, profile, show_stderr, timeout, shell_processes).await
}

pub(super) async fn shell_condition_is_true(
    command: String,
    profile: &TerminalProfile,
    shell_processes: Option<Arc<ShellProcessRegistry>>,
) -> Result<bool, RmuxError> {
    shell_condition_is_true_with_timeout(command, profile, RUN_SHELL_TIMEOUT, shell_processes).await
}

async fn shell_condition_is_true_with_timeout(
    command: String,
    profile: &TerminalProfile,
    timeout: Duration,
    shell_processes: Option<Arc<ShellProcessRegistry>>,
) -> Result<bool, RmuxError> {
    #[cfg(windows)]
    match command.trim() {
        "true" => return Ok(true),
        "false" => return Ok(false),
        _ => {}
    }

    let profile = ShellTaskProfile::from_terminal_profile(profile);
    shell_condition_is_true_async(command, profile, timeout, shell_processes).await
}

struct ShellTaskProfile {
    shell: PathBuf,
    cwd: PathBuf,
    raw_environment: Vec<(OsString, OsString)>,
}

impl ShellTaskProfile {
    fn from_terminal_profile(profile: &TerminalProfile) -> Self {
        Self {
            shell: tmux_job_shell(profile),
            cwd: profile.cwd().to_path_buf(),
            raw_environment: profile
                .raw_environment()
                .map(|(name, value)| (name.to_owned(), value.to_owned()))
                .collect(),
        }
    }

    fn shell_command(&self, command: &str) -> StdCommand {
        let mut child = crate::terminal::shell_std_command(&self.shell, &self.cwd, command);
        child.current_dir(&self.cwd).env_clear();
        for (name, value) in &self.raw_environment {
            child.env(name, value);
        }
        child
    }
}

#[cfg(unix)]
fn tmux_job_shell(_: &TerminalProfile) -> PathBuf {
    PathBuf::from("/bin/sh")
}

#[cfg(not(unix))]
fn tmux_job_shell(profile: &TerminalProfile) -> PathBuf {
    profile.shell().to_path_buf()
}

async fn run_shell_foreground_async(
    command: String,
    profile: ShellTaskProfile,
    show_stderr: bool,
    timeout: Duration,
    shell_processes: Option<Arc<ShellProcessRegistry>>,
) -> Result<Output, RmuxError> {
    let mut command_builder = profile.shell_command(&command);
    command_builder
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = ProcessTreeChild::spawn(&mut command_builder)
        .map_err(|error| RmuxError::Server(format!("failed to run shell command: {error}")))?;
    let registration_guard = register_shell_process(&mut child, shell_processes.as_ref())?;
    let stdout_task = child
        .child_mut()
        .stdout
        .take()
        .map(ChildStdout::from_std)
        .transpose()
        .map_err(|error| RmuxError::Server(format!("failed to capture shell stdout: {error}")))?
        .map(|pipe| spawn_pipe_reader(pipe, RUN_SHELL_OUTPUT_LIMIT));
    let stderr_task = child
        .child_mut()
        .stderr
        .take()
        .map(ChildStderr::from_std)
        .transpose()
        .map_err(|error| RmuxError::Server(format!("failed to capture shell stderr: {error}")))?
        .map(|pipe| spawn_pipe_reader(pipe, RUN_SHELL_OUTPUT_LIMIT));
    let status_result = wait_child_with_timeout(
        &mut child,
        registration_guard.as_ref(),
        timeout,
        "run-shell",
        "shell command",
    )
    .await;
    drop(registration_guard);
    let status = match status_result {
        Ok(status) => status,
        Err(error) => {
            let _ = finish_pipe_task(stdout_task, "stdout").await;
            let _ = finish_pipe_task(stderr_task, "stderr").await;
            return Err(error);
        }
    };
    let mut stdout = finish_pipe_task(stdout_task, "stdout").await?;
    let stderr = finish_pipe_task(stderr_task, "stderr").await?;
    if show_stderr && !stderr.is_empty() {
        stdout.extend_from_slice(&stderr);
    }
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

async fn shell_condition_is_true_async(
    command: String,
    profile: ShellTaskProfile,
    timeout: Duration,
    shell_processes: Option<Arc<ShellProcessRegistry>>,
) -> Result<bool, RmuxError> {
    let mut command_builder = profile.shell_command(&command);
    command_builder
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = ProcessTreeChild::spawn(&mut command_builder)
        .map_err(|error| RmuxError::Server(format!("failed to run if-shell condition: {error}")))?;
    let registration_guard = register_shell_process(&mut child, shell_processes.as_ref())?;
    let status = wait_child_with_timeout(
        &mut child,
        registration_guard.as_ref(),
        timeout,
        "if-shell",
        "if-shell condition",
    )
    .await?;
    drop(registration_guard);
    Ok(status.success())
}

pub(super) fn run_shell_delay_duration(seconds: f64) -> Result<Duration, RmuxError> {
    Duration::try_from_secs_f64(seconds).map_err(|_| {
        RmuxError::Server("run-shell -d expects a non-negative finite delay".to_owned())
    })
}

async fn wait_child_with_timeout(
    child: &mut ProcessTreeChild,
    registration_guard: Option<&ShellProcessGuard>,
    timeout: Duration,
    command_name: &str,
    wait_label: &str,
) -> Result<std::process::ExitStatus, RmuxError> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        match child.has_exited() {
            Ok(true) => {
                return child.wait().map_err(|error| {
                    RmuxError::Server(format!("failed to wait for {wait_label}: {error}"))
                });
            }
            Ok(false) => {}
            Err(error) => {
                terminate_shell_task(child).await;
                return Err(RmuxError::Server(format!(
                    "failed to wait for {wait_label}: {error}"
                )));
            }
        }

        if registration_guard.is_some_and(ShellProcessGuard::shutdown_started) {
            terminate_shell_task(child).await;
            return Err(RmuxError::Server(format!(
                "{command_name} interrupted by server shutdown"
            )));
        }

        if tokio::time::Instant::now() >= deadline {
            terminate_shell_task(child).await;
            return Err(RmuxError::Server(format!(
                "{command_name} timed out after {}s",
                timeout.as_secs()
            )));
        }

        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        tokio::time::sleep(remaining.min(Duration::from_millis(10))).await;
    }
}

fn register_shell_process(
    child: &mut ProcessTreeChild,
    shell_processes: Option<&Arc<ShellProcessRegistry>>,
) -> Result<Option<ShellProcessGuard>, RmuxError> {
    let Some(shell_processes) = shell_processes else {
        return Ok(None);
    };
    match shell_processes.register(child.controller()) {
        Ok(guard) => Ok(Some(guard)),
        Err(ShellProcessRegistrationError::Closing) => {
            let _ = child.terminate();
            let _ = child.wait();
            Err(RmuxError::Server(
                "shell command interrupted by server shutdown".to_owned(),
            ))
        }
        Err(ShellProcessRegistrationError::LimitReached { limit }) => {
            let _ = child.terminate();
            let _ = child.wait();
            Err(RmuxError::Server(format!(
                "too many active shell process groups; limit is {limit}"
            )))
        }
    }
}

struct PipeReaderTask {
    handle: tokio::task::JoinHandle<()>,
    finish_requested: Arc<AtomicBool>,
    output: Arc<Mutex<PipeOutput>>,
}

#[derive(Default)]
struct PipeOutput {
    bytes: Vec<u8>,
    truncated: bool,
    error: Option<String>,
}

impl PipeOutput {
    fn append(&mut self, bytes: &[u8], limit: usize) {
        let remaining = limit.saturating_sub(self.bytes.len());
        if remaining == 0 {
            self.truncated = true;
            return;
        }
        let kept = bytes.len().min(remaining);
        self.bytes.extend_from_slice(&bytes[..kept]);
        if kept < bytes.len() {
            self.truncated = true;
        }
    }

    fn finish(&self) -> Result<Vec<u8>, RmuxError> {
        if let Some(error) = &self.error {
            return Err(RmuxError::Server(error.clone()));
        }
        let mut output = self.bytes.clone();
        if self.truncated {
            output.extend_from_slice(RUN_SHELL_TRUNCATION_MARKER);
        }
        Ok(output)
    }
}

fn spawn_pipe_reader<R>(pipe: R, limit: usize) -> PipeReaderTask
where
    R: AsyncRead + Send + Unpin + 'static,
{
    let finish_requested = Arc::new(AtomicBool::new(false));
    let task_finish_requested = finish_requested.clone();
    let output = Arc::new(Mutex::new(PipeOutput::default()));
    let task_output = output.clone();
    let handle = tokio::spawn(async move {
        read_limited_pipe(pipe, limit, task_finish_requested, task_output).await
    });
    PipeReaderTask {
        handle,
        finish_requested,
        output,
    }
}

async fn finish_pipe_task(task: Option<PipeReaderTask>, label: &str) -> Result<Vec<u8>, RmuxError> {
    let Some(mut task) = task else {
        return Ok(Vec::new());
    };
    task.finish_requested.store(true, Ordering::SeqCst);
    match tokio::time::timeout(RUN_SHELL_PIPE_FINISH_TIMEOUT, &mut task.handle).await {
        Ok(Ok(())) => {}
        Ok(Err(_)) => {
            return Err(RmuxError::Server(format!(
                "failed to read shell {label}: panicked"
            )));
        }
        Err(_) => task.handle.abort(),
    }
    let output = lock_pipe_output(&task.output).finish();
    output
}

fn lock_pipe_output(output: &Arc<Mutex<PipeOutput>>) -> MutexGuard<'_, PipeOutput> {
    output
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

async fn terminate_shell_task(child: &mut ProcessTreeChild) {
    let _ = child.terminate();
    let deadline = tokio::time::Instant::now() + RUN_SHELL_TERMINATE_REAP_TIMEOUT;
    loop {
        match child.has_exited() {
            Ok(true) => {
                let _ = child.wait();
                return;
            }
            Ok(false) => {}
            Err(_) => {
                let _ = child.wait();
                return;
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return;
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        tokio::time::sleep(remaining.min(Duration::from_millis(10))).await;
    }
}

async fn read_limited_pipe<R>(
    mut pipe: R,
    limit: usize,
    finish_requested: Arc<AtomicBool>,
    output: Arc<Mutex<PipeOutput>>,
) where
    R: AsyncRead + Unpin,
{
    let mut buffer = [0_u8; 8192];
    let mut finish_started_at = None;
    let mut saw_output_after_finish = false;
    loop {
        if finish_requested.load(Ordering::SeqCst) && finish_started_at.is_none() {
            finish_started_at = Some(tokio::time::Instant::now());
        }

        let read_timeout = finish_started_at
            .and_then(|started_at| RUN_SHELL_PIPE_DRAIN_GRACE.checked_sub(started_at.elapsed()))
            .unwrap_or(RUN_SHELL_PIPE_DRAIN_GRACE);
        if read_timeout.is_zero() {
            lock_pipe_output(&output).truncated |= saw_output_after_finish;
            break;
        }

        let read = match tokio::time::timeout(read_timeout, pipe.read(&mut buffer)).await {
            Ok(Ok(read)) => read,
            Ok(Err(error)) => {
                lock_pipe_output(&output).error =
                    Some(format!("failed to read shell output: {error}"));
                break;
            }
            Err(_) if finish_requested.load(Ordering::SeqCst) => {
                lock_pipe_output(&output).truncated |= saw_output_after_finish;
                break;
            }
            Err(_) => continue,
        };
        if read == 0 {
            break;
        }
        saw_output_after_finish |= finish_started_at.is_some();
        let truncated = {
            let mut output = lock_pipe_output(&output);
            output.append(&buffer[..read], limit);
            output.truncated
        };
        if finish_requested.load(Ordering::SeqCst) && truncated {
            break;
        }
        if finish_started_at
            .is_some_and(|started_at| started_at.elapsed() >= RUN_SHELL_PIPE_DRAIN_GRACE)
        {
            lock_pipe_output(&output).truncated = true;
            break;
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::{
        finish_pipe_task, run_shell_foreground_with_timeout, spawn_pipe_reader,
        wait_child_with_timeout, BackgroundTaskLimiter, RUN_SHELL_TRUNCATION_MARKER,
    };
    use crate::terminal::TerminalProfile;
    use rmux_core::{EnvironmentStore, OptionStore};
    use rmux_os::process_tree::ProcessTreeChild;
    use rmux_proto::{OptionName, ScopeSelector, SetOptionMode};
    use rustix::process::{kill_process, Pid, Signal};
    use std::path::{Path, PathBuf};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    use tokio::io::AsyncWriteExt;

    #[test]
    fn background_task_limiter_releases_capacity_when_permits_drop() {
        let limiter = BackgroundTaskLimiter::new(2);
        let first = limiter.try_acquire().expect("first permit");
        let second = limiter.try_acquire().expect("second permit");
        let error = limiter
            .try_acquire()
            .expect_err("third permit should exceed capacity");
        assert!(
            error.to_string().contains("too many background tasks"),
            "unexpected error: {error}"
        );

        drop(first);
        let third = limiter
            .try_acquire()
            .expect("dropped permit should restore capacity");
        drop(second);
        drop(third);
    }

    #[tokio::test]
    async fn pipe_drain_keeps_reading_active_output_after_finish_request() {
        let (reader, mut writer) = tokio::io::duplex(16);
        let pipe_task = spawn_pipe_reader(reader, 1024);
        let writer_task = tokio::spawn(async move {
            for index in 0..8_u8 {
                if writer.write_all(&[b'a' + index, b'\n']).await.is_err() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(60)).await;
            }
        });

        tokio::time::sleep(Duration::from_millis(10)).await;
        let output = finish_pipe_task(Some(pipe_task), "stdout")
            .await
            .expect("pipe drain should succeed");
        writer_task.await.expect("writer task should finish");

        assert!(output.starts_with(b"a\n"), "{output:?}");
        assert!(output.ends_with(RUN_SHELL_TRUNCATION_MARKER), "{output:?}");
    }

    #[tokio::test]
    async fn wait_child_reaps_already_exited_shell_without_signal_wakeup() {
        let mut command = std::process::Command::new("/bin/sh");
        command
            .arg("-c")
            .arg("true")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let mut child = ProcessTreeChild::spawn(&mut command).expect("spawn short-lived child");

        tokio::time::sleep(Duration::from_millis(50)).await;
        let status = wait_child_with_timeout(
            &mut child,
            None,
            Duration::from_secs(1),
            "run-shell",
            "already-exited shell",
        )
        .await
        .expect("already-exited shell should be reaped");

        assert!(status.success());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_kills_descendant_process_holding_stdout() {
        let root = TestTempRoot::new("run-shell-timeout-tree");
        let pid_file = PidFileCleanup::new(root.path().join("child.pid"));
        let command = format!(
            "sleep 30 & printf '%s' \"$!\" > {}; sleep 30",
            shell_quote(pid_file.path())
        );
        let profile = test_profile(root.path());

        let result = run_shell_foreground_with_timeout(
            command,
            &profile,
            false,
            Duration::from_millis(200),
            None,
        )
        .await;

        assert!(
            result
                .expect_err("command should time out")
                .to_string()
                .contains("timed out"),
            "expected timeout error"
        );
        let child_pid = std::fs::read_to_string(pid_file.path())
            .expect("child pid file")
            .trim()
            .parse::<u32>()
            .expect("child pid");
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !rmux_os::process::is_live(child_pid),
            "run-shell timeout must kill descendants, pid {child_pid} is still live"
        );
    }

    #[tokio::test]
    async fn pipe_finish_returns_when_writer_stays_open_without_output() {
        let (reader, _writer) = tokio::io::duplex(16);
        let pipe_task = spawn_pipe_reader(reader, 1024);

        let output = tokio::time::timeout(
            Duration::from_secs(1),
            finish_pipe_task(Some(pipe_task), "stdout"),
        )
        .await
        .expect("pipe finish should not wait forever for a quiet open writer")
        .expect("pipe finish should succeed");

        assert!(output.is_empty(), "{output:?}");
    }

    #[tokio::test]
    async fn pipe_finish_returns_when_writer_keeps_stdout_active() {
        let (reader, mut writer) = tokio::io::duplex(16);
        let pipe_task = spawn_pipe_reader(reader, 1024);
        writer.write_all(b"done\n").await.expect("seed output");
        let writer_task = tokio::spawn(async move {
            loop {
                if writer.write_all(b"hi").await.is_err() {
                    break;
                }
            }
        });

        let output = tokio::time::timeout(
            Duration::from_secs(1),
            finish_pipe_task(Some(pipe_task), "stdout"),
        )
        .await
        .expect("pipe finish should not wait forever for a chatty open writer")
        .expect("pipe finish should succeed");
        writer_task.abort();

        assert!(
            output
                .windows(b"done\n".len())
                .any(|window| window == b"done\n"),
            "expected parent output among background chatter: {output:?}"
        );
        assert!(output.ends_with(RUN_SHELL_TRUNCATION_MARKER), "{output:?}");
    }

    fn test_profile(cwd: &Path) -> TerminalProfile {
        let mut options = OptionStore::default();
        options
            .set(
                ScopeSelector::Global,
                OptionName::DefaultShell,
                "/bin/sh".to_owned(),
                SetOptionMode::Replace,
            )
            .expect("default-shell test option is valid");
        TerminalProfile::for_run_shell_with_base_environment(
            &EnvironmentStore::default(),
            &options,
            None,
            None,
            Path::new("/tmp/rmux-run-shell-runtime-test.sock"),
            None,
            false,
            None,
            Some(cwd),
        )
        .expect("profile")
    }

    struct TestTempRoot {
        path: PathBuf,
    }

    impl TestTempRoot {
        fn new(label: &str) -> Self {
            let path = unique_temp_root(label);
            std::fs::create_dir_all(&path).expect("test root");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TestTempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    struct PidFileCleanup {
        path: PathBuf,
    }

    impl PidFileCleanup {
        fn new(path: PathBuf) -> Self {
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for PidFileCleanup {
        fn drop(&mut self) {
            let Ok(pid) = std::fs::read_to_string(&self.path) else {
                return;
            };
            let pid = pid.trim();
            kill_pid_text(pid, Signal::TERM);
            kill_pid_text(pid, Signal::KILL);
        }
    }

    fn kill_pid_text(pid: &str, signal: Signal) {
        let Ok(pid) = pid.parse::<i32>() else {
            return;
        };
        let Some(pid) = Pid::from_raw(pid) else {
            return;
        };
        let _ = kill_process(pid, signal);
    }

    fn unique_temp_root(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("rmux-{label}-{}-{unique}", std::process::id()))
    }

    #[cfg(unix)]
    fn shell_quote(path: &Path) -> String {
        let value = path.to_string_lossy();
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}
