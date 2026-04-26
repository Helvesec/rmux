#![cfg(windows)]

use std::error::Error;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use rmux_client::{connect, socket_path_for_label, Connection, INTERNAL_DAEMON_FLAG};
use rmux_proto::{KillServerRequest, ListSessionsRequest, Request, Response};

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

fn wait_for_connection(
    socket_path: &std::path::Path,
    child: &mut Child,
) -> Result<Connection, Box<dyn Error>> {
    let deadline = Instant::now() + Duration::from_secs(5);

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

fn wait_for_child_exit(child: &mut Child) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + Duration::from_secs(5);

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
