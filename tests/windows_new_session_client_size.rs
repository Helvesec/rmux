#![cfg(windows)]

use rmux_pty::{ChildCommand, SpawnedPty, TerminalSize};
use std::error::Error;
use std::io;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[path = "support/windows_cli_serial.rs"]
mod windows_cli_serial;

type TestResult<T> = Result<T, Box<dyn Error>>;
const TIMEOUT: Duration = Duration::from_secs(10);

#[test]
fn attached_new_session_uses_client_size_for_window_and_shell_ten_times() -> TestResult<()> {
    let _serial = windows_cli_serial::acquire("new-session-client-size")?;
    for iteration in 0..10 {
        let label = format!(
            "gh214-{}-{}-{iteration}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
        );
        let server = ServerGuard(label.clone());
        let cmd = std::env::var("COMSPEC")?;
        let child = ChildCommand::new(rmux_binary())
            .args([
                "-L",
                &label,
                "-f",
                "NUL",
                "new-session",
                "-s",
                "size",
                "-P",
                "-F",
                "GH214_INITIAL:#{window_width}x#{window_height}",
                &cmd,
                "/d",
                "/q",
            ])
            .size(TerminalSize::new(136, 35))
            .spawn()?;
        let mut client = ClientGuard::new(child, label.clone())?;
        // This response precedes attach/ResizePseudoConsole. Without the fix
        // it deterministically says 80x24 even if the later resize succeeds.
        client.wait_for_output("GH214_INITIAL:136x34")?;
        wait_until(|| {
            Ok(run(
                &label,
                &["list-clients", "-t", "size", "-F", "#{client_session}"],
            )?
            .lines()
            .any(|line| line.trim() == "size"))
        })?;
        assert_eq!(
            run(
                &label,
                &[
                    "display-message",
                    "-p",
                    "-t",
                    "size",
                    "#{window_width}x#{window_height}"
                ]
            )?
            .trim(),
            "136x34",
            "iteration {iteration}"
        );
        run(
            &label,
            &[
                "send-keys",
                "-t",
                "size",
                "-l",
                "cls & mode con & echo GH214_MODE_DONE",
            ],
        )?;
        run(&label, &["send-keys", "-t", "size", "Enter"])?;
        let mut capture = String::new();
        wait_until(|| {
            client.drain_output()?;
            capture = run(&label, &["capture-pane", "-p", "-t", "size"])?;
            Ok(capture.lines().any(|line| line.trim() == "GH214_MODE_DONE"))
        })?;
        // mode con prints rows then columns. Parse numbers, not localized labels.
        let dimensions = capture
            .lines()
            .filter_map(|line| line.split_once(':')?.1.trim().parse::<u16>().ok())
            .take(2)
            .collect::<Vec<_>>();
        assert_eq!(dimensions, [34, 136], "iteration {iteration}: {capture}");
        eprintln!("GH214 iteration {iteration}: initial=136x34 window=136x34 shell=136x34");
        drop(client);
        drop(server);
    }
    Ok(())
}

fn wait_until(mut ready: impl FnMut() -> TestResult<bool>) -> TestResult<()> {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if ready()? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err("client/shell readiness timed out".into());
        }
        thread::sleep(Duration::from_millis(25));
    }
}

struct ClientGuard {
    child: SpawnedPty,
    label: String,
    output: mpsc::Receiver<Vec<u8>>,
    reader: Option<thread::JoinHandle<()>>,
}

impl ClientGuard {
    fn new(child: SpawnedPty, label: String) -> TestResult<Self> {
        let io = child.master().try_clone_io()?;
        let (tx, output) = mpsc::channel();
        let reader = thread::spawn(move || {
            let mut buffer = [0; 8192];
            while let Ok(n) = io.read(&mut buffer) {
                if n == 0 || tx.send(buffer[..n].to_vec()).is_err() {
                    break;
                }
            }
        });
        Ok(Self {
            child,
            label,
            output,
            reader: Some(reader),
        })
    }

    fn drain_output(&self) -> io::Result<Vec<u8>> {
        Ok(self.output.try_iter().flatten().collect())
    }

    fn wait_for_output(&mut self, needle: &str) -> TestResult<()> {
        let mut output = Vec::new();
        let result = wait_until(|| {
            output.extend(self.drain_output()?);
            Ok(String::from_utf8_lossy(&output).contains(needle))
        });
        result.map_err(|error| {
            format!(
                "{error}; expected {needle:?}; PTY={:?}",
                String::from_utf8_lossy(&output)
            )
            .into()
        })
    }
}

impl Drop for ClientGuard {
    fn drop(&mut self) {
        let _ = run(&self.label, &["detach-client"]);
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            let _ = self.drain_output();
            if matches!(self.child.child_mut().try_wait(), Ok(Some(_))) {
                self.child.child().close_pseudoconsole();
                if let Some(reader) = self.reader.take() {
                    let _ = reader.join();
                }
                return;
            }
            thread::sleep(Duration::from_millis(25));
        }
        let _ = self.child.child_mut().terminate_forcefully();
        let _ = self.child.child_mut().wait();
        self.child.child().close_pseudoconsole();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

struct ServerGuard(String);
impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = run(&self.0, &["kill-server"]);
    }
}

fn rmux_binary() -> PathBuf {
    rmux_os::build_identity::resolve(std::ffi::OsStr::new(env!("CARGO_BIN_EXE_rmux")))
        .expect("candidate executable identity")
}

fn run(label: &str, args: &[&str]) -> TestResult<String> {
    let output = Command::new(rmux_binary())
        .args(["-L", label, "-f", "NUL"])
        .args(args)
        .stdin(Stdio::null())
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "{args:?}: status={} stdout={} stderr={}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(String::from_utf8(output.stdout)?)
}
