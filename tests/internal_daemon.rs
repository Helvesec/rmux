#![cfg(unix)]

mod common;

use std::error::Error;
use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use rmux_client::{connect, ensure_server_running, INTERNAL_DAEMON_FLAG};
use rmux_proto::{NewSessionRequest, NewSessionResponse, Request, Response, SessionName};

use common::{
    default_socket_path_in, env_lock, stderr, terminate_child, unique_socket_path, unique_tmpdir,
    wait_for_socket, write_hidden_launcher, AutoStartCleanup, CliHarness, EnvVarGuard,
    BINARY_OVERRIDE_ENV, BINARY_OVERRIDE_TEST_OPT_IN_ENV, STARTUP_TIMEOUT,
};

const CLIENT_VERSION_OVERRIDE_ENV: &str = "RMUX_INTERNAL_CLIENT_VERSION";

#[test]
fn hidden_daemon_mode_binds_requested_socket_and_serves_requests() -> Result<(), Box<dyn Error>> {
    let socket_path = unique_socket_path("hidden-daemon");
    let mut child = Command::new(rmux_os::build_identity::cargo_binary(env!(
        "CARGO_BIN_EXE_rmux"
    )))
    .arg(INTERNAL_DAEMON_FLAG)
    .arg(&socket_path)
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .spawn()?;

    wait_for_socket(&socket_path, &mut child)?;

    let mut connection = connect(&socket_path)?;
    let response = connection.roundtrip(&Request::NewSession(NewSessionRequest {
        session_name: SessionName::new("hidden").expect("valid session name"),
        detached: true,
        size: None,
        environment: None,
    }))?;

    assert_eq!(
        response,
        Response::NewSession(NewSessionResponse {
            session_name: SessionName::new("hidden").expect("valid session name"),
            detached: true,
            output: None,
        })
    );

    terminate_child(&mut child)?;
    let _ = std::fs::remove_file(&socket_path);
    let _ = std::fs::remove_dir_all(socket_path.parent().expect("socket parent"));
    Ok(())
}

#[test]
fn ensure_server_running_reexecs_the_hidden_rmux_daemon() -> Result<(), Box<dyn Error>> {
    let _guard = env_lock().lock().expect("lock env");
    let socket_path = unique_socket_path("auto-start");
    let launcher_dir = socket_path.parent().expect("socket parent");
    let launcher_path = launcher_dir.join("rmux-launcher.sh");
    let pid_path = launcher_dir.join("rmux.pid");
    let previous_value = std::env::var_os(BINARY_OVERRIDE_ENV);
    let previous_opt_in = std::env::var_os(BINARY_OVERRIDE_TEST_OPT_IN_ENV);
    let _env_restore = EnvVarGuard::new(BINARY_OVERRIDE_ENV, previous_value);
    let _opt_in_restore = EnvVarGuard::new(BINARY_OVERRIDE_TEST_OPT_IN_ENV, previous_opt_in);
    let _cleanup = AutoStartCleanup::new(socket_path.clone(), pid_path.clone());

    fs::create_dir_all(launcher_dir)?;
    fs::set_permissions(launcher_dir, fs::Permissions::from_mode(0o700))?;
    write_hidden_launcher(&launcher_path, &pid_path)?;
    std::env::set_var(BINARY_OVERRIDE_ENV, &launcher_path);
    std::env::set_var(BINARY_OVERRIDE_TEST_OPT_IN_ENV, "1");

    let mut connection = ensure_server_running(&socket_path)?;
    let response = connection.roundtrip(&Request::NewSession(NewSessionRequest {
        session_name: SessionName::new("autostart").expect("valid session name"),
        detached: true,
        size: None,
        environment: None,
    }))?;

    assert_eq!(
        response,
        Response::NewSession(NewSessionResponse {
            session_name: SessionName::new("autostart").expect("valid session name"),
            detached: true,
            output: None,
        })
    );

    Ok(())
}

