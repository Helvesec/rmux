#![cfg(unix)]

mod common;

use std::error::Error;
use std::time::{Duration, Instant};

use common::{assert_success, stderr, stdout, AttachedSession, CliHarness};
use rmux_pty::TerminalSize;

#[test]
fn list_panes_all_sessions_prints_all_panes_across_session_windows() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("list-panes-cli")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["split-window", "-v", "-t", "alpha"])?);
    assert_success(&harness.run(&["new-window", "-t", "alpha", "-d", "-n", "logs"])?);

    let listed = harness.run(&[
        "list-panes",
        "-a",
        "-F",
        "#{session_name}:#{window_index}:#{pane_index}:#{pane_id}:#{pane_active}",
    ])?;

    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(
        stdout(&listed),
        "alpha:0:0:%0:0\nalpha:0:1:%1:1\nalpha:1:0:%2:1\n"
    );
    assert!(stderr(&listed).is_empty());
    Ok(())
}

#[test]
fn list_panes_session_target_lists_only_the_active_window() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("list-panes-session-target-active-window")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["split-window", "-h", "-t", "alpha:0"])?);
    assert_success(&harness.run(&["new-window", "-t", "alpha", "-d", "-n", "logs"])?);

    let listed = harness.run(&[
        "list-panes",
        "-t",
        "alpha",
        "-F",
        "#{window_index}:#{pane_index}",
    ])?;

    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(stdout(&listed), "0:0\n0:1\n");
    assert!(stderr(&listed).is_empty());
    Ok(())
}

#[test]
fn list_panes_session_scope_lists_all_windows_in_target_session() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("list-panes-session-scope")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["split-window", "-h", "-t", "alpha:0"])?);
    assert_success(&harness.run(&["new-window", "-t", "alpha", "-d", "-n", "logs"])?);

    let listed = harness.run(&[
        "list-panes",
        "-s",
        "-t",
        "alpha",
        "-F",
        "#{window_index}:#{pane_index}",
    ])?;

    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(stdout(&listed), "0:0\n0:1\n1:0\n");
    assert!(stderr(&listed).is_empty());
    Ok(())
}

#[test]
fn list_panes_default_prefix_matches_scope() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("list-panes-default-prefix")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["split-window", "-v", "-t", "alpha"])?);
    assert_success(&harness.run(&["new-window", "-t", "alpha", "-d", "-n", "logs"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "beta"])?);

    let window = harness.run(&["list-panes", "-t", "alpha:0"])?;
    assert_eq!(window.status.code(), Some(0));
    let window_lines = stdout(&window);
    assert!(window_lines.lines().any(|line| line.starts_with("0: ")));
    assert!(window_lines.lines().any(|line| line.starts_with("1: ")));
    assert!(!window_lines.contains("alpha:"));

    let session = harness.run(&["list-panes", "-s", "-t", "alpha"])?;
    assert_eq!(session.status.code(), Some(0));
    let session_lines = stdout(&session);
    assert!(session_lines.lines().any(|line| line.starts_with("0.0: ")));
    assert!(session_lines.lines().any(|line| line.starts_with("1.0: ")));
    assert!(!session_lines.contains("alpha:"));

    let all = harness.run(&["list-panes", "-a"])?;
    assert_eq!(all.status.code(), Some(0));
    let all_lines = stdout(&all);
    assert!(all_lines
        .lines()
        .any(|line| line.starts_with("alpha:0.0: ")));
    assert!(all_lines.lines().any(|line| line.starts_with("beta:0.0: ")));

    Ok(())
}

#[test]
fn list_panes_exposes_pane_geometry_through_the_shared_formatter() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("list-panes-geometry-cli")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);

    let listed = harness.run(&[
        "list-panes",
        "-t",
        "alpha",
        "-F",
        "#{pane_width}x#{pane_height}",
    ])?;

    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(stdout(&listed), "80x24\n");
    assert!(stderr(&listed).is_empty());
    Ok(())
}

#[test]
fn default_list_panes_uses_attached_visible_geometry() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("list-panes-default-attached-geometry")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-x", "80", "-y", "24"])?);

    let mut attach = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(80, 24))?;
    attach.wait_for_raw_mode(Duration::from_secs(10))?;
    wait_for_attached_client(&harness)?;

    let default = harness.run(&["list-panes", "-t", "alpha"])?;
    assert_eq!(default.status.code(), Some(0));
    assert!(stderr(&default).is_empty());

    let formatted = harness.run(&[
        "list-panes",
        "-t",
        "alpha",
        "-F",
        "#{pane_width}x#{pane_height}",
    ])?;
    assert_eq!(formatted.status.code(), Some(0));
    assert!(stderr(&formatted).is_empty());

    let formatted_geometry = stdout(&formatted).trim().to_owned();
    assert_eq!(formatted_geometry, "80x23");
    assert!(
        stdout(&default).contains(&format!("[{formatted_geometry}]")),
        "default list-panes output should use attached visible geometry {formatted_geometry:?}, got {:?}",
        stdout(&default)
    );

    assert_success(&harness.run(&["kill-session", "-t", "alpha"])?);
    let status = attach.wait_for_exit(Duration::from_secs(10))?;
    assert_eq!(status.code(), Some(0));
    Ok(())
}

