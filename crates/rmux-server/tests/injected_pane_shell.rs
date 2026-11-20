#![cfg(unix)]

//! A daemon bound with `ServerDaemon::bind_with_shell` spawns every pane and
//! popup through the injected `RmuxShell`: no operating-system process is
//! started for the initial pane, a split, a respawn or a popup job.

use std::error::Error;
use std::ffi::OsString;
use std::fs::File;
use std::io::Write;
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::sync::{Arc, Condvar, Mutex as StdMutex};
use std::time::Duration;

mod common;

use common::{ClientConnection, TestHarness, PTY_TEST_LOCK};
use rmux_proto::{
    encode_attach_message, AttachMessage, AttachSessionRequest, Request, Response,
    SourceFileRequest, TerminalSize, INTERNAL_CANONICAL_COMMAND_EXECUTION_PATH,
};
use rmux_pty::{
    ChildCommand, ProcessId, PtyChild, PtyPair, PtySlave, RmuxShell, ShellChild, Signal,
    SpawnedPty, TerminalSize as PtyTerminalSize,
};
use rmux_server::{DaemonConfig, ServerDaemon};
use tokio::io::AsyncWriteExt;
use tokio::time::{timeout, Instant};

const STEP_TIMEOUT: Duration = Duration::from_secs(5);
const SESSION: &str = "injected";

#[derive(Debug, Clone)]
struct RecordedSpawn {
    program: PathBuf,
    argv0: Option<OsString>,
    arguments: Vec<OsString>,
    size: Option<PtyTerminalSize>,
}

impl RecordedSpawn {
    fn mentions(&self, needle: &str) -> bool {
        self.arguments
            .iter()
            .any(|argument| argument.to_string_lossy().contains(needle))
    }
}

#[derive(Debug, Default)]
struct ShellLog {
    spawns: StdMutex<Vec<RecordedSpawn>>,
    signals: StdMutex<Vec<Signal>>,
}

impl ShellLog {
    fn spawns(&self) -> Vec<RecordedSpawn> {
        self.spawns.lock().expect("spawn log").clone()
    }

    fn signals(&self) -> Vec<Signal> {
        self.signals.lock().expect("signal log").clone()
    }
}

/// A pane shell that runs no process: it writes a numbered banner to the PTY
/// slave, keeps the slave open until RMUX signals it, and records the spawn
/// request plus every lifecycle signal.
#[derive(Debug)]
struct RecordingShell {
    log: Arc<ShellLog>,
}

#[derive(Debug)]
struct RecordingChild {
    log: Arc<ShellLog>,
    exited: Arc<(StdMutex<bool>, Condvar)>,
    _terminal: PtySlave,
}

impl RecordingChild {
    fn exit_status() -> ExitStatus {
        ExitStatus::from_raw(0)
    }

    fn has_exited(&self) -> bool {
        *self.exited.0.lock().expect("exit flag")
    }
}

impl ShellChild for RecordingChild {
    fn pid(&self) -> ProcessId {
        ProcessId::new(std::process::id()).expect("host pid")
    }

    fn wait(&mut self) -> rmux_pty::Result<ExitStatus> {
        let (flag, exited) = &*self.exited;
        let guard = flag.lock().expect("exit flag");
        let _guard = exited
            .wait_timeout_while(guard, STEP_TIMEOUT, |exited| !*exited)
            .expect("exit flag");
        Ok(Self::exit_status())
    }

    fn try_wait(&mut self) -> rmux_pty::Result<Option<ExitStatus>> {
        Ok(self.has_exited().then(Self::exit_status))
    }

    fn kill(&self, signal: Signal) -> rmux_pty::Result<()> {
        self.log.signals.lock().expect("signal log").push(signal);
        let (flag, exited) = &*self.exited;
        *flag.lock().expect("exit flag") = true;
        exited.notify_all();
        Ok(())
    }

    fn kill_session_leader(&self, signal: Signal) -> rmux_pty::Result<()> {
        self.kill(signal)
    }

    fn continue_if_stopped(&self) -> rmux_pty::Result<bool> {
        Ok(false)
    }
}

impl RmuxShell for RecordingShell {
    fn spawn(&self, command: ChildCommand) -> rmux_pty::Result<SpawnedPty> {
        let size = command.initial_size();
        let index = {
            let mut spawns = self.log.spawns.lock().expect("spawn log");
            spawns.push(RecordedSpawn {
                program: command.program().to_path_buf(),
                argv0: command.argv0().map(ToOwned::to_owned),
                arguments: command.arguments().to_vec(),
                size,
            });
            spawns.len()
        };

        let pair = PtyPair::open_with_size(size.unwrap_or(PtyTerminalSize::new(80, 24)))?;
        let (master, slave) = pair.into_split();
        let mut terminal = File::from(slave.try_clone()?.into_owned_fd());
        write!(terminal, "INJECTED-SHELL-{index}\r\n")?;
        drop(terminal);

        Ok(SpawnedPty::new(
            master,
            PtyChild::new(RecordingChild {
                log: Arc::clone(&self.log),
                exited: Arc::new((StdMutex::new(false), Condvar::new())),
                _terminal: slave,
            }),
        ))
    }
}

fn cli(command: &str) -> Request {
    Request::SourceFile(Box::new(SourceFileRequest {
        paths: vec![INTERNAL_CANONICAL_COMMAND_EXECUTION_PATH.to_owned()],
        quiet: false,
        parse_only: false,
        verbose: false,
        expand_paths: false,
        target: None,
        caller_cwd: None,
        stdin: Some(command.to_owned()),
    }))
}

