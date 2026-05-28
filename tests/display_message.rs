#![cfg(unix)]

mod common;

use std::error::Error;

use common::{assert_success, stderr, stdout, terminate_child, CliHarness};

#[test]
fn display_message_prints_expanded_format_without_attached_client() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("display-message-print")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);

    let output = harness.run(&[
        "display-message",
        "-p",
        "-t",
        "alpha:0.0",
        "#{session_name}:#{session_windows}:#{pane_index}",
    ])?;

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout(&output), "alpha:1:0\n");
    assert!(stderr(&output).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn display_message_all_formats_prints_without_print_flag() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("display-message-all-formats")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);

    let output = harness.run(&["display-message", "-a", "-t", "alpha:0.0"])?;

    assert_eq!(output.status.code(), Some(0));
    let stdout = stdout(&output);
    assert!(stdout.contains("session_name=alpha"));
    assert!(stdout.contains("pane_index=0"));
    assert!(stdout.contains("version=3.4"));
    assert!(stderr(&output).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn bare_display_message_with_no_attached_display_is_a_noop() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("display-message-no-display")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);

    let output = harness.run(&["display-message", "-t", "alpha", "hello #{session_name}"])?;

    assert_eq!(output.status.code(), Some(0));
    assert!(stdout(&output).is_empty());
    assert!(stderr(&output).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn display_message_prints_literal_without_target_or_attached_client() -> Result<(), Box<dyn Error>>
{
    let harness = CliHarness::new("display-message-literal-no-target")?;
    let _daemon = harness.start_hidden_daemon()?;

    let output = harness.run(&["display-message", "-p", "hello"])?;

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout(&output), "hello\n");
    assert!(stderr(&output).is_empty());
    Ok(())
}

#[test]
fn default_display_message_expands_runtime_context_and_time_tokens() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("display-message-default-runtime")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);

    let output = harness.run(&["display-message", "-p", "-t", "alpha:0.0"])?;

    assert_eq!(output.status.code(), Some(0));
    let stdout = stdout(&output);
    assert!(stdout.starts_with("[alpha] 0:"));
    assert!(stdout.contains(", current pane 0 - ("));
    assert!(!stdout.contains("%H:%M"));
    assert!(!stdout.contains("%d-%b-%y"));
    assert!(stderr(&output).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}
