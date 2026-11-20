use std::io::Write;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use rmux_os::process_tree::{ConsoleWindowBehavior, ProcessTreeChild, ProcessTreeController};
use rmux_proto::RmuxError;
use tokio::sync::oneshot;
use tracing::warn;

use super::super::shell_processes::{
    terminate_and_reap_shell_process, ShellProcessRegistrationError, ShellProcessRegistry,
};
use super::super::RequestHandler;
use crate::terminal::shell_std_command;

pub(super) async fn run_pipe_command(
    handler: &RequestHandler,
    shell: &str,
    command: &str,
    working_directory: Option<&PathBuf>,
    data: &[u8],
) -> Result<(), RmuxError> {
    if command.is_empty() {
        return Ok(());
    }

    let shell = PathBuf::from(shell);
    let command = command.to_owned();
    let working_directory = working_directory
        .cloned()
        .unwrap_or_else(|| PathBuf::from("."));
    let data = data.to_vec();
    let shell_processes = Arc::clone(&handler.shell_processes);
    let startup_guard = PipeCommandStartupGuard::new();
    let startup_guard_state = startup_guard.state();
    let (started_tx, started_rx) = oneshot::channel();
    handler.spawn_blocking_process_task("rmux-copy-pipe", move || {
        run_pipe_command_blocking(
            shell,
            command,
            working_directory,
            data,
            shell_processes,
            startup_guard_state,
            started_tx,
        );
    })?;

    let result = started_rx
        .await
        .map_err(|_| RmuxError::Server("pipe command task stopped before startup".to_owned()))?;
    result?;
    startup_guard.disarm();
    Ok(())
}

fn run_pipe_command_blocking(
    shell: PathBuf,
    command: String,
    working_directory: PathBuf,
    data: Vec<u8>,
    shell_processes: Arc<ShellProcessRegistry>,
    startup_guard: Arc<PipeCommandStartupGuardState>,
    started: oneshot::Sender<Result<(), RmuxError>>,
) {
    let mut child = shell_std_command(&shell, &working_directory, &command);
    child.stdin(Stdio::piped());
    child.current_dir(&working_directory);
    let mut child = match ProcessTreeChild::spawn_with_console_window(
        &mut child,
        ConsoleWindowBehavior::Suppress,
    ) {
        Ok(child) => child,
        Err(error) => {
            let _ = started.send(Err(RmuxError::Server(format!(
                "failed to spawn pipe command '{command}': {error}"
            ))));
            return;
        }
    };
    let controller = child.controller();
    let process_guard = match shell_processes.register_spawned(&mut child) {
        Ok(guard) => guard,
        Err(ShellProcessRegistrationError::Closing) => {
            report_rejected_pipe_command(&command, started, "server shutdown started");
            return;
        }
        Err(ShellProcessRegistrationError::LimitReached { limit }) => {
            report_rejected_pipe_command(
                &command,
                started,
                &format!("active shell process limit of {limit} was reached"),
            );
            return;
        }
    };
    if let Err(controller) = startup_guard.handoff(controller) {
        drop(controller);
        terminate_and_reap_shell_process(&mut child);
        let _ = started.send(Err(RmuxError::Server(format!(
            "pipe command '{command}' was cancelled before startup completed"
        ))));
        return;
    }
    if process_guard.shutdown_started() {
        startup_guard.terminate();
        terminate_and_reap_shell_process(&mut child);
        let _ = started.send(Err(RmuxError::Server(format!(
            "pipe command '{command}' was interrupted by server shutdown"
        ))));
        return;
    }

    let _ = started.send(Ok(()));
    if let Some(mut stdin) = child.child_mut().stdin.take() {
        if let Err(error) = stdin.write_all(&data) {
            warn!(%error, %command, "failed to write selection to copy-mode pipe command");
        }
    }
    if let Err(error) = child.wait() {
        warn!(%error, %command, "failed to reap copy-mode pipe command");
    }
}

