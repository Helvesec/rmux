#![cfg(target_os = "linux")]

mod common;

use std::error::Error;
use std::fs;
use std::io::{self, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use common::{assert_success, terminate_child, AttachedSession, CliHarness, BINARY_OVERRIDE_ENV};
use rmux_client::connect;
use rmux_proto::{KillSessionRequest, ListSessionsRequest, Response, SessionName};
use rmux_pty::TerminalSize;

const PROCESS_TIMEOUT: Duration = Duration::from_secs(5);
const RETRY_OBSERVATION: Duration = Duration::from_millis(200);
const CONTROL_TEARDOWN_OBSERVATION: Duration = Duration::from_millis(425);
type SharedOutput = Arc<Mutex<Vec<u8>>>;
type OutputCollector = JoinHandle<io::Result<Vec<u8>>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ProcessGeneration {
    pid: u32,
    start_tick: u64,
}

#[test]
fn sdk_connection_survives_a_later_attach_drain_reevaluation() -> Result<(), Box<dyn Error>> {
    run_later_drain_case("retry-scope-pty-sdk", DrainPeers::Pty)
}

#[test]
fn sdk_connection_survives_a_later_control_drain_reevaluation() -> Result<(), Box<dyn Error>> {
    run_later_drain_case("retry-scope-control-sdk", DrainPeers::Control)
}

#[test]
fn sdk_connection_survives_completed_session_bound_control_teardown() -> Result<(), Box<dyn Error>>
{
    let harness = CliHarness::new("retry-scope-control-teardown-sdk")?;
    let (_cleanup, generation) = start_alpha(&harness)?;
    let mut control = ControlProcess::spawn(&harness)?;
    control.wait_for_output("%session-changed ")?;
    control.write_input("display-message -p CONTROL_READY\n")?;
    control.wait_for_output("CONTROL_READY\n")?;
    control.wait_for_begin_count(2)?;

    let mut sdk = connect(harness.socket_path())?;
    let Response::ListSessions(first) = sdk.list_sessions(empty_list_request())? else {
        panic!("expected initial list-sessions response");
    };
    assert!(
        String::from_utf8_lossy(first.command_output().stdout()).contains("alpha"),
        "the SDK transport must be accepted before the last-session kill"
    );

    assert!(matches!(kill_alpha(&mut sdk)?, Response::KillSession(_)));
    let control_output = control.wait_for_exit_with_open_stdin()?;
    assert_eq!(control_output.status.code(), Some(0));
    assert!(
        control_output.stderr.is_empty(),
        "stderr={:?}",
        control_output.stderr
    );
    assert!(
        control_output.stdout.contains("%session-changed ")
            && control_output.stdout.contains("CONTROL_READY\n"),
        "the control client was not proven admitted and session-bound: {:?}",
        control_output.stdout
    );
    assert!(
        control_output.stdout.matches("%begin ").count() >= 2
            && control_output.stdout.matches("%end ").count() >= 2
            && control_output.stdout.ends_with("%exit\n"),
        "control terminal framing was incomplete: {:?}",
        control_output.stdout
    );

    std::thread::sleep(CONTROL_TEARDOWN_OBSERVATION);
    assert_empty_sessions(sdk.list_sessions(empty_list_request())?)?;
    assert!(
        generation_is_alive(generation)?,
        "the accepted SDK transport must keep its daemon generation alive"
    );

    drop(sdk);
    assert!(
        wait_for_generation_exit(generation, PROCESS_TIMEOUT)?,
        "idle shutdown must follow the final SDK transport closing"
    );
    Ok(())
}

#[test]
fn sdk_connection_survives_later_pty_and_control_drain_reevaluations() -> Result<(), Box<dyn Error>>
{
    run_later_drain_case("retry-scope-pty-control-sdk", DrainPeers::PtyAndControl)
}

#[test]
fn requester_only_shutdown_still_follows_its_ack() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("retry-scope-no-peer")?;
    let (_cleanup, generation) = start_alpha(&harness)?;
    let mut sdk = connect(harness.socket_path())?;

    assert!(matches!(kill_alpha(&mut sdk)?, Response::KillSession(_)));
    // A reply does not close a persistent Unix requester. Prove the ACK left
    // it usable, then release the final transport before waiting for exit-empty.
    assert_empty_sessions(sdk.list_sessions(empty_list_request())?)?;
    assert!(generation_is_alive(generation)?);
    drop(sdk);
    assert!(
        wait_for_generation_exit(generation, PROCESS_TIMEOUT)?,
        "the requester-only path should stop after its acknowledged connection closes"
    );
    assert!(
        connect(harness.socket_path()).is_err(),
        "the stopped generation must not accept another connection"
    );
    Ok(())
}

