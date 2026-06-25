#![cfg(unix)]

mod common;

use std::error::Error;
use std::fs;
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use common::{assert_success, stderr, stdout, AttachedSession, CliHarness};
use rmux_pty::TerminalSize;
use serde_json::Value;

#[test]
fn wait_snapshot_and_locator_commands_work_end_to_end() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("automation-wait-snapshot")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-x", "80", "-y", "24"])?);
    assert_success(&harness.run(&[
        "send-keys",
        "-t",
        "alpha:0.0",
        "--wait-next-text",
        "AUTOMATION_READY",
        "--timeout",
        "5s",
        "--",
        "printf AUTOMATION_READY",
        "Enter",
    ])?);

    let waited = run_json(
        &harness,
        &[
            "wait-pane",
            "-t",
            "alpha:0.0",
            "--text",
            "AUTOMATION_READY",
            "--timeout",
            "2s",
            "--json",
        ],
    )?;
    assert_eq!(waited["schema_version"], 1);
    assert_eq!(waited["ok"], true);
    assert_eq!(waited["condition"], "text");

    let snapshot = run_json(&harness, &["pane-snapshot", "-t", "alpha:0.0", "--json"])?;
    assert_eq!(snapshot["schema_version"], 1);
    assert_eq!(snapshot["ok"], true);
    assert!(
        snapshot["text"]
            .as_str()
            .expect("snapshot text")
            .contains("AUTOMATION_READY"),
        "snapshot should expose rendered visible text: {snapshot}"
    );

    let locator = run_json(
        &harness,
        &[
            "locator",
            "-t",
            "alpha:0.0",
            "--get-by-text",
            "AUTOMATION_READY",
            "--json",
        ],
    )?;
    assert_eq!(locator["schema_version"], 1);
    assert_eq!(locator["ok"], true);
    assert!(locator["count"].as_u64().unwrap_or_default() >= 1);

    assert_success(&harness.run(&[
        "expect-pane",
        "-t",
        "alpha:0.0",
        "--get-by-text",
        "AUTOMATION_READY",
        "--visible",
    ])?);
    Ok(())
}

#[test]
fn wait_next_text_times_out_without_history_match() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("automation-next-text-timeout")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&[
        "send-keys",
        "-t",
        "alpha:0.0",
        "printf HISTORY_ONLY",
        "Enter",
    ])?);
    assert_success(&harness.run(&[
        "wait-pane",
        "-t",
        "alpha:0.0",
        "--text",
        "HISTORY_ONLY",
        "--timeout",
        "5s",
    ])?);
    assert_success(&harness.run(&[
        "wait-pane",
        "-t",
        "alpha:0.0",
        "--quiet",
        "--stable-for",
        "100ms",
        "--timeout",
        "5s",
    ])?);

    let output = harness.run(&[
        "wait-pane",
        "-t",
        "alpha:0.0",
        "--next-text",
        "HISTORY_ONLY",
        "--timeout",
        "100ms",
        "--json",
    ])?;
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).is_empty());
    let value: Value = serde_json::from_str(&stdout(&output))?;
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"], "timeout");
    assert_eq!(value["condition"], "next-text");
    Ok(())
}

