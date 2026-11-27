#![cfg(unix)]

use std::io::{self, Read};
use std::os::unix::net::UnixListener;
use std::process::Command;
use std::time::{Duration, Instant};

/// A wait budget covers the client's time at the daemon, not process start-up.
/// Identity builds launch each selected executable through a verifying
/// launcher, so the window starts when the daemon accepts the connection.
const WAIT_BUDGET: Duration = Duration::from_millis(250);
const SILENT_DAEMON_HOLD: Duration = Duration::from_millis(400);

#[test]
fn public_wait_commands_bound_runtime_resolution() {
    for (index, arguments) in [
        vec![
            "wait-pane",
            "--text",
            "missing",
            "--timeout",
            "50ms",
            "--json",
        ],
        vec![
            "send-keys",
            "--wait-pane-exit",
            "--timeout",
            "50ms",
            "--",
            "Enter",
        ],
    ]
    .into_iter()
    .enumerate()
    {
        let socket = std::env::temp_dir().join(format!(
            "rmux-entry-budget-{}-{index}.sock",
            std::process::id()
        ));
        let listener = UnixListener::bind(&socket).unwrap();
        let server = std::thread::spawn(move || -> io::Result<Duration> {
            let (mut stream, _) = listener.accept()?;
            let accepted = Instant::now();
            // Never answer; the client must release the connection itself.
            stream.set_read_timeout(Some(SILENT_DAEMON_HOLD))?;
            let mut request = [0; 4096];
            assert!(stream.read(&mut request)? > 0);
            while stream.read(&mut request)? > 0 {}
            Ok(accepted.elapsed())
        });
        let rmux = rmux_os::build_identity::cargo_binary(env!("CARGO_BIN_EXE_rmux"));
        let output = Command::new(rmux)
            .arg("-S")
            .arg(&socket)
            .args(arguments)
            .output()
            .unwrap();
        let held = server.join().unwrap();
        std::fs::remove_file(socket).unwrap();
        let held = held.unwrap_or_else(|error| {
            panic!("command {index} still held the silent daemon: {error}, {output:?}")
        });
        assert!(held < WAIT_BUDGET, "command {index}: {held:?}, {output:?}");
        assert_eq!(output.status.code(), Some(1));
        if index == 0 {
            let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(result["error"], "timeout");
            assert_eq!(result["timeout_ms"], 50);
        } else {
            assert!(
                String::from_utf8_lossy(&output.stderr).contains("timed out waiting for pane-exit")
            );
        }
    }
}
