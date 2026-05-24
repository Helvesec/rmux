#![cfg(unix)]

mod common;

use std::error::Error;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use rmux_client::{connect, connect_or_absent, ConnectResult};
use rmux_proto::{
    CommandOutput, ErrorResponse, HasSessionRequest, HasSessionResponse, KillSessionRequest,
    KillSessionResponse, ListSessionsRequest, ListSessionsResponse, NewSessionExtRequest,
    NewSessionRequest, NewSessionResponse, Request, Response, RmuxError, TerminalSize,
};

use common::{session_name, start_server, TestHarness};

const BINARY_OVERRIDE_ENV: &str = "RMUX_INTERNAL_BINARY_PATH";

#[test]
fn new_session_round_trip() -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("new-session");
    let mut server = start_server(&harness)?;

    let response = send_roundtrip(
        harness.socket_path(),
        &Request::NewSession(NewSessionRequest {
            session_name: session_name("alpha"),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }),
    )?;

    assert_eq!(
        response,
        Response::NewSession(NewSessionResponse {
            session_name: session_name("alpha"),
            detached: true,
            output: None,
        })
    );

    server.shutdown()?;
    Ok(())
}

#[test]
fn has_session_returns_true_for_existing_session() -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("has-exists");
    let mut server = start_server(&harness)?;
    let mut connection = connect(harness.socket_path())?;

    connection.roundtrip(&Request::NewSession(NewSessionRequest {
        session_name: session_name("beta"),
        detached: true,
        size: None,
        environment: None,
    }))?;

    let response = connection.roundtrip(&Request::HasSession(HasSessionRequest {
        target: session_name("beta"),
    }))?;

    assert_eq!(
        response,
        Response::HasSession(HasSessionResponse { exists: true })
    );

    server.shutdown()?;
    Ok(())
}

#[test]
fn has_session_returns_false_for_missing_session() -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("has-missing");
    let mut server = start_server(&harness)?;

    let response = send_roundtrip(
        harness.socket_path(),
        &Request::HasSession(HasSessionRequest {
            target: session_name("ghost"),
        }),
    )?;

    assert_eq!(
        response,
        Response::HasSession(HasSessionResponse { exists: false })
    );

    server.shutdown()?;
    Ok(())
}

#[test]
fn kill_session_destroys_existing_session() -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("kill-exists");
    let mut server = start_server(&harness)?;
    let mut connection = connect(harness.socket_path())?;

    connection.roundtrip(&Request::NewSession(NewSessionRequest {
        session_name: session_name("gamma"),
        detached: true,
        size: None,
        environment: None,
    }))?;

    let kill_response = connection.roundtrip(&Request::KillSession(KillSessionRequest {
        target: session_name("gamma"),
        kill_all_except_target: false,
        clear_alerts: false,
    }))?;
    drop(connection);

    assert_eq!(
        kill_response,
        Response::KillSession(KillSessionResponse { existed: true })
    );
    wait_for_absent_server(harness.socket_path(), Duration::from_secs(2))?;

    server.shutdown()?;
    Ok(())
}

#[test]
fn kill_session_is_idempotent_for_missing_session() -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("kill-missing");
    let mut server = start_server(&harness)?;

    let response = send_roundtrip(
        harness.socket_path(),
        &Request::KillSession(KillSessionRequest {
            target: session_name("nonexistent"),
            kill_all_except_target: false,
            clear_alerts: false,
        }),
    )?;

    assert_eq!(
        response,
        Response::Error(ErrorResponse {
            error: RmuxError::SessionNotFound("nonexistent".to_owned()),
        })
    );

    server.shutdown()?;
    Ok(())
}

#[test]
fn kill_session_absent_server_returns_absent_without_creating_a_socket(
) -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("kill-absent");
    assert!(!harness.socket_path().exists());

    let result = connect_or_absent(harness.socket_path())?;
    assert!(matches!(result, ConnectResult::Absent));
    assert!(!harness.socket_path().exists());

    Ok(())
}

#[test]
fn has_session_absent_server_returns_absent_without_spawning_a_daemon_process(
) -> Result<(), Box<dyn Error>> {
    assert_absent_server_does_not_spawn_daemon("has-absent-spawn-check")
}

#[test]
fn kill_session_absent_server_returns_absent_without_spawning_a_daemon_process(
) -> Result<(), Box<dyn Error>> {
    assert_absent_server_does_not_spawn_daemon("kill-absent-spawn-check")
}

#[test]
fn duplicate_session_returns_the_server_error_payload() -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("dup-session");
    let mut server = start_server(&harness)?;
    let mut connection = connect(harness.socket_path())?;

    connection.roundtrip(&Request::NewSession(NewSessionRequest {
        session_name: session_name("dup"),
        detached: true,
        size: None,
        environment: None,
    }))?;

    let response = connection.roundtrip(&Request::NewSession(NewSessionRequest {
        session_name: session_name("dup"),
        detached: true,
        size: None,
        environment: None,
    }))?;

    assert_eq!(
        response,
        Response::Error(ErrorResponse {
            error: RmuxError::DuplicateSession("dup".to_owned()),
        })
    );

    server.shutdown()?;
    Ok(())
}

