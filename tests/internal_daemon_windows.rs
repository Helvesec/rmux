#![cfg(windows)]

use std::error::Error;
use std::ffi::OsString;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use rmux_client::{
    connect, ensure_server_running, socket_path_for_label, Connection, INTERNAL_DAEMON_FLAG,
};
use rmux_proto::{KillServerRequest, ListSessionsRequest, Request, Response};

const BINARY_OVERRIDE_ENV: &str = "RMUX_INTERNAL_BINARY_PATH";
const BINARY_OVERRIDE_TEST_OPT_IN_ENV: &str = "RMUX_ALLOW_INTERNAL_BINARY_OVERRIDE";
const CLIENT_VERSION_OVERRIDE_ENV: &str = "RMUX_INTERNAL_CLIENT_VERSION";
const WINDOWS_DAEMON_COMMAND_TIMEOUT: Duration = Duration::from_secs(20);
const WINDOWS_DAEMON_READY_TIMEOUT: Duration = Duration::from_secs(20);
const WINDOWS_DAEMON_EXIT_TIMEOUT: Duration = Duration::from_secs(20);

#[test]
fn hidden_daemon_mode_serves_windows_ipc_requests() -> Result<(), Box<dyn Error>> {
    let socket_path =
        socket_path_for_label(format!("hidden-daemon-windows-{}", std::process::id()))?;
    let mut child = Command::new(env!("CARGO_BIN_EXE_rmux"))
        .arg(INTERNAL_DAEMON_FLAG)
        .arg(&socket_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;

    let mut connection = match wait_for_connection(&socket_path, &mut child) {
        Ok(connection) => connection,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
    };

    let response = connection.roundtrip(&Request::ListSessions(ListSessionsRequest {
        format: None,
        filter: None,
        sort_order: None,
        reversed: false,
    }))?;
    let Response::ListSessions(response) = response else {
        let _ = child.kill();
        let _ = child.wait();
        return Err("list-sessions did not return a list-sessions response".into());
    };
    assert!(response.output.stdout().is_empty());

    let response = connection.roundtrip(&Request::KillServer(KillServerRequest))?;
    assert!(matches!(response, Response::KillServer(_)));
    wait_for_child_exit(&mut child)?;
    Ok(())
}

#[test]
fn ensure_server_running_auto_starts_windows_hidden_daemon() -> Result<(), Box<dyn Error>> {
    let _guard = env_lock().lock().expect("lock env");
    let previous_binary = std::env::var_os(BINARY_OVERRIDE_ENV);
    let previous_opt_in = std::env::var_os(BINARY_OVERRIDE_TEST_OPT_IN_ENV);
    let _binary_restore = EnvVarGuard::new(BINARY_OVERRIDE_ENV, previous_binary);
    let _opt_in_restore = EnvVarGuard::new(BINARY_OVERRIDE_TEST_OPT_IN_ENV, previous_opt_in);
    std::env::set_var(BINARY_OVERRIDE_ENV, env!("CARGO_BIN_EXE_rmux"));
    std::env::set_var(BINARY_OVERRIDE_TEST_OPT_IN_ENV, "1");
    let socket_path = socket_path_for_label(format!("auto-start-windows-{}", std::process::id()))?;

    let mut connection = ensure_server_running(&socket_path)?;
    let response = connection.roundtrip(&Request::ListSessions(ListSessionsRequest {
        format: None,
        filter: None,
        sort_order: None,
        reversed: false,
    }))?;
    assert!(matches!(response, Response::ListSessions(_)));

    let response = connection.roundtrip(&Request::KillServer(KillServerRequest))?;
    assert!(matches!(response, Response::KillServer(_)));
    Ok(())
}

#[test]
fn start_server_with_captured_output_returns_after_spawning_windows_daemon(
) -> Result<(), Box<dyn Error>> {
    let _guard = env_lock().lock().expect("lock env");
    let previous_binary = std::env::var_os(BINARY_OVERRIDE_ENV);
    let previous_opt_in = std::env::var_os(BINARY_OVERRIDE_TEST_OPT_IN_ENV);
    let _binary_restore = EnvVarGuard::new(BINARY_OVERRIDE_ENV, previous_binary);
    let _opt_in_restore = EnvVarGuard::new(BINARY_OVERRIDE_TEST_OPT_IN_ENV, previous_opt_in);
    std::env::set_var(BINARY_OVERRIDE_ENV, env!("CARGO_BIN_EXE_rmux"));
    std::env::set_var(BINARY_OVERRIDE_TEST_OPT_IN_ENV, "1");

    let socket_path = socket_path_for_label(format!(
        "captured-start-server-windows-{}",
        std::process::id()
    ))?;
    let output = run_rmux_command(&socket_path, &["start-server"])?;
    assert!(
        output.status.success(),
        "captured start-server failed: status={:?}\nstdout={}\nstderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());

    let output = run_rmux_command(&socket_path, &["kill-server"])?;
    assert!(
        output.status.success(),
        "captured kill-server failed: status={:?}\nstdout={}\nstderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    wait_for_daemon_process_absent(&socket_path)?;
    Ok(())
}

#[test]
fn seamless_upgrade_restarts_idle_stale_windows_daemon() -> Result<(), Box<dyn Error>> {
    let _guard = env_lock().lock().expect("lock env");
    let socket_path =
        socket_path_for_label(format!("seamless-upgrade-windows-{}", std::process::id()))?;
    let mut old_daemon = spawn_hidden_daemon(&socket_path)?;
    let old_connection = match wait_for_connection(&socket_path, &mut old_daemon) {
        Ok(connection) => connection,
        Err(error) => {
            let _ = old_daemon.kill();
            let _ = old_daemon.wait();
            return Err(error);
        }
    };
    drop(old_connection);

    let status = run_client_as_newer_version(&socket_path, &["start-server"])?;
    assert!(
        status.success(),
        "newer client failed against idle stale daemon: status={:?}",
        status.code()
    );

    wait_for_child_exit(&mut old_daemon)?;
    let output = run_rmux_command(&socket_path, &["list-sessions"])?;
    assert!(
        output.status.success(),
        "new daemon did not serve detached RPC after seamless upgrade: status={:?}\nstdout={}\nstderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let output = run_rmux_command(&socket_path, &["kill-server"])?;
    assert!(
        output.status.success(),
        "new daemon did not accept kill-server after seamless upgrade: status={:?}\nstdout={}\nstderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    wait_for_daemon_process_absent(&socket_path)?;
    Ok(())
}

#[test]
fn seamless_upgrade_preserves_active_stale_windows_daemon() -> Result<(), Box<dyn Error>> {
    let _guard = env_lock().lock().expect("lock env");
    let socket_path =
        socket_path_for_label(format!("seamless-active-windows-{}", std::process::id()))?;
    let mut old_daemon = spawn_hidden_daemon(&socket_path)?;
    let mut active_connection = match wait_for_connection(&socket_path, &mut old_daemon) {
        Ok(connection) => connection,
        Err(error) => {
            let _ = old_daemon.kill();
            let _ = old_daemon.wait();
            return Err(error);
        }
    };

    let status = run_client_as_newer_version(&socket_path, &["start-server"])?;
    assert!(
        status.success(),
        "newer client failed against active stale daemon: status={:?}",
        status.code()
    );
    assert!(
        old_daemon.try_wait()?.is_none(),
        "active stale daemon must not be replaced while it owns another client connection"
    );

    let response = active_connection.roundtrip(&Request::KillServer(KillServerRequest))?;
    assert!(matches!(response, Response::KillServer(_)));
    drop(active_connection);
    wait_for_child_exit(&mut old_daemon)?;
    Ok(())
}

fn spawn_hidden_daemon(socket_path: &Path) -> Result<Child, Box<dyn Error>> {
    Ok(Command::new(env!("CARGO_BIN_EXE_rmux"))
        .arg(INTERNAL_DAEMON_FLAG)
        .arg(socket_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?)
}

fn run_client_as_newer_version(
    socket_path: &Path,
    args: &[&str],
) -> Result<ExitStatus, Box<dyn Error>> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_rmux"));
    command
        .arg("-S")
        .arg(socket_path)
        .args(args)
        .env(BINARY_OVERRIDE_ENV, env!("CARGO_BIN_EXE_rmux"))
        .env(BINARY_OVERRIDE_TEST_OPT_IN_ENV, "1")
        .env(CLIENT_VERSION_OVERRIDE_ENV, "999.0.0-test");
    run_command_status(command)
}

fn run_rmux_command(socket_path: &Path, args: &[&str]) -> Result<Output, Box<dyn Error>> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_rmux"));
    command.arg("-S").arg(socket_path).args(args);
    run_command_output(command)
}

fn run_command_status(mut command: Command) -> Result<ExitStatus, Box<dyn Error>> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let deadline = Instant::now() + WINDOWS_DAEMON_COMMAND_TIMEOUT;

    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let status = child.wait()?;
            return Err(format!("rmux command timed out: status={:?}", status.code()).into());
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn run_command_output(mut command: Command) -> Result<Output, Box<dyn Error>> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let deadline = Instant::now() + WINDOWS_DAEMON_COMMAND_TIMEOUT;

    loop {
        if child.try_wait()?.is_some() {
            return Ok(child.wait_with_output()?);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let output = child.wait_with_output()?;
            return Err(format!(
                "rmux command timed out: status={:?}\nstdout={}\nstderr={}",
                output.status.code(),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn wait_for_connection(
    socket_path: &std::path::Path,
    child: &mut Child,
) -> Result<Connection, Box<dyn Error>> {
    let deadline = Instant::now() + WINDOWS_DAEMON_READY_TIMEOUT;

    loop {
        if let Some(status) = child.try_wait()? {
            return Err(format!("hidden daemon exited before accepting IPC: {status}").into());
        }

        match connect(socket_path) {
            Ok(connection) => return Ok(connection),
            Err(error) if Instant::now() < deadline => {
                let _ = error;
                thread::sleep(Duration::from_millis(25));
            }
            Err(error) => return Err(Box::new(error)),
        }
    }
}

fn wait_for_daemon_process_absent(socket_path: &Path) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + WINDOWS_DAEMON_EXIT_TIMEOUT;

    loop {
        let process_count = daemon_process_count(socket_path)?;
        if process_count == 0 {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "{process_count} hidden daemon process(es) still match the test pipe after kill-server"
            )
            .into());
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn daemon_process_count(socket_path: &Path) -> Result<usize, Box<dyn Error>> {
    let script = r#"
$needle = $env:RMUX_TEST_PIPE
$count = @(
  Get-CimInstance Win32_Process |
    Where-Object { $_.Name -eq 'rmux.exe' -and $_.CommandLine -like "*$needle*" }
).Count
Write-Output $count
"#;
    let output = Command::new("powershell")
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            script,
        ])
        .env("RMUX_TEST_PIPE", socket_path)
        .stdin(Stdio::null())
        .output()?;
    assert!(
        output.status.success(),
        "failed to query daemon processes: status={:?}\nstderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout)?;
    Ok(stdout.trim().parse()?)
}

fn wait_for_child_exit(child: &mut Child) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + WINDOWS_DAEMON_EXIT_TIMEOUT;

    loop {
        if child.try_wait()?.is_some() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            child.kill()?;
            let _ = child.wait();
            return Err("hidden daemon did not exit after kill-server".into());
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

struct EnvVarGuard {
    name: &'static str,
    previous_value: Option<OsString>,
}

impl EnvVarGuard {
    fn new(name: &'static str, previous_value: Option<OsString>) -> Self {
        Self {
            name,
            previous_value,
        }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match self.previous_value.as_ref() {
            Some(value) => std::env::set_var(self.name, value),
            None => std::env::remove_var(self.name),
        }
    }
}
