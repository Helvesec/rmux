//! Bounded process execution for the SDK command escape hatch.

use std::io::{self, Read};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use rmux_os::process_tree::ProcessTreeChild;

const CHILD_POLL_INTERVAL: Duration = Duration::from_millis(5);

pub(super) fn run_with_timeout(
    command: &mut Command,
    timeout: Option<Duration>,
    started_at: Instant,
) -> io::Result<Output> {
    let deadline = CommandDeadline::from_timeout_at(timeout, started_at);
    if deadline.is_elapsed() {
        return Err(deadline.timeout_error());
    }

    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // The CLI may cold-start RMUX's independent daemon. On Windows that daemon
    // deliberately requests job breakaway; the CLI and every ordinary
    // descendant remain in this timeout-owned process tree.
    let mut child = ProcessTreeChild::spawn_allowing_explicit_job_breakaway(command)?;
    let stdout = child
        .child_mut()
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("rmux command stdout was not piped"))?;
    let stderr = child
        .child_mut()
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("rmux command stderr was not piped"))?;

    let mut stdout = OutputReader::spawn("rmux-sdk-command-stdout", stdout)?;
    let mut stderr = match OutputReader::spawn("rmux-sdk-command-stderr", stderr) {
        Ok(stderr) => stderr,
        Err(error) => {
            drop(child);
            let _ = stdout.finish();
            return Err(error);
        }
    };

    let mut child_exited = false;
    let mut stdout_bytes = None;
    let mut stderr_bytes = None;
    loop {
        if !child_exited {
            child_exited = child.has_exited()?;
        }
        stdout.poll(&mut stdout_bytes)?;
        stderr.poll(&mut stderr_bytes)?;

        if child_exited {
            if let (Some(stdout_bytes), Some(stderr_bytes)) =
                (stdout_bytes.as_mut(), stderr_bytes.as_mut())
            {
                let status = child.wait()?;
                stdout.finish()?;
                stderr.finish()?;
                return Ok(Output {
                    status,
                    stdout: std::mem::take(stdout_bytes),
                    stderr: std::mem::take(stderr_bytes),
                });
            }
        }

        if deadline.is_elapsed() {
            return timeout_child(&mut child, stdout, stderr, deadline);
        }
        thread::sleep(deadline.sleep_for(CHILD_POLL_INTERVAL));
    }
}

fn timeout_child(
    child: &mut ProcessTreeChild,
    stdout: OutputReader,
    stderr: OutputReader,
    deadline: CommandDeadline,
) -> io::Result<Output> {
    let terminate_result = child.terminate();
    if terminate_result.is_err() {
        let _ = child.child_mut().kill();
    }
    let wait_result = child.wait();
    let stdout_result = stdout.finish();
    let stderr_result = stderr.finish();

    if let Err(error) = terminate_result {
        return Err(cleanup_error(
            "terminate timed-out rmux command tree",
            error,
        ));
    }
    if let Err(error) = wait_result {
        return Err(cleanup_error("reap timed-out rmux command", error));
    }
    if let Err(error) = stdout_result {
        return Err(cleanup_error("drain timed-out rmux command stdout", error));
    }
    if let Err(error) = stderr_result {
        return Err(cleanup_error("drain timed-out rmux command stderr", error));
    }

    Err(deadline.timeout_error())
}

fn cleanup_error(operation: &str, error: io::Error) -> io::Error {
    io::Error::new(error.kind(), format!("failed to {operation}: {error}"))
}

struct OutputReader {
    receiver: Receiver<io::Result<Vec<u8>>>,
    thread: Option<JoinHandle<()>>,
}

