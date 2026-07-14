#![cfg(unix)]

mod common;

use std::error::Error;

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

    create_shell_session(&harness, "unknown", false)?;
    let unknown = harness.run(&[
        "send-keys",
        "-t",
        "unknown:0.0",
        "--wait-pane-exit",
        "--timeout",
        "5s",
        "--",
        "exit 7",
        "Enter",
    ])?;
    assert_eq!(unknown.status.code(), Some(1));
    assert!(
        stderr(&unknown).contains("could not determine the pane process exit status"),
        "a stale pane without retained metadata must fail closed: {}",
        stderr(&unknown)
    );

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
