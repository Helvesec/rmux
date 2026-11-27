use std::ffi::OsStr;
use std::fs::File;
use std::io::Write;
use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use rmux_proto::request::NewSessionExtRequest;
use rmux_proto::{KillSessionRequest, PaneTarget, Request, Response, SessionName, TerminalSize};
use rmux_pty::{
    ChildCommand, ProcessId, PtyChild, PtyPair, PtySlave, RmuxShell, ShellChild, Signal,
    SpawnedPty, TerminalSize as PtyTerminalSize,
};
use tokio::time::{Duration, Instant};

use super::clock_mode_tests::{list_panes_text, wait_for_transcript_containing};
use super::RequestHandler;

const READY_MARKER: &str = "INJECTED-SHELL-READY";

/// A pane shell that runs no process: it writes a fixed banner to the PTY
/// slave, keeps the slave open for the pane's lifetime, and records the spawn
/// request plus every lifecycle signal RMUX delivers.
#[derive(Debug)]
struct RecordingShell {
    seen: Arc<StdMutex<Option<ChildCommand>>>,
    closed: Arc<AtomicBool>,
    signals: Arc<StdMutex<Vec<Signal>>>,
}

#[derive(Debug)]
struct RecordingChild {
    exit: ExitStatus,
    killed: Arc<AtomicBool>,
    closed: Arc<AtomicBool>,
    signals: Arc<StdMutex<Vec<Signal>>>,
    _terminal: PtySlave,
}

impl ShellChild for RecordingChild {
    fn pid(&self) -> ProcessId {
        ProcessId::new(std::process::id()).expect("host pid")
    }

    fn wait(&mut self) -> rmux_pty::Result<ExitStatus> {
        Ok(self.exit)
    }

    fn try_wait(&mut self) -> rmux_pty::Result<Option<ExitStatus>> {
        let mut fds = [rustix::event::PollFd::new(
            &self._terminal,
            rustix::event::PollFlags::HUP,
        )];
        let timeout = rustix::event::Timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        rustix::event::poll(&mut fds, Some(&timeout)).map_err(std::io::Error::from)?;
        let closed = fds[0].revents().contains(rustix::event::PollFlags::HUP);
        self.closed.store(closed, Ordering::SeqCst);
        Ok((closed || self.killed.load(Ordering::SeqCst)).then_some(self.exit))
    }

    fn kill(&self, signal: Signal) -> rmux_pty::Result<()> {
        self.signals.lock().expect("signal log").push(signal);
        self.killed.store(true, Ordering::SeqCst);
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
        let size = command
            .initial_size()
            .unwrap_or(PtyTerminalSize::new(80, 24));
        *self.seen.lock().expect("spawn log") = Some(command);

        let (master, slave) = PtyPair::open_with_size(size)?.into_split();
        let mut terminal = File::from(slave.try_clone()?.into_owned_fd());
        write!(terminal, "{READY_MARKER}\r\n")?;
        drop(terminal);

        Ok(SpawnedPty::new(
            master,
            PtyChild::new(RecordingChild {
                exit: ExitStatusExt::from_raw(0),
                killed: Arc::new(AtomicBool::new(false)),
                closed: Arc::clone(&self.closed),
                signals: Arc::clone(&self.signals),
                _terminal: slave,
            }),
        ))
    }
}

#[tokio::test]
async fn new_session_spawns_its_pane_through_the_injected_shell() {
    let seen = Arc::new(StdMutex::new(None));
    let closed = Arc::new(AtomicBool::new(false));
    let signals = Arc::new(StdMutex::new(Vec::new()));
    let handler = RequestHandler::with_host_shell_for_test();
    handler
        .install_pane_shell(Arc::new(RecordingShell {
            seen: Arc::clone(&seen),
            closed: Arc::clone(&closed),
            signals: Arc::clone(&signals),
        }))
        .await;

    let session_name = SessionName::new("injected-pane-shell").expect("valid session name");
    let response = handler
        .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
            session_name: Some(session_name.clone()),
            working_directory: None,
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
            group_target: None,
            attach_if_exists: false,
            detach_other_clients: false,
            kill_other_clients: false,
            flags: None,
            window_name: None,
            print_session_info: false,
            print_format: None,
            command: None,
            process_command: None,
            client_environment: None,
            skip_environment_update: false,
        })))
        .await;
    crate::session_fixture::assert_new_session(&response);

    let target = PaneTarget::with_window(session_name.clone(), 0, 0);
    wait_for_transcript_containing(
        &handler,
        &target,
        READY_MARKER,
        "injected shell output should reach the pane transcript",
    )
    .await;

    let recorded = seen
        .lock()
        .expect("spawn log")
        .take()
        .expect("pane spawn reached the injected shell");
    let argv0 = recorded
        .argv0()
        .expect("interactive pane spawns a login shell")
        .to_string_lossy()
        .into_owned();
    assert!(
        argv0.starts_with('-'),
        "login-shell argv[0] should cross the trait boundary, got {argv0:?}"
    );
    assert_eq!(recorded.arguments(), &[] as &[&OsStr]);
    assert_eq!(recorded.initial_size(), Some(PtyTerminalSize::new(80, 24)));

    let pane_pid = list_panes_text(&handler, &target, "#{pane_pid}").await;
    assert_eq!(pane_pid.trim(), std::process::id().to_string());

    let response = handler
        .handle(Request::KillSession(KillSessionRequest {
            target: session_name,
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        }))
        .await;
    assert!(
        !matches!(response, Response::Error(_)),
        "kill-session failed: {response:?}"
    );

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if closed.load(Ordering::SeqCst) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "pane teardown never closed the injected terminal"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        signals.lock().expect("signal log").is_empty(),
        "Unix teardown closes the PTY instead of synthesizing signals"
    );
}