#[test]
fn another_accepted_sdk_peer_blocks_requester_only_shutdown() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("retry-scope-accepted-peer")?;
    let (_cleanup, generation) = start_alpha(&harness)?;
    let mut sdk = connect(harness.socket_path())?;
    let mut peer = connect(harness.socket_path())?;

    assert!(matches!(kill_alpha(&mut sdk)?, Response::KillSession(_)));
    std::thread::sleep(RETRY_OBSERVATION);
    assert!(generation_is_alive(generation)?);
    assert_empty_sessions(sdk.list_sessions(empty_list_request())?)?;
    assert_empty_sessions(peer.list_sessions(empty_list_request())?)?;

    drop(sdk);
    drop(peer);
    assert!(
        wait_for_generation_exit(generation, PROCESS_TIMEOUT)?,
        "shutdown should follow both accepted SDK peers closing"
    );
    Ok(())
}

#[test]
fn a_concurrent_rescue_session_cancels_the_pending_empty_shutdown() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("retry-scope-rescue-session")?;
    let (_cleanup, generation) = start_alpha(&harness)?;
    let mut attach = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(80, 24))?;
    attach.wait_for_raw_mode(PROCESS_TIMEOUT)?;
    let mut sdk = connect(harness.socket_path())?;
    let mut rescue = connect(harness.socket_path())?;

    assert!(matches!(kill_alpha(&mut sdk)?, Response::KillSession(_)));
    assert!(matches!(
        rescue.new_session(SessionName::new("rescue")?, true, None)?,
        Response::NewSession(_)
    ));
    wait_for_attach_exit(&mut attach)?;
    std::thread::sleep(RETRY_OBSERVATION);
    assert!(
        generation_is_alive(generation)?,
        "the concurrently admitted rescue session must cancel exit-empty"
    );
    let Response::ListSessions(response) = sdk.list_sessions(empty_list_request())? else {
        panic!("expected list-sessions response");
    };
    assert!(
        String::from_utf8_lossy(response.command_output().stdout()).contains("rescue"),
        "the rescue session must remain visible"
    );

    drop(sdk);
    assert!(matches!(
        rescue.kill_session(KillSessionRequest {
            target: SessionName::new("rescue")?,
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        })?,
        Response::KillSession(_)
    ));
    // The rescue requester now owns the last transport, just as the original
    // requester did before the rescue. Its ACK alone must not close it.
    assert_empty_sessions(rescue.list_sessions(empty_list_request())?)?;
    assert!(generation_is_alive(generation)?);
    drop(rescue);
    assert!(wait_for_generation_exit(generation, PROCESS_TIMEOUT)?);
    Ok(())
}

#[test]
fn exit_empty_off_survives_sdk_and_attach_close() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("retry-scope-exit-empty-off")?;
    let (_cleanup, generation) = start_alpha(&harness)?;
    assert_success(&harness.run(&["set-option", "-g", "exit-empty", "off"])?);
    let mut attach = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(80, 24))?;
    attach.wait_for_raw_mode(PROCESS_TIMEOUT)?;
    let mut sdk = connect(harness.socket_path())?;

    assert!(matches!(kill_alpha(&mut sdk)?, Response::KillSession(_)));
    wait_for_attach_exit(&mut attach)?;
    assert_empty_sessions(sdk.list_sessions(empty_list_request())?)?;
    drop(sdk);
    std::thread::sleep(RETRY_OBSERVATION);
    assert!(
        generation_is_alive(generation)?,
        "exit-empty=off must keep the empty daemon alive after every client closes"
    );

    assert_success(&harness.run(&["kill-server"])?);
    assert!(wait_for_generation_exit(generation, PROCESS_TIMEOUT)?);
    Ok(())
}

