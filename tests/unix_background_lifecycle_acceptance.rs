#![cfg(unix)]

mod common;

use std::error::Error;
use std::fs;
use std::process::Output;
use std::thread;
use std::time::{Duration, Instant};

use common::{assert_success, stderr, stdout, CliHarness};

const BACKGROUND_TIMEOUT: Duration = Duration::from_secs(5);

#[test]
fn background_run_shell_command_survives_originating_client_exit() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("bg-run-shell-cli-lifecycle")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&[
        "run-shell",
        "-b",
        "-d",
        "0.15",
        "-C",
        "set-buffer -b bg-run-shell-cli ok",
    ])?);

    wait_for_buffer(&harness, "bg-run-shell-cli", "ok")?;
    Ok(())
}

#[test]
fn background_if_shell_command_survives_originating_client_exit() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("bg-if-shell-cli-lifecycle")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&[
        "if-shell",
        "-b",
        "-F",
        "1",
        "set-buffer -b bg-if-shell-cli ok",
    ])?);

    wait_for_buffer(&harness, "bg-if-shell-cli", "ok")?;
    Ok(())
}

#[test]
fn source_file_background_if_shell_keeps_write_access_after_client_exit(
) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("bg-source-if-shell-cli-lifecycle")?;
    let _daemon = harness.start_hidden_daemon()?;
    let source_path = harness.tmpdir().join("source-if-shell.conf");

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    fs::write(
        &source_path,
        "if-shell -b 'sleep 0.15; true' 'set-buffer -b bg-source-if-shell-cli ok'\n",
    )?;
    assert_success(&harness.run(&[
        "source-file",
        source_path.to_str().ok_or("source path is not UTF-8")?,
    ])?);

    wait_for_buffer(&harness, "bg-source-if-shell-cli", "ok")?;
    Ok(())
}

fn wait_for_buffer(
    harness: &CliHarness,
    buffer_name: &str,
    expected: &str,
) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + BACKGROUND_TIMEOUT;
    let mut last_output: Option<Output> = None;

    while Instant::now() < deadline {
        let output = harness.run(&["show-buffer", "-b", buffer_name])?;
        if output.status.success() && stdout(&output) == expected && stderr(&output).is_empty() {
            return Ok(());
        }
        last_output = Some(output);
        thread::sleep(Duration::from_millis(25));
    }

    let diagnostic = last_output
        .map(|output| {
            format!(
                "status={:?}\nstdout:\n{}\nstderr:\n{}",
                output.status,
                stdout(&output),
                stderr(&output)
            )
        })
        .unwrap_or_else(|| "show-buffer was never attempted".to_owned());
    Err(format!(
        "timed out waiting for buffer {buffer_name:?} to become {expected:?}; last output:\n{diagnostic}"
    )
    .into())
}
