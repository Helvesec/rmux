#![cfg(unix)]

mod common;

use std::error::Error;
use std::process::{Output, Stdio};
use std::time::{Duration, Instant};

use common::{assert_success, stderr, stdout, CliHarness};

fn bounded_wait(harness: &CliHarness, args: &[&str]) -> Result<Output, Box<dyn Error>> {
    let mut child = harness
        .base_command()
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null())
        .spawn()?;
    let start = Instant::now();
    while child.try_wait()?.is_none() {
        if start.elapsed() >= Duration::from_secs(1) {
            child.kill()?;
            let output = child.wait_with_output()?;
            panic!("canonical 50ms wait exceeded 1s: {args:?}: {output:?}");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    Ok(child.wait_with_output()?)
}

#[test]
fn runtime_alias_wait_uses_canonical_timeout_and_diagnostic() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("canonical-wait-budget")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    for (alias, args, json) in [
        (
            "wait-pane=wait-pane --timeout 50ms",
            vec![
                "wait-pane",
                "-t",
                "alpha:0.0",
                "--text",
                "NEVER_MATCH_CANONICAL_BUDGET",
                "--json",
            ],
            true,
        ),
        (
            "send-keys=send-keys --timeout 50ms",
            vec![
                "send-keys",
                "-t",
                "alpha:0.0",
                "--wait-pane-exit",
                "--",
                "Enter",
            ],
            false,
        ),
        (
            "bounded=wait-pane --timeout 50ms",
            vec![
                "bounded",
                "-t",
                "alpha:0.0",
                "--text",
                "NEVER_MATCH_CANONICAL_BUDGET",
                "--json",
            ],
            true,
        ),
    ] {
        assert_success(&harness.run(&["set-option", "-s", "command-alias[99]", alias])?);
        let output = bounded_wait(&harness, &args)?;
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        if json {
            let value: serde_json::Value = serde_json::from_str(&stdout(&output))?;
            assert_eq!(value["error"], "timeout");
            assert_eq!(value["timeout_ms"], 50);
        } else {
            assert!(
                stderr(&output).contains("timed out waiting for pane-exit"),
                "{output:?}"
            );
        }
    }
    Ok(())
}

#[test]
fn runtime_alias_queue_keeps_each_wait_budget() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("canonical-queue-budget")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&[
        "set-option",
        "-s",
        "command-alias[99]",
        "wait-pane=display-message -p QUEUE_PREFIX ; wait-pane --timeout 50ms",
    ])?);
    let output = bounded_wait(
        &harness,
        &[
            "wait-pane",
            "-t",
            "alpha:0.0",
            "--text",
            "NEVER_MATCH_QUEUE_BUDGET",
            "--json",
        ],
    )?;
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let rendered = stdout(&output);
    let json = rendered
        .strip_prefix("QUEUE_PREFIX\n")
        .expect("first queued command executes");
    let value: serde_json::Value = serde_json::from_str(json)?;
    assert_eq!(value["error"], "timeout");
    assert_eq!(value["timeout_ms"], 50);
    Ok(())
}