#[test]
fn seamless_upgrade_restarts_idle_stale_daemon_on_real_socket() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("seamless-upgrade")?;
    let _cleanup = harness.auto_start_cleanup()?;
    let mut old_daemon = harness.start_hidden_daemon()?;
    let old_pid = old_daemon.pid();

    let output = harness.run_with(&["new-session", "-d", "-s", "upgraded"], |command| {
        command.env(BINARY_OVERRIDE_ENV, harness.launcher_path());
        command.env(CLIENT_VERSION_OVERRIDE_ENV, "999.0.0-test");
    })?;
    assert!(
        output.status.success(),
        "new-session after seamless upgrade failed: {}",
        stderr(&output)
    );

    wait_for_child_exit(old_daemon.child_mut(), Duration::from_secs(5))?;
    let new_pid = fs::read_to_string(harness.pid_path())?
        .trim()
        .parse::<u32>()?;
    assert_ne!(
        old_pid, new_pid,
        "seamless upgrade must replace the stale idle daemon process"
    );

    let has_session = harness.run(&["has-session", "-t", "upgraded"])?;
    assert!(
        has_session.status.success(),
        "new daemon did not serve the post-upgrade session: {}",
        stderr(&has_session)
    );

    Ok(())
}

fn wait_for_child_exit(child: &mut Child, timeout: Duration) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    loop {
        if child.try_wait()?.is_some() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!("stale daemon process {} did not exit", child.id()).into());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// A hidden daemon still starting when the harness gives up on it must not
/// outlive that failure. The harness then removes its private RMUX_TMPDIR, so
/// a late daemon that resolves its socket from that variable lands on the
/// user's default socket under /tmp.
#[test]
fn failed_hidden_daemon_startup_wait_reaps_the_daemon() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("hidden-startup-failure")?;
    let socket_path = harness.socket_path().to_path_buf();
    let _occupant = ReadinessRefusingOccupant::bind(&socket_path)?;
    let mut daemon = stopped_before_exec(&harness.hidden_daemon_command()).spawn()?;

    let startup = wait_for_socket(&socket_path, &mut daemon);
    let survived = daemon.try_wait()?.is_none();
    if survived {
        // The daemon never ran past its stop, so it bound nothing anywhere.
        daemon.kill()?;
        daemon.wait()?;
    }

    assert!(
        startup.is_err(),
        "the occupant must fail the readiness probe"
    );
    assert!(
        !survived,
        "hidden daemon {} outlived its failed startup wait: {startup:?}",
        daemon.id()
    );
    Ok(())
}

/// A starved host can stop a hidden daemon while its startup wait fails.
/// The daemon handles SIGTERM, and a stopped process runs no handler, so the
/// harness must resume it; otherwise the reap waits out the whole shutdown
/// grace and falls back to SIGKILL, which skips the daemon's cleanup.
#[test]
fn failed_hidden_daemon_startup_wait_resumes_a_stopped_daemon_before_reaping_it(
) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("hidden-startup-stopped")?;
    let socket_path = harness.socket_path().to_path_buf();
    let _occupant = ReadinessRefusingOccupant::bind(&socket_path)?;
    let mut daemon = stopped_before_exec(&harness.hidden_daemon_command()).spawn()?;
    let stopped = wait_until_stopped(&daemon, STARTUP_TIMEOUT);
    if stopped.is_err() {
        let _ = daemon.kill();
        let _ = daemon.wait();
    }
    stopped?;

    let startup = wait_for_socket(&socket_path, &mut daemon);
    let reaped = daemon.try_wait()?;
    if reaped.is_none() {
        // The daemon never ran past its stop, so it bound nothing anywhere.
        daemon.kill()?;
        daemon.wait()?;
    }

    assert!(
        startup.is_err(),
        "the occupant must fail the readiness probe"
    );
    let status = reaped.ok_or_else(|| {
        format!(
            "stopped hidden daemon {} outlived its failed startup wait",
            daemon.id()
        )
    })?;
    assert_eq!(
        status.code(),
        Some(STOPPED_WRAPPER_TERM_EXIT),
        "stopped hidden daemon {} ended with {status} instead of handling its pending SIGTERM",
        daemon.id()
    );
    Ok(())
}

