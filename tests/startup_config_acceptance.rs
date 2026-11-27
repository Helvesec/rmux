#![cfg(unix)]

mod common_cross;

use common_cross::CrossPlatformHarness;
use std::error::Error;
use std::fs;
use std::path::Path;
use std::process::Stdio;
use std::thread;
use std::time::{Duration, Instant};

#[test]
fn kill_server_preserves_many_hup_ignoring_jobs_within_client_deadline(
) -> Result<(), Box<dyn Error>> {
    let harness = CrossPlatformHarness::new("global-pty-shutdown")?;
    let ready = harness.tmpdir().join("ready");
    fs::create_dir(&ready)?;
    for index in 0..160 {
        let command = format!(
            "trap '' HUP; printf ready > '{}/{index}'; exec sleep 600",
            ready.display()
        );
        let (verb, target_flag) = if index == 0 {
            ("new-session", "-s")
        } else {
            ("new-window", "-t")
        };
        harness.success([verb, "-d", target_flag, "many", "/bin/sh", "-c", &command])?;
    }
    let ready_deadline = Instant::now() + Duration::from_secs(5);
    while fs::read_dir(&ready)?.count() != 160 {
        assert!(Instant::now() < ready_deadline, "not all 160 PTYs ready");
        thread::sleep(Duration::from_millis(5));
    }
    let pids = harness
        .stdout(["list-panes", "-a", "-F", "#{pane_pid}"])?
        .lines()
        .map(str::parse::<i32>)
        .collect::<Result<Vec<_>, _>>()?;
    assert_eq!(pids.len(), 160);
    assert!(pids.iter().all(|pid| process_exists(*pid)));
    let endpoint = harness.stdout(["display-message", "-p", "#{socket_path}"])?;
    let endpoint = Path::new(endpoint.trim());
    let started = Instant::now();
    let stop = harness
        .command(["kill-server"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    while endpoint.exists() {
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "shutdown never released endpoint"
        );
        thread::sleep(Duration::from_millis(1));
    }
    let released = started.elapsed();
    let unreaped_at_release = pids
        .iter()
        .copied()
        .filter(|pid| process_exists(*pid))
        .collect::<Vec<_>>();
    let output = stop.wait_with_output()?;
    eprintln!(
        "160 PTYs: endpoint released at {released:?}, client={}, unreaped={unreaped_at_release:?}, stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    // tmux closes the terminals without escalating against ignored HUP.
    // Record the observation before explicitly cleaning our own children.
    for &pid in &unreaped_at_release {
        // SAFETY: these are the live jobs spawned and recorded by this fixture.
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
    assert_eq!(
        unreaped_at_release.len(),
        pids.len(),
        "HUP-ignoring jobs must survive terminal shutdown"
    );
    assert!(
        released < Duration::from_secs(15),
        "global shutdown exceeded client deadline"
    );
    assert!(output.status.success(), "kill-server failed");
    Ok(())
}

fn process_exists(pid: i32) -> bool {
    // kill(0) also observes zombies: absence proves the daemon reaped the child.
    let result = unsafe { libc::kill(pid, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}
