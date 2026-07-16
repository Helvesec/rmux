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
fn nonzero_pane_base_index_preserves_targeted_automation_and_percent_resize(
) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("automation-pane-base-index")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-x", "80", "-y", "24"])?);
    assert_success(&harness.run(&[
        "set-window-option",
        "-t",
        "alpha:0",
        "pane-base-index",
        "1",
    ])?);
    assert_success(&harness.run(&["split-window", "-h", "-t", "alpha:0.1"])?);

    assert_success(&harness.run(&[
        "send-keys",
        "-t",
        "alpha:0.1",
        "printf PANE_ONE_MARKER",
        "Enter",
    ])?);
    assert_success(&harness.run(&[
        "wait-pane",
        "-t",
        "alpha:0.1",
        "--text",
        "PANE_ONE_MARKER",
        "--timeout",
        "5s",
    ])?);
    assert_success(&harness.run(&[
        "send-keys",
        "-t",
        "alpha:0.2",
        "--wait-next-text",
        "PANE_TWO_MARKER",
        "--timeout",
        "5s",
        "--",
        "printf PANE_TWO_MARKER",
        "Enter",
    ])?);

    let pane_one = run_json(&harness, &["pane-snapshot", "-t", "alpha:0.1", "--json"])?;
    let pane_two = run_json(&harness, &["pane-snapshot", "-t", "alpha:0.2", "--json"])?;
    let pane_one_text = pane_one["text"].as_str().expect("pane one snapshot text");
    let pane_two_text = pane_two["text"].as_str().expect("pane two snapshot text");
    assert!(pane_one_text.contains("PANE_ONE_MARKER"), "{pane_one}");
    assert!(!pane_one_text.contains("PANE_TWO_MARKER"), "{pane_one}");
    assert!(pane_two_text.contains("PANE_TWO_MARKER"), "{pane_two}");
    assert!(!pane_two_text.contains("PANE_ONE_MARKER"), "{pane_two}");

    assert_success(&harness.run(&["resize-pane", "-t", "alpha:0.1", "-x", "60%"])?);
    let panes = harness.run(&[
        "list-panes",
        "-t",
        "alpha:0",
        "-F",
        "#{pane_index}:#{pane_width}",
    ])?;
    assert_eq!(panes.status.code(), Some(0));
    assert!(stderr(&panes).is_empty());
    let panes = stdout(&panes);
    let pane_one_width = pane_width(&panes, 1)?;
    let pane_two_width = pane_width(&panes, 2)?;
    assert_eq!(pane_one_width, 48, "unexpected pane widths: {panes:?}");
    assert_ne!(pane_two_width, 48, "resize targeted wrong pane: {panes:?}");

    Ok(())
}

#[test]
fn automation_slot_lookup_preserves_list_panes_hook_family() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("automation-slot-lookup-hook")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["set-buffer", "-b", "slot-lookup-hook", "seed"])?);
    assert_success(&harness.run(&[
        "set-hook",
        "-g",
        "after-list-panes",
        "set-buffer -b slot-lookup-hook list-panes",
    ])?);
    assert_success(&harness.run(&[
        "set-hook",
        "-g",
        "after-display-message",
        "set-buffer -b slot-lookup-hook display-message",
    ])?);

    let snapshot = run_json(&harness, &["pane-snapshot", "-t", "alpha:0.0", "--json"])?;
    assert_eq!(snapshot["ok"], true);
    let hook_family = harness.run(&["show-buffer", "-b", "slot-lookup-hook"])?;
    assert_eq!(hook_family.status.code(), Some(0));
    assert_eq!(stdout(&hook_family), "list-panes");
    assert!(stderr(&hook_family).is_empty());

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
fn send_keys_text_wait_follows_session_identity_across_rename() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("automation-wait-text-rename")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    let mut command = harness.base_command();
    let mut wait_child = command
        .args([
            "send-keys",
            "-t",
            "alpha:0.0",
            "--wait-text",
            "WAIT_TEXT_FINISHED",
            "--timeout",
            "5s",
            "--",
            "printf WAIT_TEXT_%s STARTED; sleep 1; printf WAIT_TEXT_%s FINISHED",
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
        "WAIT_TEXT_STARTED",
        "--timeout",
        "3s",
    ])?);
    assert_success(&harness.run(&["rename-session", "-t", "alpha", "beta"])?);
    std::thread::sleep(Duration::from_millis(150));
    assert!(
        wait_child.try_wait()?.is_none(),
        "text wait must remain armed after the target session is renamed"
    );

    let output = wait_child_with_timeout(wait_child, Duration::from_secs(4))?;
    assert_eq!(
        output.status.code(),
        Some(0),
        "text wait failed after rename\nstdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    Ok(())
}

#[test]
fn send_keys_quiet_wait_follows_session_identity_across_rename() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("automation-wait-quiet-rename")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    let mut command = harness.base_command();
    let mut wait_child = command
        .args([
            "send-keys",
            "-t",
            "alpha:0.0",
            "--wait",
            "quiet",
            "--stable-for",
            "2s",
            "--timeout",
            "5s",
            "--",
            "printf WAIT_QUIET_%s STARTED; sleep 1; printf WAIT_QUIET_%s FINISHED",
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
        "WAIT_QUIET_STARTED",
        "--timeout",
        "3s",
    ])?);
    assert_success(&harness.run(&["rename-session", "-t", "alpha", "beta"])?);
    std::thread::sleep(Duration::from_millis(150));
    assert!(
        wait_child.try_wait()?.is_none(),
        "quiet wait must not report false success when the target session is renamed"
    );

    let output = wait_child_with_timeout(wait_child, Duration::from_secs(4))?;
    assert_eq!(
        output.status.code(),
        Some(0),
        "quiet wait failed after rename\nstdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    assert_success(&harness.run(&[
        "wait-pane",
        "-t",
        "beta:0.0",
        "--text",
        "WAIT_QUIET_FINISHED",
        "--timeout",
        "1s",
    ])?);
    Ok(())
}