fn report_rejected_pipe_command(
    command: &str,
    started: oneshot::Sender<Result<(), RmuxError>>,
    reason: &str,
) {
    let _ = started.send(Err(RmuxError::Server(format!(
        "pipe command '{command}' was cancelled before startup completed: {reason}"
    ))));
}

struct PipeCommandStartupGuard {
    state: Arc<PipeCommandStartupGuardState>,
}

impl PipeCommandStartupGuard {
    fn new() -> Self {
        Self {
            state: Arc::new(PipeCommandStartupGuardState::new()),
        }
    }

    fn state(&self) -> Arc<PipeCommandStartupGuardState> {
        Arc::clone(&self.state)
    }

    fn disarm(self) {
        self.state.disarm();
    }
}

impl Drop for PipeCommandStartupGuard {
    fn drop(&mut self) {
        self.state.terminate();
    }
}

struct PipeCommandStartupGuardState {
    armed: AtomicBool,
    controller: Mutex<Option<ProcessTreeController>>,
}

impl PipeCommandStartupGuardState {
    fn new() -> Self {
        Self {
            armed: AtomicBool::new(true),
            controller: Mutex::new(None),
        }
    }

    fn handoff(&self, controller: ProcessTreeController) -> Result<(), ProcessTreeController> {
        if !self.armed.load(Ordering::SeqCst) {
            return Err(controller);
        }

        let mut slot = self
            .controller
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !self.armed.load(Ordering::SeqCst) {
            return Err(controller);
        }
        *slot = Some(controller);
        Ok(())
    }

    fn terminate(&self) {
        if !self.armed.swap(false, Ordering::SeqCst) {
            return;
        }
        if let Some(controller) = self.take_controller() {
            let _ = controller.terminate();
        }
    }

    fn disarm(&self) {
        self.armed.store(false, Ordering::SeqCst);
        let _ = self.take_controller();
    }

    fn take_controller(&self) -> Option<ProcessTreeController> {
        self.controller
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }
}

#[cfg(all(test, target_os = "linux"))]
#[path = "pipe_command_linux_tests.rs"]
mod linux_tests;

#[cfg(test)]
mod tests {
    use super::run_pipe_command;
    use crate::handler::RequestHandler;
    use std::time::{Duration, Instant};

    #[tokio::test(flavor = "current_thread")]
    async fn pipe_command_returns_after_startup_without_waiting_for_exit() {
        let handler = RequestHandler::new();
        let (shell, command) = slow_shell_command();
        let start = Instant::now();
        tokio::time::timeout(
            Duration::from_secs(2),
            run_pipe_command(&handler, &shell, command, None, &vec![b'x'; 1024 * 1024]),
        )
        .await
        .expect("pipe startup must not wait for stdin delivery or child exit")
        .expect("slow pipe command should start");
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "copy-pipe waited for stdin delivery or child exit"
        );
        tokio::time::sleep(Duration::from_secs(3)).await;
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn shutdown_terminates_and_joins_copy_pipe_tree() {
        let handler = RequestHandler::new();
        let marker = std::env::temp_dir().join(format!(
            "rmux-copy-pipe-lifecycle-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time after epoch")
                .as_nanos()
        ));
        let command = copy_pipe_tree_command(&marker);

