#![cfg(unix)]

mod common;

use std::error::Error;
use std::ffi::OsString;
use std::fs;
use std::os::unix::ffi::OsStringExt;
use std::path::Path;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use common::{assert_clap_failure, assert_success, stderr, stdout, terminate_child, CliHarness};

const ATTACH_TIMEOUT: Duration = Duration::from_secs(5);

#[test]
fn list_windows_sort_activity_creation_and_global_index_match_tmux() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("list-windows-sort-order")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "w0"])?);
    std::thread::sleep(Duration::from_secs(1));
    assert_success(&harness.run(&["new-window", "-d", "-t", "alpha:", "-n", "w1"])?);
    std::thread::sleep(Duration::from_secs(1));
    assert_success(&harness.run(&["new-window", "-d", "-t", "alpha:", "-n", "w2"])?);

    let creation = harness.run(&[
        "list-windows",
        "-t",
        "alpha",
        "-O",
        "creation",
        "-F",
        "#{window_index}",
    ])?;
    assert_eq!(creation.status.code(), Some(0));
    assert!(stderr(&creation).is_empty());
    assert_eq!(stdout(&creation), "0\n1\n2\n");

    let activity = harness.run(&[
        "list-windows",
        "-t",
        "alpha",
        "-O",
        "activity",
        "-F",
        "#{window_index}",
    ])?;
    assert_eq!(activity.status.code(), Some(0));
    assert!(stderr(&activity).is_empty());
    assert_eq!(stdout(&activity), "2\n1\n0\n");

    std::thread::sleep(Duration::from_secs(1));
    assert_success(&harness.run(&["new-session", "-d", "-s", "beta"])?);
    std::thread::sleep(Duration::from_secs(1));
    assert_success(&harness.run(&["new-window", "-d", "-t", "beta:2"])?);
    std::thread::sleep(Duration::from_secs(1));
    assert_success(&harness.run(&["new-window", "-d", "-t", "alpha:3"])?);

    let all_indexes = harness.run(&[
        "list-windows",
        "-a",
        "-O",
        "index",
        "-F",
        "#{session_name}:#{window_index}",
    ])?;
    assert_eq!(all_indexes.status.code(), Some(0));
    assert!(stderr(&all_indexes).is_empty());
    assert_eq!(
        stdout(&all_indexes),
        "alpha:0\nbeta:0\nalpha:1\nalpha:2\nbeta:2\nalpha:3\n"
    );

    let all_creation = harness.run(&[
        "list-windows",
        "-a",
        "-O",
        "creation",
        "-F",
        "#{session_name}:#{window_index}",
    ])?;
    assert_eq!(all_creation.status.code(), Some(0));
    assert!(stderr(&all_creation).is_empty());
    assert_eq!(
        stdout(&all_creation),
        "alpha:0\nalpha:1\nalpha:2\nbeta:0\nbeta:2\nalpha:3\n"
    );

    let all_activity = harness.run(&[
        "list-windows",
        "-a",
        "-O",
        "activity",
        "-F",
        "#{session_name}:#{window_index}",
    ])?;
    assert_eq!(all_activity.status.code(), Some(0));
    assert!(stderr(&all_activity).is_empty());
    assert_eq!(
        stdout(&all_activity),
        "alpha:3\nbeta:2\nbeta:0\nalpha:2\nalpha:1\nalpha:0\n"
    );

    Ok(())
}

#[test]
fn list_panes_sort_activity_creation_match_tmux() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("list-panes-sort-order")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    std::thread::sleep(Duration::from_secs(1));
    assert_success(&harness.run(&["split-window", "-d", "-t", "alpha:0.0"])?);
    std::thread::sleep(Duration::from_secs(1));
    assert_success(&harness.run(&["split-window", "-d", "-t", "alpha:0.0"])?);
    std::thread::sleep(Duration::from_secs(1));
    assert_success(&harness.run(&["select-pane", "-t", "alpha:0.1"])?);
    std::thread::sleep(Duration::from_secs(1));
    assert_success(&harness.run(&["select-pane", "-t", "alpha:0.0"])?);

    let creation = harness.run(&[
        "list-panes",
        "-t",
        "alpha:0",
        "-O",
        "creation",
        "-F",
        "#{pane_index}:#{pane_created}",
    ])?;
    assert_eq!(creation.status.code(), Some(0));
    assert!(stderr(&creation).is_empty());
    let creation_stdout = stdout(&creation);
    let creation_indices = creation_stdout
        .lines()
        .map(|line| line.split_once(':').map(|(index, _)| index).unwrap_or(line))
        .collect::<Vec<_>>();
    assert_eq!(creation_indices, ["0", "2", "1"]);

    let activity = harness.run(&[
        "list-panes",
        "-t",
        "alpha:0",
        "-O",
        "activity",
        "-F",
        "#{pane_index}:#{pane_activity}",
    ])?;
    assert_eq!(activity.status.code(), Some(0));
    assert!(stderr(&activity).is_empty());
    let activity_stdout = stdout(&activity);
    let activity_indices = activity_stdout
        .lines()
        .map(|line| line.split_once(':').map(|(index, _)| index).unwrap_or(line))
        .collect::<Vec<_>>();
    assert_eq!(activity_indices, ["2", "1", "0"]);

    Ok(())
}

#[test]
fn detached_splits_leave_activity_order_and_last_pane_untouched_like_tmux(
) -> Result<(), Box<dyn Error>> {
    // Oracle probes 2026-07-09 (pinned tmux 3.7b): a detached split never
    // activates anything, so before any selection `list-panes -O activity`
    // stays in index order (forward AND reversed), and `select-pane -l`
    // fails with "no last pane". This is pinned at the CLI level because
    // the CLI detached-split path (SplitWindowExt) previously activated
    // the new pane and re-selected the old one, stamping active_point
    // twice per split while handler-level tests stayed green.
    let _guard = window_surface_guard();
    let harness = CliHarness::new("detached-split-activity-last-pane")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["split-window", "-d", "-t", "alpha:0.0"])?);
    assert_success(&harness.run(&["split-window", "-d", "-t", "alpha:0.0"])?);

    let list_activity = |reversed: bool| -> Result<Vec<String>, Box<dyn Error>> {
        let mut args = vec!["list-panes", "-t", "alpha:0", "-O", "activity"];
        if reversed {
            args.push("-r");
        }
        args.extend(["-F", "#{pane_index}"]);
        let output = harness.run(&args)?;
        assert_eq!(output.status.code(), Some(0));
        assert!(stderr(&output).is_empty());
        Ok(stdout(&output).lines().map(str::to_owned).collect())
    };

    assert_eq!(list_activity(false)?, ["0", "1", "2"]);
    assert_eq!(list_activity(true)?, ["0", "1", "2"]);

    let last_pane = harness.run(&["select-pane", "-l", "-t", "alpha:0.0"])?;
    assert_eq!(last_pane.status.code(), Some(1));
    assert!(
        stderr(&last_pane).contains("no last pane"),
        "detached splits must not create a last pane, got: {}",
        stderr(&last_pane)
    );

    let active = harness.run(&["display-message", "-t", "alpha:0", "-p", "#{pane_index}"])?;
    assert_eq!(stdout(&active).trim_end(), "0");

    Ok(())
}

