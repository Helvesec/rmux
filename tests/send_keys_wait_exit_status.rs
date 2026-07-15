#![cfg(unix)]

mod common;

use std::error::Error;
use std::process::Stdio;
use std::time::Duration;

use common::{assert_success, stderr, stdout, CliHarness};
use serde_json::Value;

#[test]
fn send_keys_wait_pane_exit_preserves_process_outcomes() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("send-keys-wait-exit-status")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "keeper", "sleep", "30"])?);

    for exit_status in [0, 7] {
        let session = format!("normal-{exit_status}");
        create_shell_session(&harness, &session, true)?;
        let command = format!("exit {exit_status}");
        let output = harness.run(&[
            "send-keys",
            "-t",
            &format!("{session}:0.0"),
            "--wait-pane-exit",
            "--timeout",
            "5s",
            "--",
            &command,
            "Enter",
        ])?;
        assert_eq!(
            output.status.code(),
            Some(exit_status),
            "normal pane exit status must cross the send-keys CLI boundary\nstdout:\n{}\nstderr:\n{}",
            stdout(&output),
            stderr(&output)
        );
        assert!(stderr(&output).is_empty());

        let observed = harness.run(&[
            "wait-pane",
            "-t",
            &format!("{session}:0.0"),
            "--pane-exit",
            "--timeout",
            "2s",
            "--json",
        ])?;
        assert_eq!(
            observed.status.code(),
            Some(0),
            "wait-pane remains a condition observer"
        );
        let value: Value = serde_json::from_str(&stdout(&observed))?;
        assert_eq!(value["ok"], true);
        assert_eq!(value["pane_exit"]["exit_status"], exit_status);
    }

    create_shell_session(&harness, "quiet-seven", true)?;
    let quiet = harness.run(&[
        "send-keys",
        "-t",
        "quiet-seven:0.0",
        "--wait",
        "quiet",
        "--stable-for",
        "100ms",
        "--timeout",
        "5s",
        "--",
        "exit 7",
        "Enter",
    ])?;
    assert_eq!(
        quiet.status.code(),
        Some(0),
        "quiet remains a completion observation and does not infer process success\nstderr:\n{}",
        stderr(&quiet)
    );

    create_shell_session(&harness, "signaled", true)?;
    let signaled = harness.run(&[
        "send-keys",
        "-t",
        "signaled:0.0",
        "--wait-pane-exit",
        "--timeout",
        "5s",
        "--",
        "kill -KILL $$",
        "Enter",
    ])?;
    assert_eq!(
        signaled.status.code(),
        Some(137),
        "signal 9 must use the conventional 128 + signal CLI status\nstdout:\n{}\nstderr:\n{}",
        stdout(&signaled),
        stderr(&signaled)
    );
    assert!(stderr(&signaled).is_empty());

    for exit_status in [0, 7] {
        let session = format!("removed-{exit_status}");
        create_shell_session(&harness, &session, false)?;
        let command = format!("exit {exit_status}");
        let removed = harness.run(&[
            "send-keys",
            "-t",
            &format!("{session}:0.0"),
            "--wait-pane-exit",
            "--timeout",
            "5s",
            "--",
            &command,
            "Enter",
        ])?;
        assert_eq!(
            removed.status.code(),
            Some(exit_status),
            "a normally removed pane must preserve its process exit status\nstdout:\n{}\nstderr:\n{}",
            stdout(&removed),
            stderr(&removed)
        );
        assert!(stderr(&removed).is_empty());
    }

    create_shell_session(&harness, "timeout", true)?;
    let timed_out = harness.run(&[
        "send-keys",
        "-t",
        "timeout:0.0",
        "--wait-pane-exit",
        "--timeout",
        "250ms",
        "--",
        ":",
        "Enter",
    ])?;
    assert_eq!(timed_out.status.code(), Some(1));
    assert!(
        stderr(&timed_out).contains("timed out waiting for pane-exit"),
        "timeout must remain an explicit wait error: {}",
        stderr(&timed_out)
    );

    Ok(())
}

#[test]
fn send_keys_pane_exit_wait_follows_session_identity_across_rename() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("send-keys-wait-exit-rename")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "keeper", "sleep", "30"])?);
    create_shell_session(&harness, "alpha", false)?;
    let mut command = harness.base_command();
    let mut wait_child = command
        .args([
            "send-keys",
            "-t",
            "alpha:0.0",
            "--wait-pane-exit",
            "--timeout",
            "5s",
            "--",
            "printf WAIT_EXIT_%s STARTED; sleep 1; exit 7",
            "Enter",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    assert_success(&harness.run(&[
        "wait-pane",
        "-t",
        "alpha:0.0",
        "--text",
        "WAIT_EXIT_STARTED",
        "--timeout",
        "3s",
    ])?);
    assert_success(&harness.run(&["rename-session", "-t", "alpha", "beta"])?);
    std::thread::sleep(Duration::from_millis(150));
    assert!(
        wait_child.try_wait()?.is_none(),
        "pane-exit wait must remain armed after the target session is renamed"
    );

    let output = wait_child.wait_with_output()?;
    assert_eq!(
        output.status.code(),
        Some(7),
        "removed-pane exit status must survive a session rename\nstdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    Ok(())
}

#[test]
fn send_keys_pane_exit_wait_rejects_old_session_name_aba() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("send-keys-wait-exit-rename-aba")?;
    let _daemon = harness.start_hidden_daemon()?;

    create_shell_session(&harness, "alpha", true)?;
    let mut command = harness.base_command();
    let mut wait_child = command
        .args([
            "send-keys",
            "-t",
            "alpha:0.0",
            "--wait-pane-exit",
            "--timeout",
            "5s",
            "--",
            "printf ABA_EXIT_%s STARTED; read marker; exit 7",
            "Enter",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    assert_success(&harness.run(&[
        "wait-pane",
        "-t",
        "alpha:0.0",
        "--text",
        "ABA_EXIT_STARTED",
        "--timeout",
        "3s",
    ])?);
    assert_success(&harness.run(&[
        "rename-session",
        "-t",
        "alpha",
        "beta",
        ";",
        "new-session",
        "-d",
        "-s",
        "alpha",
    ])?);
    std::thread::sleep(Duration::from_millis(150));
    assert!(
        wait_child.try_wait()?.is_none(),
        "pane-exit wait must not bind to a replacement session using the old name"
    );

    assert_success(&harness.run(&["send-keys", "-t", "beta:0.0", "release", "Enter"])?);
    let output = wait_child.wait_with_output()?;
    assert_eq!(
        output.status.code(),
        Some(7),
        "pane-exit wait lost the original process outcome after name reuse\nstdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    Ok(())
}

fn create_shell_session(
    harness: &CliHarness,
    session: &str,
    remain_on_exit: bool,
) -> Result<(), Box<dyn Error>> {
    assert_success(&harness.run(&["new-session", "-d", "-s", session, "sh"])?);
    if remain_on_exit {
        assert_success(&harness.run(&[
            "set-window-option",
            "-t",
            &format!("{session}:0"),
            "remain-on-exit",
            "on",
        ])?);
    }
    Ok(())
}
