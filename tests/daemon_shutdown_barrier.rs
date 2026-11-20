#![cfg(unix)]

mod common_cross;

use common_cross::CrossPlatformHarness;
use rmux_os::process::ProcessInspector;
use std::error::Error;
use std::ffi::OsStr;
use std::fs;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

#[test]
fn shutdown_reaps_panes_before_endpoint_reuse_for_startup_and_source_file(
) -> Result<(), Box<dyn Error>> {
    let mut premature_releases = Vec::new();
    for startup in [true, false] {
        let route = if startup { "startup" } else { "source-file" };
        let harness = CrossPlatformHarness::new(route)?;
        let config = harness.tmpdir().join("clustered.conf");
        if startup {
            fs::write(&config, "new-session -dP -s configured\n")?;
            harness.success([
                OsStr::new("-f"),
                config.as_os_str(),
                OsStr::new("new-session"),
                OsStr::new("-d"),
                OsStr::new("-s"),
                OsStr::new("requested"),
            ])?;
            harness.success(["has-session", "-t", "configured"])?;
        } else {
            harness.success(["new-session", "-d", "-s", "requested", "-n", "zero"])?;
            harness.success(["new-window", "-d", "-t", "requested:1", "-n", "one"])?;
            fs::write(&config, "swap-window -ds requested:0 -trequested:1\n")?;
            harness.success([OsStr::new("source-file"), config.as_os_str()])?;
            assert_eq!(
                harness.stdout([
                    "list-windows",
                    "-t",
                    "requested",
                    "-F",
                    "#{window_index}:#{window_name}"
                ])?,
                "0:one\n1:zero\n"
            );
        }

        // Ignoring HUP forces the real bounded PTY destructor to use its grace.
        // Several panes keep that interval observable even on a busy test host.
        let mut pane_pids = Vec::new();
        for index in 0..8 {
            let name = format!("slow{index}");
            let target = format!("requested:{name}");
            harness.success([
                "new-window",
                "-d",
                "-t",
                "requested",
                "-n",
                &name,
                "/bin/sh",
                "-c",
                "trap '' HUP; printf shutdown-ready; exec sleep 30",
            ])?;
            harness.wait_for_capture_contains(&target, "shutdown-ready")?;
            pane_pids.push(
                harness
                    .stdout(["display-message", "-p", "-t", &target, "#{pane_pid}"])?
                    .trim()
                    .parse::<u32>()?,
            );
        }
        let daemon_pid = harness
            .stdout(["display-message", "-p", "#{pid}"])?
            .trim()
            .parse::<u32>()?;
        let endpoint = harness.stdout(["display-message", "-p", "#{socket_path}"])?;
        harness.success(["kill-server"])?;
        assert!(
            !Path::new(endpoint.trim()).exists(),
            "successful stop retained the endpoint"
        );
        let daemon_alive = ProcessInspector.is_live(daemon_pid)? == Some(true);
        let live_panes = pane_pids
            .iter()
            .copied()
            .filter(|pid| {
                ProcessInspector
                    .is_live(*pid)
                    .expect("native pane liveness")
                    == Some(true)
            })
            .collect::<Vec<_>>();
        eprintln!("{route}: endpoint released; daemon={daemon_pid} alive={daemon_alive}; live panes={live_panes:?}");
        if !live_panes.is_empty() {
            premature_releases.push(format!(
                "{route}: daemon alive={daemon_alive}, panes={live_panes:?}"
            ));
        }

        // Preserve the original assertion while allowing this run's real children
        // to finish teardown before a deliberately red test hands back its slot.
        let deadline = Instant::now() + Duration::from_secs(5);
        while ProcessInspector.is_live(daemon_pid)? == Some(true) {
            assert!(
                Instant::now() < deadline,
                "daemon survived bounded shutdown: {daemon_pid}"
            );
            thread::sleep(Duration::from_millis(5));
        }
        for pid in pane_pids {
            assert_ne!(
                ProcessInspector.is_live(pid)?,
                Some(true),
                "unreaped live pane {pid}"
            );
        }
    }
    assert!(
        premature_releases.is_empty(),
        "endpoint reused before its owning daemon reaped panes: {premature_releases:?}"
    );
    Ok(())
}