#[test]
fn wait_text_does_not_follow_reused_slot_after_kill_and_split() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("automation-stable-pane-id")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "sleep 5"])?);
    assert_success(&harness.run(&["split-window", "-h", "-t", "alpha:0.0", "sleep 5"])?);

    let mut wait_command = harness.base_command();
    let wait_child = wait_command
        .args([
            "wait-pane",
            "-t",
            "alpha:0.1",
            "--text",
            "SLOT_REUSED_OUTPUT",
            "--timeout",
            "1s",
            "--json",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    std::thread::sleep(Duration::from_millis(150));
    assert_success(&harness.run(&["kill-pane", "-t", "alpha:0.1"])?);
    assert_success(&harness.run(&[
        "split-window",
        "-h",
        "-t",
        "alpha:0.0",
        "printf SLOT_REUSED_OUTPUT; sleep 1",
    ])?);

    let output = wait_child.wait_with_output()?;
    assert_eq!(
        output.status.code(),
        Some(1),
        "wait-pane must stay bound to the original pane id\nstdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    assert!(
        !stdout(&output).contains("\"ok\":true"),
        "wait-pane matched output from a reused slot: {}",
        stdout(&output)
    );
    Ok(())
}

#[test]
fn discovery_commands_and_list_commands_extension_filtering_work() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("automation-discovery")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["split-window", "-h", "-t", "alpha:0.0"])?);
    let sessions = run_json(
        &harness,
        &["find-sessions", "--name-prefix", "al", "--json"],
    )?;
    assert_eq!(sessions["schema_version"], 1);
    assert_eq!(sessions["ok"], true);
    assert_eq!(sessions["sessions"][0]["session_name"], "alpha");

    let panes = run_json(&harness, &["find-panes", "--json"])?;
    assert_eq!(panes["schema_version"], 1);
    assert_eq!(panes["ok"], true);
    assert!(!panes["panes"].as_array().expect("panes").is_empty());
    for pane in panes["panes"].as_array().expect("panes") {
        assert_eq!(
            pane["session_name"], "alpha",
            "find-panes must not leak record-separator newlines into session names: {panes}"
        );
    }

    assert_success(&harness.run(&["select-pane", "-t", "alpha:0.0", "-T", "TAB\tTITLE"])?);
    let panes_with_tab_title =
        run_json(&harness, &["find-panes", "--title-prefix", "TAB", "--json"])?;
    assert_eq!(panes_with_tab_title["ok"], true);
    assert_eq!(
        panes_with_tab_title["panes"][0]["title"], "TAB\tTITLE",
        "find-panes must not drop rows when fields contain tabs"
    );

    for args in [
        &[
            "locator",
            "-t",
            "alpha:0.0",
            "--get-by-text",
            "NO_SUCH_TEXT",
        ][..],
        &["find-panes", "--title", "NO_SUCH_TITLE"][..],
        &["find-sessions", "--name", "NO_SUCH_SESSION"][..],
    ] {
        let output = harness.run(args)?;
        assert_eq!(output.status.code(), Some(0), "args: {args:?}");
        assert_eq!(stdout(&output), "", "args: {args:?}");
        assert_eq!(stderr(&output), "", "args: {args:?}");
    }

    let explicit = harness.run(&["list-commands", "wait-pane"])?;
    assert_eq!(explicit.status.code(), Some(0));
    assert!(stdout(&explicit).starts_with("wait-pane "));

    let abbreviated_extension = harness.run(&["list-commands", "wait-p"])?;
    assert_eq!(abbreviated_extension.status.code(), Some(1));
    assert!(
        stderr(&abbreviated_extension).contains("unknown command: wait-p"),
        "RMUX-only extensions must not gain prefix aliases; stderr:\n{}",
        stderr(&abbreviated_extension)
    );

    let bare = harness.run(&["list-commands", "-F", "#{command_list_name}"])?;
    assert_eq!(bare.status.code(), Some(0));
    assert!(
        !stdout(&bare).lines().any(|line| line == "wait-pane"),
        "bare list-commands must hide RMUX-only automation commands"
    );
    Ok(())
}

#[test]
fn broadcast_keys_targets_multiple_panes() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("automation-broadcast")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-x", "100", "-y", "30"])?);
    assert_success(&harness.run(&["split-window", "-h", "-t", "alpha:0.0"])?);
    assert_success(&harness.run(&[
        "broadcast-keys",
        "-t",
        "alpha:0.0",
        "-t",
        "alpha:0.1",
        "--",
        "printf BROADCAST_OK",
        "Enter",
    ])?);

    for target in ["alpha:0.0", "alpha:0.1"] {
        assert_success(&harness.run(&[
            "wait-pane",
            "-t",
            target,
            "--text",
            "BROADCAST_OK",
            "--timeout",
            "5s",
        ])?);
    }
    Ok(())
}