#[test]
fn new_window_detached_keeps_session_target_commands_on_the_current_window(
) -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("new-window-detached-active-window")?;
    let mut daemon = harness.start_hidden_daemon()?;
    let output_path = harness.tmpdir().join("session-target-split.txt");

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["new-window", "-t", "alpha", "-d", "-n", "scratch"])?);
    assert_success(&harness.run(&["split-window", "-v", "-t", "alpha"])?);
    assert_success(&harness.run(&[
        "send-keys",
        "-t",
        "alpha:0.1",
        &format!("printf split-on-zero > {}", shell_quote(&output_path)),
        "Enter",
    ])?);

    wait_for_file_contents(&output_path, "split-on-zero", ATTACH_TIMEOUT)?;

    let missing = harness.run(&[
        "send-keys",
        "-t",
        "alpha:1.1",
        "printf should-not-exist",
        "Enter",
    ])?;
    assert_eq!(missing.status.code(), Some(1));
    assert_eq!(stderr(&missing), "can't find pane: 1\n");

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn new_window_reuses_window_zero_after_killing_it() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("new-window-reuse-index-zero")?;
    let mut daemon = harness.start_hidden_daemon()?;
    let output_path = harness.tmpdir().join("reused-window-zero.txt");

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["new-window", "-t", "alpha", "-d"])?);
    assert_success(&harness.run(&["kill-window", "-t", "alpha:0"])?);

    let missing = harness.run(&["send-keys", "-t", "alpha:0.0", "echo"])?;
    assert_eq!(missing.status.code(), Some(1));
    assert_eq!(stderr(&missing), "can't find window: 0\n");

    assert_success(&harness.run(&["new-window", "-t", "alpha", "-d", "-n", "reused"])?);
    assert_success(&harness.run(&[
        "send-keys",
        "-t",
        "alpha:0.0",
        &format!("printf reused-zero > {}", shell_quote(&output_path)),
        "Enter",
    ])?);

    wait_for_file_contents(&output_path, "reused-zero", ATTACH_TIMEOUT)?;

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn empty_window_id_target_does_not_resolve_to_window_zero() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("empty-window-id-target")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "alpha"])?);

    let before = harness.run(&[
        "list-windows",
        "-t",
        "alpha",
        "-F",
        "#{window_index}:#{window_id}",
    ])?;
    assert_eq!(stdout(&before), "0:@0\n1:@1\n");

    let result = harness.run(&["kill-window", "-t", "@"])?;
    assert_eq!(result.status.code(), Some(1));
    assert!(stdout(&result).is_empty());
    assert_eq!(stderr(&result), "can't find window: @\n");

    let after = harness.run(&[
        "list-windows",
        "-t",
        "alpha",
        "-F",
        "#{window_index}:#{window_id}",
    ])?;
    assert_eq!(stdout(&after), "0:@0\n1:@1\n");

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn new_window_bare_window_name_target_uses_the_existing_window_slot() -> Result<(), Box<dyn Error>>
{
    let _guard = window_surface_guard();
    let harness = CliHarness::new("new-window-bare-window-name-target")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "s", "-n", "sleep"])?);

    let result = harness.run(&["new-window", "-d", "-t", "s", "-n", "made"])?;
    assert_eq!(result.status.code(), Some(1));
    assert!(stdout(&result).is_empty());
    assert_eq!(stderr(&result), "create window failed: index 0 in use\n");

    let windows = harness.run(&[
        "list-windows",
        "-t",
        "s",
        "-F",
        "#{window_index}:#{window_name}",
    ])?;
    assert_eq!(stdout(&windows), "0:sleep\n");

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn new_window_bare_session_target_without_window_match_stays_session_scoped(
) -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("new-window-bare-session-target")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "logs"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "alpha", "-n", "made"])?);

    let windows = harness.run(&[
        "list-windows",
        "-t",
        "alpha",
        "-F",
        "#{window_index}:#{window_name}",
    ])?;
    assert_eq!(stdout(&windows), "0:logs\n1:made\n");

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn new_window_bare_numeric_target_uses_window_index_slot() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("new-window-bare-numeric-target")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "zero"])?);

    let occupied = harness.run(&["new-window", "-d", "-t", "0", "-n", "occupied"])?;
    assert_eq!(occupied.status.code(), Some(1));
    assert_eq!(stderr(&occupied), "create window failed: index 0 in use\n");

    assert_success(&harness.run(&["new-window", "-d", "-t", "5", "-n", "five"])?);

    let windows = harness.run(&[
        "list-windows",
        "-t",
        "alpha",
        "-F",
        "#{window_index}:#{window_name}",
    ])?;
    assert_eq!(stdout(&windows), "0:zero\n5:five\n");

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn new_window_accepts_missing_session_id_window_index() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("new-window-session-id-index")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    let session_id = harness.run(&["display-message", "-p", "-t", "alpha", "#{session_id}"])?;
    assert_eq!(session_id.status.code(), Some(0));
    assert!(stderr(&session_id).is_empty());
    let target = format!("{}:5", stdout(&session_id).trim());

    let created = harness.run(&[
        "new-window",
        "-d",
        "-t",
        &target,
        "-P",
        "-F",
        "#{window_index}:#{window_name}",
        "-n",
        "indexed",
    ])?;

    assert_eq!(created.status.code(), Some(0));
    assert_eq!(stdout(&created), "5:indexed\n");
    assert!(stderr(&created).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn new_window_resolves_colon_session_targets_through_runtime_lookup() -> Result<(), Box<dyn Error>>
{
    let _guard = window_surface_guard();
    let harness = CliHarness::new("new-window-runtime-session-target")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "zero"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "a:", "-n", "prefix"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "=alpha:", "-n", "exact"])?);

    let windows = harness.run(&[
        "list-windows",
        "-t",
        "alpha",
        "-F",
        "#{window_index}:#{window_name}",
    ])?;
    assert_eq!(windows.status.code(), Some(0));
    assert_eq!(stdout(&windows), "0:zero\n1:prefix\n2:exact\n");
    assert!(stderr(&windows).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn new_window_kill_existing_replaces_target_index() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("new-window-kill-existing-index")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "alpha:5", "-n", "old"])?);

    let replaced = harness.run(&[
        "new-window",
        "-d",
        "-k",
        "-t",
        "alpha:5",
        "-P",
        "-F",
        "#{window_index}:#{window_name}",
        "-n",
        "new",
    ])?;

    assert_eq!(replaced.status.code(), Some(0));
    assert_eq!(stdout(&replaced), "5:new\n");
    assert!(stderr(&replaced).is_empty());
    let windows = harness.run(&["list-windows", "-t", "alpha", "-F", "#{window_name}"])?;
    assert_shell_window_names(
        &stdout(&windows),
        &["bash\nnew\n", "zsh\nnew\n", "sh\nnew\n"],
    );

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn new_window_kill_existing_replaces_only_window() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("new-window-kill-existing-only-window")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "old"])?);
    let before = harness.run(&[
        "display-message",
        "-p",
        "-t",
        "alpha",
        "#{window_id}:#{pane_id}:#{window_name}",
    ])?;
    assert_eq!(before.status.code(), Some(0));
    assert!(stderr(&before).is_empty());

    let replaced = harness.run(&[
        "new-window",
        "-k",
        "-t",
        "alpha:0",
        "-P",
        "-F",
        "#{window_index}:#{window_id}:#{pane_id}:#{window_name}",
        "-n",
        "new",
    ])?;

    assert_eq!(replaced.status.code(), Some(0));
    assert!(stderr(&replaced).is_empty());
    let rendered = stdout(&replaced);
    let fields = rendered.trim_end().split(':').collect::<Vec<_>>();
    assert_eq!(fields.len(), 4, "{rendered:?}");
    assert_eq!(fields[0], "0");
    assert_eq!(fields[3], "new");
    assert!(
        !stdout(&before).starts_with(&format!("{}:", fields[1])),
        "new-window -k should allocate a fresh window id"
    );
    assert!(
        !stdout(&before).contains(&format!(":{}:", fields[2])),
        "new-window -k should allocate a fresh pane id"
    );

    let windows = harness.run(&[
        "list-windows",
        "-t",
        "alpha",
        "-F",
        "#{window_index}:#{window_name}",
    ])?;
    assert_eq!(stdout(&windows), "0:new\n");

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn new_window_select_existing_selects_matching_name() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("new-window-select-existing")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "alpha", "-n", "reuse"])?);
    assert_success(&harness.run(&["select-window", "-t", "alpha:0"])?);

    let selected = harness.run(&[
        "new-window",
        "-S",
        "-t",
        "alpha:",
        "-P",
        "-F",
        "#{window_index}:#{window_name}",
        "-n",
        "reuse",
    ])?;

    assert_eq!(selected.status.code(), Some(0));
    assert_eq!(stdout(&selected), "1:reuse\n");
    assert!(stderr(&selected).is_empty());
    let active = harness.run(&["display-message", "-p", "-t", "alpha", "#{window_index}"])?;
    assert_eq!(stdout(&active), "1\n");

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn unlink_window_missing_target_uses_tmux_window_lookup_error() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("unlink-window-missing-target")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    let output = harness.run(&["unlink-window", "-k", "-t", "alpha:6"])?;

    assert_eq!(output.status.code(), Some(1));
    assert!(stdout(&output).is_empty(), "stdout should be empty");
    assert!(
        stderr(&output).contains("can't find window: 6"),
        "stderr should match tmux window lookup failure, got: {}",
        stderr(&output)
    );

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn rename_window_escapes_backslashes_like_tmux() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("rename-window-backslash")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["rename-window", "-t", "alpha:0", r"hello \ wazzup 0"])?);

    let output = harness.run(&["display-message", "-p", "-t", "alpha:0", "#{window_name}"])?;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout(&output), r"hello \\ wazzup 0".to_owned() + "\n");
    assert!(stderr(&output).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn explicit_window_names_disable_automatic_rename_option() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("explicit-window-name-automatic-rename")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "explicit"])?);
    assert_window_automatic_rename_off(&harness, "alpha:0")?;

    assert_success(&harness.run(&["new-window", "-t", "alpha", "-d", "-n", "logs"])?);
    assert_window_automatic_rename_off(&harness, "alpha:1")?;

    assert_success(&harness.run(&["rename-window", "-t", "alpha:1", "renamed"])?);
    assert_window_automatic_rename_off(&harness, "alpha:1")?;

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn automatic_rename_on_refreshes_an_explicit_window_name() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("automatic-rename-on-refreshes-name")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&[
        "new-session",
        "-d",
        "-s",
        "alpha",
        "-n",
        "fixed",
        "sleep 60",
    ])?);
    assert_success(&harness.run(&["rename-window", "-t", "alpha:0", "renamed"])?);

    let before = harness.run(&["list-windows", "-F", "#{window_name}:#{automatic-rename}"])?;
    assert_eq!(before.status.code(), Some(0));
    assert_eq!(stdout(&before), "renamed:0\n");
    assert!(stderr(&before).is_empty());

    assert_success(&harness.run(&[
        "set-window-option",
        "-t",
        "alpha:0",
        "automatic-rename",
        "on",
    ])?);

    let after = harness.run(&["list-windows", "-F", "#{window_name}:#{automatic-rename}"])?;
    assert_eq!(after.status.code(), Some(0));
    assert_eq!(stdout(&after), "sleep:1\n");
    assert!(stderr(&after).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

fn assert_window_automatic_rename_off(
    harness: &CliHarness,
    target: &str,
) -> Result<(), Box<dyn Error>> {
    let shown = harness.run(&["show-window-options", "-t", target, "automatic-rename"])?;
    assert_eq!(shown.status.code(), Some(0));
    assert_eq!(stdout(&shown), "automatic-rename off\n");
    assert!(stderr(&shown).is_empty());

    let formatted = harness.run(&["display-message", "-p", "-t", target, "#{automatic-rename}"])?;
    assert_eq!(formatted.status.code(), Some(0));
    assert_eq!(stdout(&formatted), "0\n");
    assert!(stderr(&formatted).is_empty());
    Ok(())
}

#[test]
fn detached_queue_bare_window_target_uses_latest_session_context() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("detached-queue-latest-target-context")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "0", "-n", "shell"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "1", "-n", "one"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "bob", "-n", "bobwin"])?);

    let output = harness.run(&[
        "display-message",
        "-p",
        "-t",
        "0",
        "#{session_name}:#{window_index}:#{window_name}",
    ])?;

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout(&output), "bob:0:bobwin\n");
    assert!(stderr(&output).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn send_keys_literal_flag_sends_text_without_leaking_the_flag() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("send-keys-literal")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "cat"])?);
    assert_success(&harness.run(&["send-keys", "-t", "alpha:0.0", "-l", "Enter"])?);
    assert_success(&harness.run(&["send-keys", "-t", "alpha:0.0", "Enter"])?);

    let deadline = Instant::now() + ATTACH_TIMEOUT;
    loop {
        let captured = harness.run(&["capture-pane", "-p", "-t", "alpha:0.0", "-S", "-5"])?;
        assert_eq!(captured.status.code(), Some(0));
        let output = stdout(&captured);
        if output.contains("Enter") {
            assert!(
                !output.contains("-lEnter"),
                "literal flag leaked into pane output: {output:?}"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "literal send-keys output did not appear, capture={output:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn send_keys_trailing_semicolon_queue_error_does_not_send_partial_input(
) -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("send-keys-trailing-semicolon-error")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "cat"])?);

    let output = harness.run(&["send-keys", "-t", "alpha:0.0", "xyz;", "final"])?;

    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("unknown command: final"),
        "stderr={:?} stdout={:?}",
        stderr(&output),
        stdout(&output)
    );
    let captured = harness.run(&["capture-pane", "-p", "-t", "alpha:0.0"])?;
    assert_eq!(captured.status.code(), Some(0));
    assert!(
        !stdout(&captured).contains("xyz"),
        "send-keys should not run before queue validation failure: {:?}",
        stdout(&captured)
    );

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn split_window_default_and_horizontal_flag_match_tmux_geometry() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("split-window-direction-geometry")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-x", "120", "-y", "40"])?);
    assert_success(&harness.run(&["split-window", "-t", "alpha"])?);
    let default_split = harness.run(&[
        "list-panes",
        "-t",
        "alpha",
        "-F",
        "#{pane_index}:#{pane_width}x#{pane_height}:#{pane_left},#{pane_top}",
    ])?;
    assert_eq!(default_split.status.code(), Some(0));
    assert_eq!(stdout(&default_split), "0:120x20:0,0\n1:120x19:0,21\n");
    assert!(stderr(&default_split).is_empty());

    assert_success(&harness.run(&["new-session", "-d", "-s", "beta", "-x", "120", "-y", "40"])?);
    assert_success(&harness.run(&["split-window", "-h", "-t", "beta"])?);
    let horizontal_split = harness.run(&[
        "list-panes",
        "-t",
        "beta",
        "-F",
        "#{pane_index}:#{pane_width}x#{pane_height}:#{pane_left},#{pane_top}",
    ])?;
    assert_eq!(horizontal_split.status.code(), Some(0));
    assert_eq!(stdout(&horizontal_split), "0:60x40:0,0\n1:59x40:61,0\n");
    assert!(stderr(&horizontal_split).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn split_window_full_size_splits_the_root_layout() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("split-window-full-size-root")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-x", "100", "-y", "40"])?);
    assert_success(&harness.run(&["split-window", "-h", "-t", "alpha:0.0"])?);
    assert_success(&harness.run(&["split-window", "-vf", "-t", "alpha:0.0"])?);

    let output = harness.run(&[
        "list-panes",
        "-t",
        "alpha",
        "-F",
        "#{pane_index}:#{pane_width}x#{pane_height}:#{pane_left},#{pane_top}",
    ])?;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        stdout(&output),
        "0:50x20:0,0\n1:49x20:51,0\n2:100x19:0,21\n"
    );
    assert!(stderr(&output).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn split_window_reports_no_space_for_new_pane_like_tmux() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("split-window-no-space")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-x", "20", "-y", "5"])?);
    assert_success(&harness.run(&["split-window", "-v", "-t", "alpha:0.0"])?);

    let output = harness.run(&["split-window", "-v", "-t", "alpha:0.1"])?;
    assert_eq!(output.status.code(), Some(1));
    assert!(stdout(&output).is_empty(), "stdout should be empty");
    assert_eq!(stderr(&output), "no space for new pane\n");

    let panes = harness.run(&["list-panes", "-t", "alpha", "-F", "#{pane_index}"])?;
    assert_eq!(panes.status.code(), Some(0));
    assert_eq!(stdout(&panes), "0\n1\n");

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn split_window_accepts_shell_command_for_new_pane() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("split-window-command")?;
    let mut daemon = harness.start_hidden_daemon()?;
    let output_path = harness.tmpdir().join("split-command.txt");
    let login_output_path = harness.tmpdir().join("split-login-command.txt");

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&[
        "split-window",
        "-t",
        "alpha",
        "sh",
        "-c",
        &format!("printf split-command > {}", shell_quote(&output_path)),
    ])?);

    wait_for_file_contents(&output_path, "split-command", ATTACH_TIMEOUT)?;

    assert_success(&harness.run(&[
        "split-window",
        "-t",
        "alpha",
        "bash",
        "-lc",
        &format!(
            "printf split-login-command > {}",
            shell_quote(&login_output_path)
        ),
    ])?);

    wait_for_file_contents(&login_output_path, "split-login-command", ATTACH_TIMEOUT)?;

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn list_windows_keeps_stored_window_name_while_reporting_active_pane_command(
) -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("list-windows-stored-name-active-pane-command")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&[
        "new-session",
        "-d",
        "-s",
        "alpha",
        "-n",
        "rmux",
        "-x",
        "80",
        "-y",
        "24",
        "sh",
        "-c",
        "exec cat",
    ])?);

    let deadline = Instant::now() + ATTACH_TIMEOUT;
    loop {
        let listed = harness.run(&[
            "list-windows",
            "-t",
            "alpha",
            "-F",
            "#{window_name}:#{pane_current_command}",
        ])?;
        assert_eq!(listed.status.code(), Some(0));
        if stdout(&listed) == "rmux:cat\n" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "list-windows did not converge to stored window name plus active pane command: {:?}",
            stdout(&listed)
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn new_session_command_sets_initial_automatic_window_name() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("new-session-command-window-name")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&[
        "new-session",
        "-d",
        "-s",
        "alpha",
        "-x",
        "80",
        "-y",
        "24",
        "cat",
    ])?);

    let deadline = Instant::now() + ATTACH_TIMEOUT;
    loop {
        let listed = harness.run(&[
            "list-windows",
            "-t",
            "alpha",
            "-F",
            "#{window_name}:#{pane_current_command}",
        ])?;
        assert_eq!(listed.status.code(), Some(0));
        if stdout(&listed) == "cat:cat\n" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "new-session command did not converge to command window name: {:?}",
            stdout(&listed)
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn pane_directional_selection_resize_delta_and_cross_window_join_match_tmux_forms(
) -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("pane-direction-resize-join")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-x", "100", "-y", "30"])?);
    assert_success(&harness.run(&["split-window", "-h", "-t", "alpha:0.0"])?);
    assert_success(&harness.run(&["select-pane", "-t", "alpha:0.0"])?);
    assert_success(&harness.run(&["select-pane", "-R", "-t", "alpha:0.0"])?);

    let active = harness.run(&[
        "list-panes",
        "-t",
        "alpha:0",
        "-F",
        "#{pane_index}:#{pane_active}",
    ])?;
    assert_eq!(active.status.code(), Some(0));
    assert_eq!(stdout(&active), "0:0\n1:1\n");

    assert_success(&harness.run(&["resize-pane", "-R", "-t", "alpha:0.1", "5"])?);
    assert_success(&harness.run(&["break-pane", "-d", "-s", "alpha:0.1", "-t", "alpha:3"])?);
    assert_success(&harness.run(&["join-pane", "-h", "-s", "alpha:3.0", "-t", "alpha:0.0"])?);

    let windows = harness.run(&[
        "list-windows",
        "-t",
        "alpha",
        "-F",
        "#{window_index}:#{window_panes}",
    ])?;
    assert_eq!(windows.status.code(), Some(0));
    assert_eq!(stdout(&windows), "0:2\n");

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn bare_pane_targets_resolve_against_the_current_window() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("bare-pane-target-current-window")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-x", "80", "-y", "24"])?);
    assert_success(&harness.run(&["split-window", "-v", "-t", "alpha:0.0"])?);
    assert_success(&harness.run(&["select-pane", "-t", "1"])?);

    let active = harness.run(&[
        "list-panes",
        "-t",
        "alpha:0",
        "-F",
        "#{pane_index}:#{pane_active}",
    ])?;
    assert_eq!(active.status.code(), Some(0));
    assert_eq!(stdout(&active), "0:0\n1:1\n");

    assert_success(&harness.run(&["select-pane", "-t", "{up-of}"])?);
    let active = harness.run(&[
        "list-panes",
        "-t",
        "alpha:0",
        "-F",
        "#{pane_index}:#{pane_active}",
    ])?;
    assert_eq!(active.status.code(), Some(0));
    assert_eq!(stdout(&active), "0:1\n1:0\n");

    assert_success(&harness.run(&["select-pane", "-t", "{down-of}"])?);
    let active = harness.run(&[
        "list-panes",
        "-t",
        "alpha:0",
        "-F",
        "#{pane_index}:#{pane_active}",
    ])?;
    assert_eq!(active.status.code(), Some(0));
    assert_eq!(stdout(&active), "0:0\n1:1\n");

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn directional_select_while_zoomed_matches_tmux_unzoom_rules() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("zoomed-directional-select")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-x", "80", "-y", "24"])?);
    assert_success(&harness.run(&["split-window", "-h", "-t", "alpha"])?);
    assert_success(&harness.run(&["select-pane", "-t", "alpha:0.1"])?);
    assert_success(&harness.run(&["resize-pane", "-Z", "-t", "alpha:0.1"])?);

    let before = harness.run(&[
        "display-message",
        "-p",
        "-t",
        "alpha",
        "#{window_zoomed_flag}:#{pane_index}",
    ])?;
    assert_eq!(stdout(&before), "1:1\n");

    assert_success(&harness.run(&["select-pane", "-L", "-t", "alpha"])?);
    let moved = harness.run(&[
        "display-message",
        "-p",
        "-t",
        "alpha",
        "#{window_zoomed_flag}:#{pane_index}",
    ])?;
    assert_eq!(stdout(&moved), "0:0\n");

    assert_success(&harness.run(&["resize-pane", "-Z", "-t", "alpha:0.1"])?);
    assert_success(&harness.run(&["select-pane", "-U", "-t", "alpha"])?);
    let no_neighbor = harness.run(&[
        "display-message",
        "-p",
        "-t",
        "alpha",
        "#{window_zoomed_flag}:#{pane_index}",
    ])?;
    assert_eq!(stdout(&no_neighbor), "1:1\n");

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn zoomed_list_panes_formats_only_the_active_pane_as_full_size() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("zoomed-list-panes-geometry")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-x", "80", "-y", "24"])?);
    assert_success(&harness.run(&["split-window", "-h", "-t", "alpha:0.0"])?);
    assert_success(&harness.run(&["resize-pane", "-Z", "-t", "alpha:0.0"])?);
    assert_success(&harness.run(&["select-pane", "-Z", "-t", "alpha:0.1"])?);

    let output = harness.run(&[
        "list-panes",
        "-t",
        "alpha",
        "-F",
        "#{window_zoomed_flag}:#{pane_index}:#{pane_width}x#{pane_height}:#{pane_active}",
    ])?;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout(&output), "1:0:40x24:0\n1:1:80x24:1\n");
    assert!(stderr(&output).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn advanced_target_forms_resolve_through_the_server_like_tmux() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("advanced-target-forms")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "zero"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "alpha:1", "-n", "one"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "alpha:2", "-n", "two"])?);
    assert_success(&harness.run(&["select-window", "-t", "alpha:1"])?);
    assert_success(&harness.run(&["select-window", "-t", "alpha:+"])?);
    assert_success(&harness.run(&["select-window", "-t", "alpha:-"])?);
    assert_success(&harness.run(&["select-window", "-t", "alpha:{last}"])?);
    assert_success(&harness.run(&["select-window", "-t", "alpha:{start}"])?);
    assert_success(&harness.run(&["select-window", "-t", "alpha:tw"])?);

    assert_success(&harness.run(&["split-window", "-h", "-t", "alpha:two"])?);
    assert_success(&harness.run(&["split-window", "-v", "-t", "alpha:two.0"])?);
    assert_success(&harness.run(&["select-pane", "-t", "alpha:two.{bottom}"])?);
    assert_success(&harness.run(&["select-pane", "-t", "alpha:two.{left-of}"])?);
    assert_success(&harness.run(&["select-pane", "-t", "alpha:two.{right-of}"])?);

    let active = harness.run(&[
        "display-message",
        "-p",
        "-t",
        "alpha:",
        "#{window_index}:#{window_name}:#{pane_index}",
    ])?;
    assert_eq!(active.status.code(), Some(0));
    assert_eq!(stdout(&active), "2:two:1\n");
    assert!(stderr(&active).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn join_pane_horizontal_flag_splits_target_left_right_like_tmux() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("join-pane-horizontal-geometry")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-x", "80", "-y", "24"])?);
    assert_success(&harness.run(&["split-window", "-h", "-t", "alpha:0.0"])?);
    assert_success(&harness.run(&["break-pane", "-d", "-s", "alpha:0.1", "-t", "alpha:2"])?);
    assert_success(&harness.run(&["join-pane", "-h", "-s", "alpha:2.0", "-t", "alpha:0.0"])?);

    let panes = harness.run(&[
        "list-panes",
        "-t",
        "alpha:0",
        "-F",
        "#{pane_index}:#{pane_width}x#{pane_height}:#{pane_left},#{pane_top}",
    ])?;
    assert_eq!(panes.status.code(), Some(0));
    assert_eq!(stdout(&panes), "0:40x24:0,0\n1:39x24:41,0\n");

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn resize_pane_width_targets_the_addressed_non_main_pane() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("resize-pane-targeted-width")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-x", "100", "-y", "40"])?);
    assert_success(&harness.run(&["split-window", "-h", "-t", "alpha"])?);
    assert_success(&harness.run(&["split-window", "-v", "-t", "alpha:0.1"])?);
    assert_success(&harness.run(&["resize-pane", "-t", "alpha:0.1", "-x", "34"])?);

    let listed = harness.run(&[
        "list-panes",
        "-t",
        "alpha",
        "-F",
        "#{pane_index}:#{pane_width}x#{pane_height}:#{pane_left},#{pane_top}",
    ])?;
    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(
        stdout(&listed),
        "0:65x40:0,0\n1:34x20:66,0\n2:34x19:66,21\n"
    );
    assert!(stderr(&listed).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn resize_pane_mouse_and_trim_flags_are_context_noops() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("resize-pane-context-noops")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-x", "80", "-y", "24"])?);
    assert_success(&harness.run(&["split-window", "-v", "-t", "alpha"])?);

    let before = harness.run(&[
        "list-panes",
        "-t",
        "alpha",
        "-F",
        "#{pane_index}:#{pane_width}x#{pane_height}:#{pane_left},#{pane_top}",
    ])?;
    assert_eq!(before.status.code(), Some(0));

    assert_success(&harness.run(&["resize-pane", "-T", "-t", "alpha:0.0"])?);
    let after_trim = harness.run(&[
        "list-panes",
        "-t",
        "alpha",
        "-F",
        "#{pane_index}:#{pane_width}x#{pane_height}:#{pane_left},#{pane_top}",
    ])?;
    assert_eq!(after_trim.status.code(), Some(0));
    assert_eq!(stdout(&after_trim), stdout(&before));

    assert_success(&harness.run(&["resize-pane", "-M", "-t", "alpha:0.0"])?);
    let after_mouse = harness.run(&[
        "list-panes",
        "-t",
        "alpha",
        "-F",
        "#{pane_index}:#{pane_width}x#{pane_height}:#{pane_left},#{pane_top}",
    ])?;
    assert_eq!(after_mouse.status.code(), Some(0));
    assert_eq!(stdout(&after_mouse), stdout(&before));

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn resize_window_linked_slots_use_target_session_size_without_clients() -> Result<(), Box<dyn Error>>
{
    let _guard = window_surface_guard();
    for (target, flag, expected) in [
        ("alpha:0", "-A", "80x24\n"),
        ("alpha:0", "-a", "80x24\n"),
        ("beta:1", "-A", "120x40\n"),
        ("beta:1", "-a", "120x40\n"),
    ] {
        assert_linked_slot_resize_uses_target_session_size(target, flag, expected)?;
    }

    Ok(())
}

fn assert_linked_slot_resize_uses_target_session_size(
    target: &str,
    flag: &str,
    expected: &str,
) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("resize-window-linked-session-size")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-x", "80", "-y", "24"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "beta", "-x", "120", "-y", "40"])?);
    assert_success(&harness.run(&["link-window", "-s", "alpha:0", "-t", "beta:1"])?);
    assert_success(&harness.run(&["resize-window", "-t", "alpha:0", "-x", "70", "-y", "20"])?);
    let linked_sizes = harness.run(&[
        "list-windows",
        "-a",
        "-F",
        "#{session_name}:#{window_index}:#{window_width}x#{window_height}",
    ])?;
    assert_eq!(linked_sizes.status.code(), Some(0));
    assert_eq!(
        stdout(&linked_sizes),
        "alpha:0:70x20\nbeta:0:120x40\nbeta:1:70x20\n"
    );
    assert_success(&harness.run(&["resize-window", flag, "-t", target])?);
    let output = harness.run(&[
        "display-message",
        "-p",
        "-t",
        target,
        "#{window_width}x#{window_height}",
    ])?;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout(&output), expected);
    assert!(stderr(&output).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn link_window_without_source_uses_current_window() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("link-window-current-source")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "aw"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "beta", "-n", "bw"])?);
    assert_success(&harness.run(&["link-window", "-t", "alpha:1"])?);

    let output = harness.run(&[
        "list-windows",
        "-t",
        "alpha",
        "-F",
        "#{window_index}:#{window_name}",
    ])?;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout(&output), "0:aw\n1:bw\n");
    assert!(stderr(&output).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn window_navigation_wraps_and_list_windows_prints_server_rendered_stdout(
) -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("window-navigation-and-listing")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["new-window", "-t", "alpha", "-d", "-n", "logs"])?);
    assert_success(&harness.run(&["new-window", "-t", "alpha", "-d", "-n", "shell"])?);

    assert_success(&harness.run(&["next-window", "-t", "alpha"])?);
    assert_success(&harness.run(&["previous-window", "-t", "alpha"])?);
    assert_success(&harness.run(&["last-window", "-t", "alpha"])?);

    let default_fields = harness.run(&[
        "list-windows",
        "-t",
        "alpha",
        "-F",
        "#{window_index}\t#{window_name}\t#{window_raw_flags}\t#{window_panes}\t#{window_width}\t#{window_height}\t#{window_layout}\t#{window_id}\t#{window_active}",
    ])?;
    assert_eq!(default_fields.status.code(), Some(0));
    let expected_default = stdout(&default_fields)
        .lines()
        .map(|line| {
            let fields = line.split('\t').collect::<Vec<_>>();
            assert_eq!(fields.len(), 9, "unexpected list-windows fields: {line:?}");
            let active_suffix = if fields[8] == "1" { " (active)" } else { "" };
            format!(
                "{}: {}{} ({} panes) [{}x{}] [layout {}] {}{}",
                fields[0],
                fields[1],
                fields[2],
                fields[3],
                fields[4],
                fields[5],
                fields[6],
                fields[7],
                active_suffix,
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    assert!(stderr(&default_fields).is_empty());

    let listed = harness.run(&["list-windows", "-t", "alpha"])?;
    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(stdout(&listed), expected_default);
    assert!(stderr(&listed).is_empty());

    let formatted = harness.run(&[
        "list-windows",
        "-t",
        "alpha",
        "-F",
        "#{session_name}:#{window_index}:#{window_id}:#{window_last_flag}:#{window_active}",
    ])?;
    assert_eq!(formatted.status.code(), Some(0));
    assert_eq!(
        stdout(&formatted),
        "alpha:0:@0:1:0\nalpha:1:@1:0:1\nalpha:2:@2:0:0\n"
    );
    assert!(stderr(&formatted).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn list_windows_all_uses_tmux_multi_session_default_format() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("list-windows-all-default-format")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "a"])?);
    assert_success(&harness.run(&["new-window", "-t", "alpha", "-d", "-n", "b"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "beta", "-n", "c"])?);

    let listed = harness.run(&["list-windows", "-a"])?;
    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(
        stdout(&listed),
        "alpha:0: a* (1 panes) [80x24] \nalpha:1: b (1 panes) [80x24] \nbeta:0: c* (1 panes) [80x24] \n"
    );
    assert!(stderr(&listed).is_empty());

    let scoped = harness.run(&["list-windows", "-a", "-t", "alpha"])?;
    assert_eq!(scoped.status.code(), Some(0));
    assert_eq!(stdout(&scoped), stdout(&listed));
    assert!(stderr(&scoped).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn select_window_navigation_flags_match_tmux_behavior() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("select-window-navigation-flags")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "w0"])?);
    assert_success(&harness.run(&["new-window", "-t", "alpha", "-d", "-n", "w1"])?);
    assert_success(&harness.run(&["new-window", "-t", "alpha", "-d", "-n", "w2"])?);

    assert_success(&harness.run(&["select-window", "-t", "alpha:0"])?);
    assert_success(&harness.run(&["select-window", "-n", "-t", "alpha:2"])?);
    assert_active_window(&harness, "alpha", "1")?;

    assert_success(&harness.run(&["select-window", "-p", "-t", "alpha:0"])?);
    assert_active_window(&harness, "alpha", "0")?;

    assert_success(&harness.run(&["select-window", "-l", "-t", "alpha:2"])?);
    assert_active_window(&harness, "alpha", "1")?;

    assert_success(&harness.run(&["select-window", "-T", "-t", "alpha:1"])?);
    assert_active_window(&harness, "alpha", "0")?;

    assert_success(&harness.run(&["select-window", "-T", "-t", "alpha:2"])?);
    assert_active_window(&harness, "alpha", "2")?;

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn select_window_exact_match_target_strips_match_prefix() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("select-window-exact-match-target")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "zero"])?);
    assert_success(&harness.run(&["new-window", "-t", "alpha", "-d", "-n", "one"])?);

    assert_success(&harness.run(&["select-window", "-t", "=alpha:1"])?);
    assert_active_window(&harness, "alpha", "1")?;

    terminate_child(daemon.child_mut())?;
    Ok(())
}

