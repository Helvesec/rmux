#![cfg(unix)]

mod common;

use common::{
    assert_success, drain_attach_output_bytes, read_until_contains, stdout, AttachedSession,
    CliHarness,
};
use rmux_pty::TerminalSize;
use std::error::Error;
use std::os::unix::fs::FileTypeExt;
use std::path::Path;
use std::time::{Duration, Instant};

/// Identity builds verify the client and the daemon executables before the
/// daemon binds its socket. That start-up depends on host load and is not the
/// behavior under test, so it has its own bound outside the output window.
const DAEMON_LAUNCH_BUDGET: Duration = Duration::from_secs(120);
/// The geometry must be printed within this window once the daemon accepts
/// connections.
const DAEMON_OUTPUT_BUDGET: Duration = Duration::from_secs(10);

#[test]
fn new_session_initial_geometry_matches_client_and_detached_tmux_semantics(
) -> Result<(), Box<dyn Error>> {
    // Measured with tmux 3.7b: -x/-y apply only to detached creation;
    // attached creation subtracts the configured status rows before spawning.
    for (flags, status, expected) in [
        (vec![], "on", "136x34"),
        (vec!["-x", "90"], "on", "136x34"),
        (vec!["-y", "20"], "on", "136x34"),
        (vec!["-x", "90", "-y", "20"], "on", "136x34"),
        (vec![], "off", "136x35"),
        (vec![], "2", "136x33"),
        (vec!["-d"], "on", "80x24"),
        (vec!["-d", "-x", "90"], "on", "90x24"),
        (vec!["-d", "-y", "20"], "on", "80x20"),
        (vec!["-d", "-x", "90", "-y", "20"], "on", "90x20"),
    ] {
        let harness = CliHarness::new("new-session-client-size")?;
        let _cleanup = harness.auto_start_cleanup()?;
        let config = harness.tmpdir().join("size.conf");
        std::fs::write(&config, format!("set -g status {status}\n"))?;
        let initial = harness.tmpdir().join("initial-size");
        let script = format!("stty size > '{}'; exec /bin/sh", initial.display());
        let mut args = vec![
            "-f",
            config.to_str().unwrap(),
            "new-session",
            "-s",
            "size",
            "-P",
            "-F",
            "GH214_INITIAL:#{window_width}x#{window_height}",
        ];
        args.extend(flags.iter().copied());
        args.extend(["/bin/sh", "-c", &script]);
        let launched = Instant::now();
        let mut client =
            AttachedSession::spawn_command(&harness, &args, TerminalSize::new(136, 35))?;
        wait_for_daemon_socket(harness.socket_path(), &mut client)?;
        let connectable = Instant::now();
        // The printed geometry comes from NewSession, before the subsequent attach.
        read_until_contains(
            client.master_mut(),
            &format!("GH214_INITIAL:{expected}"),
            DAEMON_OUTPUT_BUDGET,
        )?;
        eprintln!(
            "flags={flags:?}, status={status}: launch to daemon socket {:?}, \
             daemon socket to geometry {:?}",
            connectable - launched,
            connectable.elapsed()
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        let initial_size = loop {
            if let Ok(value) = std::fs::read_to_string(&initial) {
                if !value.trim().is_empty() {
                    break value;
                }
            }
            assert!(
                Instant::now() < deadline,
                "shell did not report its initial size"
            );
            std::thread::sleep(Duration::from_millis(20));
        };
        let (cols, rows) = expected.split_once('x').unwrap();
        assert_eq!(
            initial_size.split_whitespace().collect::<Vec<_>>(),
            [rows, cols],
            "flags={flags:?}, status={status}"
        );
        let output = harness.run(&[
            "display-message",
            "-p",
            "-t",
            "size",
            "#{window_width}x#{window_height}",
        ])?;
        assert!(
            output.status.success(),
            "display-message failed: {output:?}"
        );
        assert_eq!(stdout(&output).trim(), expected);
        assert_success(&harness.run(&["kill-session", "-t", "size"])?);
        client.wait_for_exit(Duration::from_secs(5))?;
    }
    Ok(())
}

/// Waits until the daemon started by `client` has bound its socket.
fn wait_for_daemon_socket(
    socket: &Path,
    client: &mut AttachedSession,
) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + DAEMON_LAUNCH_BUDGET;
    loop {
        if std::fs::symlink_metadata(socket).is_ok_and(|metadata| metadata.file_type().is_socket())
        {
            return Ok(());
        }
        if let Some(status) = client.child_mut().try_wait()? {
            let output = drain_attach_output_bytes(client.master_mut())?;
            return Err(format!(
                "client exited with {status} before the daemon bound '{}': {:?}",
                socket.display(),
                String::from_utf8_lossy(&output)
            )
            .into());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "daemon did not bind '{}' within {DAEMON_LAUNCH_BUDGET:?}",
                socket.display()
            )
            .into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn s2528_dimension_diagnostics_match_tmux_37b() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("s2528-dimensions")?;
    let _cleanup = harness.auto_start_cleanup()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "dimensions", "/bin/sh"])?);
    for command in ["new-session", "resize-window", "resize-pane"] {
        for (flag, axis) in [("-x", "width"), ("-y", "height")] {
            for (value, reason) in [
                ("65536", "too large"),
                ("70000", "too large"),
                ("2147483648", "too large"),
                ("9223372036854775808", "too large"),
                ("-9223372036854775809", "too small"),
                ("999999999999999999999x", "invalid"),
                ("bad", "invalid"),
                ("-1", "too small"),
            ] {
                // resize-pane accepts absolute sizes through INT_MAX and
                // constrains them to its layout, unlike either window command.
                if command == "resize-pane" && matches!(value, "65536" | "70000") {
                    assert_success(&harness.run(&[command, flag, value, "-t", "dimensions"])?);
                    continue;
                }
                let mut arguments = vec![command, flag, value];
                if command == "new-session" {
                    arguments.push("-d");
                }
                let nested = arguments.join(" ");
                for arguments in [
                    arguments,
                    vec!["if-shell", "-F", "1", &nested],
                    vec!["run-shell", "-C", &nested],
                ] {
                    let output = harness.run(&arguments)?;
                    assert_eq!(
                        output.status.code(),
                        Some(1),
                        "{command} {flag} {value}: {output:?}"
                    );
                    assert!(output.stdout.is_empty(), "{output:?}");
                    assert_eq!(
                        output.stderr,
                        format!("{axis} {reason}\n").as_bytes(),
                        "{command} {flag} {value}"
                    );
                }
            }
        }
    }
    Ok(())
}
