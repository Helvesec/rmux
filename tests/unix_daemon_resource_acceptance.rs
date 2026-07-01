#![cfg(target_os = "linux")]

mod common;

use std::error::Error;
use std::fs;
use std::thread;
use std::time::Duration;

use common::{assert_success, CliHarness};

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
