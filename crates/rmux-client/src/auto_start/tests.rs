use std::io;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use super::{ensure_server_running_with_probe, AutoStartError};
use crate::{ClientError, ConnectResult, Connection};

// Success-path tests exercise retry behavior, not scheduler precision.
const POLL_SUCCESS_TIMEOUT: Duration = Duration::from_secs(2);
const POLL_INTERVAL: Duration = Duration::from_millis(1);

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
