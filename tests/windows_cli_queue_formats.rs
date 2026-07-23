#![cfg(windows)]

mod common_cross;

use std::error::Error;

use common_cross::CrossPlatformHarness;

#[test]
fn display_message_queued_path_preserves_dollar_regex_anchor() -> Result<(), Box<dyn Error>> {
    let harness = CrossPlatformHarness::new("windows-queued-dollar-anchor")?;

    harness.success(["new-session", "-d", "-s", "alpha"])?;

    let format = "#{s/$/Z/:session_name}";
    let direct = harness.stdout(["display-message", "-p", format])?;
    let queued = harness.stdout(["display-message", "-p", "-t", "alpha:0.0", format])?;

    assert_eq!(direct.trim(), "alphaZ");
    assert_eq!(queued, direct);
    Ok(())
}

#[test]
fn display_message_queued_path_preserves_windows_quoting_and_dollar_anchor(
) -> Result<(), Box<dyn Error>> {
    let harness = CrossPlatformHarness::new("windows-queued-path-quoting")?;

    harness.success(["new-session", "-d", "-s", "alpha"])?;

    let format = r##"C:\Users\RMUX User\quoted "dir"\#{s/$/Z/:session_name}"##;
    let direct = harness.stdout(["display-message", "-p", format])?;
    let queued = harness.stdout(["display-message", "-p", "-t", "alpha:0.0", format])?;

    assert_eq!(direct, "C:\\Users\\RMUX User\\quoted \"dir\"\\alphaZ\n");
    assert_eq!(queued, direct);
    Ok(())
}

#[test]
fn set_option_unset_scope_matrix_matches_tmux() -> Result<(), Box<dyn Error>> {
    // Windows twin of cli_surface::set_option_unset_scope_matrix_matches_tmux.
    // Oracle probe 2026-07-09 (pinned tmux 3.7b): with @agent.state set at
    // session, window, and pane scopes, plain `set -U` unsets the session
    // copy only; `set -pU` unsets the pane copy only; `set -wU` unsets the
    // window copy and clears the window's pane overrides.
    let harness = CrossPlatformHarness::new("windows-set-option-unset-scope-matrix")?;

    harness.success(["new-session", "-d", "-s", "alpha"])?;
    let set_all = |harness: &CrossPlatformHarness| -> Result<(), Box<dyn Error>> {
        harness.success(["set-option", "-t", "alpha", "@agent.state", "session"])?;
        harness.success([
            "set-option",
            "-w",
            "-t",
            "alpha:0",
            "@agent.state",
            "window",
        ])?;
        harness.success([
            "set-option",
            "-p",
            "-t",
            "alpha:0.0",
            "@agent.state",
            "pane",
        ])?;
        Ok(())
    };
    let values =
        |harness: &CrossPlatformHarness| -> Result<(String, String, String), Box<dyn Error>> {
            Ok((
                harness
                    .stdout(["show-options", "-qv", "-t", "alpha", "@agent.state"])?
                    .trim_end()
                    .to_owned(),
                harness
                    .stdout(["show-options", "-wqv", "-t", "alpha:0", "@agent.state"])?
                    .trim_end()
                    .to_owned(),
                harness
                    .stdout(["show-options", "-pqv", "-t", "alpha:0.0", "@agent.state"])?
                    .trim_end()
                    .to_owned(),
            ))
        };

    set_all(&harness)?;
    harness.success(["set-option", "-U", "-t", "alpha:0.0", "@agent.state"])?;
    assert_eq!(
        values(&harness)?,
        (String::new(), "window".to_owned(), "pane".to_owned()),
        "plain -U unsets the session copy only"
    );

    set_all(&harness)?;
    harness.success(["set-option", "-pU", "-t", "alpha:0.0", "@agent.state"])?;
    assert_eq!(
        values(&harness)?,
        ("session".to_owned(), "window".to_owned(), String::new()),
        "-pU unsets the pane copy only"
    );

    set_all(&harness)?;
    harness.success(["set-option", "-wU", "-t", "alpha:0", "@agent.state"])?;
    assert_eq!(
        values(&harness)?,
        ("session".to_owned(), String::new(), String::new()),
        "-wU unsets the window copy and clears pane overrides"
    );

    Ok(())
}