#[test]
fn kill_server_remains_forceful_with_sdk_and_attach_activity() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("retry-scope-kill-server")?;
    let (_cleanup, generation) = start_alpha(&harness)?;
    let mut attach = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(80, 24))?;
    attach.wait_for_raw_mode(PROCESS_TIMEOUT)?;
    let mut sdk = connect(harness.socket_path())?;

    assert!(matches!(sdk.kill_server()?, Response::KillServer(_)));
    let _ = attach.wait_for_exit(PROCESS_TIMEOUT)?;
    attach.assert_restored()?;
    assert!(
        wait_for_generation_exit(generation, PROCESS_TIMEOUT)?,
        "KillServer must ignore retry scopes and active client kinds"
    );
    Ok(())
}

#[derive(Clone, Copy)]
enum DrainPeers {
    Pty,
    Control,
    PtyAndControl,
}

fn run_later_drain_case(label: &str, peers: DrainPeers) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new(label)?;
    let (_cleanup, generation) = start_alpha(&harness)?;

    let mut attach = matches!(peers, DrainPeers::Pty | DrainPeers::PtyAndControl)
        .then(|| AttachedSession::spawn(&harness, "alpha", TerminalSize::new(80, 24)))
        .transpose()?;
    if let Some(attach) = attach.as_mut() {
        attach.wait_for_raw_mode(PROCESS_TIMEOUT)?;
    }
    let mut control = matches!(peers, DrainPeers::Control | DrainPeers::PtyAndControl)
        .then(|| ControlProcess::spawn(&harness))
        .transpose()?;
    if let Some(control) = control.as_mut() {
        control.wait_for_output("%session-changed ")?;
        control.write_input("run-shell 'sleep 0.30'\n")?;
        control.wait_for_begin_count(2)?;
    }
    let mut sdk = connect(harness.socket_path())?;
    let response = kill_alpha(&mut sdk)?;
    assert!(matches!(response, Response::KillSession(_)));
    let acknowledged_at = Instant::now();

    if let Some(attach) = attach.as_mut() {
        let (status, attached_output) = attach.wait_for_exit_with_output(PROCESS_TIMEOUT)?;
        assert_eq!(status.code(), Some(0));
        assert!(
            String::from_utf8_lossy(&attached_output).contains("[exited]"),
            "attached output={attached_output:?}"
        );
        attach.assert_restored()?;
    }
    if let Some(control) = control.as_mut() {
        let output = control.wait_for_exit_with_open_stdin()?;
        assert_eq!(output.status.code(), Some(0));
        assert!(output.stderr.is_empty(), "stderr={:?}", output.stderr);
        assert!(output.stdout.ends_with("%exit\n"), "{:?}", output.stdout);
    }

    std::thread::sleep(RETRY_OBSERVATION);
    let alive_before_second_request = generation_is_alive(generation)?;
    let second_request = sdk.list_sessions(ListSessionsRequest {
        format: None,
        filter: None,
        sort_order: None,
        reversed: false,
    });
    assert!(
        alive_before_second_request && second_request.is_ok(),
        "daemon {generation:?} stopped {:?} after the kill ACK while its SDK connection remained open; second request={second_request:?}",
        acknowledged_at.elapsed()
    );
    let Response::ListSessions(response) = second_request? else {
        panic!("expected list-sessions response");
    };
    assert!(response.command_output().stdout().is_empty());
    std::thread::sleep(RETRY_OBSERVATION);
    assert!(
        generation_is_alive(generation)?,
        "empty daemon {generation:?} stopped before the SDK connection closed"
    );

    drop(sdk);
    assert!(
        wait_for_generation_exit(generation, PROCESS_TIMEOUT)?,
        "empty daemon {generation:?} must stop after the SDK connection closes"
    );
    Ok(())
}