#[test]
fn default_list_panes_uses_reflowed_attached_geometry_for_vertical_splits(
) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("list-panes-default-attached-vertical")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-x", "80", "-y", "24"])?);

    let mut attach = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(80, 24))?;
    attach.wait_for_raw_mode(Duration::from_secs(10))?;
    wait_for_attached_client(&harness)?;

    assert_success(&harness.run(&["split-window", "-v", "-t", "alpha"])?);

    let default = harness.run(&["list-panes", "-t", "alpha"])?;
    assert_eq!(default.status.code(), Some(0));
    assert!(stderr(&default).is_empty());

    let formatted = harness.run(&[
        "list-panes",
        "-t",
        "alpha",
        "-F",
        "#{pane_width}x#{pane_height}",
    ])?;
    assert_eq!(formatted.status.code(), Some(0));
    assert!(stderr(&formatted).is_empty());

    let default_geometries = stdout(&default)
        .lines()
        .map(default_list_panes_geometry)
        .collect::<Result<Vec<_>, _>>()?;
    let formatted_geometries = stdout(&formatted)
        .lines()
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    assert_eq!(
        default_geometries,
        formatted_geometries,
        "default list-panes output should match formatter geometries; default={:?}, formatted={:?}",
        stdout(&default),
        stdout(&formatted)
    );

    assert_success(&harness.run(&["kill-session", "-t", "alpha"])?);
    let status = attach.wait_for_exit(Duration::from_secs(10))?;
    assert_eq!(status.code(), Some(0));
    Ok(())
}

fn default_list_panes_geometry(line: &str) -> Result<String, Box<dyn Error>> {
    let geometry = line
        .split('[')
        .nth(1)
        .and_then(|rest| rest.split(']').next())
        .ok_or_else(|| format!("missing geometry bracket in default list-panes line: {line}"))?;
    Ok(geometry.to_owned())
}

fn wait_for_attached_client(harness: &CliHarness) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        let clients = harness.run(&["list-clients", "-F", "#{session_name}"])?;
        if clients.status.code() == Some(0) && stdout(&clients).lines().any(|line| line == "alpha")
        {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    Err("timed out waiting for attached alpha client".into())
}

#[test]
fn list_panes_window_target_lists_only_that_window() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("list-panes-window-target")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["split-window", "-h", "-t", "alpha:0"])?);
    assert_success(&harness.run(&["new-window", "-t", "alpha", "-d", "-n", "logs"])?);

    let listed = harness.run(&[
        "list-panes",
        "-t",
        "alpha:0",
        "-F",
        "#{window_index}:#{pane_index}",
    ])?;

    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(stdout(&listed), "0:0\n0:1\n");
    assert!(stderr(&listed).is_empty());
    Ok(())
}

#[test]
fn list_panes_pane_target_validates_out_of_range_pane_index() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("list-panes-missing-pane")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["split-window", "-h", "-t", "alpha:0"])?);
    assert_success(&harness.run(&["split-window", "-v", "-t", "alpha:0"])?);

    let missing = harness.run(&["list-panes", "-t", "alpha:0.99", "-F", "#{pane_index}"])?;

    assert_eq!(missing.status.code(), Some(1));
    assert!(stdout(&missing).is_empty());
    assert_eq!(stderr(&missing), "can't find pane: 99\n");
    Ok(())
}

#[test]
fn list_panes_filter_matches_tmux_format_truthiness() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("list-panes-filter")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["split-window", "-h", "-t", "alpha:0"])?);

    let target = harness.run(&["display-message", "-p", "-t", "alpha:0.1", "#{pane_id}"])?;
    assert_eq!(target.status.code(), Some(0));
    assert!(stderr(&target).is_empty());
    let target_pane = stdout(&target).trim().to_owned();

    let listed = harness.run(&[
        "list-panes",
        "-t",
        "alpha",
        "-f",
        &format!("#{{m:{target_pane},#{{pane_id}}}}"),
        "-F",
        "#{pane_id}",
    ])?;

    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(stdout(&listed), format!("{target_pane}\n"));
    assert!(stderr(&listed).is_empty());
    Ok(())
}
