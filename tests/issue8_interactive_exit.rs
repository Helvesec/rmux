#![cfg(unix)]

mod common;

use std::error::Error;
use std::time::{Duration, Instant};
use std::{fs, thread};

use common::{
    assert_success, drain_attach_output_bytes, read_until_contains, terminate_child,
    AttachedSession, CliHarness,
};
use rmux_pty::TerminalSize;

const IO_TIMEOUT: Duration = Duration::from_secs(10);

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

    let release_path = harness.tmpdir().join("issue8-release-window");
    let command = format!(
        "new-window -- 'printf ISSUE8_WINDOW_READY; while [ ! -e {} ]; do sleep 0.05; done'",
        release_path.display()
    );
    send_prompt_command(&mut attach, &command)?;
    let _ = read_until_contains(attach.master_mut(), "ISSUE8_WINDOW_READY", IO_TIMEOUT)
        .map_err(|error| {
            let windows = harness
                .run(&[
                    "list-windows",
                    "-a",
                    "-F",
                    "#{session_name}:#{window_index}:#{window_active}:#{window_name}",
                ])
                .ok()
                .map(|output| common::stdout(&output))
                .unwrap_or_else(|| "<list-windows failed>".to_owned());
            let panes = harness
                .run(&[
                    "list-panes",
                    "-a",
                    "-F",
                    "#{session_name}:#{window_index}.#{pane_index}:#{pane_current_command}:#{pane_dead}",
                ])
                .ok()
                .map(|output| common::stdout(&output))
                .unwrap_or_else(|| "<list-panes failed>".to_owned());
            let capture = harness
                .run(&["capture-pane", "-p", "-t", "alpha:1.0"])
                .ok()
                .map(|output| common::stdout(&output))
                .unwrap_or_else(|| "<capture-pane failed>".to_owned());
            format!("{error}; windows={windows:?}; panes={panes:?}; capture={capture:?}")
        })?;
    wait_for_non_empty_window_name(&harness, "1")?;
    fs::write(&release_path, "")?;
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
fn fresh_pane_title_starts_as_host_short_before_shell_updates_it() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("issue8-initial-pane-title")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);

    let output = harness.run(&[
        "display-message",
        "-p",
        "-t",
        "alpha:0.0",
        "#{pane_title}|#{host_short}",
    ])?;
    let title = common::stdout(&output);
    let (pane_title, host_short) = title.trim().split_once('|').expect("format separator");
    assert_eq!(pane_title, host_short);

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn osc_title_rename_is_ignored_when_allow_set_title_is_off() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("issue8-allow-set-title-off")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&[
        "set-option",
        "-p",
        "-t",
        "alpha:0.0",
        "allow-set-title",
        "off",
    ])?);
    assert_success(&harness.run(&[
        "send-keys",
        "-t",
        "alpha:0.0",
        "printf '\\033]0;CLAUDETITLE\\007'",
        "Enter",
    ])?);
    thread::sleep(Duration::from_millis(500));

    let title = pane_title_and_host_short(&harness, "alpha:0.0")?;
    let (pane_title, host_short) = title.trim().split_once('|').expect("format separator");
    assert_eq!(pane_title, host_short);
    assert_ne!(pane_title, "CLAUDETITLE");

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn osc_title_rename_is_applied_when_allow_set_title_is_on() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("issue8-allow-set-title-on")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&[
        "set-option",
        "-p",
        "-t",
        "alpha:0.0",
        "allow-set-title",
        "on",
    ])?);
    assert_success(&harness.run(&[
        "send-keys",
        "-t",
        "alpha:0.0",
        "printf '\\033]0;CLAUDETITLE\\007'",
        "Enter",
    ])?);
    thread::sleep(Duration::from_millis(500));

    let title = pane_title_and_host_short(&harness, "alpha:0.0")?;
    let (pane_title, _host_short) = title.trim().split_once('|').expect("format separator");
    assert_eq!(pane_title, "CLAUDETITLE");

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
    drain_attach_output_until_quiet(&mut attach)?;

    attach.send_bytes(b"\x02xy")?;
    wait_for_panes(&harness, &["0:0"], &["0:1"])?;

    assert_success(&harness.run(&["kill-session", "-t", "alpha"])?);
    let status = attach.wait_for_exit(IO_TIMEOUT)?;
    assert_eq!(status.code(), Some(0));
    attach.assert_restored()?;

    terminate_child(daemon.child_mut())?;
    Ok(())
}

fn pane_title_and_host_short(harness: &CliHarness, target: &str) -> Result<String, Box<dyn Error>> {
    let output = harness.run(&[
        "display-message",
        "-p",
        "-t",
        target,
        "#{pane_title}|#{host_short}",
    ])?;
    Ok(common::stdout(&output))
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
    drain_attach_output_until_quiet(attach)
}

fn drain_attach_output_until_quiet(attach: &mut AttachedSession) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + IO_TIMEOUT;
    let quiet_period = Duration::from_millis(100);
    let mut quiet_since = Instant::now();

    loop {
        let output = drain_attach_output_bytes(attach.master_mut())?;
        if output.is_empty() {
            if Instant::now().duration_since(quiet_since) >= quiet_period {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err("attach repaint did not quiesce".into());
            }
            thread::sleep(Duration::from_millis(10));
            continue;
        }

        quiet_since = Instant::now();
    }
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