impl OutputReader {
    fn spawn(
        thread_name: &'static str,
        mut reader: impl Read + Send + 'static,
    ) -> io::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name(thread_name.to_owned())
            .spawn(move || {
                let mut bytes = Vec::new();
                let result = reader.read_to_end(&mut bytes).map(|_| bytes);
                let _ = sender.send(result);
            })?;
        Ok(Self {
            receiver,
            thread: Some(thread),
        })
    }

    fn poll(&mut self, output: &mut Option<Vec<u8>>) -> io::Result<()> {
        if output.is_some() {
            return Ok(());
        }
        match self.receiver.try_recv() {
            Ok(result) => *output = Some(result?),
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                return Err(io::Error::other(
                    "rmux command output reader stopped without a result",
                ));
            }
        }
        Ok(())
    }

    fn finish(mut self) -> io::Result<()> {
        if let Some(thread) = self.thread.take() {
            thread
                .join()
                .map_err(|_| io::Error::other("rmux command output reader panicked"))?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct CommandDeadline {
    requested: Option<Duration>,
    expires_at: Option<Instant>,
}

impl CommandDeadline {
    fn from_timeout_at(timeout: Option<Duration>, started_at: Instant) -> Self {
        Self {
            requested: timeout,
            expires_at: timeout.and_then(|timeout| started_at.checked_add(timeout)),
        }
    }

    fn is_elapsed(self) -> bool {
        self.expires_at
            .is_some_and(|expires_at| Instant::now() >= expires_at)
    }

    fn sleep_for(self, poll_interval: Duration) -> Duration {
        self.expires_at
            .map(|expires_at| {
                poll_interval.min(expires_at.saturating_duration_since(Instant::now()))
            })
            .unwrap_or(poll_interval)
    }

    fn timeout_error(self) -> io::Error {
        let timeout = self.requested.expect("elapsed deadline has a timeout");
        io::Error::new(
            io::ErrorKind::TimedOut,
            format!("rmux SDK command timed out after {timeout:?}"),
        )
    }
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::fs::{self, OpenOptions};
    use std::io::Write as _;
    use std::path::{Path, PathBuf};

    use super::*;

    const FIXTURE_MODE_ENV: &str = "RMUX_SDK_COMMAND_FIXTURE_MODE";
    const FIXTURE_DRIVER_PID_ENV: &str = "RMUX_SDK_COMMAND_FIXTURE_DRIVER_PID";
    const FIXTURE_HEARTBEAT_ENV: &str = "RMUX_SDK_COMMAND_FIXTURE_HEARTBEAT";
    const FIXTURE_TEST_NAME: &str = "handles::rmux::command_process::tests::fixture_process";

    #[test]
    fn elapsed_deadline_fails_before_spawning() {
        let mut command = Command::new("this-command-must-not-be-spawned");
        let error = run_with_timeout(&mut command, Some(Duration::ZERO), Instant::now())
            .expect_err("zero timeout expires before spawn");

        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn fast_command_preserves_captured_output() {
        let mut command = fixture_command("quick", None);
        let output = run_with_timeout(&mut command, Some(Duration::from_secs(2)), Instant::now())
            .expect("quick fixture completes within its command deadline");

        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("fixture-stdout"));
        assert!(String::from_utf8_lossy(&output.stderr).contains("fixture-stderr"));
    }

    #[test]
    fn timeout_terminates_descendant_tree_and_reaps_child() {
        let root = TestRoot::new("sdk-command-timeout");
        let heartbeat = root.path().join("heartbeat");
        let mut command = fixture_command("parent", Some(&heartbeat));
        let started_at = Instant::now();
        let error = run_with_timeout(&mut command, Some(Duration::from_secs(2)), started_at)
            .expect_err("blocking fixture exceeds the configured command deadline");

        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(
            started_at.elapsed() < Duration::from_secs(5),
            "command timeout should remain bounded: {:?}",
            started_at.elapsed()
        );
        let bytes_after_return = fs::metadata(&heartbeat)
            .expect("descendant wrote its heartbeat before the deadline")
            .len();
        thread::sleep(Duration::from_millis(150));
        let bytes_after_settle = fs::metadata(&heartbeat)
            .expect("heartbeat remains inspectable after cleanup")
            .len();
        assert_eq!(
            bytes_after_return, bytes_after_settle,
            "descendant heartbeat must stop before the timeout returns"
        );
    }

    #[test]
    fn fixture_process() {
        let Some(mode) = env::var_os(FIXTURE_MODE_ENV) else {
            return;
        };
        let driver_pid = env::var(FIXTURE_DRIVER_PID_ENV)
            .expect("fixture driver pid is configured")
            .parse::<u32>()
            .expect("fixture driver pid is numeric");
        if std::process::id() == driver_pid {
            return;
        }

        match mode.to_string_lossy().as_ref() {
            "quick" => {
                println!("fixture-stdout");
                eprintln!("fixture-stderr");
            }
            "parent" => run_parent_fixture(),
            "heartbeat" => run_heartbeat_fixture(),
            mode => panic!("unknown SDK command fixture mode {mode:?}"),
        }
    }

    fn fixture_command(mode: &str, heartbeat: Option<&Path>) -> Command {
        let mut command = Command::new(env::current_exe().expect("resolve test executable"));
        command
            .args(["--exact", FIXTURE_TEST_NAME, "--nocapture"])
            .env(FIXTURE_MODE_ENV, mode)
            .env(FIXTURE_DRIVER_PID_ENV, std::process::id().to_string());
        if let Some(heartbeat) = heartbeat {
            command.env(FIXTURE_HEARTBEAT_ENV, heartbeat);
        }
        command
    }

    #[allow(
        clippy::zombie_processes,
        reason = "the outer ProcessTreeChild deliberately owns and terminates this fixture tree"
    )]
    fn run_parent_fixture() -> ! {
        let heartbeat = heartbeat_path();
        let mut descendant = fixture_command("heartbeat", Some(&heartbeat));
        descendant.stdout(Stdio::null()).stderr(Stdio::null());
        let _descendant = descendant.spawn().expect("spawn heartbeat descendant");
        let ready_deadline = Instant::now() + Duration::from_secs(1);
        while fs::metadata(&heartbeat).map_or(true, |metadata| metadata.len() == 0) {
            assert!(
                Instant::now() < ready_deadline,
                "heartbeat descendant did not become ready"
            );
            thread::sleep(Duration::from_millis(5));
        }
        loop {
            thread::sleep(Duration::from_secs(1));
        }
    }

    fn run_heartbeat_fixture() -> ! {
        let heartbeat = heartbeat_path();
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(heartbeat)
            .expect("open heartbeat file");
        loop {
            file.write_all(b"x").expect("write heartbeat");
            file.flush().expect("flush heartbeat");
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn heartbeat_path() -> PathBuf {
        env::var_os(FIXTURE_HEARTBEAT_ENV)
            .map(PathBuf::from)
            .expect("fixture heartbeat path is configured")
    }

    struct TestRoot {
        path: PathBuf,
    }

    impl TestRoot {
        fn new(label: &str) -> Self {
            let path = env::temp_dir().join(format!(
                "rmux-{label}-{}-{}",
                std::process::id(),
                monotonic_suffix()
            ));
            fs::create_dir_all(&path).expect("create test root");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn monotonic_suffix() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};

        static NEXT: AtomicU64 = AtomicU64::new(0);
        NEXT.fetch_add(1, Ordering::Relaxed)
    }
}
