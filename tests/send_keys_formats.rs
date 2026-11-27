#![cfg(unix)]

mod common;

use common::{assert_success, stdout, CliHarness};
use std::{
    error::Error,
    fs, thread,
    time::{Duration, Instant},
};

fn expect_line(harness: &CliHarness, expected: &str) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let output = harness.run(&["capture-pane", "-p", "-t", "alpha:0.0"])?;
        assert!(output.status.success(), "{output:?}");
        let text = stdout(&output);
        if text.lines().any(|line| line == expected) {
            return Ok(());
        }
        assert!(Instant::now() < deadline, "missing {expected:?}: {text:?}");
        thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn send_keys_formats_cli_neighbors() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("send-format-neighbors")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "cat"])?);
    assert_success(&harness.run(&["send-keys", "-t", "alpha:0.0", "#{session_name}", "Enter"])?);
    expect_line(&harness, "#{session_name}")?;
    assert_success(&harness.run(&[
        "send-keys",
        "-t",
        "alpha:0.0",
        "-Fl",
        "#{session_name}",
        "Enter",
    ])?);
    assert_success(&harness.run(&["send-keys", "-t", "alpha:0.0", "Enter"])?);
    expect_line(&harness, "#{session_name}Enter")?;
    // A fresh sink cannot satisfy this assertion with an earlier pane line.
    let ready = harness.tmpdir().join("hex-ready");
    let captured = harness.tmpdir().join("hex-input");
    let script = harness.tmpdir().join("hex.sh");
    let quote =
        |path: &std::path::Path| format!("'{}'", path.to_str().unwrap().replace('\'', "'\\''"));
    fs::write(
        &script,
        format!(
            "stty -echo\nprintf ready > {}\nhead -n 1 > {}\nsleep 60\n",
            quote(&ready),
            quote(&captured),
        ),
    )?;
    let command = format!("sh {}", quote(&script));
    assert_success(&harness.run(&["new-session", "-d", "-s", "hex", &command])?);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready.exists() {
        assert!(Instant::now() < deadline, "hex sink did not become ready");
        thread::sleep(Duration::from_millis(25));
    }
    assert_success(&harness.run(&[
        "send-keys",
        "-t",
        "hex:0.0",
        "-FH",
        "23",
        "7b",
        "73",
        "65",
        "73",
        "73",
        "69",
        "6f",
        "6e",
        "5f",
        "6e",
        "61",
        "6d",
        "65",
        "7d",
        "0d",
    ])?);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let bytes = fs::read(&captured).unwrap_or_default();
        if bytes.ends_with(b"\n") {
            assert_eq!(bytes, b"#{session_name}\n");
            break;
        }
        assert!(Instant::now() < deadline, "incomplete hex input: {bytes:?}");
        thread::sleep(Duration::from_millis(25));
    }
    assert_success(&harness.run(&[
        "send-keys",
        "-t",
        "alpha:0.0",
        "-F",
        "-N",
        "2",
        "#{session_name}",
        "-",
        "#{missing_send_key_format}",
        "z z",
        "Enter",
    ])?);
    expect_line(&harness, "#{session_name}-#{missing_send_key_format}z z")?;
    let bad = harness.run(&["send-keys", "-t", "missing:0.0", "-F", "#{session_name}"])?;
    assert!(!bad.status.success());
    assert_success(&harness.run(&["send-keys", "-t", "alpha:0.0", "-FH", "#{session_name}"])?);
    Ok(())
}

#[test]
fn send_keys_formats_source_and_queue_use_execution_context() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("send-format-source-queue")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "cat"])?);
    let config = harness.tmpdir().join("send.conf");
    fs::write(&config, "rename-window -t alpha:0 before\nsend-keys -F -t alpha:0.0 'source:#{window_name}:#{pane_index}' Enter\n")?;
    assert_success(&harness.run(&["source-file", config.to_str().unwrap()])?);
    expect_line(&harness, "source:#{window_name}:#{pane_index}")?;
    assert_success(&harness.run(&[
        "rename-window",
        "-t",
        "alpha:0",
        "after",
        ";",
        "send-keys",
        "-F",
        "-t",
        "alpha:0.0",
        "queue:#{window_name}:#{pane_index}",
        "Enter",
    ])?);
    expect_line(&harness, "queue:#{window_name}:#{pane_index}")?;
    Ok(())
}

#[test]
fn send_keys_formats_startup_config() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("send-format-startup")?;
    let _cleanup = harness.auto_start_cleanup()?;
    let config = harness.tmpdir().join("send.conf");
    fs::write(&config, "new-session -d -s alpha cat\nsend-keys -F -t alpha:0.0 'startup:#{session_name}:#{pane_index}' Enter\n")?;
    assert_success(&harness.run(&[
        "-f",
        config.to_str().unwrap(),
        "new-session",
        "-d",
        "-s",
        "bootstrap",
        "cat",
    ])?);
    expect_line(&harness, "startup:#{session_name}:#{pane_index}")?;
    Ok(())
}

fn copy_mode_argument_boundary(expand_formats: bool) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("copy-argv-boundary")?;
    let _daemon = harness.start_hidden_daemon()?;
    let ready = harness.tmpdir().join("raw-ready");
    let captured = harness.tmpdir().join("raw-input");
    let script = harness.tmpdir().join("raw.sh");
    let quote =
        |path: &std::path::Path| format!("'{}'", path.to_str().unwrap().replace('\'', "'\\''"));
    fs::write(
        &script,
        format!(
            "stty raw -echo\n: > {}\nprintf ready > {}\ncat >> {}\n",
            quote(&captured),
            quote(&ready),
            quote(&captured),
        ),
    )?;
    assert_success(&harness.run(&[
        "new-session",
        "-d",
        "-s",
        "alpha",
        &format!("sh {}", quote(&script)),
    ])?);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready.exists() {
        assert!(Instant::now() < deadline, "raw sink did not become ready");
        thread::sleep(Duration::from_millis(25));
    }
    assert_success(&harness.run(&["set-window-option", "-t", "alpha:0", "mode-keys", "emacs"])?);
    assert_success(&harness.run(&["copy-mode", "-t", "alpha:0.0"])?);
    assert_eq!(
        stdout(&harness.run(&[
            "display-message",
            "-p",
            "-t",
            "alpha:0.0",
            "#{pane_in_mode}"
        ])?)
        .trim(),
        "1"
    );
    let mut argv = vec!["send-keys", "-t", "alpha:0.0"];
    if expand_formats {
        argv.push("-F");
    }
    argv.push("#{?session_name,q,invalid}");
    let output = harness.run(&argv)?;
    assert_eq!(
        stdout(&harness.run(&[
            "display-message",
            "-p",
            "-t",
            "alpha:0.0",
            "#{pane_in_mode}"
        ])?)
        .trim(),
        "0"
    );
    thread::sleep(Duration::from_millis(100));
    let bytes = fs::read(&captured)?;
    eprintln!(
        "argv={argv:?}, exit={:?}, input_hex={}, stderr={}",
        output.status.code(),
        bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(bytes, b"");
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("not in a mode"));
    Ok(())
}

#[test]
fn send_keys_formats_copy_mode_single_argv_boundary() -> Result<(), Box<dyn Error>> {
    copy_mode_argument_boundary(true)
}

#[test]
fn send_keys_plain_copy_mode_single_argv_boundary() -> Result<(), Box<dyn Error>> {
    copy_mode_argument_boundary(false)
}
