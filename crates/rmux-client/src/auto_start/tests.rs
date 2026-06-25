use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use super::{
    ensure_server_running_with_probe, hidden_daemon_binary_path,
    hidden_daemon_binary_path_for_config, incompatible_daemon_kill_server_command,
    probe_connected_server, startup_readiness_poll_sleep, AutoStartConfig, AutoStartError,
    STARTUP_POLL_INTERVAL,
};
use crate::{ClientError, ConnectResult, Connection};
use rmux_proto::{
    encode_frame, DaemonStatusResponse, ErrorResponse, Response, RmuxError, RMUX_WIRE_VERSION,
};

// Success-path tests exercise retry behavior, not scheduler precision.
const POLL_SUCCESS_TIMEOUT: Duration = Duration::from_secs(2);
const POLL_INTERVAL: Duration = Duration::from_millis(1);
static AUTO_START_ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn unix_startup_readiness_poll_uses_short_backoff() {
    let mut attempt = 0;
    let remaining = Duration::from_secs(1);

    let sleeps = (0..8)
        .map(|_| startup_readiness_poll_sleep(&mut attempt, remaining))
        .collect::<Vec<_>>();

    assert_eq!(
        sleeps,
        [
            Duration::from_millis(1),
            Duration::from_millis(2),
            Duration::from_millis(4),
            Duration::from_millis(8),
            Duration::from_millis(16),
            STARTUP_POLL_INTERVAL,
            STARTUP_POLL_INTERVAL,
            STARTUP_POLL_INTERVAL,
        ]
    );
}

#[test]
fn unix_startup_readiness_poll_respects_remaining_deadline() {
    let mut attempt = 6;

    assert_eq!(
        startup_readiness_poll_sleep(&mut attempt, Duration::from_millis(7)),
        Duration::from_millis(7)
    );
}

