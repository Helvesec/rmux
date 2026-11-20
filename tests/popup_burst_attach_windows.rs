#![cfg(windows)]

use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rmux_pty::{ChildCommand, SpawnedPty, TerminalSize};

#[path = "support/windows_cli_serial.rs"]
mod windows_cli_serial;

const IO_TIMEOUT: Duration = Duration::from_secs(15);
const POLL_INTERVAL: Duration = Duration::from_millis(20);
/// The issue #216 reporter's ghostty geometry.
const REPORTER_SIZE: TerminalSize = TerminalSize::new(225, 54);
/// The attached client must still be present this long after the burst.
const ATTACHED_HOLD: Duration = Duration::from_secs(2);
const SESSION: &str = "alpha";
/// Paints all 54 popup rows 40 times in a single ~500 KiB write, then keeps
/// the popup open long enough for the attached client to be observed.
const FULL_SCREEN_BURST_SCRIPT: &str = concat!(
    "import sys,time\n",
    "s=\"\".join(\"\\x1b[%d;1H\\x1b[31m\"%r+\"x\"*225 for r in range(1,55))\n",
    "sys.stdout.write(s*40)\n",
    "sys.stdout.flush()\n",
    "time.sleep(3)\n",
);
/// A painted run no shell prompt or status line produces.
const PAINTED_RUN: &[u8] = b"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx";
const PYTHON_ENV: &str = "RMUX_TEST_PYTHON";

