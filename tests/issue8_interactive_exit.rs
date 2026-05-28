#![cfg(unix)]

mod common;

use std::error::Error;
use std::thread;
use std::time::{Duration, Instant};

use common::{
    assert_success, drain_attach_output, read_until_contains, terminate_child, AttachedSession,
    CliHarness,
};
use rmux_pty::TerminalSize;

const IO_TIMEOUT: Duration = Duration::from_secs(5);

#[test]
fn issue_8_prompt_created_window_exit_removes_dead_pane() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("issue8-interactive-exit")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-x", "100", "-y", "30"])?);
    assert_success(&harness.run(&["split-window", "-h", "-t", "alpha:0.0"])?);

    let mut attach = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(100, 30))?;
    attach.wait_for_raw_mode(IO_TIMEOUT)?;
    let _ = read_until_contains(attach.master_mut(), "[alpha]", IO_TIMEOUT)?;
    wait_for_panes(&harness, &["0:0", "0:1"], &[])?;
    wait_for_attach_repaint(&mut attach)?;

    send_prompt_command(
        &mut attach,
        "new-window -- 'printf ISSUE8_WINDOW_READY; sleep 0.2'",
    )?;
    wait_for_non_empty_window_name(&harness, "1")?;
    let _ = read_until_contains(attach.master_mut(), "ISSUE8_WINDOW_READY", IO_TIMEOUT)?;
    wait_for_panes(&harness, &["0:0", "0:1"], &["1:0"])?;

    attach.send_bytes(b"printf STILL_ALIVE\r")?;
    let output = read_until_contains(attach.master_mut(), "STILL_ALIVE", IO_TIMEOUT)?;
    assert!(
        output.contains("STILL_ALIVE"),
        "attach should continue on the remaining window after exiting the prompt-created one"
    );

    assert_success(&harness.run(&["kill-session", "-t", "alpha"])?);
    let status = attach.wait_for_exit(IO_TIMEOUT)?;
    assert_eq!(status.code(), Some(0));
    attach.assert_restored()?;

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn fresh_pane_title_has_user_host_and_path_before_shell_updates_it() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("issue8-initial-pane-title")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);

    let output = harness.run(&["display-message", "-p", "-t", "alpha:0.0", "#{pane_title}"])?;
    let title = common::stdout(&output);
    let title = title.trim();
    assert!(
        title.contains('@') && title.contains(':') && !title.ends_with(':'),
        "initial pane title should include user, host, and path, got {title:?}"
    );

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn rapid_command_prompt_after_split_keeps_first_typed_byte() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("issue8-rapid-command-prompt")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-x", "100", "-y", "30"])?);

    let mut attach = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(100, 30))?;
    attach.wait_for_raw_mode(IO_TIMEOUT)?;
    let _ = read_until_contains(attach.master_mut(), "[alpha]", IO_TIMEOUT)?;

    attach.send_bytes(b"\x02%\x02:split-window -h")?;
    let output = read_until_contains(attach.master_mut(), ":split-window -h", IO_TIMEOUT)?;
    assert!(
        !output.contains(":plit-window -h"),
        "command prompt lost its first typed byte: {output:?}"
    );

    attach.send_bytes(b"\r")?;
    wait_for_panes(&harness, &["0:0", "0:1", "0:2"], &[])?;

    assert_success(&harness.run(&["kill-session", "-t", "alpha"])?);
    let status = attach.wait_for_exit(IO_TIMEOUT)?;
    assert_eq!(status.code(), Some(0));
    attach.assert_restored()?;

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn rapid_confirm_before_accept_after_split_reaches_prompt() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("issue8-rapid-confirm-before")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-x", "100", "-y", "30"])?);

    let mut attach = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(100, 30))?;
    attach.wait_for_raw_mode(IO_TIMEOUT)?;
    let _ = read_until_contains(attach.master_mut(), "[alpha]", IO_TIMEOUT)?;

    attach.send_bytes(b"\x02%")?;
    wait_for_panes(&harness, &["0:0", "0:1"], &[])?;
    wait_for_attach_repaint(&mut attach)?;

    attach.send_bytes(b"\x02xy")?;
    wait_for_panes(&harness, &["0:0"], &["0:1"])?;

    assert_success(&harness.run(&["kill-session", "-t", "alpha"])?);
    let status = attach.wait_for_exit(IO_TIMEOUT)?;
    assert_eq!(status.code(), Some(0));
    attach.assert_restored()?;

    terminate_child(daemon.child_mut())?;
    Ok(())
}

fn send_prompt_command(attach: &mut AttachedSession, command: &str) -> Result<(), Box<dyn Error>> {
    let mut bytes = Vec::with_capacity(2 + command.len() + 1);
    bytes.extend_from_slice(b"\x02:");
    bytes.extend_from_slice(command.as_bytes());
    bytes.push(b'\r');
    attach.send_bytes(&bytes)?;
    Ok(())
}

fn wait_for_attach_repaint(attach: &mut AttachedSession) -> Result<(), Box<dyn Error>> {
    thread::sleep(Duration::from_millis(200));
    drain_attach_output(attach.master_mut())
}

fn wait_for_panes(
    harness: &CliHarness,
    present: &[&str],
    absent: &[&str],
) -> Result<String, Box<dyn Error>> {
    wait_for_cli_lines(
        "list-panes",
        || {
            let output = harness.run(&[
                "list-panes",
                "-a",
                "-t",
                "alpha",
                "-F",
                "#{window_index}:#{pane_index}",
            ])?;
            Ok(common::stdout(&output))
        },
        |output| {
            present.iter().all(|pane| has_line(output, pane))
                && absent.iter().all(|pane| !has_line(output, pane))
        },
    )
}

fn wait_for_non_empty_window_name(
    harness: &CliHarness,
    window_index: &str,
) -> Result<String, Box<dyn Error>> {
    wait_for_cli_lines(
        "list-windows",
        || {
            let output = harness.run(&[
                "list-windows",
                "-t",
                "alpha",
                "-F",
                "#{window_index}:#{window_name}",
            ])?;
            Ok(common::stdout(&output))
        },
        |output| {
            output.lines().any(|line| {
                line.strip_prefix(&format!("{window_index}:"))
                    .is_some_and(|name| !name.trim().is_empty())
            })
        },
    )
}

fn wait_for_cli_lines<F, C>(
    label: &str,
    mut read_output: F,
    converged: C,
) -> Result<String, Box<dyn Error>>
where
    F: FnMut() -> Result<String, Box<dyn Error>>,
    C: Fn(&str) -> bool,
{
    let deadline = Instant::now() + IO_TIMEOUT;
    loop {
        let output = read_output()?;
        if converged(&output) {
            return Ok(output);
        }
        if Instant::now() >= deadline {
            return Err(format!("{label} did not converge; last output: {output:?}").into());
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn has_line(output: &str, needle: &str) -> bool {
    output.lines().any(|line| line.trim() == needle)
}