#[test]
fn auto_start_prefers_installed_hidden_daemon_sibling() {
    let base = std::env::temp_dir().join(format!("rmux-auto-start-sibling-{}", std::process::id()));
    std::fs::create_dir_all(&base).expect("create temp dir");
    let current = base.join("rmux");
    let daemon = base.join("rmux-daemon");
    std::fs::write(&current, b"cli").expect("write fake cli");
    std::fs::write(&daemon, b"daemon").expect("write fake daemon");

    assert_eq!(hidden_daemon_binary_path(&current), Some(daemon.clone()));
    assert_eq!(hidden_daemon_binary_path(&daemon), None);

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn auto_start_uses_web_capable_cli_binary_when_web_is_required() {
    let base = std::env::temp_dir().join(format!(
        "rmux-auto-start-web-sibling-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&base).expect("create temp dir");
    let current = base.join("rmux");
    let daemon = base.join("rmux-daemon");
    std::fs::write(&current, b"cli").expect("write fake cli");
    std::fs::write(&daemon, b"daemon").expect("write fake daemon");

    let config = AutoStartConfig::disabled().with_web_required();

    assert_eq!(
        hidden_daemon_binary_path_for_config(&current, &config),
        None
    );

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn auto_start_returns_existing_connection_without_launching() {
    let launch_calls = AtomicUsize::new(0);
    let mut connect = || -> Result<ConnectResult, ClientError> {
        let (client, _server) = UnixStream::pair().expect("create unix stream pair");
        let connection = Connection::new(client).expect("connection with timeout");
        Ok(ConnectResult::Connected(connection))
    };
    let mut launch = || -> Result<(), AutoStartError> {
        launch_calls.fetch_add(1, Ordering::Relaxed);
        Ok(())
    };

    let result = ensure_server_running_with_probe(
        PathBuf::from("/tmp/rmux-auto-start-existing.sock").as_path(),
        Duration::from_millis(10),
        Duration::from_millis(1),
        &mut connect,
        &mut launch,
        |_| Ok(()),
    );

    assert!(result.is_ok(), "connected server should be returned");
    assert_eq!(launch_calls.load(Ordering::Relaxed), 0);
}

#[test]
fn auto_start_launches_then_polls_until_connected() {
    let connect_calls = AtomicUsize::new(0);
    let launch_calls = AtomicUsize::new(0);
    let mut connect = || -> Result<ConnectResult, ClientError> {
        let call = connect_calls.fetch_add(1, Ordering::Relaxed);
        if call < 3 {
            return Ok(ConnectResult::Absent);
        }

        let (client, _server) = UnixStream::pair().expect("create unix stream pair");
        let connection = Connection::new(client).expect("connection with timeout");
        Ok(ConnectResult::Connected(connection))
    };
    let mut launch = || -> Result<(), AutoStartError> {
        launch_calls.fetch_add(1, Ordering::Relaxed);
        Ok(())
    };

    let result = ensure_server_running_with_probe(
        PathBuf::from("/tmp/rmux-auto-start-poll.sock").as_path(),
        POLL_SUCCESS_TIMEOUT,
        POLL_INTERVAL,
        &mut connect,
        &mut launch,
        |_| Ok(()),
    );

    assert!(result.is_ok(), "poll loop should eventually connect");
    assert_eq!(launch_calls.load(Ordering::Relaxed), 1);
    assert!(
        connect_calls.load(Ordering::Relaxed) >= 4,
        "expected at least initial absent check plus polling retries"
    );
}

#[test]
fn auto_start_propagates_real_connect_errors_without_launching() {
    let launch_calls = AtomicUsize::new(0);
    let mut connect = || -> Result<ConnectResult, ClientError> {
        Err(ClientError::Io(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "permission denied",
        )))
    };
    let mut launch = || -> Result<(), AutoStartError> {
        launch_calls.fetch_add(1, Ordering::Relaxed);
        Ok(())
    };

    let error = ensure_server_running_with_probe(
        PathBuf::from("/tmp/rmux-auto-start-error.sock").as_path(),
        Duration::from_millis(10),
        Duration::from_millis(1),
        &mut connect,
        &mut launch,
        |_| Ok(()),
    )
    .expect_err("real connect error should fail");

    assert!(matches!(
        error,
        AutoStartError::Client(ClientError::Io(ref io_error))
            if io_error.kind() == io::ErrorKind::PermissionDenied
    ));
    assert_eq!(launch_calls.load(Ordering::Relaxed), 0);
}

#[test]
fn auto_start_propagates_real_poll_errors_after_launch() {
    let call_count = AtomicUsize::new(0);
    let mut connect = || -> Result<ConnectResult, ClientError> {
        let call = call_count.fetch_add(1, Ordering::Relaxed);
        if call == 0 {
            return Ok(ConnectResult::Absent);
        }

        Err(ClientError::Io(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "broken pipe",
        )))
    };
    let mut launch = || -> Result<(), AutoStartError> { Ok(()) };

    let error = ensure_server_running_with_probe(
        PathBuf::from("/tmp/rmux-auto-start-poll-error.sock").as_path(),
        Duration::from_millis(10),
        Duration::from_millis(1),
        &mut connect,
        &mut launch,
        |_| Ok(()),
    )
    .expect_err("poll error should fail");

    assert!(matches!(
        error,
        AutoStartError::Client(ClientError::Io(ref io_error))
            if io_error.kind() == io::ErrorKind::BrokenPipe
    ));
}

#[test]
fn auto_start_retries_transient_poll_errors_after_launch() {
    let call_count = AtomicUsize::new(0);
    let mut connect = || -> Result<ConnectResult, ClientError> {
        let call = call_count.fetch_add(1, Ordering::Relaxed);
        match call {
            0 => Ok(ConnectResult::Absent),
            1 | 2 => Err(ClientError::Io(io::Error::from(io::ErrorKind::WouldBlock))),
            _ => {
                let (client, _server) = UnixStream::pair().expect("create unix stream pair");
                let connection = Connection::new(client).expect("connection with timeout");
                Ok(ConnectResult::Connected(connection))
            }
        }
    };
    let mut launch = || -> Result<(), AutoStartError> { Ok(()) };

    let result = ensure_server_running_with_probe(
        PathBuf::from("/tmp/rmux-auto-start-would-block.sock").as_path(),
        POLL_SUCCESS_TIMEOUT,
        POLL_INTERVAL,
        &mut connect,
        &mut launch,
        |_| Ok(()),
    );

    assert!(result.is_ok(), "transient poll errors should keep polling");
    assert!(
        call_count.load(Ordering::Relaxed) >= 4,
        "expected absent, transient retries, then connected"
    );
}

#[test]
fn auto_start_waits_for_a_ready_response_after_connecting() {
    let connect_call_count = AtomicUsize::new(0);
    let probe_call_count = AtomicUsize::new(0);
    let mut connect = || -> Result<ConnectResult, ClientError> {
        let call = connect_call_count.fetch_add(1, Ordering::Relaxed);
        let (client, server) = UnixStream::pair().expect("create unix stream pair");
        match call {
            0 => Ok(ConnectResult::Absent),
            _ => {
                let connection = Connection::new(client).expect("connection with timeout");
                drop(server);
                Ok(ConnectResult::Connected(connection))
            }
        }
    };
    let mut launch = || -> Result<(), AutoStartError> { Ok(()) };
    let mut probe = |_: &mut Connection| -> Result<(), ClientError> {
        let call = probe_call_count.fetch_add(1, Ordering::Relaxed);
        if call == 0 {
            return Err(ClientError::Io(io::Error::from(io::ErrorKind::WouldBlock)));
        }
        Ok(())
    };

    let result = ensure_server_running_with_probe(
        PathBuf::from("/tmp/rmux-auto-start-ready.sock").as_path(),
        POLL_SUCCESS_TIMEOUT,
        POLL_INTERVAL,
        &mut connect,
        &mut launch,
        &mut probe,
    );

    assert!(
        result.is_ok(),
        "readiness probe should wait for a real response"
    );
    assert!(
        connect_call_count.load(Ordering::Relaxed) >= 3,
        "expected absent, unready connect, then ready connect"
    );
    assert!(
        probe_call_count.load(Ordering::Relaxed) >= 2,
        "expected an unready probe before the ready probe"
    );
}

#[test]
fn probe_connected_server_waits_for_startup_config_to_finish() {
    let (client, mut server) = UnixStream::pair().expect("create unix stream pair");
    server
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("set probe server read timeout");
    server
        .set_write_timeout(Some(Duration::from_secs(2)))
        .expect("set probe server write timeout");

    let server_thread = std::thread::spawn(move || {
        let mut buffer = [0_u8; 1024];
        for config_loading in [true, false] {
            let bytes_read = server
                .read(&mut buffer)
                .expect("read daemon-status request");
            assert!(bytes_read > 0, "daemon-status request should not be empty");
            let response = Response::DaemonStatus(DaemonStatusResponse {
                rmux_version: "test".to_owned(),
                wire_version: RMUX_WIRE_VERSION,
                session_count: 0,
                client_count: 0,
                config_loading,
            });
            let frame = encode_frame(&response).expect("encode daemon-status response");
            server
                .write_all(&frame)
                .expect("write daemon-status response");
        }
    });

    let connection = Connection::new(client).expect("connection with timeout");
    let ready = probe_connected_server(
        connection,
        &AutoStartConfig::disabled(),
        Path::new("/tmp/rmux-probe-loading.sock"),
    )
    .expect("probe should wait until config_loading is false");
    drop(ready);
    server_thread
        .join()
        .expect("probe server thread should exit");
}

#[test]
fn probe_connected_server_allows_legacy_daemon_status_error() {
    let (client, mut server) = UnixStream::pair().expect("create unix stream pair");
    let server_thread = std::thread::spawn(move || {
        let mut buffer = [0_u8; 1024];
        let bytes_read = server
            .read(&mut buffer)
            .expect("read daemon-status request");
        assert!(bytes_read > 0, "daemon-status request should not be empty");
        let response = Response::Error(ErrorResponse {
            error: RmuxError::UnknownCommand("daemon-status".to_owned()),
        });
        let frame = encode_frame(&response).expect("encode daemon-status response");
        server
            .write_all(&frame)
            .expect("write daemon-status response");
    });

    let connection = Connection::new(client).expect("connection with timeout");
    let ready = probe_connected_server(
        connection,
        &AutoStartConfig::disabled(),
        Path::new("/tmp/rmux-probe-legacy.sock"),
    )
    .expect("legacy daemon status errors should remain upgrade-inspectable");
    drop(ready);
    server_thread
        .join()
        .expect("probe server thread should exit");
}

#[test]
fn probe_connected_server_reports_incompatible_wire_version() {
    let (client, mut server) = UnixStream::pair().expect("create unix stream pair");
    let server_thread = std::thread::spawn(move || {
        let mut buffer = [0_u8; 1024];
        let bytes_read = server
            .read(&mut buffer)
            .expect("read daemon-status request");
        assert!(bytes_read > 0, "daemon-status request should not be empty");
        let response = Response::Error(ErrorResponse {
            error: RmuxError::UnsupportedWireVersion {
                got: RMUX_WIRE_VERSION,
                minimum: 1,
                maximum: 1,
            },
        });
        let frame = legacy_wire_v1_frame(&response);
        server
            .write_all(&frame)
            .expect("write legacy daemon-status response");
    });

    let connection = Connection::new(client).expect("connection with timeout");
    let error = probe_connected_server(
        connection,
        &AutoStartConfig::disabled(),
        Path::new("/tmp/rmux-probe-wire-v1.sock"),
    )
    .expect_err("wire v1 daemon should be reported as incompatible");

    assert!(matches!(error, AutoStartError::IncompatibleDaemon { .. }));
    let message = error.to_string();
    assert!(message.contains("running daemon from an older release uses incompatible protocol 1"));
    assert!(message.contains("rmux -S /tmp/rmux-probe-wire-v1.sock kill-server"));
    server_thread
        .join()
        .expect("probe server thread should exit");
}

#[test]
fn incompatible_daemon_command_is_simple_for_default_socket() {
    let default_socket = crate::default_socket_path().expect("default socket path");

    assert_eq!(
        incompatible_daemon_kill_server_command(&default_socket),
        "rmux kill-server"
    );
}

#[test]
fn incompatible_daemon_command_targets_non_default_socket() {
    assert_eq!(
        incompatible_daemon_kill_server_command(Path::new("/tmp/rmux-custom/default")),
        "rmux -S /tmp/rmux-custom/default kill-server"
    );
}

#[test]
fn probe_connected_server_waits_for_reentrant_startup_source_clients() {
    let _guard = AUTO_START_ENV_LOCK
        .lock()
        .expect("auto-start env lock must not be poisoned");
    let original_rmux = std::env::var_os("RMUX");
    let original_depth = std::env::var_os("RMUX_SOURCE_DEPTH");
    let socket = PathBuf::from(format!(
        "/tmp/rmux-reentrant-source-{}.sock",
        std::process::id()
    ));
    std::env::set_var("RMUX", format!("{},123,0", socket.display()));
    std::env::set_var("RMUX_SOURCE_DEPTH", "1");

    let (client, mut server) = UnixStream::pair().expect("create unix stream pair");
    let server_thread = std::thread::spawn(move || {
        let mut buffer = [0_u8; 1024];
        for config_loading in [true, false] {
            let bytes_read = server
                .read(&mut buffer)
                .expect("read daemon-status request");
            assert!(bytes_read > 0, "daemon-status request should not be empty");
            let response = Response::DaemonStatus(DaemonStatusResponse {
                rmux_version: "test".to_owned(),
                wire_version: RMUX_WIRE_VERSION,
                session_count: 0,
                client_count: 0,
                config_loading,
            });
            let frame = encode_frame(&response).expect("encode daemon-status response");
            server
                .write_all(&frame)
                .expect("write daemon-status response");
        }
    });

    let connection = Connection::new(client).expect("connection with timeout");
    let ready = probe_connected_server(connection, &AutoStartConfig::disabled(), &socket)
        .expect("reentrant source client should still wait for config_loading=false");
    drop(ready);
    server_thread
        .join()
        .expect("probe server thread should exit");

    restore_env("RMUX", original_rmux);
    restore_env("RMUX_SOURCE_DEPTH", original_depth);
}

fn restore_env(name: &str, value: Option<std::ffi::OsString>) {
    match value {
        Some(value) => std::env::set_var(name, value),
        None => std::env::remove_var(name),
    }
}

fn legacy_wire_v1_frame(response: &Response) -> Vec<u8> {
    let mut frame = encode_frame(response).expect("encode response");
    assert_eq!(frame.get(1).copied(), Some(RMUX_WIRE_VERSION as u8));
    frame[1] = 1;
    frame
}

#[test]
fn auto_start_times_out_if_server_never_appears() {
    let mut connect = || -> Result<ConnectResult, ClientError> { Ok(ConnectResult::Absent) };
    let mut launch = || -> Result<(), AutoStartError> { Ok(()) };
    let socket_path = PathBuf::from("/tmp/rmux-auto-start-timeout.sock");

    let error = ensure_server_running_with_probe(
        socket_path.as_path(),
        Duration::from_millis(10),
        Duration::from_millis(1),
        &mut connect,
        &mut launch,
        |_| Ok(()),
    )
    .expect_err("missing server should time out");

    assert!(matches!(
        error,
        AutoStartError::TimedOut {
            ref socket_path,
            waited
        } if socket_path == Path::new("/tmp/rmux-auto-start-timeout.sock")
            && waited == Duration::from_millis(10)
    ));
}

#[test]
fn auto_start_treats_competing_startup_success_as_connected() {
    let connect_results = Arc::new(Mutex::new(vec![
        Ok(ConnectResult::Absent),
        Ok(ConnectResult::Absent),
        Ok(ConnectResult::Connected(
            Connection::new(UnixStream::pair().expect("pair").0).expect("connection with timeout"),
        )),
    ]));
    let launch_calls = AtomicUsize::new(0);
    let connect_results_clone = Arc::clone(&connect_results);
    let mut connect = move || -> Result<ConnectResult, ClientError> {
        connect_results_clone
            .lock()
            .expect("lock results")
            .remove(0)
    };
    let mut launch = || -> Result<(), AutoStartError> {
        launch_calls.fetch_add(1, Ordering::Relaxed);
        Ok(())
    };

    let result = ensure_server_running_with_probe(
        PathBuf::from("/tmp/rmux-auto-start-race.sock").as_path(),
        POLL_SUCCESS_TIMEOUT,
        POLL_INTERVAL,
        &mut connect,
        &mut launch,
        |_| Ok(()),
    );

    assert!(
        result.is_ok(),
        "polling success should win even if another daemon bound first"
    );
    assert_eq!(launch_calls.load(Ordering::Relaxed), 1);
}
