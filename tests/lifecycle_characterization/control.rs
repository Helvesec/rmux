use super::characterization::transcript;
use super::*;

#[test]
fn l05_1_control_failed_rename_keeps_queue_context() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("control-failed-rename")?;
    let _daemon = harness.start_hidden_daemon()?;
    for session in ["alpha", "beta"] {
        assert_success(&harness.run(&["new-session", "-d", "-s", session, "sleep 120"])?);
    }
    let mut control = ControlProcess::spawn(&harness, &["attach-session", "-t", "alpha"])?;
    control.wait_for_output("%session-changed $0 alpha\n")?;
    let start = transcript(&control).len();
    // A newline starts a distinct abort group, so the observation must still
    // execute after the duplicate-destination failure.
    control.write_input(
        "rename-session beta\ndisplay-message -p 'CONTEXT:#{session_id}:#{session_name}'\n",
    )?;
    control.wait_for_output("CONTEXT:$0:alpha\n")?;
    let text = transcript(&control);
    let delta = &text[start..];
    assert_eq!(
        delta
            .lines()
            .filter(|line| line.starts_with("%error "))
            .count(),
        1,
        "{delta}"
    );
    assert!(
        !delta
            .lines()
            .any(|line| line.starts_with("%session-renamed ")),
        "{delta}"
    );
    control.write_input("set-option @context intact\nshow-options -v @context\n")?;
    control.wait_for_output("intact\n")?;
    let alpha = harness.run(&["show-options", "-v", "-t", "alpha", "@context"])?;
    assert_eq!(common::stdout(&alpha), "intact\n");
    let beta = harness.run(&["show-options", "-qv", "-t", "beta", "@context"])?;
    assert_eq!(common::stdout(&beta), "");
    Ok(())
}

#[test]
fn l05_1_control_select_notification_after_commit() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("control-select-commit")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "sleep 120"])?);
    assert_success(&harness.run(&["split-window", "-d", "-t", "alpha", "sleep 120"])?);
    let panes = harness.run(&["list-panes", "-t", "alpha", "-F", "#{pane_id}"])?;
    let ids = common::stdout(&panes)
        .lines()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    assert_eq!(ids.len(), 2);
    let mut control = ControlProcess::spawn(&harness, &["attach-session", "-t", "alpha"])?;
    control.wait_for_output("%session-changed $0 alpha\n")?;
    let start = transcript(&control).len();
    control.write_input(&format!(
        "select-pane -t {}\ndisplay-message -p 'SELECTED:#{{pane_id}}'\n",
        ids[1]
    ))?;
    control.wait_for_output(&format!("SELECTED:{}\n", ids[1]))?;
    control.wait_for_output(&format!("%window-pane-changed @0 {}\n", ids[1]))?;
    let text = transcript(&control);
    let delta = &text[start..];
    assert_eq!(
        delta
            .lines()
            .filter(|line| line.starts_with("%window-pane-changed "))
            .count(),
        1,
        "{delta}"
    );
    // tmux may flush the notification after the following display response.
    // Both observers must identify the committed pane; internal focus order
    // is asserted separately by the stable-ID lifecycle fixture.
    let start = text.len();
    control.write_input(&format!(
        "select-pane -t {}\nselect-pane -t %999999\ndisplay-message -p 'UNCHANGED:#{{pane_id}}'\n",
        ids[1]
    ))?;
    control.wait_for_output(&format!("UNCHANGED:{}\n", ids[1]))?;
    let text = transcript(&control);
    let delta = &text[start..];
    assert!(
        !delta
            .lines()
            .any(|line| line.starts_with("%window-pane-changed ")),
        "{delta}"
    );
    assert_eq!(
        delta
            .lines()
            .filter(|line| line.starts_with("%error "))
            .count(),
        1,
        "{delta}"
    );
    Ok(())
}