#[test]
fn multiple_requests_on_same_connection() -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("multi-req");
    let mut server = start_server(&harness)?;
    let mut connection = connect(harness.socket_path())?;

    let create_first = connection.roundtrip(&Request::NewSession(NewSessionRequest {
        session_name: session_name("s1"),
        detached: true,
        size: None,
        environment: None,
    }))?;
    let create_second = connection.roundtrip(&Request::NewSession(NewSessionRequest {
        session_name: session_name("s2"),
        detached: true,
        size: None,
        environment: None,
    }))?;
    let has_first = connection.roundtrip(&Request::HasSession(HasSessionRequest {
        target: session_name("s1"),
    }))?;
    let has_second = connection.roundtrip(&Request::HasSession(HasSessionRequest {
        target: session_name("s2"),
    }))?;

    assert_eq!(
        create_first,
        Response::NewSession(NewSessionResponse {
            session_name: session_name("s1"),
            detached: true,
            output: None,
        })
    );
    assert_eq!(
        create_second,
        Response::NewSession(NewSessionResponse {
            session_name: session_name("s2"),
            detached: true,
            output: None,
        })
    );
    assert_eq!(
        has_first,
        Response::HasSession(HasSessionResponse { exists: true })
    );
    assert_eq!(
        has_second,
        Response::HasSession(HasSessionResponse { exists: true })
    );

    server.shutdown()?;
    Ok(())
}

#[test]
fn grouped_new_session_without_explicit_name_round_trips_through_the_real_socket(
) -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("grouped-new-session-auto-name");
    let mut server = start_server(&harness)?;
    let mut connection = connect(harness.socket_path())?;

    connection.roundtrip(&Request::NewSession(NewSessionRequest {
        session_name: session_name("alpha"),
        detached: true,
        size: None,
        environment: None,
    }))?;

    let grouped = connection.roundtrip(&Request::NewSessionExt(NewSessionExtRequest {
        session_name: None,
        working_directory: None,
        detached: true,
        size: None,
        environment: None,
        group_target: Some(session_name("alpha")),
        attach_if_exists: false,
        detach_other_clients: false,
        kill_other_clients: false,
        flags: None,
        window_name: None,
        print_session_info: true,
        print_format: Some("#{session_name}".to_owned()),
        command: None,
        process_command: None,
        passthrough: false,
    }))?;

    assert_eq!(
        grouped,
        Response::NewSession(NewSessionResponse {
            session_name: session_name("alpha-1"),
            detached: true,
            output: Some(CommandOutput::from_stdout(b"alpha-1\n".to_vec())),
        })
    );

    let listed = connection.roundtrip(&Request::ListSessions(ListSessionsRequest {
        format: Some("#{session_name}".to_owned()),
        filter: None,
        sort_order: None,
        reversed: false,
    }))?;
    assert_eq!(
        listed,
        Response::ListSessions(ListSessionsResponse {
            output: CommandOutput::from_stdout(b"alpha\nalpha-1\n".to_vec()),
        })
    );

    server.shutdown()?;
    Ok(())
}

fn send_roundtrip(socket_path: &Path, request: &Request) -> Result<Response, Box<dyn Error>> {
    let mut connection = connect(socket_path)?;
    Ok(connection.roundtrip(request)?)
}

fn wait_for_absent_server(socket_path: &Path, timeout: Duration) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + timeout;

    loop {
        match connect_or_absent(socket_path)? {
            ConnectResult::Absent => return Ok(()),
            ConnectResult::Connected(_) => {}
        }

        if Instant::now() >= deadline {
            return Err(format!(
                "timed out waiting for server '{}' to become absent",
                socket_path.display()
            )
            .into());
        }

        std::thread::sleep(Duration::from_millis(20));
    }
}

fn assert_absent_server_does_not_spawn_daemon(label: &str) -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new(label);
    let marker_dir = std::env::temp_dir().join(format!(
        "rmux-client-spawn-marker-{label}-{}",
        std::process::id()
    ));
    let marker_path = marker_dir.join("spawned");
    let launcher_path = marker_dir.join("fake-rmux");
    let _guard = env_lock().lock().expect("lock env");
    let previous_value = std::env::var_os(BINARY_OVERRIDE_ENV);
    let _env_restore = EnvVarGuard::new(BINARY_OVERRIDE_ENV, previous_value.clone());

    fs::create_dir_all(&marker_dir)?;
    write_fake_launcher(&launcher_path, &marker_path)?;
    std::env::set_var(BINARY_OVERRIDE_ENV, &launcher_path);

    let result = connect_or_absent(harness.socket_path())?;
    assert!(matches!(result, ConnectResult::Absent));
    assert!(
        !marker_path.exists(),
        "absent-server commands must not auto-start a daemon process"
    );

    let _ = fs::remove_dir_all(marker_dir);
    Ok(())
}

fn write_fake_launcher(launcher_path: &Path, marker_path: &Path) -> Result<(), Box<dyn Error>> {
    fs::write(
        launcher_path,
        format!("#!/bin/sh\ntouch '{}'\n", marker_path.display()),
    )?;
    let mut permissions = fs::metadata(launcher_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(launcher_path, permissions)?;
    Ok(())
}

fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

struct EnvVarGuard {
    name: &'static str,
    previous_value: Option<std::ffi::OsString>,
}

impl EnvVarGuard {
    fn new(name: &'static str, previous_value: Option<std::ffi::OsString>) -> Self {
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