fn start_alpha(
    harness: &CliHarness,
) -> Result<(common::AutoStartCleanup, ProcessGeneration), Box<dyn Error>> {
    let cleanup = harness.auto_start_cleanup()?;
    let created = harness.run_with(
        &["new-session", "-d", "-s", "alpha", "/bin/sleep 30"],
        |command| {
            command.env(BINARY_OVERRIDE_ENV, harness.launcher_path());
        },
    )?;
    assert_success(&created);
    let generation = wait_for_recorded_generation(harness.pid_path(), PROCESS_TIMEOUT)?;
    Ok((cleanup, generation))
}

fn kill_alpha(connection: &mut rmux_client::Connection) -> Result<Response, Box<dyn Error>> {
    Ok(connection.kill_session(KillSessionRequest {
        target: SessionName::new("alpha")?,
        kill_all_except_target: false,
        clear_alerts: false,
        kill_group: false,
    })?)
}

fn empty_list_request() -> ListSessionsRequest {
    ListSessionsRequest {
        format: None,
        filter: None,
        sort_order: None,
        reversed: false,
    }
}

fn assert_empty_sessions(response: Response) -> Result<(), Box<dyn Error>> {
    let Response::ListSessions(response) = response else {
        return Err(format!("expected list-sessions response, got {response:?}").into());
    };
    assert!(response.command_output().stdout().is_empty());
    Ok(())
}

fn wait_for_attach_exit(attach: &mut AttachedSession) -> Result<(), Box<dyn Error>> {
    let (status, attached_output) = attach.wait_for_exit_with_output(PROCESS_TIMEOUT)?;
    assert_eq!(status.code(), Some(0));
    assert!(
        String::from_utf8_lossy(&attached_output).contains("[exited]"),
        "attached output={attached_output:?}"
    );
    attach.assert_restored()?;
    Ok(())
}

fn wait_for_recorded_generation(
    pid_path: &Path,
    timeout: Duration,
) -> Result<ProcessGeneration, Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(pid) = fs::read_to_string(pid_path)
            .and_then(|value| value.trim().parse::<u32>().map_err(std::io::Error::other))
        {
            if let Some(start_tick) = process_start_tick(pid)? {
                return Ok(ProcessGeneration { pid, start_tick });
            }
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "timed out reading a live daemon generation from '{}'",
                pid_path.display()
            )
            .into());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn generation_is_alive(generation: ProcessGeneration) -> Result<bool, Box<dyn Error>> {
    Ok(process_start_tick(generation.pid)? == Some(generation.start_tick))
}

