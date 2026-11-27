#![cfg(unix)]

mod common;

use std::error::Error;
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use common::{
    assert_success, cli_command_lock_owner, output_under_cli_command_lock, stderr, stdout,
    CliHarness, CLI_COMMAND_LOCK_LONG_WAIT,
};

fn wait_until(
    what: &str,
    timeout: Duration,
    mut condition: impl FnMut() -> bool,
) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    while !condition() {
        if Instant::now() >= deadline {
            return Err(format!("timed out after {timeout:?} waiting until {what}").into());
        }
        thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

fn lock_held_here() -> bool {
    cli_command_lock_owner() == Some(std::process::id())
}

#[test]
fn long_cli_wait_lets_another_cli_command_run() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("cli-lock-long-wait")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "cat"])?);

    thread::scope(|scope| -> Result<(), Box<dyn Error>> {
        // The wait can only succeed through the second command below; with
        // the lock held for the whole wait, that command queues behind it
        // until the wait times out.
        let waiter = scope.spawn(|| {
            harness
                .run(&[
                    "wait-pane",
                    "-t",
                    "alpha:0.0",
                    "--text",
                    "LOCK_RELEASED",
                    "--timeout",
                    "10s",
                ])
                .map_err(|error| error.to_string())
        });
        wait_until(
            "the waiting command holds the CLI lock",
            Duration::from_secs(5),
            lock_held_here,
        )?;

        let release = harness.run(&["send-keys", "-t", "alpha:0.0", "LOCK_RELEASED", "Enter"]);
        let waited = waiter.join().map_err(|_| "waiting CLI thread panicked")??;
        assert_success(&release?);
        assert_eq!(
            waited.status.code(),
            Some(0),
            "the wait must end through a CLI command run while it waited\nstdout:\n{}\nstderr:\n{}",
            stdout(&waited),
            stderr(&waited)
        );
        Ok(())
    })
}

#[test]
fn cli_lock_stays_held_until_the_daemon_socket_exists() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("cli-lock-startup")?;
    let socket = harness.tmpdir().join("late-daemon.sock");
    let release = harness.tmpdir().join("release");
    // Bounded so a failed assertion cannot leave the command running forever.
    let script = format!(
        "i=0; while [ ! -e '{}' ] && [ $i -lt 1000 ]; do sleep 0.01; i=$((i+1)); done",
        release.display()
    );
    let mut command = Command::new("/bin/sh");
    command
        .args(["-c", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    thread::scope(|scope| -> Result<(), Box<dyn Error>> {
        let runner = scope.spawn(|| output_under_cli_command_lock(command, &socket));
        // Declared after `runner`, so it releases the command before the
        // scope joins it, even when an assertion below panics.
        let release_guard = ReleaseOnDrop(&release);
        wait_until(
            "the command holds the CLI lock",
            Duration::from_secs(5),
            lock_held_here,
        )?;
        thread::sleep(CLI_COMMAND_LOCK_LONG_WAIT * 3);
        assert!(
            lock_held_here() && !runner.is_finished(),
            "a long command must keep the lock while its daemon socket is absent"
        );

        fs::write(&socket, b"")?;
        wait_until(
            "the long wait releases the CLI lock",
            Duration::from_secs(5),
            || !lock_held_here(),
        )?;
        assert!(
            !runner.is_finished(),
            "the lock must be released while the command is still waiting"
        );
        drop(release_guard);
        let output = runner.join().map_err(|_| "lock runner panicked")??;
        assert_eq!(output.status.code(), Some(0));
        Ok(())
    })
}

#[test]
fn command_window_keeps_other_cli_commands_out_until_it_closes() -> Result<(), Box<dyn Error>> {
    let mut harness = CliHarness::new("cli-lock-window")?;
    let _daemon = harness.start_hidden_daemon()?;
    let socket = harness.socket_path().to_owned();
    let window = harness.command_window()?;
    let mut outsider = Command::new("/bin/sh");
    outsider
        .args(["-c", "true"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    thread::scope(|scope| -> Result<(), Box<dyn Error>> {
        let competitor = scope.spawn(|| output_under_cli_command_lock(outsider, &socket));
        // A per-command lock is free between two commands, so the competitor
        // would take it there and the next command would queue behind it.
        for args in [
            &["new-session", "-d", "-s", "alpha", "cat"][..],
            &["rename-session", "-t", "alpha", "beta"][..],
        ] {
            assert_success(&window.run(args)?);
            thread::sleep(CLI_COMMAND_LOCK_LONG_WAIT);
            assert!(
                lock_held_here() && !competitor.is_finished(),
                "a command window must hold the CLI lock between its commands"
            );
        }

        drop(window);
        let output = competitor
            .join()
            .map_err(|_| "competing CLI thread panicked")??;
        assert_eq!(output.status.code(), Some(0));
        Ok(())
    })
}

struct ReleaseOnDrop<'a>(&'a Path);

impl Drop for ReleaseOnDrop<'_> {
    fn drop(&mut self) {
        let _ = fs::write(self.0, b"");
    }
}
