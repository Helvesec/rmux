#![cfg(unix)]

mod common;

use std::error::Error;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

use common::{assert_success, stdout, CliHarness};

const BURST_EVENTS: usize = 270;
const CLOSED_SESSIONS: usize = 16;
const BURST_DELIVERY_TIMEOUT: Duration = Duration::from_secs(30);

#[test]
fn slow_lifecycle_hook_does_not_drop_a_large_burst() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("lifecycle-dispatch-backpressure")?;
    let _daemon = harness.start_hidden_daemon()?;
    let state_dir = harness.tmpdir().join("hook-state");
    let hook_path = harness.tmpdir().join("slow-hook.sh");
    let event_path = state_dir.join("events.log");
    let started_path = state_dir.join("started");
    fs::write(
        &hook_path,
        format!(
            "#!/bin/sh\nset -eu\nmkdir -p '{state}'\nif mkdir '{state}/first' 2>/dev/null; then : > '{started}'; sleep 1; fi\nprintf 'event\\n' >> '{events}'\n",
            state = state_dir.display(),
            started = started_path.display(),
            events = event_path.display(),
        ),
    )?;
    let mut permissions = fs::metadata(&hook_path)?.permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&hook_path, permissions)?;

    let config_path = harness.tmpdir().join("lifecycle-burst.conf");
    let config = (1..=BURST_EVENTS)
        .map(|index| format!("rename-window -t lifecycle:0 burst-{index:03}\n"))
        .collect::<String>();
    fs::write(&config_path, config)?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "lifecycle"])?);
    assert_success(&harness.run(&[
        "set-hook",
        "-g",
        "window-renamed",
        &format!("run-shell '{}'", hook_path.display()),
    ])?);
    assert_success(&harness.run(&["rename-window", "-t", "lifecycle:0", "initial-block"])?);
    wait_until(Duration::from_secs(3), || started_path.is_file())?;

    let sourced = harness.run(&[
        "source-file",
        config_path.to_str().expect("utf-8 config path"),
    ])?;
    assert_success(&sourced);
    wait_until(BURST_DELIVERY_TIMEOUT, || {
        fs::read_to_string(&event_path)
            .map(|events| events.lines().count() == BURST_EVENTS + 1)
            .unwrap_or(false)
    })?;

    let events = fs::read_to_string(&event_path)?;
    assert_eq!(events.lines().count(), BURST_EVENTS + 1);
    assert_eq!(
        stdout(&harness.run(&["display-message", "-p", "-t", "lifecycle:0", "#W",])?),
        format!("burst-{BURST_EVENTS:03}\n")
    );
    Ok(())
}

#[test]
fn kill_server_drains_accepted_session_closed_hooks() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("lifecycle-dispatch-shutdown-drain")?;
    let _daemon = harness.start_hidden_daemon()?;
    let hook_path = harness.tmpdir().join("session-closed-hook.sh");
    let event_path = harness.tmpdir().join("session-closed.log");
    fs::write(
        &hook_path,
        format!(
            "#!/bin/sh\nset -eu\nsleep 0.02\nprintf 'closed\\n' >> '{}'\n",
            event_path.display(),
        ),
    )?;
    let mut permissions = fs::metadata(&hook_path)?.permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&hook_path, permissions)?;

    for index in 0..CLOSED_SESSIONS {
        assert_success(&harness.run(&[
            "new-session",
            "-d",
            "-s",
            &format!("closed-{index:02}"),
        ])?);
    }
    assert_success(&harness.run(&[
        "set-hook",
        "-g",
        "session-closed",
        &format!("run-shell '{}'", hook_path.display()),
    ])?);

    assert_success(&harness.run(&["kill-server"])?);
    wait_until(Duration::from_secs(5), || {
        fs::read_to_string(&event_path)
            .map(|events| events.lines().count() == CLOSED_SESSIONS)
            .unwrap_or(false)
    })?;
    assert_eq!(
        fs::read_to_string(&event_path)?.lines().count(),
        CLOSED_SESSIONS
    );
    Ok(())
}

#[test]
fn kill_server_cancels_wait_for_inside_accepted_session_closed_hook() -> Result<(), Box<dyn Error>>
{
    let harness = CliHarness::new("lifecycle-dispatch-shutdown-wait-for")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "blocked-hook"])?);
    assert_success(&harness.run(&[
        "set-hook",
        "-g",
        "session-closed",
        "wait-for lifecycle-shutdown-never-signaled",
    ])?);
    assert_success(&harness.run(&["kill-server"])?);

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = daemon.child_mut().try_wait()? {
            assert!(status.success(), "hidden daemon exited with {status}");
            break;
        }
        if Instant::now() >= deadline {
            return Err("kill-server remained blocked in a lifecycle hook wait-for".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    Ok(())
}

fn wait_until(
    timeout: Duration,
    mut condition: impl FnMut() -> bool,
) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    while !condition() {
        if Instant::now() >= deadline {
            return Err("timed out waiting for lifecycle hook evidence".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}