#[test]
fn deferred_pane_pid_is_ready_before_queued_format_evaluation() -> Result<(), Box<dyn Error>> {
    let harness = CrossPlatformHarness::new("windows-deferred-pane-pid-format")?;

    let rendered = harness.stdout([
        "new-session",
        "-d",
        "-s",
        "race",
        ";",
        "if-shell",
        "-F",
        "-t",
        "race",
        "#{pane_pid}",
        "display-message -p TRUE",
        "display-message -p FALSE",
    ])?;

    assert_eq!(rendered.trim(), "TRUE");
    let pane_pid = harness.stdout(["list-panes", "-t", "race", "-F", "#{pane_pid}"])?;
    assert!(
        pane_pid.trim().parse::<u32>().is_ok(),
        "deferred pane did not publish a numeric PID: {pane_pid:?}"
    );
    Ok(())
}

#[test]
fn deferred_pane_pid_is_ready_before_list_filters() -> Result<(), Box<dyn Error>> {
    let harness = CrossPlatformHarness::new("windows-deferred-pane-pid-list-filter")?;

    let panes = harness.stdout([
        "new-session",
        "-d",
        "-s",
        "filter-race",
        ";",
        "list-panes",
        "-t",
        "filter-race",
        "-f",
        "#{pane_pid}",
        "-F",
        "#{pane_pid}",
    ])?;
    assert!(
        panes.trim().parse::<u32>().is_ok(),
        "list-panes filtered out the deferred PID: {panes:?}"
    );

    let windows = harness.stdout([
        "new-session",
        "-d",
        "-s",
        "window-filter-race",
        ";",
        "list-windows",
        "-t",
        "window-filter-race",
        "-f",
        "#{pane_pid}",
        "-F",
        "#{pane_pid}",
    ])?;
    assert!(
        windows.trim().parse::<u32>().is_ok(),
        "list-windows filtered out the deferred PID: {windows:?}"
    );
    Ok(())
}

#[test]
fn deferred_pane_pid_is_ready_before_all_session_window_filter() -> Result<(), Box<dyn Error>> {
    let harness = CrossPlatformHarness::new("windows-deferred-pane-pid-list-windows-all")?;

    let windows = harness.stdout([
        "new-session",
        "-d",
        "-s",
        "window-all-filter-race",
        ";",
        "list-windows",
        "-a",
        "-f",
        "#{pane_pid}",
        "-F",
        "#{pane_pid}",
    ])?;
    let pane_pid = windows
        .trim()
        .parse::<u32>()
        .expect("list-windows -a must render a numeric pane PID");
    assert_ne!(
        pane_pid, 0,
        "list-windows -a observed the deferred pane before PID publication"
    );
    Ok(())
}

#[test]
fn direct_all_session_window_filter_waits_for_deferred_pane_pid() -> Result<(), Box<dyn Error>> {
    let harness = CrossPlatformHarness::new("windows-direct-list-windows-all-pane-pid")?;
    harness.success(["new-session", "-d", "-s", "direct-window-all-race"])?;

    let windows = harness.stdout([
        "list-windows",
        "-a",
        "-f",
        "#{pane_pid}",
        "-F",
        "#{pane_pid}",
    ])?;
    let pane_pid = windows
        .trim()
        .parse::<u32>()
        .expect("direct list-windows -a must render a numeric pane PID");
    assert_ne!(pane_pid, 0);
    Ok(())
}

#[test]
fn rejected_respawn_preserves_deferred_pane_queued_input() -> Result<(), Box<dyn Error>> {
    let harness = CrossPlatformHarness::new("windows-respawn-deferred-input-rollback")?;
    let marker = "RMUX_RESPAWN_REJECTED_QUEUED_INPUT_SURVIVES";
    let echo_marker = format!("echo {marker}");

    let output = harness.run([
        "new-session",
        "-d",
        "-s",
        "respawn-rollback",
        ";",
        "send-keys",
        "-t",
        "respawn-rollback:0.0",
        echo_marker.as_str(),
        "Enter",
        ";",
        "respawn-pane",
        "-k",
        "-t",
        "respawn-rollback:0.0",
        "-e",
        "INVALID",
    ])?;
    assert!(
        !output.status.success(),
        "invalid respawn environment must be rejected"
    );

    harness.wait_for_capture_contains("respawn-rollback:0.0", marker)?;
    Ok(())
}