        run_pipe_command(&handler, "/bin/sh", &command, None, b"selection")
            .await
            .expect("copy-pipe helper starts");
        tokio::time::timeout(Duration::from_secs(2), async {
            while !marker.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("copy-pipe helper reports its process tree");
        let pids = std::fs::read_to_string(&marker).expect("read copy-pipe process ids");
        let mut pids = pids
            .split_whitespace()
            .map(|pid| pid.parse::<u32>().expect("numeric copy-pipe pid"));
        let helper_pid = pids.next().expect("helper pid");
        let descendant_pid = pids.next().expect("descendant pid");
        assert!(rmux_os::process::is_live(helper_pid));
        assert!(rmux_os::process::is_live(descendant_pid));

        let unfinished = tokio::time::timeout(
            Duration::from_secs(3),
            handler.shutdown_background_tasks_and_shell_processes(),
        )
        .await
        .expect("copy-pipe shutdown is bounded");
        assert!(unfinished.is_empty(), "unfinished tasks: {unfinished:?}");
        assert!(
            !rmux_os::process::is_live(helper_pid),
            "copy-pipe helper survived shutdown"
        );
        assert!(
            !rmux_os::process::is_live(descendant_pid),
            "copy-pipe descendant survived shutdown"
        );

        let _ = std::fs::remove_file(marker);
    }

    #[cfg(unix)]
    fn copy_pipe_tree_command(marker: &std::path::Path) -> String {
        format!(
            "trap '' TERM HUP INT; \
             (trap '' TERM HUP INT; while :; do sleep 1; done) & \
             printf '%s %s' \"$$\" \"$!\" > {0}.tmp && mv {0}.tmp {0}; wait",
            marker.display()
        )
    }

    #[cfg(unix)]
    fn copy_pipe_publication_marker() -> std::path::PathBuf {
        copy_pipe_publication_marker_at(std::time::SystemTime::now())
    }

    /// The clock only separates runs: macOS reports microseconds, so tests
    /// running in parallel can read the same instant. The sequence separates
    /// markers made by this process.
    #[cfg(unix)]
    fn copy_pipe_publication_marker_at(now: std::time::SystemTime) -> std::path::PathBuf {
        static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "rmux-copy-pipe-publication-{}-{}-{}",
            std::process::id(),
            now.duration_since(std::time::UNIX_EPOCH)
                .expect("system time after epoch")
                .as_nanos(),
            SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ))
    }

    #[cfg(unix)]
    #[test]
    fn copy_pipe_publication_markers_differ_within_one_clock_instant() {
        let now = std::time::SystemTime::now();
        let first = copy_pipe_publication_marker_at(now);
        let second = copy_pipe_publication_marker_at(now);
        assert_ne!(
            first, second,
            "two copy-pipe tests read the same instant and share one marker"
        );
    }

    #[cfg(unix)]
    struct CopyPipeMarkerCleanup<'a>(&'a std::path::Path);

    #[cfg(unix)]
    impl Drop for CopyPipeMarkerCleanup<'_> {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(self.0);
            let _ = std::fs::remove_file(self.0.with_extension("tmp"));
        }
    }

    #[cfg(unix)]
    fn with_held_copy_pipe_pid_writer<T>(
        marker: &std::path::Path,
        check: impl FnOnce(&mut std::os::unix::net::UnixStream, u32) -> T,
    ) -> T {
        use std::os::fd::OwnedFd;
        use std::os::unix::net::UnixStream;

        // Locals drop in reverse declaration order: the owned child terminates
        // and reaps before the marker paths are removed, even when check unwinds.
        let _marker_cleanup = CopyPipeMarkerCleanup(marker);
        let (mut gate, child_gate) = UnixStream::pair().expect("publication gate");
        gate.set_read_timeout(Some(Duration::from_secs(2)))
            .expect("bound publication handshake");
        gate.set_write_timeout(Some(Duration::from_secs(2)))
            .expect("bound publication release");

        // Shell redirection opens the output before entering printf. Hold that
        // exact window until the reader has checked the publication marker.
        let command = format!(
            "printf() {{ command printf ready >&0; read -r release; command printf \"$@\"; }}; {}",
            copy_pipe_tree_command(marker)
        );
        let mut shell = std::process::Command::new("/bin/sh");
        shell.args(["-c", &command]);
        shell.stdin(std::process::Stdio::from(OwnedFd::from(child_gate)));
        let mut child = super::ProcessTreeChild::spawn(&mut shell).expect("start held PID writer");
        let result = check(&mut gate, child.child_mut().id());

        // Reap the owned tree before reporting the regression, including on red.
        super::terminate_and_reap_shell_process(&mut child);
        result
    }

    #[cfg(unix)]
    #[test]
    fn copy_pipe_pid_marker_is_not_visible_before_pid_write() {
        use std::io::{Read, Write};

        let marker = copy_pipe_publication_marker();
        let (ready, early_contents) = with_held_copy_pipe_pid_writer(&marker, |gate, _| {
            let mut ready = [0; 5];
            gate.read_exact(&mut ready)
                .expect("PID writer reaches gate");
            let early_contents = std::fs::read(&marker);
            gate.write_all(b"publish\n").expect("release PID writer");
            (ready, early_contents)
        });
        assert_eq!(&ready, b"ready");
        assert!(
            matches!(&early_contents, Err(error) if error.kind() == std::io::ErrorKind::NotFound),
            "PID marker became visible before printf wrote the PIDs: {early_contents:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn copy_pipe_pid_writer_cleans_markers_after_io_error() {
        use std::io::{Read, Write};
        use std::net::Shutdown;
        use std::panic::{catch_unwind, AssertUnwindSafe};

        let mut outcomes = Vec::new();
        for failure in [None, Some(Shutdown::Read), Some(Shutdown::Write)] {
            let marker = copy_pipe_publication_marker();
            let temporary = marker.with_extension("tmp");
            let mut helper_pid = None;
            let result = catch_unwind(AssertUnwindSafe(|| {
                with_held_copy_pipe_pid_writer(&marker, |gate, pid| {
                    helper_pid = Some(pid);
                    let mut ready = [0; 5];
                    gate.read_exact(&mut ready)
                        .expect("PID writer reaches gate");
                    assert_eq!(&ready, b"ready");
                    assert!(temporary.exists(), "writer opened the temporary marker");
                    // Exercise ownership of both paths while the writer is held.
                    std::fs::write(&marker, b"cleanup probe").expect("create marker probe");
                    if let Some(direction) = failure {
                        gate.shutdown(direction).expect("close test gate direction");
                        match direction {
                            Shutdown::Read => gate
                                .read_exact(&mut ready)
                                .expect("injected PID gate read failure"),
                            Shutdown::Write => gate
                                .write_all(b"publish\n")
                                .expect("injected PID gate write failure"),
                            Shutdown::Both => unreachable!(),
                        }
                    } else {
                        gate.write_all(b"publish\n").expect("release PID writer");
                    }
                });
            }));
            let expected_result = match (failure, &result) {
                (None, Ok(())) => true,
                (Some(direction), Err(payload)) => {
                    payload.downcast_ref::<String>().is_some_and(|message| {
                        let expected = match direction {
                            Shutdown::Read => "injected PID gate read failure",
                            Shutdown::Write => "injected PID gate write failure",
                            Shutdown::Both => unreachable!(),
                        };
                        message.starts_with(expected)
                    })
                }
                _ => false,
            };
            let reaped = helper_pid.is_some_and(|pid| !rmux_os::process::is_live(pid));
            let marker_removed = !marker.exists();
            let temporary_removed = !temporary.exists();
            // Keep the failing regression itself clean, after observing the leak.
            let _ = std::fs::remove_file(&marker);
            let _ = std::fs::remove_file(&temporary);
            outcomes.push((
                failure,
                expected_result,
                reaped,
                marker_removed,
                temporary_removed,
            ));
        }
        assert!(
            outcomes.iter().all(|(_, expected, reaped, marker, temporary)|
                *expected && *reaped && *marker && *temporary),
            "PID writer cleanup (failure, expected result, reaped, marker removed, temporary removed): {outcomes:?}"
        );
    }

    #[cfg(windows)]
    fn slow_shell_command() -> (String, &'static str) {
        (
            std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_owned()),
            "ping -n 3 127.0.0.1 >NUL",
        )
    }

    #[cfg(unix)]
    fn slow_shell_command() -> (String, &'static str) {
        ("/bin/sh".to_owned(), "sleep 1.5")
    }
}