#[test]
fn send_keys_waits_fail_closed_when_observed_session_is_killed() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("automation-wait-killed-session")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "keeper", "sleep", "30"])?);

    for (session, wait_args, started) in [
        (
            "quiet-target",
            vec!["--wait", "quiet", "--stable-for", "2s"],
            "KILL_QUIET_STARTED",
        ),
        (
            "visible-target",
            vec!["--wait-visible-text", "KILL_VISIBLE_EXPECTED"],
            "KILL_VISIBLE_STARTED",
        ),
    ] {
        assert_success(&harness.run(&["new-session", "-d", "-s", session])?);
        let target = format!("{session}:0.0");
        let mut arguments = vec!["send-keys", "-t", &target];
        arguments.extend(wait_args);
        arguments.extend([
            "--timeout",
            "5s",
            "--",
            "printf KILL_QUIET_%s STARTED; printf KILL_VISIBLE_%s STARTED; sleep 5",
            "Enter",
        ]);
        let mut command = harness.base_command();
        let wait_child = command
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        assert_success(&harness.run(&[
            "wait-pane",
            "-t",
            &target,
            "--text",
            started,
            "--timeout",
            "3s",
        ])?);
        assert_success(&harness.run(&["kill-session", "-t", session])?);

        let output = wait_child_with_timeout(wait_child, Duration::from_secs(2))?;
        assert_ne!(
            output.status.code(),
            Some(0),
            "wait must not claim success after its stable session identity is destroyed\nstdout:\n{}\nstderr:\n{}",
            stdout(&output),
            stderr(&output)
        );
    }
    Ok(())
}

#[test]
fn send_keys_visible_wait_rejects_old_session_name_aba_after_rename() -> Result<(), Box<dyn Error>>
{
    let harness = CliHarness::new("automation-wait-rename-aba")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    let mut command = harness.base_command();
    let mut wait_child = command
        .args([
            "send-keys",
            "-t",
            "alpha:0.0",
            "--wait-visible-text",
            "ABA_TARGET_DONE",
            "--timeout",
            "5s",
            "--",
            "printf ABA_%s STARTED; read marker; printf ABA_%s TARGET_DONE",
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
        "ABA_STARTED",
        "--timeout",
        "3s",
    ])?);
    assert_success(&harness.run(&["rename-session", "-t", "alpha", "beta"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&[
        "send-keys",
        "-t",
        "alpha:0.0",
        "printf ABA_%s TARGET_DONE",
        "Enter",
    ])?);
    std::thread::sleep(Duration::from_millis(150));
    assert!(
        wait_child.try_wait()?.is_none(),
        "wait followed a replacement session that reused the original name"
    );

    assert_success(&harness.run(&["send-keys", "-t", "beta:0.0", "release", "Enter"])?);
    let output = wait_child_with_timeout(wait_child, Duration::from_secs(3))?;
    assert_eq!(
        output.status.code(),
        Some(0),
        "wait did not follow the original session identity through rename and name reuse\nstdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    Ok(())
}

#[test]
fn send_keys_quiet_wait_rejects_old_session_name_aba_after_rename() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("automation-quiet-wait-rename-aba")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    let mut command = harness.base_command();
    let mut wait_child = command
        .args([
            "send-keys",
            "-t",
            "alpha:0.0",
            "--wait",
            "quiet",
            "--stable-for",
            "5s",
            "--timeout",
            "8s",
            "--",
            "printf ABA_QUIET_%s STARTED; read marker; printf ABA_QUIET_%s DONE",
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
        "ABA_QUIET_STARTED",
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
        "quiet wait mistook a replacement session without the stable pane id for pane exit"
    );

    assert_success(&harness.run(&["send-keys", "-t", "beta:0.0", "release", "Enter"])?);
    let output = wait_child_with_timeout(wait_child, Duration::from_secs(6))?;
    assert_eq!(
        output.status.code(),
        Some(0),
        "quiet wait did not remain bound to the original pane identity\nstdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    Ok(())
}

#[test]
fn wait_pane_visible_text_follows_session_identity_across_rename() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("wait-pane-visible-rename")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    let mut command = harness.base_command();
    let mut wait_child = command
        .args([
            "wait-pane",
            "-t",
            "alpha:0.0",
            "--visible-text",
            "DIRECT_WAIT_DONE",
            "--timeout",
            "5s",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    std::thread::sleep(Duration::from_millis(150));
    assert!(
        wait_child.try_wait()?.is_none(),
        "wait-pane must be armed before the rename"
    );

    assert_success(&harness.run(&["rename-session", "-t", "alpha", "beta"])?);
    std::thread::sleep(Duration::from_millis(150));
    assert!(
        wait_child.try_wait()?.is_none(),
        "wait-pane must remain armed after the target session is renamed"
    );
    assert_success(&harness.run(&[
        "send-keys",
        "-t",
        "beta:0.0",
        "printf DIRECT_WAIT_%s DONE",
        "Enter",
    ])?);

    let output = wait_child_with_timeout(wait_child, Duration::from_secs(3))?;
    assert_eq!(
        output.status.code(),
        Some(0),
        "wait-pane failed to follow the stable session identity\nstdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
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

fn pane_width(output: &str, pane_index: u32) -> Result<u16, Box<dyn Error>> {
    let prefix = format!("{pane_index}:");
    let width = output
        .lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .ok_or_else(|| format!("pane {pane_index} missing from {output:?}"))?;
    Ok(width.parse()?)
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