fn wait_for_generation_exit(
    generation: ProcessGeneration,
    timeout: Duration,
) -> Result<bool, Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    loop {
        if !generation_is_alive(generation)? {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn process_start_tick(pid: u32) -> Result<Option<u64>, Box<dyn Error>> {
    let stat = match fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat,
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound
                || error.raw_os_error() == Some(libc::ESRCH) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(error.into()),
    };
    let fields = stat
        .rsplit_once(") ")
        .ok_or_else(|| format!("malformed /proc/{pid}/stat"))?
        .1
        .split_whitespace()
        .collect::<Vec<_>>();
    let start_tick = fields
        .get(19)
        .ok_or_else(|| format!("missing start tick in /proc/{pid}/stat"))?
        .parse::<u64>()?;
    Ok(Some(start_tick))
}

struct ControlProcess {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    stdout: SharedOutput,
    stdout_collector: Option<OutputCollector>,
    stderr_collector: Option<OutputCollector>,
}

impl ControlProcess {
    fn spawn(harness: &CliHarness) -> Result<Self, Box<dyn Error>> {
        let mut command = harness.base_command();
        command
            .args(["-C", "attach-session", "-t", "alpha"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn()?;
        let stdin = child.stdin.take().expect("control stdin is piped");
        let stdout = child.stdout.take().expect("control stdout is piped");
        let stderr = child.stderr.take().expect("control stderr is piped");
        let (stdout, stdout_collector) = spawn_output_collector(stdout);
        let (_stderr, stderr_collector) = spawn_output_collector(stderr);

        Ok(Self {
            child: Some(child),
            stdin: Some(stdin),
            stdout,
            stdout_collector: Some(stdout_collector),
            stderr_collector: Some(stderr_collector),
        })
    }

    fn wait_for_output(&self, expected: &str) -> Result<(), Box<dyn Error>> {
        let deadline = Instant::now() + PROCESS_TIMEOUT;
        while Instant::now() < deadline {
            let output = {
                let bytes = self.stdout.lock().expect("control output lock");
                String::from_utf8_lossy(&bytes).into_owned()
            };
            if output.contains(expected) {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(10));
        }
        let output = {
            let bytes = self.stdout.lock().expect("control output lock");
            String::from_utf8_lossy(&bytes).into_owned()
        };
        Err(format!("timed out waiting for {expected:?} in {output:?}").into())
    }

    fn write_input(&mut self, input: &str) -> Result<(), Box<dyn Error>> {
        let stdin = self.stdin.as_mut().expect("control stdin remains open");
        stdin.write_all(input.as_bytes())?;
        stdin.flush()?;
        Ok(())
    }

    fn wait_for_begin_count(&self, expected: usize) -> Result<(), Box<dyn Error>> {
        let deadline = Instant::now() + PROCESS_TIMEOUT;
        while Instant::now() < deadline {
            let count = {
                let bytes = self.stdout.lock().expect("control output lock");
                String::from_utf8_lossy(&bytes).matches("%begin ").count()
            };
            if count >= expected {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(10));
        }
        Err(format!("timed out waiting for {expected} admitted control frames").into())
    }

    fn wait_for_exit_with_open_stdin(&mut self) -> Result<ControlOutput, Box<dyn Error>> {
        let deadline = Instant::now() + PROCESS_TIMEOUT;
        let status = loop {
            if let Some(status) = self.child_mut().try_wait()? {
                break status;
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "timed out waiting for control client process {}",
                    self.child_mut().id()
                )
                .into());
            }
            thread::sleep(Duration::from_millis(10));
        };
        self.collect_output(status)
    }

    fn collect_output(&mut self, status: ExitStatus) -> Result<ControlOutput, Box<dyn Error>> {
        drop(self.stdin.take());
        drop(self.child.take());
        let stdout = join_output_collector(&mut self.stdout_collector, "stdout")?;
        let stderr = join_output_collector(&mut self.stderr_collector, "stderr")?;
        Ok(ControlOutput {
            status,
            stdout: String::from_utf8(stdout)?,
            stderr: String::from_utf8(stderr)?,
        })
    }

    fn child_mut(&mut self) -> &mut Child {
        self.child.as_mut().expect("control child remains owned")
    }
}

impl Drop for ControlProcess {
    fn drop(&mut self) {
        drop(self.stdin.take());
        if let Some(child) = self.child.as_mut() {
            let _ = terminate_child(child);
        }
        drop(self.child.take());
        let _ = join_output_collector(&mut self.stdout_collector, "stdout");
        let _ = join_output_collector(&mut self.stderr_collector, "stderr");
    }
}

struct ControlOutput {
    status: ExitStatus,
    stdout: String,
    stderr: String,
}

fn spawn_output_collector<R>(mut reader: R) -> (SharedOutput, OutputCollector)
where
    R: Read + Send + 'static,
{
    let shared = Arc::new(Mutex::new(Vec::new()));
    let mirror = Arc::clone(&shared);
    let handle = thread::spawn(move || {
        let mut output = Vec::new();
        let mut buffer = [0_u8; 4096];
        loop {
            let bytes_read = reader.read(&mut buffer)?;
            if bytes_read == 0 {
                break;
            }
            output.extend_from_slice(&buffer[..bytes_read]);
            mirror
                .lock()
                .expect("control output lock")
                .extend_from_slice(&buffer[..bytes_read]);
        }
        Ok(output)
    });
    (shared, handle)
}

fn join_output_collector(
    collector: &mut Option<OutputCollector>,
    label: &str,
) -> Result<Vec<u8>, Box<dyn Error>> {
    let Some(collector) = collector.take() else {
        return Ok(Vec::new());
    };
    collector
        .join()
        .map_err(|_| format!("{label} collector panicked"))?
        .map_err(Into::into)
}