#[test]
fn send_keys_wait_preserves_synchronize_panes_semantics() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("automation-send-keys-sync")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-x", "100", "-y", "30"])?);
    assert_success(&harness.run(&["split-window", "-h", "-t", "alpha:0.0"])?);
    assert_success(&harness.run(&[
        "set-window-option",
        "-t",
        "alpha:0",
        "synchronize-panes",
        "on",
    ])?);
    assert_success(&harness.run(&[
        "send-keys",
        "-t",
        "alpha:0.0",
        "--wait-next-text",
        "SYNC_WAIT_OK",
        "--timeout",
        "5s",
        "--",
        "printf SYNC_WAIT_OK",
        "Enter",
    ])?);
    assert_success(&harness.run(&[
        "wait-pane",
        "-t",
        "alpha:0.1",
        "--text",
        "SYNC_WAIT_OK",
        "--timeout",
        "5s",
    ])?);
    Ok(())
}

#[test]
fn send_keys_wait_preserves_target_client_current_pane() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("automation-send-keys-target-client")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "beta"])?);
    let mut attach = AttachedSession::spawn(&harness, "beta", TerminalSize::new(80, 24))?;
    attach.wait_for_raw_mode(Duration::from_secs(2))?;
    let target_client = attach.child_mut().id().to_string();

    assert_success(&harness.run(&[
        "send-keys",
        "-c",
        &target_client,
        "--wait-next-text",
        "TARGET_CLIENT_WAIT_OK",
        "--timeout",
        "5s",
        "--",
        "printf TARGET_CLIENT_WAIT_OK",
        "Enter",
    ])?);
    assert_success(&harness.run(&[
        "wait-pane",
        "-t",
        "beta:0.0",
        "--text",
        "TARGET_CLIENT_WAIT_OK",
        "--timeout",
        "5s",
    ])?);

    let alpha = run_json(&harness, &["pane-snapshot", "-t", "alpha:0.0", "--json"])?;
    assert!(
        !alpha["text"]
            .as_str()
            .expect("alpha snapshot text")
            .contains("TARGET_CLIENT_WAIT_OK"),
        "send-keys -c without -t must not pre-resolve and write to the detached fallback pane"
    );
    Ok(())
}

#[test]
fn send_keys_wait_rejects_unobservable_target_client() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("automation-send-keys-missing-target-client")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    let output = harness.run(&[
        "send-keys",
        "-c",
        "999999",
        "--wait-next-text",
        "NEVER_OBSERVED",
        "--timeout",
        "2s",
        "--",
        "printf NEVER_OBSERVED",
        "Enter",
    ])?;
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("cannot observe a pane for target client"),
        "unexpected stderr: {}",
        stderr(&output)
    );
    let rejected_payload = harness.run(&[
        "wait-pane",
        "-t",
        "alpha:0.0",
        "--text",
        "NEVER_OBSERVED",
        "--timeout",
        "100ms",
    ])?;
    assert_eq!(
        rejected_payload.status.code(),
        Some(1),
        "send-keys --wait must not send payload before rejecting an unobservable target client"
    );
    Ok(())
}

#[test]
fn collect_pane_output_drains_until_pane_exit_eof() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("automation-collect-output")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "keeper", "sleep 10"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&[
        "set-window-option",
        "-t",
        "alpha:0",
        "remain-on-exit",
        "on",
    ])?);
    assert_success(&harness.run(&[
        "respawn-pane",
        "-k",
        "-t",
        "alpha:0.0",
        "printf COLLECT_FINAL",
    ])?);
    assert_success(&harness.run(&[
        "wait-pane",
        "-t",
        "alpha:0.0",
        "--pane-exit",
        "--timeout",
        "5s",
    ])?);

    let output = harness.run(&[
        "collect-pane-output",
        "-t",
        "alpha:0.0",
        "--until-pane-exit",
        "--max-bytes",
        "1024",
    ])?;
    assert_eq!(
        output.status.code(),
        Some(0),
        "collect-pane-output failed\nstdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    assert_eq!(stdout(&output), "COLLECT_FINAL");
    assert!(stderr(&output).is_empty());
    Ok(())
}