#[test]
fn harness_hidden_daemon_names_its_private_socket() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("hidden-explicit-socket")?;
    let command = harness.hidden_daemon_command();

    let args: Vec<&OsStr> = command.get_args().collect();

    assert_eq!(
        args,
        [
            OsStr::new(INTERNAL_DAEMON_FLAG),
            harness.socket_path().as_os_str()
        ],
        "a hidden daemon left to resolve RMUX_TMPDIR can bind the user's default socket"
    );
    Ok(())
}

#[test]
fn default_socket_path_in_refuses_to_fall_back_to_the_user_default() -> Result<(), Box<dyn Error>> {
    let missing = unique_tmpdir("missing-socket-root");
    assert!(!missing.exists());

    let resolved = default_socket_path_in(&missing);

    assert!(
        resolved.is_err(),
        "a missing private root resolved {resolved:?} outside '{}'",
        missing.display()
    );
    Ok(())
}

/// Exit code of the `stopped_before_exec` shell once it handles SIGTERM.
const STOPPED_WRAPPER_TERM_EXIT: i32 = 143;

/// Runs `command` from a shell that stops itself before the exec, so the
/// child cannot bind or resolve anything until it receives SIGCONT. Like the
/// daemon it stands for, the shell handles SIGTERM: Darwin ends a stopped
/// process at once on a SIGTERM left to its default action, so only a
/// handled SIGTERM needs the resume.
fn stopped_before_exec(command: &Command) -> Command {
    let mut wrapper = Command::new("/bin/sh");
    wrapper
        .arg("-c")
        .arg(format!(
            "trap 'exit {STOPPED_WRAPPER_TERM_EXIT}' TERM; kill -STOP $$ && exec \"$@\""
        ))
        .arg("sh")
        .arg(command.get_program())
        .args(command.get_args())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for (name, value) in command.get_envs() {
        match value {
            Some(value) => wrapper.env(name, value),
            None => wrapper.env_remove(name),
        };
    }
    wrapper
}

/// Waits until `child` reports a stop. The stop report is consumed, which
/// leaves the exit status to `Child`.
fn wait_until_stopped(child: &Child, timeout: Duration) -> Result<(), Box<dyn Error>> {
    let pid = libc::pid_t::try_from(child.id())?;
    let deadline = Instant::now() + timeout;
    loop {
        let mut status: libc::c_int = 0;
        // SAFETY: `child` is an owned, unreaped child, so `pid` names it.
        let waited = unsafe { libc::waitpid(pid, &mut status, libc::WUNTRACED | libc::WNOHANG) };
        if waited == pid && libc::WIFSTOPPED(status) {
            return Ok(());
        }
        if waited != 0 {
            return Err(
                format!("child {pid} did not stop: waitpid {waited}, status {status:#x}").into(),
            );
        }
        if Instant::now() >= deadline {
            return Err(format!("child {pid} did not stop within {timeout:?}").into());
        }
        thread::sleep(Duration::from_millis(10));
    }
}

/// Listens on a private socket path and closes every connection unanswered,
/// so a readiness probe fails at once instead of after the startup bound.
struct ReadinessRefusingOccupant {
    stop: Arc<AtomicBool>,
    acceptor: Option<JoinHandle<()>>,
}

impl ReadinessRefusingOccupant {
    fn bind(socket_path: &Path) -> Result<Self, Box<dyn Error>> {
        let parent = socket_path.parent().expect("socket parent");
        fs::create_dir_all(parent)?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
        let listener = UnixListener::bind(socket_path)?;
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let acceptor_stop = Arc::clone(&stop);
        let acceptor = thread::spawn(move || {
            while !acceptor_stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => drop(stream),
                    Err(_) => thread::sleep(Duration::from_millis(10)),
                }
            }
        });
        Ok(Self {
            stop,
            acceptor: Some(acceptor),
        })
    }
}

impl Drop for ReadinessRefusingOccupant {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(acceptor) = self.acceptor.take() {
            let _ = acceptor.join();
        }
    }
}
