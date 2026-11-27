#![cfg(target_os = "linux")]

mod common;

use std::error::Error;
use std::fs;
use std::thread;
use std::time::Duration;

use common::{assert_success, AttachedSession, CliHarness};
use rmux_pty::TerminalSize;

const DEFAULT_ITERATIONS: usize = 50;
const MAX_FD_DRIFT: i64 = 8;
const MAX_RSS_DRIFT_KIB: i64 = 32 * 1024;

#[test]
fn daemon_fd_and_rss_do_not_drift_under_cli_churn() -> Result<(), Box<dyn Error>> {
    let iterations = std::env::var("RMUX_RESOURCE_ACCEPTANCE_ITERATIONS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(DEFAULT_ITERATIONS);
    let harness = CliHarness::new("daemon-resource-churn")?;
    let daemon = harness.start_hidden_daemon()?;
    let pid = daemon.pid();

    assert_success(&harness.run(&["new-session", "-d", "-s", "resource", "--", "/bin/sh"])?);
    thread::sleep(Duration::from_millis(150));
    let baseline = measure(pid)?;

    for index in 0..iterations {
        assert_status_ok(&harness.run(&[
            "display-message",
            "-p",
            "-t",
            "resource",
            "#{session_name}:#{window_panes}:#{pane_current_command}",
        ])?);
        assert_success(&harness.run(&[
            "send-keys",
            "-t",
            "resource:0.0",
            &format!("echo resource-{index}"),
            "Enter",
        ])?);
        assert_status_ok(&harness.run(&["capture-pane", "-p", "-t", "resource:0.0"])?);
        if index % 5 == 0 {
            assert_success(&harness.run(&["split-window", "-d", "-t", "resource:0.0"])?);
            assert_success(&harness.run(&["kill-pane", "-t", "resource:0.1"])?);
        }
    }

    thread::sleep(Duration::from_millis(250));
    let final_measurement = measure(pid)?;
    let fd_drift = final_measurement.fd_count - baseline.fd_count;
    let rss_drift = final_measurement.rss_kib - baseline.rss_kib;

    assert!(
        fd_drift <= MAX_FD_DRIFT,
        "daemon fd drift exceeded budget: baseline={baseline:?}, final={final_measurement:?}, drift={fd_drift}, iterations={iterations}"
    );
    assert!(
        rss_drift <= MAX_RSS_DRIFT_KIB,
        "daemon RSS drift exceeded budget: baseline={baseline:?}, final={final_measurement:?}, drift={rss_drift}KiB, iterations={iterations}"
    );

    Ok(())
}

fn assert_status_ok(output: &std::process::Output) {
    assert_eq!(
        output.status.code(),
        Some(0),
        "expected successful command, got status {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

const ISSUE_37_CYCLES: usize = 20;
const ISSUE_37_SETTLE: Duration = Duration::from_secs(10);

/// issue #37: an attached daemon under the reported pane churn (short-lived
/// pane commands, `respawn-pane -k`, split then kill) must hold exactly one
/// `/dev/ptmx` descriptor per live pane, instead of accumulating thousands.
#[test]
fn issue_37_attached_pane_churn_holds_one_ptmx_per_live_pane() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("issue-37-ptmx-churn")?;
    let daemon = harness.start_hidden_daemon()?;
    let pid = daemon.pid();
    assert_success(&harness.run(&[
        "new-session",
        "-d",
        "-s",
        "churn",
        "-x",
        "80",
        "-y",
        "24",
        "--",
        "/bin/sh",
    ])?);
    let _attached = AttachedSession::spawn(&harness, "churn", TerminalSize { cols: 80, rows: 24 })?;

    for _ in 0..ISSUE_37_CYCLES {
        assert_success(&harness.run(&["split-window", "-d", "-t", "churn:0.0", "true"])?);
        assert_success(&harness.run(&["new-window", "-d", "-t", "churn", "sleep 0.1"])?);
        assert_success(&harness.run(&["respawn-pane", "-k", "-t", "churn:0.0"])?);
        assert_success(&harness.run(&["split-window", "-d", "-t", "churn:0.0"])?);
        assert_success(&harness.run(&["kill-pane", "-t", "churn:0.1"])?);
    }

    let deadline = std::time::Instant::now() + ISSUE_37_SETTLE;
    loop {
        let panes = live_pane_count(&harness)?;
        let ptmx = ptmx_descriptor_count(pid)?;
        if ptmx == panes {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!(
                "daemon holds {ptmx} /dev/ptmx descriptors for {panes} live panes after {ISSUE_37_CYCLES} churn cycles"
            )
            .into());
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn live_pane_count(harness: &CliHarness) -> Result<usize, Box<dyn Error>> {
    let output = harness.run(&["list-panes", "-a", "-F", "#{pane_id}"])?;
    assert_status_ok(&output);
    Ok(String::from_utf8_lossy(&output.stdout).lines().count())
}

fn ptmx_descriptor_count(pid: u32) -> Result<usize, Box<dyn Error>> {
    let mut count = 0;
    for entry in fs::read_dir(format!("/proc/{pid}/fd"))? {
        if let Ok(target) = fs::read_link(entry?.path()) {
            if target == std::path::Path::new("/dev/ptmx") {
                count += 1;
            }
        }
    }
    Ok(count)
}

#[derive(Debug)]
struct Measurement {
    fd_count: i64,
    rss_kib: i64,
}

fn measure(pid: u32) -> Result<Measurement, Box<dyn Error>> {
    Ok(Measurement {
        fd_count: fd_count(pid)?,
        rss_kib: rss_kib(pid)?,
    })
}

fn fd_count(pid: u32) -> Result<i64, Box<dyn Error>> {
    let count = fs::read_dir(format!("/proc/{pid}/fd"))?.count();
    Ok(i64::try_from(count)?)
}

fn rss_kib(pid: u32) -> Result<i64, Box<dyn Error>> {
    let status = fs::read_to_string(format!("/proc/{pid}/status"))?;
    let line = status
        .lines()
        .find(|line| line.starts_with("VmRSS:"))
        .ok_or("VmRSS not found in /proc status")?;
    let value = line
        .split_whitespace()
        .nth(1)
        .ok_or("VmRSS value missing")?
        .parse::<i64>()?;
    Ok(value)
}
