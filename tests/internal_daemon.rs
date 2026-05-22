#![cfg(unix)]

mod common;

use std::error::Error;
use std::fs;
use std::process::{Command, Stdio};

use rmux_client::{connect, ensure_server_running, INTERNAL_DAEMON_FLAG};
use rmux_proto::{NewSessionRequest, NewSessionResponse, Request, Response, SessionName};

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
    // TODO: Audit that the environment access only happens in single-threaded code.
    unsafe { std::env::set_var(BINARY_OVERRIDE_ENV, &launcher_path) };
    // TODO: Audit that the environment access only happens in single-threaded code.
    unsafe { std::env::set_var(BINARY_OVERRIDE_TEST_OPT_IN_ENV, "1") };

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