#[test]
fn stream_pane_lines_flushes_final_partial_line() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("automation-stream-lines")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "keeper", "sleep 10"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&[
        "set-window-option",
        "-t",
        "alpha:0",
        "remain-on-exit",
        "on",
    ])?);
    assert_success(&harness.run(&[
        "respawn-pane",
        "-k",
        "-t",
        "alpha:0.0",
        "printf STREAM_FINAL",
    ])?);
    assert_success(&harness.run(&[
        "wait-pane",
        "-t",
        "alpha:0.0",
        "--pane-exit",
        "--timeout",
        "5s",
    ])?);

    let output = harness.run(&["stream-pane", "-t", "alpha:0.0", "--lines"])?;
    assert_eq!(
        output.status.code(),
        Some(0),
        "stream-pane failed\nstdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    assert_eq!(stdout(&output), "STREAM_FINAL\n");
    assert!(stderr(&output).is_empty());
    Ok(())
}

#[test]
fn stream_pane_exits_cleanly_when_stdout_pipe_closes() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("automation-stream-broken-pipe")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "yes BROKEN_PIPE"])?);
    let mut command = harness.base_command();
    let mut child = command
        .args(["stream-pane", "-t", "alpha:0.0", "--raw"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdout_pipe = child.stdout.take().expect("stream stdout");
    let mut first_byte = [0_u8; 1];
    stdout_pipe.read_exact(&mut first_byte)?;
    drop(stdout_pipe);

    let output = wait_child_with_timeout(child, Duration::from_secs(2))?;
    assert_eq!(
        output.status.code(),
        Some(0),
        "stream-pane should exit 0 after downstream closes stdout\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

#[test]
fn stream_pane_pipeline_emits_before_hot_live_output_lag_starves_head() -> Result<(), Box<dyn Error>>
{
    let harness = CliHarness::new("automation-stream-hot-head")?;
    let _daemon = harness.start_hidden_daemon()?;
    let output_path = harness.tmpdir().join("stream-head.out");
    let error_path = harness.tmpdir().join("stream-head.err");

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "yes BROKEN_PIPE"])?);

    let child = Command::new("sh")
        .arg("-c")
        .arg("\"$RMUX_BIN\" -S \"$RMUX_SOCKET\" stream-pane -t alpha:0.0 --raw 2>\"$RMUX_ERR\" | head -c 1 >\"$RMUX_OUT\"")
        .env("RMUX_BIN", env!("CARGO_BIN_EXE_rmux"))
        .env("RMUX_SOCKET", harness.socket_path())
        .env("RMUX_OUT", &output_path)
        .env("RMUX_ERR", &error_path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let output = wait_child_with_timeout(child, Duration::from_secs(8))?;
    assert_eq!(
        output.status.code(),
        Some(0),
        "stream-pane | head should finish once the first byte is delivered\nstdout:\n{}\nstderr:\n{}\nstream stderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
        fs::read_to_string(&error_path).unwrap_or_default()
    );
    assert_eq!(
        fs::read(&output_path)?.len(),
        1,
        "head should capture exactly one byte"
    );
    Ok(())
}

#[test]
fn with_session_kill_on_owner_exit_releases_name_immediately() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("automation-with-session-kill")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "owned", "sleep", "60"])?);
    assert_success(&harness.run(&[
        "with-session",
        "owned",
        "--kill-on-owner-exit",
        "--ttl",
        "30s",
        "--",
        "sh",
        "-c",
        "true",
    ])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "owned", "sleep", "60"])?);
    Ok(())
}

fn run_json(harness: &CliHarness, args: &[&str]) -> Result<Value, Box<dyn Error>> {
    let output = harness.run(args)?;
    assert_eq!(
        output.status.code(),
        Some(0),
        "command failed\nstdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    assert!(stderr(&output).is_empty());
    Ok(serde_json::from_str(&stdout(&output))?)
}

fn wait_child_with_timeout(
    mut child: Child,
    timeout: Duration,
) -> Result<std::process::Output, Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    loop {
        if child.try_wait()?.is_some() {
            return Ok(child.wait_with_output()?);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let output = child.wait_with_output()?;
            return Err(format!(
                "child did not exit within {timeout:?}; status: {:?}; stderr:\n{}",
                output.status.code(),
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}