async fn run_cli(socket: &std::path::Path, command: &str) -> Result<String, Box<dyn Error>> {
    let response = common::send_request(socket, &cli(command)).await?;
    let Response::SourceFile(response) = response else {
        return Err(format!("`{command}` failed: {response:?}").into());
    };
    if response.exit_status.is_some_and(|status| status != 0) {
        return Err(format!("`{command}` exited with {response:?}").into());
    }
    Ok(response
        .output
        .map(|output| String::from_utf8_lossy(output.stdout()).into_owned())
        .unwrap_or_default())
}

async fn wait_for_capture(
    socket: &std::path::Path,
    pane: &str,
    needle: &str,
) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + STEP_TIMEOUT;
    loop {
        let captured = run_cli(socket, &format!("capture-pane -p -t {pane}")).await?;
        if captured.contains(needle) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(
                format!("{pane} never showed {needle:?}; last capture {captured:?}").into(),
            );
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_for_spawns(log: &ShellLog, count: usize) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + STEP_TIMEOUT;
    while log.spawns().len() < count {
        if Instant::now() >= deadline {
            return Err(format!("expected {count} injected spawns, saw {:?}", log.spawns()).into());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    Ok(())
}

#[tokio::test]
async fn bind_with_shell_spawns_panes_splits_respawns_and_popups_through_the_injected_shell(
) -> Result<(), Box<dyn Error>> {
    let _guard = PTY_TEST_LOCK.lock().await;
    let harness = TestHarness::new("injected-shell");
    let socket = harness.socket_path().to_path_buf();
    let log = Arc::new(ShellLog::default());
    let handle = ServerDaemon::new(DaemonConfig::new(socket.clone()))
        .bind_with_shell(Arc::new(RecordingShell {
            log: Arc::clone(&log),
        }))
        .await?;
    let host_pid = std::process::id().to_string();

    // Initial pane: an interactive login shell request of the session size.
    run_cli(&socket, &format!("new-session -d -s {SESSION} -x 80 -y 24")).await?;
    wait_for_capture(&socket, &format!("{SESSION}:0.0"), "INJECTED-SHELL-1").await?;
    let initial = &log.spawns()[0];
    assert!(
        initial
            .argv0
            .as_ref()
            .is_some_and(|argv0| argv0.to_string_lossy().starts_with('-')),
        "the initial pane asks the shell for a login shell: {initial:?}"
    );
    assert!(initial.arguments.is_empty(), "{initial:?}");
    assert_eq!(initial.size, Some(PtyTerminalSize::new(80, 24)));

    // Split: the new pane comes from the same injected backend.
    run_cli(&socket, &format!("split-window -d -t {SESSION}:0.0")).await?;
    wait_for_capture(&socket, &format!("{SESSION}:0.1"), "INJECTED-SHELL-2").await?;
    let split = &log.spawns()[1];
    assert_eq!(split.program, initial.program, "{split:?}");
    let pane_pid = run_cli(
        &socket,
        &format!("display-message -p -t {SESSION}:0.1 '#{{pane_pid}}'"),
    )
    .await?;
    assert_eq!(
        pane_pid.trim(),
        host_pid,
        "#{{pane_pid}} is the injected child's"
    );

    // Respawn: the replacement process is requested from the injected shell.
    run_cli(
        &socket,
        &format!("respawn-pane -k -t {SESSION}:0.1 'echo respawned-marker'"),
    )
    .await?;
    wait_for_capture(&socket, &format!("{SESSION}:0.1"), "INJECTED-SHELL-3").await?;
    assert!(
        log.spawns()[2].mentions("respawned-marker"),
        "{:?}",
        log.spawns()
    );
    assert!(
        !log.signals().is_empty(),
        "respawn -k signals the previous injected child"
    );

    // Popup: an attached client opens a popup job through the injected shell.
    let (_attached, mut attach_stream) = ClientConnection::connect(&socket)
        .await?
        .begin_attach(AttachSessionRequest {
            target: common::session_name(SESSION),
        })
        .await?;
    let resize =
        encode_attach_message(&AttachMessage::Resize(TerminalSize { cols: 80, rows: 24 }))?;
    attach_stream.write_all(&resize).await?;
    let popup_socket = socket.clone();
    let popup = tokio::spawn(async move {
        let request = cli(&format!(
            "display-popup -c {} -w 30 -h 6 -E 'echo popup-marker'",
            std::process::id()
        ));
        common::send_request(&popup_socket, &request)
            .await
            .map_err(|error| error.to_string())
    });
    wait_for_spawns(&log, 4).await?;
    assert!(
        log.spawns()[3].mentions("popup-marker"),
        "{:?}",
        log.spawns()
    );
    run_cli(
        &socket,
        &format!("display-popup -C -c {}", std::process::id()),
    )
    .await?;
    // Closing the popup answers its waiting CLI; the exit status is not under test.
    let popup_response = timeout(STEP_TIMEOUT, popup)
        .await??
        .map_err(|error| -> Box<dyn Error> { error.into() })?;
    assert!(
        matches!(popup_response, Response::SourceFile(_)),
        "{popup_response:?}"
    );

    assert_eq!(log.spawns().len(), 4, "{:?}", log.spawns());
    drop(attach_stream);
    run_cli(&socket, &format!("kill-session -t {SESSION}")).await?;
    let deadline = Instant::now() + STEP_TIMEOUT;
    while !log.signals().contains(&Signal::HUP) {
        assert!(
            Instant::now() < deadline,
            "pane teardown never reached the injected child: {:?}",
            log.signals()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    handle.shutdown().await?;
    Ok(())
}
