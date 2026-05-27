#![cfg(unix)]

mod common;

use std::error::Error;
use std::fs;
use std::process::{Command, Stdio};

use rmux_client::{connect, ensure_server_running, INTERNAL_DAEMON_FLAG};
use rmux_proto::{
    ListSessionsRequest, NewSessionRequest, NewSessionResponse, Request, Response, SessionName,
};

use common::{
    env_lock, terminate_child, unique_socket_path, wait_for_socket, write_hidden_launcher,
    AutoStartCleanup, EnvVarGuard, BINARY_OVERRIDE_ENV, BINARY_OVERRIDE_TEST_OPT_IN_ENV,
};

#[test]
fn hidden_daemon_mode_binds_requested_socket_and_serves_requests() -> Result<(), Box<dyn Error>> {
    let socket_path = unique_socket_path("hidden-daemon");
    let mut child = Command::new(env!("CARGO_BIN_EXE_rmux"))
        .arg(INTERNAL_DAEMON_FLAG)
        .arg(&socket_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;

    if let Err(error) = wait_for_socket(&socket_path, &mut child) {
        let _ = terminate_child(&mut child);
        return Err(error);
    }

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

/// SIGHUP must NOT terminate the daemon — it's the signal an orphaned
/// foreground process receives when its controlling terminal disappears
/// (SSH session torn down, login shell exit, etc.). If the persistent
/// rmux server inherited the terminal and didn't ignore SIGHUP, every
/// SSH disconnect would silently kill the user's long-lived sessions.
///
/// systemd's `app.slice` isolation handles the common case, but the
/// daemon itself must defend in depth: anyone spawning rmux without
/// systemd (the original `rmux -D` path, ad-hoc launches) still needs
/// the same survival guarantee.
#[test]
fn server_ignores_sighup_and_keeps_serving_sessions() -> Result<(), Box<dyn Error>> {
    use rustix::process::{kill_process, Pid, Signal};

    let socket_path = unique_socket_path("sighup-survival");
    let mut child = Command::new(env!("CARGO_BIN_EXE_rmux"))
        .arg(INTERNAL_DAEMON_FLAG)
        .arg(&socket_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;

    if let Err(error) = wait_for_socket(&socket_path, &mut child) {
        let _ = terminate_child(&mut child);
        return Err(error);
    }

    // Establish a session BEFORE the signal so we can prove state survives.
    let mut connection = connect(&socket_path)?;
    let session = SessionName::new("survivor").expect("valid session name");
    let create = connection.roundtrip(&Request::NewSession(NewSessionRequest {
        session_name: session.clone(),
        detached: true,
        size: None,
        environment: None,
    }))?;
    assert!(
        matches!(create, Response::NewSession(_)),
        "session creation returned unexpected response: {create:?}"
    );

    let pid = Pid::from_raw(child.id() as i32).expect("daemon pid is non-zero");
    kill_process(pid, Signal::HUP)?;

    // Give the signal thread a beat to be observed; then prove the daemon
    // is still alive AND its state is intact.
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert!(
        child.try_wait()?.is_none(),
        "daemon exited after SIGHUP (expected to be ignored)"
    );

    let after = connection.roundtrip(&Request::ListSessions(ListSessionsRequest {
        format: None,
        filter: None,
        sort_order: None,
        reversed: false,
    }))?;
    let rendered = match after {
        Response::ListSessions(resp) => String::from_utf8_lossy(resp.output.stdout()).into_owned(),
        other => panic!("expected ListSessions response, got {other:?}"),
    };
    assert!(
        rendered.contains("survivor"),
        "session list lost the pre-SIGHUP session: {rendered}"
    );

    terminate_child(&mut child)?;
    let _ = std::fs::remove_file(&socket_path);
    let _ = std::fs::remove_dir_all(socket_path.parent().expect("socket parent"));
    Ok(())
}

/// A client closing its socket abruptly must not propagate back as a
/// SIGPIPE that kills the daemon. This is what `rmux attach` followed
/// by ^C or a network blip looks like to the server side.
#[test]
fn client_socket_close_does_not_kill_daemon() -> Result<(), Box<dyn Error>> {
    let socket_path = unique_socket_path("client-disconnect");
    let mut child = Command::new(env!("CARGO_BIN_EXE_rmux"))
        .arg(INTERNAL_DAEMON_FLAG)
        .arg(&socket_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;

    if let Err(error) = wait_for_socket(&socket_path, &mut child) {
        let _ = terminate_child(&mut child);
        return Err(error);
    }

    let session = SessionName::new("clientgone").expect("valid session name");
    {
        let mut connection = connect(&socket_path)?;
        connection.roundtrip(&Request::NewSession(NewSessionRequest {
            session_name: session.clone(),
            detached: true,
            size: None,
            environment: None,
        }))?;
        // Drop the connection — abrupt half-close from the daemon's POV.
    }

    std::thread::sleep(std::time::Duration::from_millis(200));
    assert!(
        child.try_wait()?.is_none(),
        "daemon exited after client disconnect (expected to keep serving)"
    );

    // Reconnect on a fresh socket and prove the session survived.
    let mut connection = connect(&socket_path)?;
    let after = connection.roundtrip(&Request::ListSessions(ListSessionsRequest {
        format: None,
        filter: None,
        sort_order: None,
        reversed: false,
    }))?;
    let rendered = match after {
        Response::ListSessions(resp) => String::from_utf8_lossy(resp.output.stdout()).into_owned(),
        other => panic!("expected ListSessions response, got {other:?}"),
    };
    assert!(
        rendered.contains("clientgone"),
        "session list lost the session after client disconnect: {rendered}"
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
