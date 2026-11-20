#![cfg(unix)]

mod common;

use std::error::Error;
use std::fs;
use std::time::Duration;

use common::{assert_success, stdout, wait_for_socket_directory_empty, CliHarness};
use rmux_proto::RmuxWindowSnapshot;

const SHUTDOWN_CLEANUP_TIMEOUT: Duration = Duration::from_secs(5);

fn export(harness: &CliHarness) -> Result<RmuxWindowSnapshot, Box<dyn Error>> {
    let output = harness.run(&["export-window-snapshot", "-t", "snapshot:0"])?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    Ok(serde_json::from_str(&stdout(&output))?)
}

fn create(harness: &CliHarness) -> Result<(), Box<dyn Error>> {
    assert_success(&harness.run(&[
        "-f",
        "/dev/null",
        "new-session",
        "-d",
        "-s",
        "snapshot",
        "-x",
        "120",
        "-y",
        "40",
    ])?);
    Ok(())
}

#[test]
fn snapshot_rejects_respawned_pane_window_and_restarted_daemon() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("snapshot-runtime-birth")?;
    let _cleanup = harness.auto_start_cleanup()?;
    create(&harness)?;
    let snapshot = export(&harness)?;
    let path = harness.tmpdir().join("snapshot.json");
    fs::write(&path, serde_json::to_vec(&snapshot)?)?;
    let path = path.to_str().unwrap();
    for command in ["respawn-pane", "respawn-window"] {
        let runtime_before = stdout(&harness.run(&[
            "list-panes",
            "-t",
            "snapshot:0",
            "-F",
            "#{pane_id} #{pane_pid} #{pane_tty}",
        ])?);
        assert_success(&harness.run(&[command, "-k", "-t", "snapshot:0"])?);
        let before = export(&harness)?;
        assert_eq!(before.window_id, snapshot.window_id);
        assert_eq!(before.active_pane, snapshot.active_pane);
        assert_ne!(before.runtimes, snapshot.runtimes);
        let runtime_after = stdout(&harness.run(&[
            "list-panes",
            "-t",
            "snapshot:0",
            "-F",
            "#{pane_id} #{pane_pid} #{pane_tty}",
        ])?);
        assert_ne!(runtime_before, runtime_after);
        let rejected = harness.run(&["restore-window-snapshot", "-t", "snapshot:0", path])?;
        assert_eq!(rejected.status.code(), Some(1));
        assert_eq!(
            export(&harness)?,
            before,
            "rejection must preserve the new runtime state"
        );
    }
    assert_success(&harness.run(&["kill-server"])?);
    wait_for_socket_directory_empty(harness.socket_path(), SHUTDOWN_CLEANUP_TIMEOUT)?;
    create(&harness)?;
    let before = export(&harness)?;
    assert_eq!(before.session_id, snapshot.session_id);
    assert_eq!(before.window_id, snapshot.window_id);
    assert_eq!(before.active_pane, snapshot.active_pane);
    assert_ne!(before.runtimes, snapshot.runtimes);
    let rejected = harness.run(&["restore-window-snapshot", "-t", "snapshot:0", path])?;
    assert_eq!(rejected.status.code(), Some(1));
    assert_eq!(export(&harness)?, before);
    Ok(())
}

#[test]
fn snapshot_runtime_guard_survives_linking_and_layout_restore() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("snapshot-linked-runtime")?;
    let _cleanup = harness.auto_start_cleanup()?;
    create(&harness)?;
    assert_success(&harness.run(&["split-window", "-d", "-t", "snapshot:0"])?);
    let snapshot = export(&harness)?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "alias"])?);
    assert_success(&harness.run(&["link-window", "-s", "snapshot:0", "-t", "alias:1"])?);
    assert_success(&harness.run(&["select-layout", "-t", "alias:1", "even-vertical"])?);
    let path = harness.tmpdir().join("snapshot.json");
    fs::write(&path, serde_json::to_vec(&snapshot)?)?;
    assert_success(&harness.run(&[
        "restore-window-snapshot",
        "-t",
        "snapshot:0",
        path.to_str().unwrap(),
    ])?);
    assert_eq!(export(&harness)?, snapshot);
    let alias = harness.run(&["export-window-snapshot", "-t", "alias:1"])?;
    assert!(
        alias.status.success(),
        "{}",
        String::from_utf8_lossy(&alias.stderr)
    );
    assert!(alias.stderr.is_empty());
    let alias: RmuxWindowSnapshot = serde_json::from_str(&stdout(&alias))?;
    assert_eq!(alias.runtimes, snapshot.runtimes);
    assert_eq!(alias.window_id, snapshot.window_id);
    Ok(())
}
