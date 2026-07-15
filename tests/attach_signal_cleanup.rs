#![cfg(unix)]

mod common;

use std::error::Error;
use std::fs::{self, File};
use std::io::{self, Write};
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use common::{
    assert_success, assert_termios_eq, drain_attach_output_bytes, prepare_canonical_termios,
    AttachedSession, CliHarness,
};
use rmux_client::attach_with_terminal;
use rmux_proto::{encode_attach_message, AttachMessage};
use rmux_pty::{PtyPair, TerminalSize};
use rustix::termios::{tcgetattr, LocalModes};

const SIGNAL_EXIT_TIMEOUT: Duration = Duration::from_secs(5);
const HELPER_ENV: &str = "RMUX_ATTACH_BACKPRESSURE_HELPER";
const HELPER_KIND_ENV: &str = "RMUX_ATTACH_BACKPRESSURE_KIND";
const HELPER_READY_ENV: &str = "RMUX_ATTACH_BACKPRESSURE_READY";
const BACKPRESSURE_PAYLOAD_LEN: usize = 512 * 1024;

#[test]
fn sigterm_during_attach_restores_terminal_before_termination() -> Result<(), Box<dyn Error>> {
    assert_attach_signal_cleanup(libc::SIGTERM, "sigterm")
}

#[test]
fn sighup_during_attach_restores_terminal_before_termination() -> Result<(), Box<dyn Error>> {
    assert_attach_signal_cleanup(libc::SIGHUP, "sighup")
}

#[test]
fn sigint_during_attach_restores_terminal_before_termination() -> Result<(), Box<dyn Error>> {
    assert_attach_signal_cleanup(libc::SIGINT, "sigint")
}

#[test]
fn sigquit_during_attach_restores_terminal_before_termination() -> Result<(), Box<dyn Error>> {
    assert_attach_signal_cleanup(libc::SIGQUIT, "sigquit")
}

#[test]
fn sigterm_interrupts_backpressured_attach_data_output() -> Result<(), Box<dyn Error>> {
    assert_backpressured_attach_signal_cleanup(libc::SIGTERM, "data")
}

#[test]
fn sighup_interrupts_backpressured_attach_render_output() -> Result<(), Box<dyn Error>> {
    assert_backpressured_attach_signal_cleanup(libc::SIGHUP, "render")
}

#[test]
fn sigint_interrupts_backpressured_attach_data_output() -> Result<(), Box<dyn Error>> {
    assert_backpressured_attach_signal_cleanup(libc::SIGINT, "data-sigint")
}

#[test]
fn sigquit_interrupts_backpressured_attach_render_output() -> Result<(), Box<dyn Error>> {
    assert_backpressured_attach_signal_cleanup(libc::SIGQUIT, "render-sigquit")
}

#[test]
#[ignore = "subprocess helper for attach output backpressure signal tests"]
fn attach_output_backpressure_subprocess_helper() -> Result<(), Box<dyn Error>> {
    if std::env::var_os(HELPER_ENV).is_none() {
        return Ok(());
    }

    let kind = std::env::var(HELPER_KIND_ENV)?;
    let ready_path = PathBuf::from(
        std::env::var_os(HELPER_READY_ENV)
            .ok_or("backpressure subprocess helper requires a readiness path")?,
    );
    let stdin = io::stdin();
    let terminal = File::from(stdin.as_fd().try_clone_to_owned()?);
    let input = terminal.try_clone()?;
    let terminal_probe = terminal.try_clone()?;
    let blocked_output = terminal.try_clone()?;
    let (client_stream, mut server_stream) = UnixStream::pair()?;

    let server = thread::spawn(move || -> Result<(), Box<dyn Error + Send + Sync>> {
        wait_for_raw_terminal(&terminal_probe, SIGNAL_EXIT_TIMEOUT)?;
        fs::write(&ready_path, b"ready")?;
        let payload = vec![b'X'; BACKPRESSURE_PAYLOAD_LEN];
        let message = match kind.as_str() {
            "data" | "data-sigint" => AttachMessage::Data(payload),
            "render" | "render-sigquit" => AttachMessage::Render(payload),
            other => return Err(format!("unknown backpressure helper kind: {other}").into()),
        };
        server_stream.write_all(&encode_attach_message(&message)?)?;
        server_stream.flush()?;
        Ok(())
    });

    let attach_result = attach_with_terminal(client_stream, &terminal, input, blocked_output);
    let server_result = server
        .join()
        .map_err(|_| "backpressure helper server thread panicked")?;
    server_result.map_err(|error| io::Error::other(error.to_string()))?;
    attach_result?;
    Ok(())
}