fn assert_active_window(
    harness: &CliHarness,
    session: &str,
    expected_index: &str,
) -> Result<(), Box<dyn Error>> {
    let output = harness.run(&["display-message", "-p", "-t", session, "#{window_index}"])?;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout(&output).trim_end(), expected_index);
    assert!(stderr(&output).is_empty());
    Ok(())
}

#[test]
fn list_windows_filter_matches_tmux_format_truthiness() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("list-windows-filter")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["new-window", "-t", "alpha", "-d", "-n", "keep_logs"])?);
    assert_success(&harness.run(&["new-window", "-t", "alpha", "-d", "-n", "drop_shell"])?);

    let listed = harness.run(&[
        "list-windows",
        "-t",
        "alpha",
        "-f",
        "#{m:keep_*,#{window_name}}",
        "-F",
        "#{window_name}",
    ])?;

    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(stdout(&listed), "keep_logs\n");
    assert!(stderr(&listed).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn single_window_navigation_errors_use_bare_tmux_messages() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("single-window-navigation-errors")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);

    for (command, expected) in [
        ("next-window", "no next window\n"),
        ("previous-window", "no previous window\n"),
        ("last-window", "no last window\n"),
    ] {
        let output = harness.run(&[command, "-t", "alpha"])?;
        assert_eq!(output.status.code(), Some(1), "{command} should fail");
        assert!(
            stdout(&output).is_empty(),
            "{command} should not print stdout"
        );
        assert_eq!(
            stderr(&output),
            expected,
            "{command} stderr should match tmux"
        );
    }

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn binary_roundtrip_covers_the_public_window_command_surface() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("public-window-command-roundtrip")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-x", "200", "-y", "50"])?);
    assert_success(&harness.run(&["has-session", "-t", "alpha"])?);
    assert_success(&harness.run(&["new-window", "-t", "alpha", "-d", "-n", "logs"])?);
    assert_success(&harness.run(&["select-window", "-t", "alpha:1"])?);
    assert_success(&harness.run(&["rename-window", "-t", "alpha:1", "renamed"])?);
    assert_success(&harness.run(&["previous-window", "-t", "alpha"])?);
    assert_success(&harness.run(&["last-window", "-t", "alpha"])?);
    assert_success(&harness.run(&["next-window", "-t", "alpha"])?);
    let listed = harness.run(&["list-windows", "-t", "alpha"])?;
    assert_eq!(listed.status.code(), Some(0));
    assert!(stdout(&listed).contains("renamed-"));
    assert!(stderr(&listed).is_empty());
    assert_success(&harness.run(&["split-window", "-v", "-t", "alpha"])?);
    assert_success(&harness.run(&["split-window", "-h", "-t", "alpha:0.1"])?);
    assert_success(&harness.run(&["select-layout", "-t", "alpha:0", "main-vertical"])?);
    assert_success(&harness.run(&["select-layout", "-t", "alpha:0", "main-horizontal"])?);
    assert_success(&harness.run(&["select-layout", "-t", "alpha:0", "tiled"])?);
    assert_success(&harness.run(&["select-layout", "-o", "-t", "alpha:0"])?);
    assert_success(&harness.run(&["next-layout", "-t", "alpha:0"])?);
    assert_success(&harness.run(&["previous-layout", "-t", "alpha:0"])?);
    assert_success(&harness.run(&["resize-pane", "-t", "alpha:0.0", "-x", "34"])?);
    assert_success(&harness.run(&["resize-pane", "-t", "alpha:0.1", "-Z"])?);
    assert_success(&harness.run(&["select-pane", "-t", "alpha:0.1"])?);
    assert_success(&harness.run(&["send-keys", "-t", "alpha:0.1", "echo", "Enter"])?);
    assert_success(&harness.run(&["kill-pane", "-t", "alpha:0.2"])?);
    assert_success(&harness.run(&["new-window", "-t", "alpha", "-d", "-n", "scratch"])?);
    assert_success(&harness.run(&["move-window", "-s", "alpha:2", "-t", "alpha:4"])?);
    assert_success(&harness.run(&["swap-window", "-s", "alpha:1", "-t", "alpha:4"])?);
    assert_success(&harness.run(&["rotate-window", "-t", "alpha:0"])?);
    assert_success(&harness.run(&["move-window", "-r", "-t", "alpha"])?);
    assert_success(&harness.run(&["set-option", "-g", "status", "off"])?);
    assert_success(&harness.run(&["set-option", "-as", "terminal-features", "screen-256color"])?);
    assert_success(&harness.run(&[
        "set-window-option",
        "-t",
        "alpha:0",
        "pane-border-style",
        "fg=colour1",
    ])?);
    assert_success(&harness.run(&["set-environment", "-t", "alpha", "TERM", "screen"])?);
    assert_success(&harness.run(&[
        "set-hook",
        "-g",
        "client-attached",
        "display-message hook-ok",
    ])?);
    assert_success(&harness.run(&["kill-window", "-t", "alpha:1"])?);
    assert_success(&harness.run(&["kill-session", "-t", "alpha"])?);

    let has_after_kill = harness.run(&["has-session", "-t", "alpha"])?;
    assert_eq!(has_after_kill.status.code(), Some(1));
    assert!(stdout(&has_after_kill).is_empty());
    assert!(
        stderr(&has_after_kill).contains("no server running on "),
        "has-session after the last session is killed should report absent server, got: {}",
        stderr(&has_after_kill)
    );

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn link_window_shares_runtime_and_updates_linked_formats() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("link-window-runtime-share")?;
    let mut daemon = harness.start_hidden_daemon()?;
    let output_path = harness.tmpdir().join("linked-runtime.txt");

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "beta"])?);
    assert_success(&harness.run(&["link-window", "-s", "alpha:0", "-t", "beta:1"])?);
    assert_success(&harness.run(&[
        "send-keys",
        "-t",
        "beta:1.0",
        &format!("printf linked-runtime > {}", shell_quote(&output_path)),
        "Enter",
    ])?);

    wait_for_file_contents(&output_path, "linked-runtime", ATTACH_TIMEOUT)?;

    assert_success(&harness.run(&["rename-window", "-t", "beta:1", "logs"])?);

    let linked = harness.run(&[
        "display-message",
        "-p",
        "-t",
        "alpha:0",
        "#{window_name}:#{window_linked}:#{window_linked_sessions}:#{window_linked_sessions_list}",
    ])?;
    assert_eq!(linked.status.code(), Some(0));
    assert_eq!(stdout(&linked), "logs:1:2:alpha,beta\n");
    assert!(stderr(&linked).is_empty());

    assert_success(&harness.run(&["unlink-window", "-t", "beta:1"])?);

    let missing = harness.run(&[
        "send-keys",
        "-t",
        "beta:1.0",
        "printf should-not-exist",
        "Enter",
    ])?;
    assert_eq!(missing.status.code(), Some(1));
    assert_eq!(stderr(&missing), "can't find window: 1\n");

    let unlinked = harness.run(&[
        "display-message",
        "-p",
        "-t",
        "alpha:0",
        "#{window_name}:#{window_linked}:#{window_linked_sessions}:#{window_linked_sessions_list}",
    ])?;
    assert_eq!(unlinked.status.code(), Some(0));
    assert_eq!(stdout(&unlinked), "logs:0:1:alpha\n");
    assert!(stderr(&unlinked).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn rename_window_from_group_peer_propagates_to_linked_family() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("rename-window-linked-group-peer")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["new-session", "-d", "-t", "alpha", "-s", "beta"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "gamma"])?);
    assert_success(&harness.run(&["new-session", "-d", "-t", "gamma", "-s", "delta"])?);
    assert_success(&harness.run(&["link-window", "-s", "alpha:0", "-t", "gamma:1"])?);
    assert_success(&harness.run(&["rename-window", "-t", "beta:0", "peername"])?);

    let listed = harness.run(&[
        "list-windows",
        "-a",
        "-F",
        "#{session_name}:#{window_index}:#{window_name}",
    ])?;
    let listed_stdout = stdout(&listed);
    let mut lines = listed_stdout.lines().collect::<Vec<_>>();
    lines.sort_unstable();

    for expected in [
        "alpha:0:peername",
        "beta:0:peername",
        "delta:1:peername",
        "gamma:1:peername",
    ] {
        assert!(
            lines.contains(&expected),
            "missing {expected:?} in list-windows output: {lines:?}"
        );
    }

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn swap_window_with_linked_slot_resizes_the_link_runtime_owner() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("swap-window-linked-slot")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "alpha"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "beta", "-n", "beta"])?);
    assert_success(&harness.run(&["link-window", "-s", "alpha:0", "-t", "beta:1"])?);
    assert_success(&harness.run(&["swap-window", "-s", "beta:1", "-t", "beta:0"])?);

    let beta_windows = harness.run(&[
        "list-windows",
        "-t",
        "beta",
        "-F",
        "#{window_index}:#{window_name}:#{window_active}:#{window_flags}",
    ])?;
    assert_eq!(beta_windows.status.code(), Some(0));
    assert_eq!(stdout(&beta_windows), "0:alpha:0:-\n1:beta:1:*\n");
    assert!(stderr(&beta_windows).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn link_window_position_detached_and_kill_flags_control_slot_selection(
) -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("link-window-flag-surface")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["rename-window", "-t", "alpha:0", "source"])?);

    assert_success(&harness.run(&["new-session", "-d", "-s", "beta"])?);
    assert_success(&harness.run(&["rename-window", "-t", "beta:0", "keep0"])?);
    assert_success(&harness.run(&["new-window", "-t", "beta", "-d", "-n", "keep1"])?);
    assert_success(&harness.run(&["select-window", "-t", "beta:0"])?);
    assert_success(&harness.run(&["link-window", "-a", "-d", "-s", "alpha:0", "-t", "beta:0"])?);

    let beta_windows = harness.run(&[
        "list-windows",
        "-t",
        "beta",
        "-F",
        "#{window_index}:#{window_name}:#{window_active}",
    ])?;
    assert_eq!(beta_windows.status.code(), Some(0));
    assert_eq!(stdout(&beta_windows), "0:keep0:1\n1:source:0\n2:keep1:0\n");
    assert!(stderr(&beta_windows).is_empty());

    let source_id = harness.run(&["display-message", "-p", "-t", "alpha:0", "#{window_id}"])?;
    let beta_link_id = harness.run(&["display-message", "-p", "-t", "beta:1", "#{window_id}"])?;
    assert_eq!(source_id.status.code(), Some(0));
    assert_eq!(beta_link_id.status.code(), Some(0));
    assert_eq!(stdout(&source_id), stdout(&beta_link_id));

    assert_success(&harness.run(&["new-session", "-d", "-s", "gamma"])?);
    assert_success(&harness.run(&["rename-window", "-t", "gamma:0", "anchor"])?);
    assert_success(&harness.run(&["new-window", "-t", "gamma", "-d", "-n", "victim"])?);
    assert_success(&harness.run(&["select-window", "-t", "gamma:0"])?);
    assert_success(&harness.run(&["link-window", "-d", "-k", "-s", "alpha:0", "-t", "gamma:1"])?);

    let gamma_windows = harness.run(&[
        "list-windows",
        "-t",
        "gamma",
        "-F",
        "#{window_index}:#{window_name}:#{window_active}",
    ])?;
    assert_eq!(gamma_windows.status.code(), Some(0));
    assert_eq!(stdout(&gamma_windows), "0:anchor:1\n1:source:0\n");
    assert!(stderr(&gamma_windows).is_empty());

    let gamma_link_id = harness.run(&["display-message", "-p", "-t", "gamma:1", "#{window_id}"])?;
    assert_eq!(gamma_link_id.status.code(), Some(0));
    assert_eq!(stdout(&source_id), stdout(&gamma_link_id));

    assert_success(&harness.run(&["new-session", "-d", "-s", "delta"])?);
    assert_success(&harness.run(&["rename-window", "-t", "delta:0", "keep0"])?);
    assert_success(&harness.run(&["new-window", "-t", "delta", "-d", "-n", "keep1"])?);
    assert_success(&harness.run(&["select-window", "-t", "delta:0"])?);
    assert_success(&harness.run(&["link-window", "-b", "-d", "-s", "alpha:0", "-t", "delta:1"])?);

    let delta_windows = harness.run(&[
        "list-windows",
        "-t",
        "delta",
        "-F",
        "#{window_index}:#{window_name}:#{window_active}",
    ])?;
    assert_eq!(delta_windows.status.code(), Some(0));
    assert_eq!(stdout(&delta_windows), "0:keep0:1\n1:source:0\n2:keep1:0\n");
    assert!(stderr(&delta_windows).is_empty());

    let delta_link_id = harness.run(&["display-message", "-p", "-t", "delta:1", "#{window_id}"])?;
    assert_eq!(delta_link_id.status.code(), Some(0));
    assert_eq!(stdout(&source_id), stdout(&delta_link_id));

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn kill_pane_of_last_pane_destroys_the_window_and_updates_session_targets(
) -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("kill-pane-destroys-window")?;
    let mut daemon = harness.start_hidden_daemon()?;
    let output_path = harness.tmpdir().join("kill-pane-fallback.txt");

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["new-window", "-t", "alpha", "-d", "-n", "scratch"])?);
    assert_success(&harness.run(&["select-window", "-t", "alpha:1"])?);
    std::thread::sleep(Duration::from_millis(25));
    assert_success(&harness.run(&["kill-pane", "-t", "alpha:1.0"])?);

    let listed = harness.run(&[
        "list-windows",
        "-t",
        "alpha",
        "-F",
        "#{window_index}:#{window_active}:#{window_panes}",
    ])?;
    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(stdout(&listed), "0:1:1\n");
    assert!(stderr(&listed).is_empty());

    assert_success(&harness.run(&["split-window", "-v", "-t", "alpha"])?);
    assert_success(&harness.run(&[
        "send-keys",
        "-t",
        "alpha:0.1",
        &format!("printf fallback-pane > {}", shell_quote(&output_path)),
        "Enter",
    ])?);

    wait_for_file_contents(&output_path, "fallback-pane", ATTACH_TIMEOUT)?;

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn kill_pane_missing_target_uses_tmux_pane_lookup_error() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("kill-pane-missing-pane-error")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);

    let missing = harness.run(&["kill-pane", "-t", "alpha:0.99"])?;
    assert_eq!(missing.status.code(), Some(1));
    assert_eq!(stderr(&missing), "can't find pane: 99\n");
    assert!(stdout(&missing).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn display_panes_target_without_attached_client_uses_tmux_client_lookup_error(
) -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("display-panes-missing-client-error")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);

    let missing = harness.run(&["display-panes", "-t", "alpha:0"])?;
    assert_eq!(missing.status.code(), Some(1));
    assert_eq!(stderr(&missing), "can't find client: alpha:0\n");
    assert!(stdout(&missing).is_empty());

    let trailing_colon = harness.run(&["display-panes", "-t", "alpha:"])?;
    assert_eq!(trailing_colon.status.code(), Some(1));
    assert_eq!(stderr(&trailing_colon), "can't find client: alpha\n");
    assert!(stdout(&trailing_colon).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn display_panes_without_attached_client_reports_no_current_client() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("display-panes-no-current-client")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);

    let output = harness.run(&["display-panes"])?;
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(stderr(&output), "no current client\n");
    assert!(stdout(&output).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn display_popup_without_attached_client_reports_no_current_client() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("display-popup-no-current-client")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);

    let output = harness.run(&["display-popup", "-E", "printf popup"])?;
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(stderr(&output), "no current client\n");
    assert!(stdout(&output).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn new_window_after_and_before_insert_like_tmux() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("new-window-after-before")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "zero"])?);
    assert_success(&harness.run(&["new-window", "-d", "-n", "one", "-t", "alpha:1"])?);
    assert_success(&harness.run(&["new-window", "-d", "-n", "five", "-t", "alpha:5"])?);
    assert_success(&harness.run(&[
        "new-window",
        "-d",
        "-a",
        "-t",
        "alpha:1",
        "-n",
        "after-one",
    ])?);
    assert_success(&harness.run(&[
        "new-window",
        "-d",
        "-b",
        "-t",
        "alpha:5",
        "-n",
        "before-five",
    ])?);

    let windows = harness.run(&[
        "list-windows",
        "-t",
        "alpha",
        "-F",
        "#{window_index}:#{window_name}",
    ])?;
    assert_eq!(windows.status.code(), Some(0));
    assert_eq!(
        stdout(&windows),
        "0:zero\n1:one\n2:after-one\n5:before-five\n6:five\n"
    );
    assert!(stderr(&windows).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn move_window_reindex_without_target_uses_current_session() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("move-window-reindex-current-session")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "alpha0"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "alpha:1", "-n", "alpha5"])?);
    assert_success(&harness.run(&["move-window", "-s", "alpha:1", "-t", "alpha:5"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "beta", "-n", "beta0"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "beta:1", "-n", "beta5"])?);
    assert_success(&harness.run(&["move-window", "-s", "beta:1", "-t", "beta:5"])?);

    assert_success(&harness.run(&["move-window", "-r"])?);

    let alpha = harness.run(&[
        "list-windows",
        "-t",
        "alpha",
        "-F",
        "#{window_index}:#{window_name}",
    ])?;
    let beta = harness.run(&[
        "list-windows",
        "-t",
        "beta",
        "-F",
        "#{window_index}:#{window_name}",
    ])?;
    assert_eq!(stdout(&alpha), "0:alpha0\n5:alpha5\n");
    assert_eq!(stdout(&beta), "0:beta0\n1:beta5\n");

    assert_success(&harness.run(&["move-window", "-r", "-t", "alpha:"])?);
    let alpha = harness.run(&[
        "list-windows",
        "-t",
        "alpha",
        "-F",
        "#{window_index}:#{window_name}",
    ])?;
    assert_eq!(stdout(&alpha), "0:alpha0\n1:alpha5\n");

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn move_window_reindex_with_source_and_window_target_renumbers_like_tmux(
) -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("move-window-reindex-window-target-renumber")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "s", "-n", "zero"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "s:2", "-n", "two"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "s:5", "-n", "five"])?);

    assert_success(&harness.run(&["move-window", "-r", "-s", "s:2", "-t", "s:5"])?);

    let windows = harness.run(&[
        "list-windows",
        "-t",
        "s",
        "-F",
        "#{window_index}:#{window_name}",
    ])?;
    assert_eq!(stdout(&windows), "0:zero\n1:two\n2:five\n");
    assert!(stderr(&windows).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn new_window_target_window_index_creates_at_requested_index() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("new-window-target-index")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "zero"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "alpha:5", "-n", "five"])?);

    let windows = harness.run(&[
        "list-windows",
        "-t",
        "alpha",
        "-F",
        "#{window_index}:#{window_name}",
    ])?;
    assert_eq!(windows.status.code(), Some(0));
    assert_eq!(stdout(&windows), "0:zero\n5:five\n");
    assert!(stderr(&windows).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn swap_window_preserves_explicit_window_names_after_auto_rename_tracking(
) -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("swap-window-explicit-name")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "alpha:2", "-n", "two"])?);
    assert_success(&harness.run(&[
        "new-window",
        "-d",
        "-b",
        "-t",
        "alpha:2",
        "-n",
        "before-two",
    ])?);
    assert_success(&harness.run(&["swap-window", "-s", "alpha:0", "-t", "alpha:2"])?);

    let windows = harness.run(&[
        "list-windows",
        "-t",
        "alpha",
        "-F",
        "#{window_index}:#{window_name}",
    ])?;
    assert_eq!(windows.status.code(), Some(0));
    assert_shell_window_names(
        &stdout(&windows),
        &[
            "0:before-two\n2:bash\n3:two\n",
            "0:before-two\n2:zsh\n3:two\n",
        ],
    );
    assert!(stderr(&windows).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn select_layout_old_flag_reapplies_the_saved_layout() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("select-layout-old-flag")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-x", "90", "-y", "30"])?);
    assert_success(&harness.run(&["split-window", "-h", "-t", "alpha:0.0"])?);
    assert_success(&harness.run(&["split-window", "-v", "-t", "alpha:0.1"])?);

    assert_success(&harness.run(&["select-layout", "-t", "alpha:0", "main-vertical"])?);
    let vertical = harness.run(&["list-windows", "-t", "alpha", "-F", "#{window_layout}"])?;
    assert_eq!(vertical.status.code(), Some(0));
    assert!(stderr(&vertical).is_empty());
    let vertical_layout = stdout(&vertical);

    assert_success(&harness.run(&["select-layout", "-t", "alpha:0", "tiled"])?);
    let tiled = harness.run(&["list-windows", "-t", "alpha", "-F", "#{window_layout}"])?;
    assert_eq!(tiled.status.code(), Some(0));
    assert!(stderr(&tiled).is_empty());
    assert_ne!(stdout(&tiled), vertical_layout);

    assert_success(&harness.run(&["select-layout", "-o", "-t", "alpha:0"])?);
    let old = harness.run(&["list-windows", "-t", "alpha", "-F", "#{window_layout}"])?;
    assert_eq!(old.status.code(), Some(0));
    assert!(stderr(&old).is_empty());
    assert_eq!(stdout(&old), vertical_layout);

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn select_layout_accepts_mirrored_layout_names_like_tmux() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("select-layout-mirrored-accepted")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    for layout in ["main-horizontal-mirrored", "main-vertical-mirrored"] {
        let output = harness.run(&["select-layout", "-t", "alpha:0", layout])?;
        assert_eq!(output.status.code(), Some(0), "layout={layout}");
        assert!(stdout(&output).is_empty(), "layout={layout}");
        assert!(stderr(&output).is_empty(), "layout={layout}");
    }

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn select_layout_uses_window_main_pane_size_options_like_tmux() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("select-layout-main-pane-size-options")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-x", "120", "-y", "35"])?);
    assert_success(&harness.run(&["split-window", "-h", "-t", "alpha:0"])?);
    assert_success(&harness.run(&[
        "set-window-option",
        "-t",
        "alpha:0",
        "main-pane-width",
        "90",
    ])?);
    assert_success(&harness.run(&[
        "set-window-option",
        "-t",
        "alpha:0",
        "main-pane-height",
        "10",
    ])?);

    assert_success(&harness.run(&["select-layout", "-t", "alpha:0", "main-vertical"])?);
    let vertical = harness.run(&[
        "list-panes",
        "-t",
        "alpha:0",
        "-F",
        "#{pane_index}:#{pane_left},#{pane_width}",
    ])?;
    assert_eq!(vertical.status.code(), Some(0));
    assert_eq!(stdout(&vertical), "0:0,90\n1:91,29\n");

    assert_success(&harness.run(&["select-layout", "-t", "alpha:0", "main-horizontal"])?);
    let horizontal = harness.run(&[
        "list-panes",
        "-t",
        "alpha:0",
        "-F",
        "#{pane_index}:#{pane_top},#{pane_height}",
    ])?;
    assert_eq!(horizontal.status.code(), Some(0));
    assert_eq!(stdout(&horizontal), "0:0,10\n1:11,24\n");

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn kill_pane_all_except_keeps_target_and_removes_other_panes() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("kill-pane-all-except")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-x", "100", "-y", "30"])?);
    assert_success(&harness.run(&["split-window", "-h", "-t", "alpha:0.0"])?);
    assert_success(&harness.run(&["split-window", "-v", "-t", "alpha:0.1"])?);
    assert_success(&harness.run(&["kill-pane", "-a", "-t", "alpha:0.0"])?);

    let panes = harness.run(&[
        "list-panes",
        "-t",
        "alpha:0",
        "-F",
        "#{pane_index}:#{pane_active}:#{pane_left},#{pane_top},#{pane_width}x#{pane_height}",
    ])?;
    assert_eq!(panes.status.code(), Some(0));
    assert_eq!(stdout(&panes), "0:1:0,0,100x30\n");
    assert!(stderr(&panes).is_empty());

    let single = harness.run(&["kill-pane", "-a", "-t", "alpha:0.0"])?;
    assert_eq!(single.status.code(), Some(0));
    assert!(stdout(&single).is_empty());
    assert!(stderr(&single).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn pane_border_status_reserves_outer_edge_rows() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("pane-border-status-edge-rows")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-x", "80", "-y", "24"])?);
    assert_success(&harness.run(&["set-option", "-g", "status", "off"])?);

    let single = harness.run(&[
        "display-message",
        "-p",
        "-t",
        "alpha",
        "#{pane_width}x#{pane_height}:#{pane_top}:#{pane_bottom}:#{window_height}",
    ])?;
    assert_eq!(single.status.code(), Some(0));
    assert_eq!(stdout(&single), "80x24:0:23:24\n");
    assert!(stderr(&single).is_empty());

    assert_success(&harness.run(&["set-option", "-g", "pane-border-status", "top"])?);
    let top = harness.run(&[
        "display-message",
        "-p",
        "-t",
        "alpha",
        "#{pane_width}x#{pane_height}:#{pane_top}:#{pane_bottom}:#{window_height}",
    ])?;
    assert_eq!(top.status.code(), Some(0));
    assert_eq!(stdout(&top), "80x23:1:23:24\n");
    assert!(stderr(&top).is_empty());

    assert_success(&harness.run(&["set-option", "-g", "pane-border-status", "bottom"])?);
    let bottom = harness.run(&[
        "display-message",
        "-p",
        "-t",
        "alpha",
        "#{pane_width}x#{pane_height}:#{pane_top}:#{pane_bottom}:#{window_height}",
    ])?;
    assert_eq!(bottom.status.code(), Some(0));
    assert_eq!(stdout(&bottom), "80x23:0:22:24\n");
    assert!(stderr(&bottom).is_empty());

    assert_success(&harness.run(&["set-option", "-g", "pane-border-status", "off"])?);
    assert_success(&harness.run(&["split-window", "-v", "-t", "alpha"])?);

    assert_success(&harness.run(&["set-option", "-g", "pane-border-status", "top"])?);
    let split_top = harness.run(&[
        "list-panes",
        "-t",
        "alpha",
        "-F",
        "#{pane_index}:#{pane_width}x#{pane_height}:#{pane_top}:#{pane_bottom}",
    ])?;
    assert_eq!(split_top.status.code(), Some(0));
    assert_eq!(stdout(&split_top), "0:80x11:1:11\n1:80x11:13:23\n");
    assert!(stderr(&split_top).is_empty());

    assert_success(&harness.run(&["set-option", "-g", "pane-border-status", "bottom"])?);
    let split_bottom = harness.run(&[
        "list-panes",
        "-t",
        "alpha",
        "-F",
        "#{pane_index}:#{pane_width}x#{pane_height}:#{pane_top}:#{pane_bottom}",
    ])?;
    assert_eq!(split_bottom.status.code(), Some(0));
    assert_eq!(stdout(&split_bottom), "0:80x12:0:11\n1:80x10:13:22\n");
    assert!(stderr(&split_bottom).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn swap_window_accepts_relative_targets() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("swap-window-relative-target")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-x", "80", "-y", "24"])?);
    assert_success(&harness.run(&["new-window", "-t", "alpha", "-n", "two"])?);
    assert_success(&harness.run(&["new-window", "-t", "alpha", "-n", "three"])?);
    assert_success(&harness.run(&["select-window", "-t", "alpha:1"])?);

    assert_success(&harness.run(&["swap-window", "-t", "-1"])?);

    let listed = harness.run(&["list-windows", "-F", "#{window_index}:#{window_name}"])?;
    assert_eq!(listed.status.code(), Some(0));
    assert_shell_window_names(
        &stdout(&listed),
        &[
            "0:two\n1:bash\n2:three\n",
            "0:two\n1:zsh\n2:three\n",
            "0:two\n1:sh\n2:three\n",
        ],
    );
    assert!(stderr(&listed).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn link_window_accepts_relative_targets() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("link-window-relative-target")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-x", "80", "-y", "24"])?);
    assert_success(&harness.run(&["new-window", "-t", "alpha", "-n", "two"])?);
    assert_success(&harness.run(&["select-window", "-t", "alpha:1"])?);

    let output = harness.run(&["link-window", "-s", "alpha:1", "-t", "-1"])?;

    assert_eq!(output.status.code(), Some(1));
    assert!(stdout(&output).is_empty());
    assert_eq!(stderr(&output), "index in use: 0\n");

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn link_window_numeric_target_uses_current_session_window_index() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("link-window-numeric-target")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-x", "80", "-y", "24"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "beta", "-x", "80", "-y", "24"])?);
    assert_success(&harness.run(&["select-window", "-t", "beta:0"])?);
    assert_success(&harness.run(&["link-window", "-s", "alpha:0", "-t", "3"])?);

    let listed = harness.run(&[
        "list-windows",
        "-t",
        "beta",
        "-F",
        "#{window_index}:#{window_name}",
    ])?;
    assert_eq!(listed.status.code(), Some(0));
    assert_shell_window_names(
        &stdout(&listed),
        &["0:bash\n3:bash\n", "0:zsh\n3:zsh\n", "0:sh\n3:sh\n"],
    );
    assert!(stderr(&listed).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn link_window_session_only_target_uses_first_available_index() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("link-window-session-target-first-free")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "source"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "beta", "-n", "dest"])?);
    assert_success(&harness.run(&["link-window", "-s", "alpha:0", "-t", "beta:"])?);

    let listed = harness.run(&[
        "list-windows",
        "-a",
        "-F",
        "#{session_name}:#{window_index}:#{window_name}",
    ])?;
    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(
        stdout(&listed),
        "alpha:0:source\nbeta:0:dest\nbeta:1:source\n"
    );
    assert!(stderr(&listed).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn link_window_exact_session_target_uses_first_available_index() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("link-window-exact-session-target")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "source"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "beta", "-n", "dest"])?);
    assert_success(&harness.run(&["link-window", "-s", "alpha:0", "-t", "=beta"])?);

    let listed = harness.run(&[
        "list-windows",
        "-a",
        "-F",
        "#{session_name}:#{window_index}:#{window_name}",
    ])?;
    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(
        stdout(&listed),
        "alpha:0:source\nbeta:0:dest\nbeta:1:source\n"
    );
    assert!(stderr(&listed).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn link_window_bare_session_target_uses_first_available_index() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("link-window-bare-session-target")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "source"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "beta", "-n", "dest"])?);
    assert_success(&harness.run(&["link-window", "-s", "alpha:0", "-t", "beta"])?);

    let listed = harness.run(&[
        "list-windows",
        "-a",
        "-F",
        "#{session_name}:#{window_index}:#{window_name}",
    ])?;
    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(
        stdout(&listed),
        "alpha:0:source\nbeta:0:dest\nbeta:1:source\n"
    );
    assert!(stderr(&listed).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn link_window_bare_session_target_ignores_matching_window_name_in_session(
) -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("link-window-bare-session-window-prefix")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "source"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "beta", "-n", "beta0"])?);

    assert_success(&harness.run(&["link-window", "-s", "alpha:0", "-t", "beta"])?);

    let listed = harness.run(&[
        "list-windows",
        "-a",
        "-F",
        "#{session_name}:#{window_index}:#{window_name}",
    ])?;
    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(
        stdout(&listed),
        "alpha:0:source\nbeta:0:beta0\nbeta:1:source\n"
    );
    assert!(stderr(&listed).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn link_window_bare_session_target_prefers_exact_current_window_name() -> Result<(), Box<dyn Error>>
{
    let _guard = window_surface_guard();
    let harness = CliHarness::new("link-window-bare-session-exact-window")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "source"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "beta", "-n", "beta"])?);

    let output = harness.run(&["link-window", "-s", "alpha:0", "-t", "beta"])?;

    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("index in use: 0"),
        "{}",
        stderr(&output)
    );

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn link_window_special_end_target_resolves_as_window_target() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("link-window-special-end-target")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "zero"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "alpha:1", "-n", "one"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "alpha:2", "-n", "two"])?);

    assert_success(&harness.run(&["link-window", "-k", "-s", "alpha:0", "-t", "{end}"])?);

    let listed = harness.run(&[
        "list-windows",
        "-t",
        "alpha",
        "-F",
        "#{window_index}:#{window_name}",
    ])?;
    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(stdout(&listed), "0:zero\n1:one\n2:zero\n");
    assert!(stderr(&listed).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn link_window_caret_target_resolves_as_window_target() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("link-window-caret-target")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "zero"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "alpha:1", "-n", "one"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "alpha:2", "-n", "two"])?);

    assert_success(&harness.run(&["link-window", "-k", "-s", "alpha:1", "-t", "^"])?);

    let listed = harness.run(&[
        "list-windows",
        "-t",
        "alpha",
        "-F",
        "#{window_index}:#{window_name}",
    ])?;
    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(stdout(&listed), "0:one\n1:one\n2:two\n");
    assert!(stderr(&listed).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn link_window_sign_only_target_uses_relative_current_window() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("link-window-sign-target")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "source"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "beta", "-n", "zero"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "beta:2", "-n", "two"])?);
    assert_success(&harness.run(&["select-window", "-t", "beta:0"])?);
    assert_success(&harness.run(&["link-window", "-s", "alpha:0", "-t", "+"])?);

    let listed = harness.run(&[
        "list-windows",
        "-t",
        "beta",
        "-F",
        "#{window_index}:#{window_name}",
    ])?;
    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(stdout(&listed), "0:zero\n1:source\n2:two\n");
    assert!(stderr(&listed).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn source_file_link_window_sign_target_uses_relative_current_window() -> Result<(), Box<dyn Error>>
{
    let _guard = window_surface_guard();
    let harness = CliHarness::new("source-link-window-sign-target")?;
    let mut daemon = harness.start_hidden_daemon()?;
    let source_file = harness.tmpdir().join("link-window.conf");

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "source"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "beta", "-n", "zero"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "beta:2", "-n", "two"])?);
    assert_success(&harness.run(&["select-window", "-t", "beta:0"])?);
    fs::write(&source_file, "link-window -s alpha:0 -t +\n")?;
    assert_success(&harness.run(&["source-file", source_file.to_str().expect("utf-8 path")])?);

    let listed = harness.run(&[
        "list-windows",
        "-t",
        "beta",
        "-F",
        "#{window_index}:#{window_name}",
    ])?;
    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(stdout(&listed), "0:zero\n1:source\n2:two\n");
    assert!(stderr(&listed).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn source_file_link_window_placement_is_not_flag_order_dependent() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("source-link-window-placement-order")?;
    let mut daemon = harness.start_hidden_daemon()?;
    let source_file = harness.tmpdir().join("link-window-order.conf");

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "source"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "beta", "-n", "zero"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "beta:2", "-n", "two"])?);
    assert_success(&harness.run(&["select-window", "-t", "beta:0"])?);
    fs::write(&source_file, "link-window -s alpha:0 -t beta:+1 -a\n")?;
    assert_success(&harness.run(&["source-file", source_file.to_str().expect("utf-8 path")])?);

    let listed = harness.run(&[
        "list-windows",
        "-t",
        "beta",
        "-F",
        "#{window_index}:#{window_name}",
    ])?;
    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(stdout(&listed), "0:zero\n1:source\n2:two\n");
    assert!(stderr(&listed).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn link_window_session_qualified_sign_target_uses_session_active_window(
) -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("link-window-session-sign-target")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "zero"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "alpha:2", "-n", "two"])?);
    assert_success(&harness.run(&["select-window", "-t", "alpha:2"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "beta", "-n", "source"])?);
    assert_success(&harness.run(&["link-window", "-s", "beta:0", "-t", "alpha:+3"])?);

    let listed = harness.run(&[
        "list-windows",
        "-t",
        "alpha",
        "-F",
        "#{window_index}:#{window_name}",
    ])?;
    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(stdout(&listed), "0:zero\n2:two\n5:source\n");
    assert!(stderr(&listed).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn placement_commands_session_qualified_sign_target_use_session_active_window(
) -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("placement-session-sign-target")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "s", "-n", "zero"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "s", "-n", "one"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "s", "-n", "two"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "move-src", "-n", "move"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "link-src", "-n", "link"])?);
    assert_success(&harness.run(&["select-window", "-t", "s:1"])?);
    assert_success(&harness.run(&["move-window", "-a", "-s", "move-src:0", "-t", "s:+1"])?);
    assert_success(&harness.run(&["select-window", "-t", "s:1"])?);
    assert_success(&harness.run(&["link-window", "-a", "-s", "link-src:0", "-t", "s:+1"])?);

    let listed = harness.run(&[
        "list-windows",
        "-a",
        "-F",
        "#{session_name}:#{window_index}:#{window_name}",
    ])?;
    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(
        stdout(&listed),
        "link-src:0:link\ns:0:zero\ns:1:one\ns:2:link\ns:3:move\ns:4:two\n"
    );
    assert!(stderr(&listed).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn move_window_linked_source_preserves_surviving_link_runtime() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("move-linked-window-runtime")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "shared"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "beta", "-n", "b0"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "gamma", "-n", "g0"])?);
    assert_success(&harness.run(&[
        "set-hook",
        "-w",
        "-t",
        "alpha:0",
        "window-layout-changed",
        "display-message moved-hook",
    ])?);
    assert_success(&harness.run(&["link-window", "-s", "alpha:0", "-t", "beta:1"])?);
    assert_success(&harness.run(&["move-window", "-s", "alpha:0", "-t", "gamma:1"])?);

    let listed = harness.run(&[
        "list-windows",
        "-a",
        "-F",
        "#{session_name}:#{window_index}:#{window_name}",
    ])?;
    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(
        stdout(&listed),
        "beta:0:b0\nbeta:1:shared\ngamma:0:g0\ngamma:1:shared\n"
    );
    assert!(stderr(&listed).is_empty());

    let captured = harness.run(&["capture-pane", "-p", "-t", "beta:1.0"])?;
    assert_eq!(captured.status.code(), Some(0));
    assert!(stderr(&captured).is_empty());

    let moved_hook =
        harness.run(&["show-hooks", "-w", "-t", "gamma:1", "window-layout-changed"])?;
    assert_eq!(moved_hook.status.code(), Some(0));
    assert_eq!(
        stdout(&moved_hook),
        "window-layout-changed[0] display-message moved-hook\n"
    );
    assert!(stderr(&moved_hook).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn new_window_sign_only_target_uses_relative_current_window() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("new-window-sign-target")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "zero"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "+", "-n", "plus"])?);

    let listed = harness.run(&[
        "list-windows",
        "-t",
        "alpha",
        "-F",
        "#{window_index}:#{window_name}",
    ])?;
    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(stdout(&listed), "0:zero\n1:plus\n");
    assert!(stderr(&listed).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn new_window_special_targets_resolve_as_window_indices() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("new-window-special-target")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "zero"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "alpha:1", "-n", "one"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "alpha:2", "-n", "two"])?);
    assert_success(&harness.run(&["select-window", "-t", "alpha:1"])?);
    assert_success(&harness.run(&["select-window", "-t", "alpha:2"])?);

    assert_success(&harness.run(&["new-window", "-d", "-k", "-t", "^", "-n", "caret"])?);
    assert_success(&harness.run(&["new-window", "-d", "-k", "-t", "!", "-n", "bang"])?);

    let listed = harness.run(&[
        "list-windows",
        "-t",
        "alpha",
        "-F",
        "#{window_index}:#{window_name}",
    ])?;
    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(stdout(&listed), "0:caret\n1:bang\n2:two\n");
    assert!(stderr(&listed).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn move_window_without_target_uses_current_session_destination() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("move-window-current-session-dest")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&[
        "new-session",
        "-d",
        "-s",
        "alpha",
        "-n",
        "from",
        "-x",
        "80",
        "-y",
        "24",
    ])?);
    assert_success(&harness.run(&[
        "new-session",
        "-d",
        "-s",
        "beta",
        "-n",
        "dest",
        "-x",
        "80",
        "-y",
        "24",
    ])?);
    assert_success(&harness.run(&["select-window", "-t", "beta:0"])?);
    assert_success(&harness.run(&["move-window", "-s", "alpha:0"])?);

    let alpha = harness.run(&[
        "list-windows",
        "-t",
        "alpha",
        "-F",
        "#{window_index}:#{window_name}",
    ])?;
    assert_eq!(alpha.status.code(), Some(1));
    assert!(stdout(&alpha).is_empty());

    let beta = harness.run(&[
        "list-windows",
        "-t",
        "beta",
        "-F",
        "#{window_index}:#{window_name}",
    ])?;
    assert_eq!(beta.status.code(), Some(0));
    assert_eq!(stdout(&beta), "0:dest\n1:from\n");
    assert!(stderr(&beta).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn new_window_session_qualified_sign_target_uses_session_active_window(
) -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("new-window-session-sign-target")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "zero"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "alpha:2", "-n", "two"])?);
    assert_success(&harness.run(&["select-window", "-t", "alpha:2"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "alpha:+3", "-n", "plus"])?);

    let listed = harness.run(&[
        "list-windows",
        "-t",
        "alpha",
        "-F",
        "#{window_index}:#{window_name}",
    ])?;
    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(stdout(&listed), "0:zero\n2:two\n5:plus\n");
    assert!(stderr(&listed).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn new_window_placement_sign_target_uses_session_active_window() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("new-window-placement-sign-target")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "zero"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "alpha:2", "-n", "two"])?);
    assert_success(&harness.run(&["select-window", "-t", "alpha:2"])?);
    assert_success(&harness.run(&["new-window", "-a", "-d", "-t", "alpha:+1", "-n", "after"])?);
    assert_success(&harness.run(&["new-window", "-b", "-d", "-t", "alpha:+1", "-n", "before"])?);

    let listed = harness.run(&[
        "list-windows",
        "-t",
        "alpha",
        "-F",
        "#{window_index}:#{window_name}",
    ])?;
    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(stdout(&listed), "0:zero\n2:before\n3:two\n4:after\n");
    assert!(stderr(&listed).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn move_window_session_only_target_uses_first_available_index() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("move-window-session-target-first-free")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "from"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "beta", "-n", "dest"])?);
    assert_success(&harness.run(&["move-window", "-s", "alpha:0", "-t", "beta:"])?);

    let listed = harness.run(&[
        "list-windows",
        "-a",
        "-F",
        "#{session_name}:#{window_index}:#{window_name}",
    ])?;
    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(stdout(&listed), "beta:0:dest\nbeta:1:from\n");
    assert!(stderr(&listed).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn move_window_exact_session_target_uses_first_available_index() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("move-window-exact-session-target")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "from"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "beta", "-n", "dest"])?);
    assert_success(&harness.run(&["move-window", "-s", "alpha:0", "-t", "=beta"])?);

    let listed = harness.run(&[
        "list-windows",
        "-a",
        "-F",
        "#{session_name}:#{window_index}:#{window_name}",
    ])?;
    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(stdout(&listed), "beta:0:dest\nbeta:1:from\n");
    assert!(stderr(&listed).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn move_window_bare_session_target_uses_first_available_index() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("move-window-bare-session-target")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "from"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "beta", "-n", "dest"])?);
    assert_success(&harness.run(&["move-window", "-s", "alpha:0", "-t", "beta"])?);

    let listed = harness.run(&[
        "list-windows",
        "-a",
        "-F",
        "#{session_name}:#{window_index}:#{window_name}",
    ])?;
    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(stdout(&listed), "beta:0:dest\nbeta:1:from\n");
    assert!(stderr(&listed).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn move_window_bare_session_target_ignores_matching_window_name_in_session(
) -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("move-window-bare-session-window-prefix")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "from"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "beta", "-n", "beta0"])?);

    assert_success(&harness.run(&["move-window", "-s", "alpha:0", "-t", "beta"])?);

    let listed = harness.run(&[
        "list-windows",
        "-a",
        "-F",
        "#{session_name}:#{window_index}:#{window_name}",
    ])?;
    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(stdout(&listed), "beta:0:beta0\nbeta:1:from\n");
    assert!(stderr(&listed).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn move_window_bare_session_target_prefers_exact_current_window_name() -> Result<(), Box<dyn Error>>
{
    let _guard = window_surface_guard();
    let harness = CliHarness::new("move-window-bare-session-exact-window")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "from"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "beta", "-n", "beta"])?);

    let output = harness.run(&["move-window", "-s", "alpha:0", "-t", "beta"])?;

    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("index in use: 0"),
        "{}",
        stderr(&output)
    );

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn move_window_after_without_target_uses_current_window_as_anchor() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("move-window-after-current-anchor")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "zero"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "alpha:1", "-n", "one"])?);
    assert_success(&harness.run(&["select-window", "-t", "alpha:0"])?);
    assert_success(&harness.run(&["move-window", "-a", "-s", "alpha:1"])?);

    let listed = harness.run(&[
        "list-windows",
        "-t",
        "alpha",
        "-F",
        "#{window_index}:#{window_name}",
    ])?;
    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(stdout(&listed), "0:zero\n1:one\n");
    assert!(stderr(&listed).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn move_window_after_session_target_uses_session_active_window_as_anchor(
) -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("move-window-after-session-anchor")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "zero"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "alpha:2", "-n", "two"])?);
    assert_success(&harness.run(&["select-window", "-t", "alpha:2"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "beta", "-n", "source"])?);
    assert_success(&harness.run(&["move-window", "-a", "-s", "beta:0", "-t", "alpha"])?);

    let listed = harness.run(&[
        "list-windows",
        "-a",
        "-F",
        "#{session_name}:#{window_index}:#{window_name}",
    ])?;
    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(
        stdout(&listed),
        "alpha:0:zero\nalpha:2:two\nalpha:3:source\n"
    );
    assert!(stderr(&listed).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn move_window_special_end_target_resolves_as_window_target() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("move-window-special-end-target")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "zero"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "alpha:1", "-n", "one"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "alpha:2", "-n", "two"])?);

    assert_success(&harness.run(&["move-window", "-k", "-s", "alpha:0", "-t", "{end}"])?);

    let listed = harness.run(&[
        "list-windows",
        "-t",
        "alpha",
        "-F",
        "#{window_index}:#{window_name}",
    ])?;
    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(stdout(&listed), "1:one\n2:zero\n");
    assert!(stderr(&listed).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn move_window_sign_only_target_uses_relative_current_window() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("move-window-sign-target")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "source"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "beta", "-n", "zero"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "beta:2", "-n", "two"])?);
    assert_success(&harness.run(&["select-window", "-t", "beta:0"])?);
    assert_success(&harness.run(&["move-window", "-s", "alpha:0", "-t", "+"])?);

    let listed = harness.run(&[
        "list-windows",
        "-a",
        "-F",
        "#{session_name}:#{window_index}:#{window_name}",
    ])?;
    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(stdout(&listed), "beta:0:zero\nbeta:1:source\nbeta:2:two\n");
    assert!(stderr(&listed).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn move_window_bang_target_resolves_as_last_window() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("move-window-bang-target")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "zero"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "alpha:1", "-n", "one"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "alpha:2", "-n", "two"])?);
    assert_success(&harness.run(&["select-window", "-t", "alpha:1"])?);
    assert_success(&harness.run(&["select-window", "-t", "alpha:2"])?);

    assert_success(&harness.run(&["move-window", "-k", "-s", "alpha:0", "-t", "!"])?);

    let listed = harness.run(&[
        "list-windows",
        "-t",
        "alpha",
        "-F",
        "#{window_index}:#{window_name}",
    ])?;
    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(stdout(&listed), "1:zero\n2:two\n");
    assert!(stderr(&listed).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn move_window_session_qualified_sign_target_uses_session_active_window(
) -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("move-window-session-sign-target")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "zero"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "alpha:2", "-n", "two"])?);
    assert_success(&harness.run(&["select-window", "-t", "alpha:2"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "beta", "-n", "source"])?);
    assert_success(&harness.run(&["move-window", "-s", "beta:0", "-t", "alpha:+3"])?);

    let listed = harness.run(&[
        "list-windows",
        "-a",
        "-F",
        "#{session_name}:#{window_index}:#{window_name}",
    ])?;
    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(
        stdout(&listed),
        "alpha:0:zero\nalpha:2:two\nalpha:5:source\n"
    );
    assert!(stderr(&listed).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn link_and_move_window_placement_sign_targets_use_session_active_window(
) -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("window-placement-sign-target")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "zero"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "alpha:2", "-n", "two"])?);
    assert_success(&harness.run(&["select-window", "-t", "alpha:2"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "beta", "-n", "link-source"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "beta:1", "-n", "move-source"])?);

    assert_success(&harness.run(&["link-window", "-a", "-s", "beta:0", "-t", "alpha:+1"])?);
    assert_success(&harness.run(&["select-window", "-t", "alpha:2"])?);
    assert_success(&harness.run(&["move-window", "-a", "-s", "beta:1", "-t", "alpha:+1"])?);

    let listed = harness.run(&[
        "list-windows",
        "-a",
        "-F",
        "#{session_name}:#{window_index}:#{window_name}",
    ])?;
    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(
        stdout(&listed),
        "alpha:0:zero\nalpha:2:two\nalpha:3:move-source\nalpha:4:link-source\nbeta:0:link-source\n"
    );
    assert!(stderr(&listed).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn move_window_relative_across_sessions_removes_single_window_source() -> Result<(), Box<dyn Error>>
{
    let _guard = window_surface_guard();
    let harness = CliHarness::new("move-window-relative-single-source")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "source"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "beta", "-n", "zero"])?);
    assert_success(&harness.run(&["move-window", "-a", "-s", "alpha:0", "-t", "beta:0"])?);

    let sessions = harness.run(&["list-sessions", "-F", "#{session_name}"])?;
    assert_eq!(sessions.status.code(), Some(0));
    assert_eq!(stdout(&sessions), "beta\n");
    assert!(stderr(&sessions).is_empty());

    let windows = harness.run(&[
        "list-windows",
        "-t",
        "beta",
        "-F",
        "#{window_index}:#{window_name}",
    ])?;
    assert_eq!(windows.status.code(), Some(0));
    assert_eq!(stdout(&windows), "0:zero\n1:source\n");
    assert!(stderr(&windows).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn source_file_move_window_placement_sign_target_uses_session_active_window(
) -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("source-move-window-placement-sign-target")?;
    let mut daemon = harness.start_hidden_daemon()?;
    let source_file = harness.tmpdir().join("move-window.conf");

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "zero"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "alpha:2", "-n", "two"])?);
    assert_success(&harness.run(&["select-window", "-t", "alpha:2"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "beta", "-n", "source"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "beta:1", "-n", "keep"])?);
    fs::write(&source_file, "move-window -a -s beta:0 -t alpha:+1\n")?;
    assert_success(&harness.run(&["source-file", source_file.to_str().expect("utf-8 path")])?);

    let listed = harness.run(&[
        "list-windows",
        "-a",
        "-F",
        "#{session_name}:#{window_index}:#{window_name}",
    ])?;
    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(
        stdout(&listed),
        "alpha:0:zero\nalpha:2:two\nalpha:3:source\nbeta:1:keep\n"
    );
    assert!(stderr(&listed).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn queued_source_file_target_sets_context_for_sourced_commands() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("queued-source-file-target-context")?;
    let mut daemon = harness.start_hidden_daemon()?;
    let source_file = harness.tmpdir().join("queued-source.conf");

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "a0"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "beta", "-n", "b0"])?);
    fs::write(&source_file, "new-window -d -n sourced\n")?;
    assert_success(&harness.run(&[
        "run-shell",
        "-t",
        "alpha:0.0",
        "-C",
        &format!("source-file -t beta:0 {}", source_file.display()),
    ])?);

    let listed = harness.run(&[
        "list-windows",
        "-a",
        "-F",
        "#{session_name}:#{window_index}:#{window_name}",
    ])?;
    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(stdout(&listed), "alpha:0:a0\nbeta:0:b0\nbeta:1:sourced\n");
    assert!(stderr(&listed).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn source_file_target_context_flows_into_run_shell_commands() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("source-run-shell-target-context")?;
    let mut daemon = harness.start_hidden_daemon()?;
    let source_file = harness.tmpdir().join("run-shell-target.conf");

    assert_success(&harness.run(&["new-session", "-d", "-s", "beta", "-n", "b0"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "a0"])?);
    fs::write(&source_file, "run-shell -C 'new-window -d -n nested'\n")?;
    assert_success(&harness.run(&[
        "source-file",
        "-t",
        "beta:0.0",
        source_file.to_str().expect("utf-8 path"),
    ])?);

    let listed = harness.run(&[
        "list-windows",
        "-a",
        "-F",
        "#{session_name}:#{window_index}:#{window_name}",
    ])?;
    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(stdout(&listed), "alpha:0:a0\nbeta:0:b0\nbeta:1:nested\n");
    assert!(stderr(&listed).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn source_file_window_session_targets_use_base_index() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("source-window-session-base-index")?;
    let mut daemon = harness.start_hidden_daemon()?;
    let link_file = harness.tmpdir().join("link-window.conf");
    let move_file = harness.tmpdir().join("move-window.conf");

    assert_success(&harness.run(&["set-option", "-g", "base-index", "3"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "link-anchor"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "alpha:5", "-n", "link-src"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "beta", "-n", "link-dst"])?);
    fs::write(&link_file, "link-window -s alpha:5 -t beta\n")?;
    assert_success(&harness.run(&["source-file", link_file.to_str().expect("utf-8 path")])?);

    let linked = harness.run(&[
        "list-windows",
        "-a",
        "-F",
        "#{session_name}:#{window_index}:#{window_name}",
    ])?;
    assert_eq!(linked.status.code(), Some(0));
    assert_eq!(
        stdout(&linked),
        "alpha:3:link-anchor\nalpha:5:link-src\nbeta:3:link-dst\nbeta:4:link-src\n"
    );
    assert!(stderr(&linked).is_empty());

    assert_success(&harness.run(&["new-session", "-d", "-s", "gamma", "-n", "move-anchor"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "gamma:6", "-n", "move-src"])?);
    fs::write(&move_file, "move-window -s gamma:6 -t beta\n")?;
    assert_success(&harness.run(&["source-file", move_file.to_str().expect("utf-8 path")])?);

    let moved = harness.run(&[
        "list-windows",
        "-a",
        "-F",
        "#{session_name}:#{window_index}:#{window_name}",
    ])?;
    assert_eq!(moved.status.code(), Some(0));
    assert_eq!(
        stdout(&moved),
        "alpha:3:link-anchor\nalpha:5:link-src\nbeta:3:link-dst\nbeta:4:link-src\nbeta:5:move-src\ngamma:3:move-anchor\n"
    );
    assert!(stderr(&moved).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn source_file_link_window_without_target_uses_current_session() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("source-link-window-no-target")?;
    let mut daemon = harness.start_hidden_daemon()?;
    let source_file = harness.tmpdir().join("link-window.conf");

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "src"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "beta", "-n", "dst"])?);
    fs::write(&source_file, "link-window -s alpha:0\n")?;
    assert_success(&harness.run(&["source-file", source_file.to_str().expect("utf-8 path")])?);

    let listed = harness.run(&[
        "list-windows",
        "-a",
        "-F",
        "#{session_name}:#{window_index}:#{window_name}",
    ])?;
    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(stdout(&listed), "alpha:0:src\nbeta:0:dst\nbeta:1:src\n");
    assert!(stderr(&listed).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn source_file_link_window_session_only_target_uses_first_available_index(
) -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("source-link-window-session-target-first-free")?;
    let mut daemon = harness.start_hidden_daemon()?;
    let source_file = harness.tmpdir().join("link-window-session-target.conf");

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "src"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "beta", "-n", "dst"])?);
    fs::write(&source_file, "link-window -s alpha:0 -t beta:\n")?;
    assert_success(&harness.run(&["source-file", source_file.to_str().expect("utf-8 path")])?);

    let listed = harness.run(&[
        "list-windows",
        "-a",
        "-F",
        "#{session_name}:#{window_index}:#{window_name}",
    ])?;
    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(stdout(&listed), "alpha:0:src\nbeta:0:dst\nbeta:1:src\n");
    assert!(stderr(&listed).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn source_file_link_window_bare_session_target_uses_first_available_index(
) -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("source-link-window-bare-session-target")?;
    let mut daemon = harness.start_hidden_daemon()?;
    let source_file = harness
        .tmpdir()
        .join("link-window-bare-session-target.conf");

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "src"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "beta", "-n", "dst"])?);
    fs::write(&source_file, "link-window -s alpha:0 -t beta\n")?;
    assert_success(&harness.run(&["source-file", source_file.to_str().expect("utf-8 path")])?);

    let listed = harness.run(&[
        "list-windows",
        "-a",
        "-F",
        "#{session_name}:#{window_index}:#{window_name}",
    ])?;
    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(stdout(&listed), "alpha:0:src\nbeta:0:dst\nbeta:1:src\n");
    assert!(stderr(&listed).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn source_file_link_window_bare_session_target_ignores_matching_window_name_in_session(
) -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("source-link-window-bare-session-window-prefix")?;
    let mut daemon = harness.start_hidden_daemon()?;
    let source_file = harness
        .tmpdir()
        .join("link-window-bare-session-window-prefix.conf");

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "src"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "beta", "-n", "beta0"])?);
    fs::write(&source_file, "link-window -s alpha:0 -t beta\n")?;
    assert_success(&harness.run(&["source-file", source_file.to_str().expect("utf-8 path")])?);

    let listed = harness.run(&[
        "list-windows",
        "-a",
        "-F",
        "#{session_name}:#{window_index}:#{window_name}",
    ])?;
    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(stdout(&listed), "alpha:0:src\nbeta:0:beta0\nbeta:1:src\n");
    assert!(stderr(&listed).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn source_file_link_window_bare_session_target_prefers_exact_current_window_name(
) -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("source-link-window-bare-session-exact-window")?;
    let mut daemon = harness.start_hidden_daemon()?;
    let source_file = harness
        .tmpdir()
        .join("link-window-bare-session-exact-window.conf");

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "src"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "beta", "-n", "beta"])?);
    fs::write(&source_file, "link-window -s alpha:0 -t beta\n")?;

    let output = harness.run(&["source-file", source_file.to_str().expect("utf-8 path")])?;

    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("index in use: 0"),
        "{}",
        stderr(&output)
    );

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn source_file_move_window_without_target_uses_current_session() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("source-move-window-no-target")?;
    let mut daemon = harness.start_hidden_daemon()?;
    let source_file = harness.tmpdir().join("move-window.conf");

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "src"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "beta", "-n", "dst"])?);
    fs::write(&source_file, "move-window -s alpha:0\n")?;
    assert_success(&harness.run(&["source-file", source_file.to_str().expect("utf-8 path")])?);

    let listed = harness.run(&[
        "list-windows",
        "-a",
        "-F",
        "#{session_name}:#{window_index}:#{window_name}",
    ])?;
    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(stdout(&listed), "beta:0:dst\nbeta:1:src\n");
    assert!(stderr(&listed).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn source_file_move_window_session_only_target_uses_first_available_index(
) -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("source-move-window-session-target-first-free")?;
    let mut daemon = harness.start_hidden_daemon()?;
    let source_file = harness.tmpdir().join("move-window-session-target.conf");

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "src"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "beta", "-n", "dst"])?);
    fs::write(&source_file, "move-window -s alpha:0 -t beta:\n")?;
    assert_success(&harness.run(&["source-file", source_file.to_str().expect("utf-8 path")])?);

    let listed = harness.run(&[
        "list-windows",
        "-a",
        "-F",
        "#{session_name}:#{window_index}:#{window_name}",
    ])?;
    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(stdout(&listed), "beta:0:dst\nbeta:1:src\n");
    assert!(stderr(&listed).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn source_file_move_window_bare_session_target_uses_first_available_index(
) -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("source-move-window-bare-session-target")?;
    let mut daemon = harness.start_hidden_daemon()?;
    let source_file = harness
        .tmpdir()
        .join("move-window-bare-session-target.conf");

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "src"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "beta", "-n", "dst"])?);
    fs::write(&source_file, "move-window -s alpha:0 -t beta\n")?;
    assert_success(&harness.run(&["source-file", source_file.to_str().expect("utf-8 path")])?);

    let listed = harness.run(&[
        "list-windows",
        "-a",
        "-F",
        "#{session_name}:#{window_index}:#{window_name}",
    ])?;
    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(stdout(&listed), "beta:0:dst\nbeta:1:src\n");
    assert!(stderr(&listed).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn source_file_move_window_bare_session_target_ignores_matching_window_name_in_session(
) -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("source-move-window-bare-session-window-prefix")?;
    let mut daemon = harness.start_hidden_daemon()?;
    let source_file = harness
        .tmpdir()
        .join("move-window-bare-session-window-prefix.conf");

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "src"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "beta", "-n", "beta0"])?);
    fs::write(&source_file, "move-window -s alpha:0 -t beta\n")?;
    assert_success(&harness.run(&["source-file", source_file.to_str().expect("utf-8 path")])?);

    let listed = harness.run(&[
        "list-windows",
        "-a",
        "-F",
        "#{session_name}:#{window_index}:#{window_name}",
    ])?;
    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(stdout(&listed), "beta:0:beta0\nbeta:1:src\n");
    assert!(stderr(&listed).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn source_file_move_window_bare_session_target_prefers_exact_current_window_name(
) -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("source-move-window-bare-session-exact-window")?;
    let mut daemon = harness.start_hidden_daemon()?;
    let source_file = harness
        .tmpdir()
        .join("move-window-bare-session-exact-window.conf");

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "src"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "beta", "-n", "beta"])?);
    fs::write(&source_file, "move-window -s alpha:0 -t beta\n")?;

    let output = harness.run(&["source-file", source_file.to_str().expect("utf-8 path")])?;

    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("index in use: 0"),
        "{}",
        stderr(&output)
    );

    terminate_child(daemon.child_mut())?;
    Ok(())
}