/// Issue #216 through the native Windows attach path: a full-screen popup
/// painting ~500 KiB at once must coalesce into bounded overlay frames instead
/// of overflowing the attach-control backlog, whose prune detaches the client
/// with `attach-control-detach`.
#[test]
fn popup_full_screen_500k_burst_keeps_windows_client_attached() -> Result<(), Box<dyn Error>> {
    let _serial_guard = windows_cli_serial::acquire("popup-burst-attach")?;
    let label = unique_label("popup-burst-attach")?;
    let workdir = std::env::temp_dir().join(&label);
    std::fs::create_dir_all(&workdir)?;
    let exit_log = workdir.join("attach-exit.log");
    let burst_script = workdir.join("burst.py");
    std::fs::write(&burst_script, FULL_SCREEN_BURST_SCRIPT)?;
    let _server = ServerGuard::start(&label, &exit_log)?;

    assert_success(
        rmux_command(&label, &exit_log)
            .args(["new-session", "-d", "-s", SESSION])
            .output()?,
        "new-session",
    )?;
    let mut attach = AttachGuard::spawn(&label, &exit_log)?;
    let client_pid = wait_for_single_client_pid(&label, &exit_log)?;

    let popup_command = format!(
        "\"{}\" \"{}\"",
        python_program().display(),
        burst_script.display()
    );
    let _popup = ChildGuard(
        rmux_command(&label, &exit_log)
            .args([
                "display-popup",
                "-c",
                &client_pid,
                "-B",
                "-E",
                "-w",
                "100%",
                "-h",
                "100%",
                &popup_command,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?,
    );

    let paint_deadline = Instant::now() + IO_TIMEOUT;
    while !attach.output_contains(PAINTED_RUN) {
        if Instant::now() >= paint_deadline {
            return Err("the popup burst never reached the attached terminal".into());
        }
        attach.require_attached(&exit_log)?;
        thread::sleep(POLL_INTERVAL);
    }
    let hold_deadline = Instant::now() + ATTACHED_HOLD;
    while Instant::now() < hold_deadline {
        attach.require_attached(&exit_log)?;
        thread::sleep(POLL_INTERVAL);
    }

    let clients = list_client_pids(&label, &exit_log)?;
    assert_eq!(
        clients.trim(),
        client_pid,
        "the attached client must survive the popup burst"
    );
    let exit_log_text = std::fs::read_to_string(&exit_log).unwrap_or_default();
    assert!(
        !exit_log_text.contains("attach-control-detach"),
        "the popup burst pruned the attach-control backlog: {exit_log_text:?}"
    );
    Ok(())
}

struct AttachGuard {
    child: SpawnedPty,
    output: Arc<Mutex<Vec<u8>>>,
}

impl AttachGuard {
    fn spawn(label: &str, exit_log: &Path) -> Result<Self, Box<dyn Error>> {
        let child = ChildCommand::new(rmux_binary())
            .args(["-L", label, "attach-session", "-t", SESSION])
            .env("RMUX_ATTACH_EXIT_LOG", exit_log.as_os_str())
            .size(REPORTER_SIZE)
            .spawn()?;
        let io = child.master().try_clone_io()?;
        let output = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&output);
        // A live terminal keeps reading; the reader never pauses between reads.
        thread::spawn(move || {
            let mut buffer = [0_u8; 65536];
            while let Ok(bytes_read) = io.read(&mut buffer) {
                if bytes_read == 0 {
                    return;
                }
                sink.lock()
                    .expect("attach output mutex")
                    .extend_from_slice(&buffer[..bytes_read]);
            }
        });
        Ok(Self { child, output })
    }

    fn output_contains(&self, needle: &[u8]) -> bool {
        self.output
            .lock()
            .expect("attach output mutex")
            .windows(needle.len())
            .any(|window| window == needle)
    }

    fn require_attached(&mut self, exit_log: &Path) -> Result<(), Box<dyn Error>> {
        if let Some(status) = self.child.child_mut().try_wait()? {
            return Err(format!(
                "attach client exited during the popup burst ({status:?}); exit log: {:?}",
                std::fs::read_to_string(exit_log).unwrap_or_default()
            )
            .into());
        }
        Ok(())
    }
}

impl Drop for AttachGuard {
    fn drop(&mut self) {
        if matches!(self.child.child_mut().try_wait(), Ok(Some(_))) {
            return;
        }
        let _ = self.child.child_mut().terminate_forcefully();
        let _ = self.child.child_mut().wait();
    }
}

/// Owns the daemon so it runs inside the test's process tree; SSH sessions
/// on Windows may refuse the auto-start breakaway from their job object.
struct ServerGuard {
    label: String,
    exit_log: PathBuf,
    daemon: Child,
}

impl ServerGuard {
    fn start(label: &str, exit_log: &Path) -> Result<Self, Box<dyn Error>> {
        let ready = rmux_os::daemon::StartupReadyEvent::new()?;
        let mut daemon = Command::new(env!("CARGO_BIN_EXE_rmux-daemon"))
            .arg("--__internal-daemon")
            .arg(pipe_name(label)?)
            .arg("--startup-ready-event")
            .arg(ready.name())
            .env("RMUX_ATTACH_EXIT_LOG", exit_log)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        match ready.wait(IO_TIMEOUT) {
            Ok(true) => {}
            Ok(false) => {
                let _ = daemon.kill();
                let _ = daemon.wait();
                return Err("the private daemon did not become ready".into());
            }
            Err(error) => {
                let _ = daemon.kill();
                let _ = daemon.wait();
                return Err(error.into());
            }
        }
        Ok(Self {
            label: label.to_owned(),
            exit_log: exit_log.to_owned(),
            daemon,
        })
    }
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = rmux_command(&self.label, &self.exit_log)
            .arg("kill-server")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let deadline = Instant::now() + IO_TIMEOUT;
        while Instant::now() < deadline {
            match self.daemon.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => thread::sleep(POLL_INTERVAL),
                Err(_) => break,
            }
        }
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

fn pipe_name(label: &str) -> Result<String, Box<dyn Error>> {
    let output = assert_success(
        Command::new(rmux_binary())
            .args(["-L", label, "diagnose", "--json"])
            .stdin(Stdio::null())
            .output()?,
        "resolve the private endpoint",
    )?;
    let diagnostics: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let pipe_name = diagnostics
        .get("socket_path")
        .and_then(serde_json::Value::as_str)
        .ok_or("diagnose output omitted socket_path")?;
    Ok(pipe_name.to_owned())
}

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(Some(_))) {
            return;
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn wait_for_single_client_pid(label: &str, exit_log: &Path) -> Result<String, Box<dyn Error>> {
    let deadline = Instant::now() + IO_TIMEOUT;
    loop {
        let clients = list_client_pids(label, exit_log)?;
        let clients = clients.trim();
        if !clients.is_empty() && !clients.contains('\n') {
            return Ok(clients.to_owned());
        }
        if Instant::now() >= deadline {
            return Err(format!("expected one attached client, got {clients:?}").into());
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn list_client_pids(label: &str, exit_log: &Path) -> Result<String, Box<dyn Error>> {
    let output = rmux_command(label, exit_log)
        .args(["list-clients", "-F", "#{client_pid}"])
        .stdin(Stdio::null())
        .output()?;
    Ok(String::from_utf8_lossy(&output.stdout).replace('\r', ""))
}

fn rmux_command(label: &str, exit_log: &Path) -> Command {
    let mut command = Command::new(rmux_binary());
    command
        .args(["-L", label])
        .env("RMUX_ATTACH_EXIT_LOG", exit_log)
        .env_remove("RMUX")
        .env_remove("TMUX");
    command
}

fn rmux_binary() -> PathBuf {
    rmux_os::build_identity::resolve(std::ffi::OsStr::new(env!("CARGO_BIN_EXE_rmux")))
        .expect("candidate executable identity")
}

fn python_program() -> PathBuf {
    std::env::var_os(PYTHON_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("python"))
}

fn assert_success(
    output: std::process::Output,
    context: &str,
) -> Result<std::process::Output, Box<dyn Error>> {
    if output.status.success() {
        return Ok(output);
    }
    Err(format!(
        "{context} failed: status={:?}\nstdout={}\nstderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
    .into())
}

fn unique_label(prefix: &str) -> Result<String, Box<dyn Error>> {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    Ok(format!("{prefix}-{}-{nanos}", std::process::id()))
}
