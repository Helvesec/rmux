#![cfg(unix)]

mod common;

use std::error::Error;
use std::fs;

use common::{assert_success, stdout, terminate_child, CliHarness};

// Regression test for a tmux divergence: a detached `new-window` issued from a
// *different* working directory than the one the session was created in must
// inherit the caller's cwd (matching tmux behavior), not the session's original
// start directory.
#[test]
fn new_window_detached_inherits_caller_cwd_not_session_cwd() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("new-window-caller-cwd")?;
    let mut daemon = harness.start_hidden_daemon()?;

    let launch_dir = harness.tmpdir().join("launch");
    let caller_dir = harness.tmpdir().join("caller");
    fs::create_dir_all(&launch_dir)?;
    fs::create_dir_all(&caller_dir)?;
    let launch_dir = fs::canonicalize(&launch_dir)?;
    let caller_dir = fs::canonicalize(&caller_dir)?;

    // Session is created from `launch_dir`, so its start directory is `launch_dir`.
    assert_success(
        &harness.run_with(&["new-session", "-d", "-s", "alpha"], |command| {
            command.current_dir(&launch_dir);
        })?,
    );

    // The detached new-window is invoked from `caller_dir`. tmux uses the
    // caller's cwd here; rmux must match.
    let new_window = harness.run_with(
        &[
            "new-window",
            "-d",
            "-t",
            "alpha",
            "-P",
            "-F",
            "#{pane_current_path}",
        ],
        |command| {
            command.current_dir(&caller_dir);
        },
    )?;
    assert_eq!(
        new_window.status.code(),
        Some(0),
        "new-window failed\nstdout:\n{}\nstderr:\n{}",
        stdout(&new_window),
        common::stderr(&new_window),
    );

    let reported = stdout(&new_window);
    let reported = reported.trim_end_matches('\n');

    terminate_child(daemon.child_mut())?;

    assert_eq!(
        reported,
        caller_dir.to_string_lossy(),
        "new-window should inherit the caller's cwd ({}), not the session's start dir ({})",
        caller_dir.display(),
        launch_dir.display(),
    );
    Ok(())
}