fn assert_source_file_window_alias_bare_session_first_available(
    command: &str,
    harness_name: &str,
    expected_windows: &str,
) -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new(harness_name)?;
    let mut daemon = harness.start_hidden_daemon()?;
    let source_file = harness.tmpdir().join(format!("{harness_name}.conf"));

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "src"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "beta", "-n", "beta0"])?);
    fs::write(&source_file, format!("{command} -s alpha:0 -t beta\n"))?;
    assert_success(&harness.run(&["source-file", source_file.to_str().expect("utf-8 path")])?);

    let listed = harness.run(&[
        "list-windows",
        "-a",
        "-F",
        "#{session_name}:#{window_index}:#{window_name}",
    ])?;
    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(stdout(&listed), expected_windows);
    assert!(stderr(&listed).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn source_file_link_window_alias_bare_session_target_ignores_matching_window_name_in_session(
) -> Result<(), Box<dyn Error>> {
    assert_source_file_window_alias_bare_session_first_available(
        "linkw",
        "source-link-window-alias-bare-session-window-prefix",
        "alpha:0:src\nbeta:0:beta0\nbeta:1:src\n",
    )
}

#[test]
fn source_file_move_window_alias_bare_session_target_ignores_matching_window_name_in_session(
) -> Result<(), Box<dyn Error>> {
    assert_source_file_window_alias_bare_session_first_available(
        "movew",
        "source-move-window-alias-bare-session-window-prefix",
        "beta:0:beta0\nbeta:1:src\n",
    )
}

#[test]
fn source_file_swap_window_without_target_uses_current_window() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("source-swap-window-current-target")?;
    let mut daemon = harness.start_hidden_daemon()?;
    let source_file = harness.tmpdir().join("swap-window.conf");

    assert_success(&harness.run(&["new-session", "-d", "-s", "s", "-n", "zero"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "s", "-n", "one"])?);
    assert_success(&harness.run(&["select-window", "-t", "s:0"])?);
    fs::write(&source_file, "swap-window -s s:1\n")?;
    assert_success(&harness.run(&["source-file", source_file.to_str().expect("utf-8 path")])?);

    let listed = harness.run(&[
        "list-windows",
        "-t",
        "s",
        "-F",
        "#{window_index}:#{window_name}",
    ])?;
    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(stdout(&listed), "0:one\n1:zero\n");
    assert!(stderr(&listed).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn source_file_unlink_window_without_target_uses_current_window() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("source-unlink-window-current-target")?;
    let mut daemon = harness.start_hidden_daemon()?;
    let source_file = harness.tmpdir().join("unlink-window.conf");

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "shared"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "beta", "-n", "beta"])?);
    assert_success(&harness.run(&["link-window", "-s", "alpha:0", "-t", "beta:1"])?);
    assert_success(&harness.run(&["select-window", "-t", "beta:1"])?);
    fs::write(&source_file, "unlink-window\n")?;
    assert_success(&harness.run(&["source-file", source_file.to_str().expect("utf-8 path")])?);

    let listed = harness.run(&[
        "list-windows",
        "-a",
        "-F",
        "#{session_name}:#{window_index}:#{window_name}",
    ])?;
    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(stdout(&listed), "alpha:0:shared\nbeta:0:beta\n");
    assert!(stderr(&listed).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn move_window_before_preserves_explicit_window_metadata() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("move-window-before-preserves-metadata")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&[
        "new-session",
        "-d",
        "-s",
        "alpha",
        "-n",
        "root",
        "-x",
        "80",
        "-y",
        "24",
    ])?);
    assert_success(&harness.run(&["new-window", "-t", "alpha", "-n", "a"])?);
    assert_success(&harness.run(&["new-window", "-t", "alpha", "-n", "b"])?);

    assert_success(&harness.run(&["move-window", "-b", "-s", "alpha:2", "-t", "alpha:0"])?);

    let listed = harness.run(&[
        "list-windows",
        "-t",
        "alpha",
        "-F",
        "#{window_index}:#{window_name}:#{automatic-rename}:#{window_active}",
    ])?;
    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(stdout(&listed), "0:b:0:1\n1:root:0:0\n2:a:0:0\n");
    assert!(stderr(&listed).is_empty());

    let shown = harness.run(&["show-window-options", "-t", "alpha:0", "automatic-rename"])?;
    assert_eq!(shown.status.code(), Some(0));
    assert_eq!(stdout(&shown), "automatic-rename off\n");
    assert!(stderr(&shown).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn session_base_environment_feeds_split_respawn_and_run_shell() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("session-base-env-spawns")?;
    let mut daemon = harness.start_hidden_daemon()?;
    let split_path = harness.tmpdir().join("split-env.txt");
    let respawn_path = harness.tmpdir().join("respawn-env.txt");
    let run_shell_path = harness.tmpdir().join("run-shell-env.txt");

    assert_success(&harness.run_with(
        &["new-session", "-d", "-s", "s", "sleep 60"],
        |command| {
            command.env("FOO", "base");
        },
    )?);
    assert_success(&harness.run(&["set-environment", "-t", "s", "-u", "FOO"])?);
    assert_success(&harness.run(&[
        "split-window",
        "-d",
        "-t",
        "s:0.0",
        &format!("printf '%s' \"$FOO\" > {}", shell_quote(&split_path)),
    ])?);
    assert_success(&harness.run(&[
        "run-shell",
        "-t",
        "s:0.0",
        &format!("printf '%s' \"$FOO\" > {}", shell_quote(&run_shell_path)),
    ])?);
    assert_success(&harness.run(&[
        "respawn-pane",
        "-k",
        "-t",
        "s:0.0",
        &format!(
            "printf '%s' \"$FOO\" > {}; sleep 1",
            shell_quote(&respawn_path)
        ),
    ])?);

    wait_for_file_contents(&split_path, "base", ATTACH_TIMEOUT)?;
    wait_for_file_contents(&run_shell_path, "base", ATTACH_TIMEOUT)?;
    wait_for_file_contents(&respawn_path, "base", ATTACH_TIMEOUT)?;

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn session_base_environment_preserves_raw_non_utf8_for_later_windows() -> Result<(), Box<dyn Error>>
{
    let _guard = window_surface_guard();
    let harness = CliHarness::new("session-base-env-raw")?;
    let mut daemon = harness.start_hidden_daemon()?;
    let count_path = harness.tmpdir().join("raw-env-count.txt");
    let raw_value = OsString::from_vec(b"A\xffB".to_vec());

    assert_success(&harness.run_with(
        &["new-session", "-d", "-s", "s", "sleep 60"],
        |command| {
            command.env("BAD_VAR", raw_value);
        },
    )?);
    assert_success(&harness.run(&[
        "new-window",
        "-d",
        "-t",
        "s:",
        &format!(
            "bytes=$(printf '%s' \"$BAD_VAR\" | wc -c | tr -d '[:space:]'); printf '%s' \"$bytes\" > {}",
            shell_quote(&count_path)
        ),
    ])?);

    wait_for_file_contents(&count_path, "3", ATTACH_TIMEOUT)?;

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn moved_window_preserves_source_base_environment_for_later_splits() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("moved-window-base-env")?;
    let mut daemon = harness.start_hidden_daemon()?;
    let output_path = harness.tmpdir().join("moved-env.txt");

    assert_success(&harness.run_with(
        &["new-session", "-d", "-s", "alpha", "sleep 60"],
        |command| {
            command.env("FOO", "alpha");
        },
    )?);
    assert_success(&harness.run_with(
        &["new-session", "-d", "-s", "beta", "sleep 60"],
        |command| {
            command.env("FOO", "beta");
        },
    )?);
    assert_success(&harness.run(&["move-window", "-s", "alpha:0", "-t", "beta:1"])?);
    assert_success(&harness.run(&[
        "split-window",
        "-d",
        "-t",
        "beta:1.0",
        &format!("printf '%s' \"$FOO\" > {}", shell_quote(&output_path)),
    ])?);

    wait_for_file_contents(&output_path, "alpha", ATTACH_TIMEOUT)?;

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn moved_window_preserves_source_base_environment_for_respawn_window() -> Result<(), Box<dyn Error>>
{
    let _guard = window_surface_guard();
    let harness = CliHarness::new("moved-window-respawn-base-env")?;
    let mut daemon = harness.start_hidden_daemon()?;
    let output_path = harness.tmpdir().join("respawn-window-env.txt");

    assert_success(&harness.run_with(
        &["new-session", "-d", "-s", "alpha", "sleep 60"],
        |command| {
            command.env("FOO", "alpha");
        },
    )?);
    assert_success(&harness.run_with(
        &["new-session", "-d", "-s", "beta", "sleep 60"],
        |command| {
            command.env("FOO", "beta");
        },
    )?);
    assert_success(&harness.run(&["move-window", "-s", "alpha:0", "-t", "beta:1"])?);
    assert_success(&harness.run(&[
        "respawn-window",
        "-k",
        "-t",
        "beta:1",
        &format!(
            "printf '%s' \"$FOO\" > {}; sleep 1",
            shell_quote(&output_path)
        ),
    ])?);

    wait_for_file_contents(&output_path, "alpha", ATTACH_TIMEOUT)?;

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn linked_window_survivor_preserves_source_base_environment_for_later_splits(
) -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("linked-window-base-env")?;
    let mut daemon = harness.start_hidden_daemon()?;
    let output_path = harness.tmpdir().join("linked-env.txt");

    assert_success(&harness.run_with(
        &["new-session", "-d", "-s", "alpha", "sleep 60"],
        |command| {
            command.env("FOO", "alpha");
        },
    )?);
    assert_success(&harness.run_with(
        &["new-session", "-d", "-s", "beta", "sleep 60"],
        |command| {
            command.env("FOO", "beta");
        },
    )?);
    assert_success(&harness.run(&["link-window", "-s", "alpha:0", "-t", "beta:1"])?);
    assert_success(&harness.run(&["kill-session", "-t", "alpha"])?);
    assert_success(&harness.run(&[
        "split-window",
        "-d",
        "-t",
        "beta:1.0",
        &format!("printf '%s' \"$FOO\" > {}", shell_quote(&output_path)),
    ])?);

    wait_for_file_contents(&output_path, "alpha", ATTACH_TIMEOUT)?;

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn unsupported_window_command_flags_fail_before_server_contact() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("unsupported-window-command-flags")?;

    for args in [
        &["new-window", "-t", "alpha", "-P"][..],
        &["new-window", "-t", "alpha", "echo"][..],
        &["next-window", "-a", "-t", "alpha"][..],
        &["previous-window", "-a", "-t", "alpha"][..],
        &["swap-window", "-a", "-s", "alpha:0", "-t", "alpha:1"][..],
        &["rename-window", "-t", "alpha:0", "logs", "extra"][..],
    ] {
        let output = harness.run(args)?;
        assert_clap_failure(&output);
        assert!(
            !harness.socket_path().exists(),
            "unsupported arguments must fail before touching the server"
        );
    }

    Ok(())
}

#[test]
fn unsupported_window_command_flags_use_tmux_error_text() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("unsupported-window-command-error-text")?;

    let swap = harness.run(&["swap-window", "-a", "-s", "alpha:0", "-t", "alpha:1"])?;
    assert_eq!(swap.status.code(), Some(1));
    assert_eq!(stdout(&swap), "");
    assert_eq!(stderr(&swap), "command swap-window: unknown flag -a\n");

    let rename = harness.run(&["rename-window", "-t", "alpha:0", "logs", "extra"])?;
    assert_eq!(rename.status.code(), Some(1));
    assert_eq!(stdout(&rename), "");
    assert_eq!(
        stderr(&rename),
        "command rename-window: too many arguments (need at most 1)\n"
    );

    Ok(())
}

#[test]
fn detach_client_requires_a_reachable_server() -> Result<(), Box<dyn Error>> {
    let _guard = window_surface_guard();
    let harness = CliHarness::new("detach-client-absent")?;
    let output = harness.run(&["detach-client"])?;

    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("no server running on "));
    assert!(stdout(&output).is_empty());
    Ok(())
}

fn wait_for_file_contents(
    path: &Path,
    expected: &str,
    timeout: Duration,
) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + timeout;

    while Instant::now() < deadline {
        match fs::read_to_string(path) {
            Ok(contents) if contents == expected => return Ok(()),
            Ok(_) | Err(_) => std::thread::sleep(Duration::from_millis(25)),
        }
    }

    Err(format!("timed out waiting for '{}'", path.display()).into())
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

fn assert_shell_window_names(actual: &str, accepted: &[&str]) {
    assert!(
        accepted.contains(&actual),
        "expected one of {accepted:?}, got {actual:?}"
    );
}

fn window_surface_guard() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