fn assert_attach_signal_cleanup(signal: i32, label: &str) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new(&format!("attach-{label}-cleanup"))?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-x", "80", "-y", "24"])?);

    let mut attach = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(80, 24))?;
    attach.wait_for_raw_mode(SIGNAL_EXIT_TIMEOUT)?;

    send_signal(i32::try_from(attach.child_mut().id())?, signal)?;
    let status = attach.wait_for_exit(SIGNAL_EXIT_TIMEOUT)?;
    assert_eq!(
        status.signal(),
        Some(signal),
        "attach must preserve termination by the received signal: {status}"
    );
    attach.assert_restored()?;
    Ok(())
}

fn assert_backpressured_attach_signal_cleanup(
    signal: i32,
    kind: &str,
) -> Result<(), Box<dyn Error>> {
    let pair = PtyPair::open_with_size(TerminalSize::new(80, 24))?;
    let (master, slave) = pair.into_split();
    let mut master = File::from(master.into_owned_fd()?);
    let terminal = File::from(slave.try_clone()?.into_owned_fd());
    let original_termios = prepare_canonical_termios(&terminal)?;
    let ready_path = unique_ready_path(kind)?;
    let _ = fs::remove_file(&ready_path);

    let mut child = Command::new(std::env::current_exe()?)
        .args([
            "--exact",
            "attach_output_backpressure_subprocess_helper",
            "--ignored",
            "--nocapture",
        ])
        .env(HELPER_ENV, "1")
        .env(HELPER_KIND_ENV, kind)
        .env(HELPER_READY_ENV, &ready_path)
        .stdin(Stdio::from(slave.into_owned_fd()))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;

    wait_for_ready_path(&mut child, &ready_path, SIGNAL_EXIT_TIMEOUT)?;
    thread::sleep(Duration::from_millis(500));
    send_signal(i32::try_from(child.id())?, signal)?;
    let status = wait_for_exit_without_output(&mut child, SIGNAL_EXIT_TIMEOUT)?;
    let _ = fs::remove_file(&ready_path);

    assert_eq!(
        status.signal(),
        Some(signal),
        "{kind} backpressure must preserve termination by the received signal: {status}"
    );
    assert_termios_eq(&original_termios, &tcgetattr(&terminal)?);
    let cleanup_output = drain_attach_output_bytes(&mut master)?;
    assert!(
        cleanup_output
            .windows(b"\x1b[?2004l".len())
            .any(|window| window == b"\x1b[?2004l"),
        "terminal cleanup sequence must survive {kind} output backpressure; bytes={:?}",
        &cleanup_output[cleanup_output.len().saturating_sub(256)..]
    );
    Ok(())
}

fn wait_for_raw_terminal(
    terminal: &File,
    timeout: Duration,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let termios = tcgetattr(terminal)?;
        if !termios.local_modes.contains(LocalModes::ICANON)
            && !termios.local_modes.contains(LocalModes::ECHO)
        {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(10));
    }
    Err("attach subprocess never entered raw mode".into())
}

fn wait_for_ready_path(
    child: &mut Child,
    path: &Path,
    timeout: Duration,
) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if path.exists() {
            return Ok(());
        }
        if let Some(status) = child.try_wait()? {
            return Err(format!("attach backpressure helper exited before ready: {status}").into());
        }
        thread::sleep(Duration::from_millis(10));
    }
    let _ = child.kill();
    let _ = child.wait();
    Err("attach backpressure helper did not become ready".into())
}

fn wait_for_exit_without_output(
    child: &mut Child,
    timeout: Duration,
) -> Result<ExitStatus, Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err("attach signal was swallowed by backpressured output".into());
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn send_signal(pid: i32, signal: i32) -> io::Result<()> {
    // SAFETY: the PID belongs to a live child owned by the caller and signal
    // is restricted by tests to the four attach termination signals.
    if unsafe { libc::kill(pid, signal) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn unique_ready_path(kind: &str) -> Result<PathBuf, Box<dyn Error>> {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    Ok(std::env::temp_dir().join(format!(
        "rmux-attach-backpressure-{kind}-{}-{nonce}.ready",
        std::process::id()
    )))
}
