use std::io::Write;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use rmux_os::process_tree::{ConsoleWindowBehavior, ProcessTreeChild, ProcessTreeController};
use rmux_proto::RmuxError;
use tokio::sync::oneshot;
use tracing::warn;

use super::super::shell_processes::{ShellProcessRegistrationError, ShellProcessRegistry};
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
    let process_guard = match shell_processes.register(controller.clone()) {
        Ok(guard) => guard,
        Err(ShellProcessRegistrationError::Closing) => {
            terminate_rejected_pipe_command(
                &mut child,
                &command,
                started,
                "server shutdown started",
            );
            return;
        }
        Err(ShellProcessRegistrationError::LimitReached { limit }) => {
            terminate_rejected_pipe_command(
                &mut child,
                &command,
                started,
                &format!("active shell process limit of {limit} was reached"),
            );
            return;
        }
    };
    if let Err(controller) = startup_guard.handoff(controller) {
        let _ = controller.terminate();
        let _ = child.wait();
        let _ = started.send(Err(RmuxError::Server(format!(
            "pipe command '{command}' was cancelled before startup completed"
        ))));
        return;
    }
    if process_guard.shutdown_started() {
        startup_guard.terminate();
        let _ = child.terminate();
        let _ = child.wait();
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

fn terminate_rejected_pipe_command(
    child: &mut ProcessTreeChild,
    command: &str,
    started: oneshot::Sender<Result<(), RmuxError>>,
    reason: &str,
) {
    let _ = child.terminate();
    let _ = child.wait();
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
        let command = format!(
            "trap '' TERM HUP INT; \
             (trap '' TERM HUP INT; while :; do sleep 1; done) & \
             printf '%s %s' \"$$\" \"$!\" > {}; wait",
            marker.display()
        );

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
