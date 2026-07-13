#![cfg(unix)]

mod common;

use std::error::Error;
use std::fs;

use common::{assert_success, stdout, terminate_child, CliHarness};

// Regression test for a tmux divergence shared by `new-window` and
// `split-window`: a detached command issued from a *different* working
// directory than the one the session was created in must inherit the caller's
// cwd (matching tmux behavior), not the session's original start directory.
//
// Both subcommands take the same relevant flags (`-d -t <session> -P -F`), so
// the scenario is parametrized over the subcommand name to avoid duplication.
fn detached_command_inherits_caller_cwd(subcommand: &str) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new(&format!("{subcommand}-caller-cwd"))?;
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

    // The detached command is invoked from `caller_dir`. tmux uses the caller's
    // cwd here; rmux must match.
    let created = harness.run_with(
        &[
            subcommand,
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
        created.status.code(),
        Some(0),
        "{subcommand} failed\nstdout:\n{}\nstderr:\n{}",
        stdout(&created),
        common::stderr(&created),
    );

    let reported = stdout(&created);
    let reported = reported.trim_end_matches('\n');

    terminate_child(daemon.child_mut())?;

    assert_eq!(
        reported,
        caller_dir.to_string_lossy(),
        "{subcommand} should inherit the caller's cwd ({}), not the session's start dir ({})",
        caller_dir.display(),
        launch_dir.display(),
    );
    Ok(())
}

#[test]
fn new_window_detached_inherits_caller_cwd_not_session_cwd() -> Result<(), Box<dyn Error>> {
    detached_command_inherits_caller_cwd("new-window")
}

#[test]
fn split_window_detached_inherits_caller_cwd_not_session_cwd() -> Result<(), Box<dyn Error>> {
    detached_command_inherits_caller_cwd("split-window")
}
