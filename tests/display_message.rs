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
fn display_message_keeps_tmux_format_edge_semantics() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("display-message-format-edges")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);

    for (template, expected) in [
        ("#{?0,yes}tail", ""),
        ("pre#{?foo,yes}tail", "pre"),
        ("#{?0,a,b,c}", "b,c"),
        ("#{?#{?b},yes,no}Z", "noZ"),
        ("#{x}", ""),
        ("#{#{#{x}}}", ""),
        ("#{&&:1,1,0}", "1"),
        ("#{&&:1,0,1}", "1"),
        ("#{||:0,0,0}", "1"),
        ("#{e|+|:999999999999999999999,1}", "9223372036854775808"),
        ("#{e|/|:-9223372036854775808,-1}", "9223372036854775808"),
        ("#{e|%|:-9223372036854775808,-1}", ""),
        ("#{s//Z/:session_name}", "alpha"),
        ("#{s//Z/:#{l:hi}}", "hi"),
        ("#{s/[0-9]*/Z/:session_name}", "aZlZpZhZaZ"),
        ("#", "#"),
        ("a#", "a#"),
        ("#{pane_id}#", "%0#"),
    ] {
        let output = harness.run(&["display-message", "-p", "-t", "alpha:0.0", template])?;
        assert_eq!(output.status.code(), Some(0), "template={template}");
        assert_eq!(
            stdout(&output),
            format!("{expected}\n"),
            "template={template}"
        );
        assert!(stderr(&output).is_empty(), "template={template}");
    }

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn display_message_target_keeps_session_context_when_window_lookup_can_fail(
) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("display-message-canfail-window-context")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "s", "-n", "zero"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "s", "-n", "abc"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "s", "-n", "abd"])?);

    let prefix = harness.run(&[
        "display-message",
        "-p",
        "-t",
        "s:ab",
        "#{session_name}:#{window_name}:#{pane_id}",
    ])?;
    assert_eq!(prefix.status.code(), Some(0));
    assert_eq!(stdout(&prefix), "s:abc:%1\n");
    assert!(stderr(&prefix).is_empty());

    let missing = harness.run(&[
        "display-message",
        "-p",
        "-t",
        "s:nope",
        "#{session_name}:#{window_name}:#{pane_id}",
    ])?;
    assert_eq!(missing.status.code(), Some(0));
    assert_eq!(stdout(&missing), "s:zero:%0\n");
    assert!(stderr(&missing).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn display_message_reports_pane_tabs_inside_visible_pane_width() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("display-message-pane-tabs")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-x", "80", "-y", "24"])?);

    let output = harness.run(&["display-message", "-p", "-t", "alpha:0.0", "#{pane_tabs}"])?;

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout(&output), "8,16,24,32,40,48,56,64,72\n");
    assert!(stderr(&output).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn display_message_keeps_pane_zoomed_flag_empty_for_tmux34() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("display-message-pane-zoomed-flag")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-x", "80", "-y", "24"])?);
    assert_success(&harness.run(&["split-window", "-h", "-t", "alpha:0.0"])?);
    assert_success(&harness.run(&["resize-pane", "-Z", "-t", "alpha:0.0"])?);

    let output = harness.run(&[
        "list-panes",
        "-t",
        "alpha",
        "-F",
        "#{pane_index}:#{pane_zoomed_flag}:#{window_zoomed_flag}",
    ])?;

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout(&output), "0::1\n1::1\n");
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

#[test]
fn display_message_stdin_flag_errors_on_nonempty_pane() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("display-message-stdin-nonempty-pane")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);

    let output = harness.run(&["display-message", "-I", "-t", "alpha:0.0", "hello"])?;

    assert_eq!(output.status.code(), Some(1));
    assert!(stdout(&output).is_empty());
    assert_eq!(stderr(&output), "pane is not empty\n");

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn display_message_stdin_flag_missing_target_is_noop() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("display-message-stdin-missing-target")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);

    let output = harness.run(&["display-message", "-I", "-t", "missing", "hello"])?;

    assert_eq!(output.status.code(), Some(0));
    assert!(stdout(&output).is_empty());
    assert!(stderr(&output).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn display_message_verbose_prints_expansion_trace() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("display-message-verbose")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);

    let literal = harness.run(&["display-message", "-v", "hello"])?;
    assert_eq!(literal.status.code(), Some(0));
    assert_eq!(
        stdout(&literal),
        "# expanding format: hello\n# result is: hello\n"
    );
    assert!(stderr(&literal).is_empty());

    let formatted = harness.run(&["display-message", "-vp", "-t", "alpha", "#{session_name}"])?;
    assert_eq!(formatted.status.code(), Some(0));
    assert_eq!(
        stdout(&formatted),
        "# expanding format: #{session_name}\n\
# found #{}: session_name\n\
# format 'session_name' found: alpha\n\
# replaced 'session_name' with 'alpha'\n\
# result is: alpha\n\
alpha\n"
    );
    assert!(stderr(&formatted).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}
