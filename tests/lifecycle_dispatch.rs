#![cfg(unix)]

mod common;

use std::error::Error;
use std::fs;
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
    common::executable_fixture::write_executable(
        &hook_path,
        format!(
            "#!/bin/sh\nset -eu\nmkdir -p '{state}'\nif mkdir '{state}/first' 2>/dev/null; then : > '{started}'; sleep 1; fi\nprintf 'event\\n' >> '{events}'\n",
            state = state_dir.display(),
            started = started_path.display(),
            events = event_path.display(),
        ),
        0o700,
    )?;

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
    common::executable_fixture::write_executable(
        &hook_path,
        format!(
            "#!/bin/sh\nset -eu\nsleep 0.02\nprintf 'closed\\n' >> '{}'\n",
            event_path.display(),
        ),
        0o700,
    )?;

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

#[test]
fn a_session_closed_hook_calling_the_server_back_does_not_hold_its_stop(
) -> Result<(), Box<dyn Error>> {
    // tmux 3.7b, measured with this hook and no client attached: kill-server
    // runs the hook, whose server-starting call fails at once with
    // "server exited unexpectedly" (rc=1), and the server dies within about
    // 100 ms; SIGTERM and SIGINT never start the hook. A call left waiting on
    // the stopping server's endpoint held its stop for the whole 5 s
    // lifecycle-hook grace, then lost the end of the hook.
    const PROMPT_STOP: Duration = Duration::from_millis(2500);
    for stop in ["kill-server", "-TERM", "-INT"] {
        let harness = CliHarness::new("lifecycle-dispatch-stop-hook-callback")?;
        let mut daemon = harness.start_hidden_daemon()?;
        assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "sleep 30"])?);
        let hook_log = harness.tmpdir().join("session-closed.log");
        let hook_script = harness.tmpdir().join("session-closed.sh");
        fs::write(
            &hook_script,
            format!(
                "echo start >> '{log}'\n\
                 '{rmux}' -S '{socket}' new-session -d -s zombie >> '{log}' 2>&1\n\
                 echo \"rc=$?\" >> '{log}'\n",
                rmux = harness.base_command().get_program().to_string_lossy(),
                socket = harness.socket_path().display(),
                log = hook_log.display(),
            ),
        )?;
        let hook = format!("run-shell 'sh {}'", hook_script.display());
        assert_success(&harness.run(&["set-hook", "-g", "session-closed", &hook])?);

        let stopped_at = Instant::now();
        if stop == "kill-server" {
            assert_success(&harness.run(&["kill-server"])?);
        } else {
            // The signal goes to the daemon this test started and still owns.
            let signalled = std::process::Command::new("kill")
                .args([stop, &daemon.pid().to_string()])
                .status()?;
            assert!(signalled.success(), "{stop}: kill {signalled:?}");
        }
        daemon.child_mut().wait()?;
        let server_lifetime = stopped_at.elapsed();

        let hook_lines = fs::read_to_string(&hook_log).unwrap_or_default();
        let hook_lines: Vec<&str> = hook_lines.lines().collect();
        assert!(
            server_lifetime < PROMPT_STOP,
            "{stop}: the server lived {server_lifetime:?} after the stop; hook: {hook_lines:?}"
        );
        if stop == "kill-server" {
            assert_eq!(
                hook_lines,
                ["start", "server exited unexpectedly", "rc=1"],
                "{stop}: the hook's call fails at once and the hook runs to its end"
            );
        } else {
            assert!(
                hook_lines.is_empty(),
                "{stop}: no client holds the server, so no session-closed hook runs: {hook_lines:?}"
            );
        }
        let zombie = harness.run(&["has-session", "-t", "zombie"])?;
        assert!(
            !zombie.status.success(),
            "{stop}: the hook reached no newer daemon"
        );
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
