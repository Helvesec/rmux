#![cfg(unix)]

mod common;

use std::error::Error;
use std::ffi::OsString;
use std::fs::{self, File};
use std::os::unix::ffi::OsStringExt;
use std::process::Stdio;
use std::time::{Duration, Instant};

use common::{assert_success, stderr, stdout, terminate_child, CliHarness, BINARY_OVERRIDE_ENV};

const SETTLE: Duration = Duration::from_millis(150);

#[test]
fn split_window_full_size_uses_the_full_window_axis() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("split-window-full-size-semantics")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-x", "80", "-y", "24", "-s", "s"])?);
    assert_success(&harness.run(&["split-window", "-h", "-t", "s:0.0"])?);
    assert_success(&harness.run(&["split-window", "-f", "-v", "-t", "s:0.0"])?);

    let panes = harness.run(&[
        "list-panes",
        "-t",
        "s",
        "-F",
        "#{pane_index}:#{pane_left}:#{pane_top}:#{pane_width}:#{pane_height}",
    ])?;
    assert_eq!(panes.status.code(), Some(0));
    assert_eq!(stdout(&panes), "0:0:0:40:12\n1:41:0:39:12\n2:0:13:80:11\n");
    assert!(stderr(&panes).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn split_window_stdin_flag_feeds_the_created_pane() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("split-window-stdin-flag")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-x", "40", "-y", "12", "-s", "s"])?);
    let input_path = harness.tmpdir().join("split-stdin.txt");
    fs::write(&input_path, "hello\nworld\n")?;
    let split = harness.run_with(&["split-window", "-I", "-t", "s:0.0"], |command| {
        command.stdin(Stdio::from(
            File::open(&input_path).expect("open split stdin"),
        ));
    })?;
    assert_success(&split);

    let panes_stdout = wait_for_stdout_contains(
        &harness,
        &["list-panes", "-t", "s", "-F", "#{pane_index}:#{pane_id}"],
        "1:%",
    )?;
    assert!(
        panes_stdout.contains("1:%"),
        "split-window -I should create pane 1: {panes_stdout:?}"
    );

    let capture_stdout = wait_for_capture_contains(&harness, "s:0.1", "hello\nworld")?;
    let payload_count = capture_stdout.matches("hello").count();
    assert_eq!(
        payload_count, 1,
        "capture should contain exactly one stdin payload: {:?}",
        capture_stdout
    );
    assert!(
        capture_stdout.contains("hello\nworld"),
        "LF stdin should render as separate terminal lines without indentation: {:?}",
        capture_stdout
    );

    let commands = harness.run(&[
        "list-panes",
        "-t",
        "s",
        "-F",
        "#{pane_index}:#{pane_current_command}:#{pane_start_command}",
    ])?;
    assert_eq!(commands.status.code(), Some(0));
    assert!(
        !stdout(&commands).contains("printf"),
        "split-window -I must not shell a printf wrapper: {:?}",
        stdout(&commands)
    );

    let dead_metadata_stdout = wait_for_stdout_matching(
        &harness,
        &[
            "list-panes",
            "-t",
            "s",
            "-F",
            "#{pane_index}:dead=#{pane_dead}:status=#{pane_dead_status}:time=#{pane_dead_time}:cmd=#{pane_current_command}",
        ],
        |stdout| {
            ["bash", "zsh", "sh"].iter().any(|shell| {
                stdout.contains(&format!("1:dead=1:status=:time=:cmd={shell}"))
            })
        },
    )?;
    assert!(
        ["bash", "zsh", "sh"]
            .iter()
            .any(|shell| dead_metadata_stdout
                .contains(&format!("1:dead=1:status=:time=:cmd={shell}"))),
        "split-window -I synthetic pane should not expose exit status or time: {:?}",
        dead_metadata_stdout
    );

    let binary_path = harness.tmpdir().join("split-stdin-binary.bin");
    fs::write(&binary_path, b"a\xffb\n")?;
    let binary_split = harness.run_with(&["split-window", "-I", "-t", "s:0.0"], |command| {
        command.stdin(Stdio::from(
            File::open(&binary_path).expect("open binary split stdin"),
        ));
    })?;
    assert_success(&binary_split);

    let ignored_path = harness.tmpdir().join("split-stdin-command.txt");
    fs::write(&ignored_path, "ignored by tmux\n")?;
    let command_split =
        harness.run_with(&["split-window", "-I", "-t", "s:0.0", "cat"], |command| {
            command.stdin(Stdio::from(
                File::open(&ignored_path).expect("open command split stdin"),
            ));
        })?;
    assert_success(&command_split);
    std::thread::sleep(SETTLE);

    let command_panes = harness.run(&["list-panes", "-t", "s", "-F", "#{pane_id}"])?;
    assert_eq!(command_panes.status.code(), Some(0));
    assert!(stderr(&command_panes).is_empty());
    let pane_ids = stdout(&command_panes)
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    assert!(
        !pane_ids.is_empty(),
        "session should retain panes after explicit-command split"
    );
    for pane_id in pane_ids {
        let command_capture = harness.run(&["capture-pane", "-p", "-t", &pane_id])?;
        assert_eq!(
            command_capture.status.code(),
            Some(0),
            "capture-pane should resolve listed pane {pane_id}: {:?}",
            stderr(&command_capture)
        );
        assert!(
            !stdout(&command_capture).contains("ignored by tmux"),
            "split-window -I with an explicit command must not inject stdin into {pane_id}: {:?}",
            stdout(&command_capture)
        );
    }

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn source_file_split_window_stdin_flag_creates_empty_pane() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("source-file-split-window-stdin-flag")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-x", "40", "-y", "12", "-s", "s"])?);
    let source = harness.tmpdir().join("split-stdin.conf");
    fs::write(&source, "split-window -I -t s:0.0\n")?;

    let output = harness.run(&["source-file", source.to_str().expect("utf-8 path")])?;
    assert_eq!(
        output.status.code(),
        Some(0),
        "source-file split-window -I should succeed; stdout={:?}, stderr={:?}",
        stdout(&output),
        stderr(&output)
    );
    assert!(stderr(&output).is_empty());

    let panes_stdout = wait_for_stdout_contains(
        &harness,
        &["list-panes", "-t", "s", "-F", "#{pane_index}:#{pane_id}"],
        "1:%",
    )?;
    assert!(
        panes_stdout.contains("1:%"),
        "source-file split-window -I should create pane 1: {panes_stdout:?}"
    );

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn initial_session_pane_preserves_non_utf8_client_environment() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("non-utf8-client-env")?;
    let mut daemon = harness.start_hidden_daemon()?;

    let output = harness.run_with(
        &[
            "new-session",
            "-d",
            "-x",
            "80",
            "-y",
            "12",
            "-s",
            "s",
            "count=$(printenv BAD_VAR | wc -c | tr -d ' '); printf 'BAD_VAR_BYTES=%s\\n' \"$count\"; sleep 30",
        ],
        |command| {
            command.env(
                OsString::from("BAD_VAR"),
                OsString::from_vec(b"foo\xffbar".to_vec()),
            );
        },
    )?;
    assert_success(&output);
    std::thread::sleep(SETTLE);

    let capture = harness.run(&["capture-pane", "-p", "-t", "s:0.0"])?;
    assert_eq!(capture.status.code(), Some(0));
    assert!(
        stdout(&capture).contains("BAD_VAR_BYTES=8"),
        "pane did not inherit raw BAD_VAR bytes: {:?}",
        stdout(&capture)
    );
    assert!(stderr(&capture).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn show_environment_global_escapes_non_utf8_daemon_environment() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("show-non-utf8-global-env")?;

    let output = harness.run_with(&["new-session", "-d", "-s", "s", "sleep 30"], |command| {
        command.env(
            OsString::from("FOO_SHOW"),
            OsString::from_vec(b"A\xffB".to_vec()),
        );
    })?;
    assert_success(&output);

    let shown = harness.run(&["show-environment", "-g", "FOO_SHOW"])?;
    assert_eq!(shown.status.code(), Some(0));
    assert!(stderr(&shown).is_empty());
    assert_eq!(stdout(&shown), "FOO_SHOW=A\\377B\n");

    let shell_shown = harness.run(&["show-environment", "-gs", "FOO_SHOW"])?;
    assert_eq!(shell_shown.status.code(), Some(0));
    assert!(stderr(&shell_shown).is_empty());
    assert_eq!(
        stdout(&shell_shown),
        "FOO_SHOW=\"A\\377B\"; export FOO_SHOW;\n"
    );

    let _ = harness.run(&["kill-server"]);
    Ok(())
}

#[test]
fn global_environment_unset_suppresses_next_client_spawn_value() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("global-env-unset-suppresses-client")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run_with(
        &["new-session", "-d", "-s", "base", "sleep 30"],
        |command| {
            command.env("FOO", "base");
        },
    )?);
    assert_success(&harness.run(&["set-environment", "-gu", "FOO"])?);
    assert_success(&harness.run_with(
        &[
            "new-session",
            "-d",
            "-s",
            "probe",
            "if [ -z \"${FOO+x}\" ]; then printf 'FOO=GONE\\n'; else printf 'FOO=%s\\n' \"$FOO\"; fi; sleep 30",
        ],
        |command| {
            command.env("FOO", "leak");
        },
    )?);
    std::thread::sleep(SETTLE);

    let capture = harness.run(&["capture-pane", "-p", "-t", "probe:0.0"])?;
    assert_eq!(capture.status.code(), Some(0));
    assert!(
        stdout(&capture).contains("FOO=GONE"),
        "unset environment leaked into new pane: {:?}",
        stdout(&capture)
    );
    assert!(stderr(&capture).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn session_environment_unset_keeps_captured_base_environment() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("session-env-unset-keeps-base")?;
    let mut daemon = harness.start_hidden_daemon()?;
    let output_path = harness.tmpdir().join("session-env.txt");

    assert_success(&harness.run_with(
        &["new-session", "-d", "-s", "s", "sleep 30"],
        |command| {
            command.env("FOO", "base");
        },
    )?);
    assert_success(&harness.run(&["set-environment", "-t", "s", "-u", "FOO"])?);
    assert_success(&harness.run_with(
        &[
            "new-window",
            "-d",
            "-t",
            "s:",
            &format!(
                "sh -c 'printf %s \"$FOO\" > {}; sleep 1'",
                output_path.display()
            ),
        ],
        |command| {
            command.env("FOO", "leak");
        },
    )?);
    std::thread::sleep(SETTLE * 8);

    assert_eq!(fs::read_to_string(&output_path)?, "base");

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn pipe_pane_stdin_flag_writes_command_output_to_pane() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("pipe-pane-stdin-flag")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&[
        "new-session",
        "-d",
        "-x",
        "40",
        "-y",
        "12",
        "-s",
        "s",
        "cat",
    ])?);
    let pipe = harness.run(&["pipe-pane", "-I", "-t", "s:0.0", "printf 'frompipe\\n'"])?;
    assert_success(&pipe);

    let capture_stdout = wait_for_capture_contains(&harness, "s:0.0", "frompipe")?;
    assert!(
        capture_stdout.contains("frompipe"),
        "pipe-pane -I should inject command output into the target pane: {:?}",
        capture_stdout
    );

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn send_keys_format_flag_sends_literal_format_text() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("send-keys-format-literal")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&[
        "new-session",
        "-d",
        "-x",
        "40",
        "-y",
        "12",
        "-s",
        "alpha",
        "cat",
    ])?);
    assert_success(&harness.run(&[
        "send-keys",
        "-t",
        "alpha:0.0",
        "-F",
        "#{session_name}",
        "Enter",
    ])?);
    std::thread::sleep(SETTLE);

    let capture = harness.run(&["capture-pane", "-p", "-t", "alpha:0.0"])?;
    assert_eq!(capture.status.code(), Some(0));
    assert!(
        stdout(&capture).contains("\n#{session_name}\n"),
        "send-keys -F should send format text literally: {:?}",
        stdout(&capture)
    );
    assert!(
        !stdout(&capture).contains("\nalpha\n"),
        "send-keys -F should not expand the session_name format: {:?}",
        stdout(&capture)
    );
    assert!(stderr(&capture).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn send_keys_rejects_zero_repeat_count() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("send-keys-zero-repeat")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "sleep", "60"])?);

    let output = harness.run(&["send-keys", "-N", "0", "-t", "alpha:0.0", "A"])?;

    assert_eq!(output.status.code(), Some(1));
    assert!(stdout(&output).is_empty());
    assert_eq!(stderr(&output), "repeat count too small\n");

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn last_pane_input_flags_apply_to_the_selected_last_pane() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("last-pane-input-flags")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&[
        "new-session",
        "-d",
        "-x",
        "40",
        "-y",
        "12",
        "-s",
        "s",
        "cat",
    ])?);
    assert_success(&harness.run(&["split-window", "-h", "-t", "s:0.0", "cat"])?);
    std::thread::sleep(SETTLE);

    assert_success(&harness.run(&["last-pane", "-d", "-t", "s:0"])?);
    assert_success(&harness.run(&["send-keys", "-t", "s:0.0", "blocked", "Enter"])?);
    std::thread::sleep(SETTLE);

    let disabled_capture = harness.run(&["capture-pane", "-p", "-t", "s:0.0"])?;
    assert_eq!(disabled_capture.status.code(), Some(0));
    assert!(
        !stdout(&disabled_capture).contains("blocked"),
        "last-pane -d should disable input to the selected last pane: {:?}",
        stdout(&disabled_capture)
    );

    assert_success(&harness.run(&["select-pane", "-t", "s:0.1"])?);
    assert_success(&harness.run(&["last-pane", "-e", "-t", "s:0"])?);
    assert_success(&harness.run(&["send-keys", "-t", "s:0.0", "allowed", "Enter"])?);
    std::thread::sleep(SETTLE);

    let enabled_capture = harness.run(&["capture-pane", "-p", "-t", "s:0.0"])?;
    assert_eq!(enabled_capture.status.code(), Some(0));
    assert!(
        stdout(&enabled_capture).contains("allowed"),
        "last-pane -e should re-enable input to the selected last pane: {:?}",
        stdout(&enabled_capture)
    );
    assert!(stderr(&enabled_capture).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn resize_pane_absolute_width_preserves_horizontal_chain_layout() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("resize-pane-horizontal-chain")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-x", "100", "-y", "40", "-s", "a"])?);
    assert_success(&harness.run(&["split-window", "-h"])?);
    assert_success(&harness.run(&["split-window", "-h"])?);
    assert_success(&harness.run(&["resize-pane", "-t", "a.0", "-x", "60"])?);

    let panes = harness.run(&[
        "list-panes",
        "-t",
        "a",
        "-F",
        "#{pane_index} L=#{pane_left} T=#{pane_top} #{pane_width}x#{pane_height}",
    ])?;
    assert_eq!(panes.status.code(), Some(0));
    assert_eq!(
        stdout(&panes),
        "0 L=0 T=0 60x40\n1 L=61 T=0 14x40\n2 L=76 T=0 24x40\n"
    );
    assert!(stderr(&panes).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn resize_pane_absolute_height_preserves_vertical_chain_layout() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("resize-pane-vertical-chain")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-x", "80", "-y", "30", "-s", "a"])?);
    assert_success(&harness.run(&["split-window", "-v"])?);
    assert_success(&harness.run(&["split-window", "-v"])?);
    assert_success(&harness.run(&["resize-pane", "-t", "a.0", "-y", "4"])?);

    let panes = harness.run(&[
        "list-panes",
        "-t",
        "a",
        "-F",
        "#{pane_index} L=#{pane_left} T=#{pane_top} #{pane_width}x#{pane_height}",
    ])?;
    assert_eq!(panes.status.code(), Some(0));
    assert_eq!(
        stdout(&panes),
        "0 L=0 T=0 80x4\n1 L=0 T=5 80x18\n2 L=0 T=24 80x6\n"
    );
    assert!(stderr(&panes).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn resize_pane_compact_direction_and_trailing_adjustment_match_tmux() -> Result<(), Box<dyn Error>>
{
    let harness = CliHarness::new("resize-pane-compact-trailing")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-x", "80", "-y", "24", "-s", "s"])?);
    assert_success(&harness.run(&["split-window", "-h", "-t", "s:0.0"])?);
    assert_success(&harness.run(&["resize-pane", "-R", "-L", "-t", "s:0.0", "3"])?);

    let panes = harness.run(&[
        "list-panes",
        "-t",
        "s",
        "-F",
        "#{pane_index}:#{pane_width}x#{pane_height}",
    ])?;
    assert_eq!(panes.status.code(), Some(0));
    assert_eq!(stdout(&panes), "0:37x24\n1:42x24\n");
    assert!(stderr(&panes).is_empty());

    let source = harness.tmpdir().join("resize.conf");
    fs::write(&source, "resize-pane -RL -t s:0.0\n")?;
    assert_success(&harness.run(&["source-file", source.to_str().expect("utf-8 path")])?);

    let sourced = harness.run(&[
        "list-panes",
        "-t",
        "s",
        "-F",
        "#{pane_index}:#{pane_width}x#{pane_height}",
    ])?;
    assert_eq!(sourced.status.code(), Some(0));
    assert_eq!(stdout(&sourced), "0:36x24\n1:43x24\n");
    assert!(stderr(&sourced).is_empty());

    let before_no_direction = stdout(&sourced);
    assert_success(&harness.run(&["resize-pane", "-t", "s:0.0", "3"])?);
    let after_no_direction = harness.run(&[
        "list-panes",
        "-t",
        "s",
        "-F",
        "#{pane_index}:#{pane_width}x#{pane_height}",
    ])?;
    assert_eq!(after_no_direction.status.code(), Some(0));
    assert_eq!(stdout(&after_no_direction), before_no_direction);

    let no_direction_source = harness.tmpdir().join("resize-no-direction.conf");
    fs::write(&no_direction_source, "resize-pane -t s:0.0 3\n")?;
    assert_success(&harness.run(&[
        "source-file",
        no_direction_source.to_str().expect("utf-8 path"),
    ])?);
    let sourced_no_direction = harness.run(&[
        "list-panes",
        "-t",
        "s",
        "-F",
        "#{pane_index}:#{pane_width}x#{pane_height}",
    ])?;
    assert_eq!(sourced_no_direction.status.code(), Some(0));
    assert_eq!(stdout(&sourced_no_direction), before_no_direction);

    let zero = harness.run(&["resize-pane", "-L", "-t", "s:0.0", "0"])?;
    assert_eq!(zero.status.code(), Some(1));
    assert!(stderr(&zero).contains("adjustment too small"));

    let zero_source = harness.tmpdir().join("resize-zero.conf");
    fs::write(&zero_source, "resize-pane -L -t s:0.0 0\n")?;
    let sourced_zero = harness.run(&["source-file", zero_source.to_str().expect("utf-8 path")])?;
    assert_eq!(sourced_zero.status.code(), Some(1));
    assert!(
        format!("{}{}", stdout(&sourced_zero), stderr(&sourced_zero))
            .contains("adjustment too small")
    );

    let huge = "999999999999999999999999999999";
    let overflow = harness.run(&["resize-pane", "-R", "-t", "s:0.0", huge])?;
    assert_eq!(overflow.status.code(), Some(1));
    assert!(stderr(&overflow).contains("adjustment too large"));

    let overflow_source = harness.tmpdir().join("resize-overflow.conf");
    fs::write(
        &overflow_source,
        format!("resize-pane -R -t s:0.0 {huge}\n"),
    )?;
    let sourced_overflow =
        harness.run(&["source-file", overflow_source.to_str().expect("utf-8 path")])?;
    assert_eq!(sourced_overflow.status.code(), Some(1));
    assert!(
        format!("{}{}", stdout(&sourced_overflow), stderr(&sourced_overflow))
            .contains("adjustment too large")
    );

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn pane_percentage_size_flags_match_tmux_compat() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("split-window-legacy-percentage")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-x", "100", "-y", "24", "-s", "s"])?);
    assert_success(&harness.run(&["split-window", "-p", "35", "-t", "s:0"])?);
    let percent_panes = harness.run(&[
        "list-panes",
        "-t",
        "s",
        "-F",
        "#{pane_index}:#{pane_width}x#{pane_height}",
    ])?;
    assert_eq!(percent_panes.status.code(), Some(0));
    assert_eq!(stdout(&percent_panes), "0:100x15\n1:100x8\n");
    assert!(stderr(&percent_panes).is_empty());

    assert_success(&harness.run(&["new-session", "-d", "-x", "100", "-y", "24", "-s", "l"])?);
    assert_success(&harness.run(&["split-window", "-l", "5", "-p", "50", "-t", "l:0"])?);
    let panes = harness.run(&[
        "list-panes",
        "-t",
        "l",
        "-F",
        "#{pane_index}:#{pane_width}x#{pane_height}",
    ])?;
    assert_eq!(panes.status.code(), Some(0));
    assert_eq!(stdout(&panes), "0:100x18\n1:100x5\n");
    assert!(stderr(&panes).is_empty());

    assert_success(&harness.run(&["new-session", "-d", "-x", "100", "-y", "24", "-s", "r"])?);
    assert_success(&harness.run(&["split-window", "-pabc", "-l", "5", "-t", "r:0"])?);
    let direct_opaque = harness.run(&[
        "list-panes",
        "-t",
        "r",
        "-F",
        "#{pane_index}:#{pane_width}x#{pane_height}",
    ])?;
    assert_eq!(direct_opaque.status.code(), Some(0));
    assert_eq!(stdout(&direct_opaque), "0:100x18\n1:100x5\n");
    assert!(stderr(&direct_opaque).is_empty());

    assert_success(&harness.run(&["new-session", "-d", "-x", "100", "-y", "24", "-s", "mv"])?);
    assert_success(&harness.run(&["split-window", "-h", "-t", "mv:0.0"])?);
    assert_success(&harness.run(&["move-pane", "-p", "35", "-s", "mv:0.1", "-t", "mv:0.0"])?);
    let moved_percent = harness.run(&[
        "list-panes",
        "-t",
        "mv",
        "-F",
        "#{pane_index}:#{pane_width}x#{pane_height}",
    ])?;
    assert_eq!(moved_percent.status.code(), Some(0));
    assert_eq!(stdout(&moved_percent), "0:100x15\n1:100x8\n");
    assert!(stderr(&moved_percent).is_empty());

    assert_success(&harness.run(&["new-session", "-d", "-x", "100", "-y", "24", "-s", "j"])?);
    assert_success(&harness.run(&["split-window", "-h", "-t", "j:0.0"])?);
    let join_source = harness.tmpdir().join("join.conf");
    fs::write(
        &join_source,
        "select-pane -t j:0.1 -m\njoin-pane -p35 -t j:0.0\n",
    )?;
    assert_success(&harness.run(&["source-file", join_source.to_str().expect("utf-8 path")])?);
    let joined = harness.run(&[
        "list-panes",
        "-t",
        "j",
        "-F",
        "#{pane_index}:#{pane_width}x#{pane_height}",
    ])?;
    assert_eq!(joined.status.code(), Some(0));
    assert_eq!(stdout(&joined), "0:100x15\n1:100x8\n");
    assert!(stderr(&joined).is_empty());

    assert_success(&harness.run(&["new-session", "-d", "-x", "100", "-y", "24", "-s", "m"])?);
    assert_success(&harness.run(&["split-window", "-h", "-t", "m:0.0"])?);
    let move_source = harness.tmpdir().join("move.conf");
    fs::write(
        &move_source,
        "select-pane -t m:0.1 -m\nmove-pane -p35 -t m:0.0\n",
    )?;
    assert_success(&harness.run(&["source-file", move_source.to_str().expect("utf-8 path")])?);
    let moved = harness.run(&[
        "list-panes",
        "-t",
        "m",
        "-F",
        "#{pane_index}:#{pane_width}x#{pane_height}",
    ])?;
    assert_eq!(moved.status.code(), Some(0));
    assert_eq!(stdout(&moved), "0:100x15\n1:100x8\n");
    assert!(stderr(&moved).is_empty());

    assert_success(&harness.run(&["new-session", "-d", "-x", "100", "-y", "24", "-s", "jd"])?);
    assert_success(&harness.run(&["split-window", "-h", "-t", "jd:0.0"])?);
    let default_join_target_source = harness.tmpdir().join("join-default-target.conf");
    fs::write(
        &default_join_target_source,
        "select-pane -t jd:0.1 -m\nselect-pane -t jd:0.0\njoin-pane -p35\n",
    )?;
    assert_success(&harness.run(&[
        "source-file",
        default_join_target_source.to_str().expect("utf-8 path"),
    ])?);
    let default_joined = harness.run(&[
        "list-panes",
        "-t",
        "jd",
        "-F",
        "#{pane_index}:#{pane_width}x#{pane_height}",
    ])?;
    assert_eq!(default_joined.status.code(), Some(0));
    assert_eq!(stdout(&default_joined), "0:100x15\n1:100x8\n");
    assert!(stderr(&default_joined).is_empty());

    assert_success(&harness.run(&["new-session", "-d", "-x", "100", "-y", "24", "-s", "md"])?);
    assert_success(&harness.run(&["split-window", "-h", "-t", "md:0.0"])?);
    let default_move_target_source = harness.tmpdir().join("move-default-target.conf");
    fs::write(
        &default_move_target_source,
        "select-pane -t md:0.1 -m\nselect-pane -t md:0.0\nmove-pane -p35\n",
    )?;
    assert_success(&harness.run(&[
        "source-file",
        default_move_target_source.to_str().expect("utf-8 path"),
    ])?);
    let default_moved = harness.run(&[
        "list-panes",
        "-t",
        "md",
        "-F",
        "#{pane_index}:#{pane_width}x#{pane_height}",
    ])?;
    assert_eq!(default_moved.status.code(), Some(0));
    assert_eq!(stdout(&default_moved), "0:100x15\n1:100x8\n");
    assert!(stderr(&default_moved).is_empty());

    assert_success(&harness.run(&["new-session", "-d", "-x", "100", "-y", "24", "-s", "jx"])?);
    assert_success(&harness.run(&["split-window", "-h", "-t", "jx:0.0"])?);
    let explicit_join_source = harness.tmpdir().join("join-explicit-source.conf");
    fs::write(
        &explicit_join_source,
        "join-pane -l 5 -s jx:0.1 -t jx:0.0\n",
    )?;
    assert_success(&harness.run(&[
        "source-file",
        explicit_join_source.to_str().expect("utf-8 path"),
    ])?);
    let explicit_joined = harness.run(&[
        "list-panes",
        "-t",
        "jx",
        "-F",
        "#{pane_index}:#{pane_width}x#{pane_height}",
    ])?;
    assert_eq!(explicit_joined.status.code(), Some(0));
    assert_eq!(stdout(&explicit_joined), "0:100x18\n1:100x5\n");
    assert!(stderr(&explicit_joined).is_empty());

    assert_success(&harness.run(&["new-session", "-d", "-x", "100", "-y", "24", "-s", "mx"])?);
    assert_success(&harness.run(&["split-window", "-h", "-t", "mx:0.0"])?);
    let explicit_move_source = harness.tmpdir().join("move-explicit-source.conf");
    fs::write(
        &explicit_move_source,
        "move-pane -l 5 -s mx:0.1 -t mx:0.0\n",
    )?;
    assert_success(&harness.run(&[
        "source-file",
        explicit_move_source.to_str().expect("utf-8 path"),
    ])?);
    let explicit_moved = harness.run(&[
        "list-panes",
        "-t",
        "mx",
        "-F",
        "#{pane_index}:#{pane_width}x#{pane_height}",
    ])?;
    assert_eq!(explicit_moved.status.code(), Some(0));
    assert_eq!(stdout(&explicit_moved), "0:100x18\n1:100x5\n");
    assert!(stderr(&explicit_moved).is_empty());

    assert_success(&harness.run(&["new-session", "-d", "-x", "100", "-y", "24", "-s", "sx"])?);
    assert_success(&harness.run(&["split-window", "-h", "-t", "sx:0.0"])?);
    let explicit_swap_source = harness.tmpdir().join("swap-explicit-source.conf");
    fs::write(&explicit_swap_source, "swap-pane -s sx:0.1 -t sx:0.0\n")?;
    assert_success(&harness.run(&[
        "source-file",
        explicit_swap_source.to_str().expect("utf-8 path"),
    ])?);
    let explicit_swapped = harness.run(&[
        "list-panes",
        "-t",
        "sx",
        "-F",
        "#{pane_index}:#{pane_width}x#{pane_height}",
    ])?;
    assert_eq!(explicit_swapped.status.code(), Some(0));
    assert_eq!(stdout(&explicit_swapped), "0:50x24\n1:49x24\n");
    assert!(stderr(&explicit_swapped).is_empty());

    assert_success(&harness.run(&["new-session", "-d", "-x", "100", "-y", "24", "-s", "q"])?);
    let source = harness.tmpdir().join("split.conf");
    fs::write(&source, "split-window -pabc -l 5 -t q:0\n")?;
    assert_success(&harness.run(&["source-file", source.to_str().expect("utf-8 path")])?);
    let sourced = harness.run(&[
        "list-panes",
        "-t",
        "q",
        "-F",
        "#{pane_index}:#{pane_width}x#{pane_height}",
    ])?;
    assert_eq!(sourced.status.code(), Some(0));
    assert_eq!(stdout(&sourced), "0:100x18\n1:100x5\n");
    assert!(stderr(&sourced).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn source_file_explicit_pane_sources_do_not_require_marked_pane() -> Result<(), Box<dyn Error>> {
    for (label, command) in [
        ("join", "join-pane -l 5 -s s:0.1 -t s:0.0\n"),
        ("move", "move-pane -l 5 -s s:0.1 -t s:0.0\n"),
        ("swap", "swap-pane -s s:0.1 -t s:0.0\n"),
        ("join-legacy-p", "join-pane -pabc -l 5 -s s:0.1 -t s:0.0\n"),
        ("move-legacy-p", "move-pane -pabc -l 5 -s s:0.1 -t s:0.0\n"),
    ] {
        let harness = CliHarness::new(&format!("source-file-explicit-pane-source-{label}"))?;
        let mut daemon = harness.start_hidden_daemon()?;

        assert_success(&harness.run(&["new-session", "-d", "-x", "100", "-y", "24", "-s", "s"])?);
        assert_success(&harness.run(&["split-window", "-h", "-t", "s:0.0"])?);
        let source = harness.tmpdir().join(format!("{label}.conf"));
        fs::write(&source, command)?;

        let output = harness.run(&["source-file", source.to_str().expect("utf-8 path")])?;
        assert_eq!(
            output.status.code(),
            Some(0),
            "{label} should source successfully; stdout={:?}, stderr={:?}",
            stdout(&output),
            stderr(&output)
        );
        assert!(
            !stdout(&output).contains("requires a marked pane"),
            "{label} must not fall back to marked pane: {:?}",
            stdout(&output)
        );
        assert!(
            stderr(&output).is_empty(),
            "{label} stderr: {:?}",
            stderr(&output)
        );

        terminate_child(daemon.child_mut())?;
    }

    Ok(())
}

#[test]
fn source_file_omitted_pane_sources_fall_back_to_current_pane() -> Result<(), Box<dyn Error>> {
    for (label, command) in [
        ("join", "join-pane -t s:0.0\n"),
        ("move", "move-pane -t s:0.0\n"),
        ("swap", "swap-pane -t s:0.0\n"),
    ] {
        let harness = CliHarness::new(&format!("source-file-omitted-pane-source-{label}"))?;
        let _cleanup = harness.auto_start_cleanup()?;
        let run = |args: &[&str]| {
            harness.run_with(args, |process| {
                process.env(BINARY_OVERRIDE_ENV, harness.launcher_path());
            })
        };

        assert_success(&run(&[
            "new-session",
            "-d",
            "-x",
            "100",
            "-y",
            "24",
            "-s",
            "s",
        ])?);
        assert_success(&run(&["split-window", "-t", "s:0.0"])?);
        assert_success(&run(&["select-pane", "-t", "s:0.1"])?);

        let source = harness.tmpdir().join(format!("{label}.conf"));
        fs::write(&source, command)?;
        let output = run(&["source-file", source.to_str().expect("utf-8 path")])?;
        assert_eq!(
            output.status.code(),
            Some(0),
            "{label} should source successfully; stdout={:?}, stderr={:?}",
            stdout(&output),
            stderr(&output)
        );
        assert!(
            stderr(&output).is_empty(),
            "{label} stderr: {:?}",
            stderr(&output)
        );

        let panes = run(&[
            "list-panes",
            "-t",
            "s",
            "-F",
            "#{pane_index}:#{pane_width}x#{pane_height}:#{pane_active}",
        ])?;
        assert_eq!(panes.status.code(), Some(0));
        assert!(stderr(&panes).is_empty());
        if label != "swap" {
            assert!(
                !stdout(&panes).contains("0:100x12:0\n1:100x11:1"),
                "{label} should mutate the layout using current pane as source: {:?}",
                stdout(&panes)
            );
        }
    }

    Ok(())
}

#[test]
fn same_window_join_and_move_pane_size_matches_tmux_target_side() -> Result<(), Box<dyn Error>> {
    for command in ["join-pane", "move-pane"] {
        let harness = CliHarness::new(&format!("same-window-{command}-size-target-side"))?;
        let _cleanup = harness.auto_start_cleanup()?;

        let run = |args: &[&str]| {
            harness.run_with(args, |process| {
                process.env(BINARY_OVERRIDE_ENV, harness.launcher_path());
            })
        };

        assert_success(&run(&[
            "new-session",
            "-d",
            "-x",
            "100",
            "-y",
            "24",
            "-s",
            "s",
        ])?);
        assert_success(&run(&["split-window", "-t", "s:0"])?);
        assert_success(&run(&[command, "-l", "5", "-s", "s:0.1", "-t", "s:0.0"])?);

        let panes = run(&[
            "list-panes",
            "-t",
            "s:0",
            "-F",
            "#{pane_index}:#{pane_width}x#{pane_height}:#{pane_active}",
        ])?;
        assert_eq!(panes.status.code(), Some(0), "{command}");
        assert_eq!(stdout(&panes), "0:100x6:0\n1:100x17:1\n", "{command}");
        assert!(stderr(&panes).is_empty(), "{command}");

        assert_success(&run(&[
            "new-session",
            "-d",
            "-x",
            "100",
            "-y",
            "24",
            "-s",
            "b",
        ])?);
        assert_success(&run(&["split-window", "-t", "b:0"])?);
        assert_success(&run(&[
            command, "-b", "-l", "5", "-s", "b:0.1", "-t", "b:0.0",
        ])?);

        let before_panes = run(&[
            "list-panes",
            "-t",
            "b:0",
            "-F",
            "#{pane_index}:#{pane_width}x#{pane_height}:#{pane_active}",
        ])?;
        assert_eq!(before_panes.status.code(), Some(0), "{command} -b");
        assert_eq!(
            stdout(&before_panes),
            "0:100x5:1\n1:100x18:0\n",
            "{command} -b"
        );
        assert!(stderr(&before_panes).is_empty(), "{command} -b");
    }

    Ok(())
}

#[test]
fn cross_window_join_and_move_pane_size_preserves_non_target_neighbor() -> Result<(), Box<dyn Error>>
{
    for command in ["join-pane", "move-pane"] {
        let harness = CliHarness::new(&format!("cross-window-{command}-non-target-neighbor-size"))?;
        let _cleanup = harness.auto_start_cleanup()?;

        let run = |args: &[&str]| {
            harness.run_with(args, |process| {
                process.env(BINARY_OVERRIDE_ENV, harness.launcher_path());
            })
        };

        assert_success(&run(&[
            "new-session",
            "-d",
            "-x",
            "200",
            "-y",
            "50",
            "-s",
            "s",
            "-n",
            "target",
        ])?);
        assert_success(&run(&["split-window", "-h", "-t", "s:0.0"])?);
        assert_success(&run(&["new-window", "-d", "-t", "s:1", "-n", "source"])?);

        assert_success(&run(&[
            command, "-h", "-l", "30", "-s", "s:1.0", "-t", "s:0.0",
        ])?);

        let panes = run(&[
            "list-panes",
            "-t",
            "s:0",
            "-F",
            "#{pane_index}:#{pane_width}x#{pane_height}:#{pane_active}",
        ])?;
        assert_eq!(panes.status.code(), Some(0), "{command}");
        assert_eq!(
            stdout(&panes),
            "0:69x50:0\n1:30x50:1\n2:99x50:0\n",
            "{command}"
        );
        assert!(stderr(&panes).is_empty(), "{command}");
    }

    Ok(())
}

#[test]
fn detached_swap_pane_selects_source_at_target_position_like_tmux() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("detached-swap-pane-active-target")?;
    let _cleanup = harness.auto_start_cleanup()?;
    let run = |args: &[&str]| {
        harness.run_with(args, |process| {
            process.env(BINARY_OVERRIDE_ENV, harness.launcher_path());
        })
    };

    assert_success(&run(&[
        "new-session",
        "-d",
        "-x",
        "120",
        "-y",
        "40",
        "-s",
        "s",
    ])?);
    assert_success(&run(&["split-window", "-d", "-h", "-t", "s:0.0"])?);
    assert_success(&run(&["select-pane", "-t", "s:0.0"])?);
    assert_success(&run(&["swap-pane", "-d", "-s", "s:0.1", "-t", "s:0.0"])?);

    let panes = run(&[
        "list-panes",
        "-t",
        "s",
        "-F",
        "#{pane_index}:#{pane_active}:#{pane_id}",
    ])?;
    assert_eq!(panes.status.code(), Some(0));
    let pane_output = stdout(&panes);
    let lines = pane_output.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 2);
    assert!(
        lines[0].starts_with("0:1:%"),
        "target position should stay active after detached swap: {:?}",
        pane_output
    );
    assert!(
        lines[1].starts_with("1:0:%"),
        "source position should become inactive after detached swap: {:?}",
        pane_output
    );
    assert!(stderr(&panes).is_empty());

    Ok(())
}

#[test]
fn detached_swap_pane_preserves_unrelated_active_pane_like_tmux() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("detached-swap-pane-active-unrelated")?;
    let _cleanup = harness.auto_start_cleanup()?;
    let run = |args: &[&str]| {
        harness.run_with(args, |process| {
            process.env(BINARY_OVERRIDE_ENV, harness.launcher_path());
        })
    };

    assert_success(&run(&[
        "new-session",
        "-d",
        "-x",
        "100",
        "-y",
        "24",
        "-s",
        "s",
    ])?);
    assert_success(&run(&["split-window", "-h", "-t", "s:0.0"])?);
    assert_success(&run(&["split-window", "-h", "-t", "s:0.1"])?);
    assert_success(&run(&["select-pane", "-t", "s:0.2"])?);
    assert_success(&run(&["swap-pane", "-d", "-s", "s:0.1", "-t", "s:0.0"])?);

    let panes = run(&[
        "list-panes",
        "-t",
        "s",
        "-F",
        "#{pane_index}:#{pane_active}",
    ])?;
    assert_eq!(panes.status.code(), Some(0));
    assert_eq!(stdout(&panes), "0:0\n1:0\n2:1\n");
    assert!(stderr(&panes).is_empty());

    Ok(())
}

#[test]
fn pane_transfer_clears_mark_when_marked_source_is_consumed() -> Result<(), Box<dyn Error>> {
    for (label, command, format) in [
        (
            "join",
            "join-pane -pabc -l 5 -s s:0.1 -t s:0.0",
            "#{pane_index}:#{pane_active}:#{pane_marked}:#{pane_id}",
        ),
        (
            "move",
            "move-pane -pabc -l 5 -s s:0.1 -t s:0.0",
            "#{pane_index}:#{pane_active}:#{pane_marked}:#{pane_id}",
        ),
        (
            "break",
            "break-pane -d -s s:0.1",
            "#{session_name}:#{window_index}.#{pane_index}:#{pane_marked}:#{pane_id}",
        ),
    ] {
        let harness = CliHarness::new(&format!("pane-transfer-clears-mark-{label}"))?;
        let mut daemon = harness.start_hidden_daemon()?;

        assert_success(&harness.run(&["new-session", "-d", "-x", "120", "-y", "40", "-s", "s"])?);
        assert_success(&harness.run(&["split-window", "-d", "-h", "-t", "s:0.0"])?);
        assert_success(&harness.run(&["select-pane", "-t", "s:0.1", "-m"])?);
        let argv = command.split_whitespace().collect::<Vec<_>>();
        assert_success(&harness.run(&argv)?);

        let panes = harness.run(&["list-panes", "-a", "-F", format])?;
        assert_eq!(panes.status.code(), Some(0), "{label}");
        assert!(
            stdout(&panes).lines().all(|line| !line.contains(":1:%")),
            "{label} should clear a mark on the consumed source pane: {:?}",
            stdout(&panes)
        );
        assert!(stderr(&panes).is_empty(), "{label}");

        terminate_child(daemon.child_mut())?;
    }

    Ok(())
}

#[test]
fn source_file_join_and_move_direction_flags_follow_tmux_priority() -> Result<(), Box<dyn Error>> {
    for (label, command) in [
        ("join-hv", "join-pane -h -v -s s:0.1 -t s:1.0\n"),
        ("join-vh", "join-pane -v -h -s s:0.1 -t s:1.0\n"),
        ("join-cluster-hv", "join-pane -hv -s s:0.1 -t s:1.0\n"),
        ("join-cluster-vh", "join-pane -vh -s s:0.1 -t s:1.0\n"),
        ("move-hv", "move-pane -h -v -s s:0.1 -t s:1.0\n"),
        ("move-vh", "move-pane -v -h -s s:0.1 -t s:1.0\n"),
        ("move-cluster-hv", "move-pane -hv -s s:0.1 -t s:1.0\n"),
        ("move-cluster-vh", "move-pane -vh -s s:0.1 -t s:1.0\n"),
    ] {
        let harness = CliHarness::new(&format!("source-file-pane-direction-priority-{label}"))?;
        let mut daemon = harness.start_hidden_daemon()?;

        assert_success(&harness.run(&["new-session", "-d", "-x", "80", "-y", "24", "-s", "s"])?);
        assert_success(&harness.run(&["split-window", "-h", "-t", "s:0.0"])?);
        assert_success(&harness.run(&["new-window", "-d", "-t", "s:"])?);

        let source = harness.tmpdir().join(format!("{label}.conf"));
        fs::write(&source, command)?;
        let output = harness.run(&["source-file", source.to_str().expect("utf-8 path")])?;
        assert_eq!(
            output.status.code(),
            Some(0),
            "{label} should source successfully; stdout={:?}, stderr={:?}",
            stdout(&output),
            stderr(&output)
        );
        assert!(stderr(&output).is_empty(), "{label} stderr");

        let panes = harness.run(&[
            "list-panes",
            "-t",
            "s:1",
            "-F",
            "#{pane_index}:#{pane_width}x#{pane_height}",
        ])?;
        assert_eq!(panes.status.code(), Some(0), "{label} panes status");
        assert_eq!(
            stdout(&panes),
            "0:40x24\n1:39x24\n",
            "{label} should use horizontal split geometry"
        );

        terminate_child(daemon.child_mut())?;
    }

    Ok(())
}

#[test]
fn source_file_resize_pane_accepts_absolute_percentages() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("source-file-resize-pane-percent")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-x", "80", "-y", "24", "-s", "s"])?);
    assert_success(&harness.run(&["split-window", "-h", "-t", "s:0.0"])?);

    let source = harness.tmpdir().join("resize-percent.conf");
    fs::write(&source, "resize-pane -x 25% -y 50% -t s:0.0\n")?;
    let output = harness.run(&["source-file", source.to_str().expect("utf-8 path")])?;
    assert_eq!(
        output.status.code(),
        Some(0),
        "source-file should accept resize-pane percentages; stdout={:?}, stderr={:?}",
        stdout(&output),
        stderr(&output)
    );
    assert!(stderr(&output).is_empty());

    let panes = harness.run(&[
        "list-panes",
        "-t",
        "s:0",
        "-F",
        "#{pane_index}:#{pane_width}x#{pane_height}",
    ])?;
    assert_eq!(panes.status.code(), Some(0));
    assert_eq!(stdout(&panes), "0:20x24\n1:59x24\n");

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn source_file_resize_pane_clamps_large_absolute_sizes() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("source-file-resize-pane-large-absolute")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-x", "80", "-y", "24", "-s", "s"])?);
    assert_success(&harness.run(&["split-window", "-h", "-t", "s:0.0"])?);

    let source = harness.tmpdir().join("resize-large.conf");
    fs::write(
        &source,
        "resize-pane -x 70000 -t s:0.0\nresize-pane -y 70000 -t s:0.0\n",
    )?;
    let output = harness.run(&["source-file", source.to_str().expect("utf-8 path")])?;
    assert_eq!(
        output.status.code(),
        Some(0),
        "source-file should clamp large resize-pane sizes; stdout={:?}, stderr={:?}",
        stdout(&output),
        stderr(&output)
    );
    assert!(stderr(&output).is_empty());

    let panes = harness.run(&[
        "list-panes",
        "-t",
        "s:0",
        "-F",
        "#{pane_index}:#{pane_width}x#{pane_height}",
    ])?;
    assert_eq!(panes.status.code(), Some(0));
    assert_eq!(stdout(&panes), "0:78x24\n1:1x24\n");

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn source_file_compact_pane_flags_match_cli_surface() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("source-file-compact-pane-flags")?;
    let mut daemon = harness.start_hidden_daemon()?;

    for (session, command) in [
        ("si", "split-window -Id -t si:0.0\n"),
        ("sv", "split-window -vf -t sv:0.0\n"),
        ("sb", "split-window -bd -t sb:0.0\n"),
    ] {
        assert_success(&harness.run(&[
            "new-session",
            "-d",
            "-x",
            "100",
            "-y",
            "24",
            "-s",
            session,
        ])?);
        let source = harness.tmpdir().join(format!("{session}.conf"));
        fs::write(&source, command)?;
        assert_success(&harness.run(&["source-file", source.to_str().expect("utf-8 path")])?);
        let panes = harness.run(&[
            "list-panes",
            "-t",
            session,
            "-F",
            "#{pane_index}:#{pane_width}x#{pane_height}",
        ])?;
        assert_eq!(panes.status.code(), Some(0), "session={session}");
        assert_eq!(
            stdout(&panes),
            "0:100x12\n1:100x11\n",
            "session={session}, command={command:?}"
        );
        assert!(stderr(&panes).is_empty(), "session={session}");
    }

    for (session, command) in [
        ("jds", "join-pane -ds jds:0.1 -t jds:0.0\n"),
        ("mds", "move-pane -ds mds:0.1 -t mds:0.0\n"),
    ] {
        assert_success(&harness.run(&[
            "new-session",
            "-d",
            "-x",
            "100",
            "-y",
            "24",
            "-s",
            session,
        ])?);
        assert_success(&harness.run(&["split-window", "-h", "-t", &format!("{session}:0.0")])?);
        let source = harness.tmpdir().join(format!("{session}.conf"));
        fs::write(&source, command)?;
        assert_success(&harness.run(&["source-file", source.to_str().expect("utf-8 path")])?);
        let panes = harness.run(&[
            "list-panes",
            "-t",
            session,
            "-F",
            "#{pane_index}:#{pane_width}x#{pane_height}",
        ])?;
        assert_eq!(panes.status.code(), Some(0), "session={session}");
        assert_eq!(
            stdout(&panes),
            "0:100x12\n1:100x11\n",
            "session={session}, command={command:?}"
        );
        assert!(stderr(&panes).is_empty(), "session={session}");
    }

    assert_success(&harness.run(&["new-session", "-d", "-x", "100", "-y", "24", "-s", "swds"])?);
    assert_success(&harness.run(&["split-window", "-h", "-t", "swds:0.0"])?);
    let before_swap =
        harness.run(&["list-panes", "-t", "swds", "-F", "#{pane_index}:#{pane_id}"])?;
    assert_eq!(before_swap.status.code(), Some(0));
    let before_ids = stdout(&before_swap)
        .lines()
        .map(|line| line.split_once(':').expect("pane id line").1.to_owned())
        .collect::<Vec<_>>();
    assert_eq!(before_ids.len(), 2);

    let source = harness.tmpdir().join("swap-ds.conf");
    fs::write(&source, "swap-pane -ds swds:0.1 -t swds:0.0\n")?;
    assert_success(&harness.run(&["source-file", source.to_str().expect("utf-8 path")])?);
    let after_swap =
        harness.run(&["list-panes", "-t", "swds", "-F", "#{pane_index}:#{pane_id}"])?;
    assert_eq!(after_swap.status.code(), Some(0));
    assert_eq!(
        stdout(&after_swap),
        format!("0:{}\n1:{}\n", before_ids[1], before_ids[0])
    );
    assert!(stderr(&after_swap).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn resize_pane_absolute_extremes_are_validated_or_clamped() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("resize-pane-absolute-extremes")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "s", "-x", "80", "-y", "24"])?);
    assert_success(&harness.run(&["resize-pane", "-t", "s:0.0", "-x", "0"])?);
    assert_success(&harness.run(&["resize-pane", "-t", "s:0.0", "-y", "0"])?);

    for (args, expected) in [
        (
            &["resize-pane", "-t", "s:0.0", "-x", "-1"][..],
            "width too small\n",
        ),
        (
            &["resize-pane", "-t", "s:0.0", "-y", "-1"][..],
            "height too small\n",
        ),
    ] {
        let output = harness.run(args)?;
        assert_eq!(output.status.code(), Some(1), "args={args:?}");
        assert!(stdout(&output).is_empty(), "args={args:?}");
        assert_eq!(stderr(&output), expected, "args={args:?}");
    }
    assert_success(&harness.run(&["resize-pane", "-t", "s:0.0", "-x", "99999"])?);

    let panes = harness.run(&[
        "list-panes",
        "-t",
        "s",
        "-F",
        "#{pane_width}x#{pane_height}",
    ])?;
    assert_eq!(panes.status.code(), Some(0));
    assert_eq!(stdout(&panes), "80x24\n");
    assert!(stderr(&panes).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn resize_window_expand_and_shrink_use_linked_session_size() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("resize-window-linked-session-size")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-x", "80", "-y", "20", "-s", "s1"])?);
    assert_success(&harness.run(&[
        "new-session",
        "-d",
        "-x",
        "100",
        "-y",
        "30",
        "-t",
        "s1",
        "-s",
        "s2",
    ])?);
    assert_success(&harness.run(&["resize-window", "-t", "s1:0", "-x", "70", "-y", "15"])?);
    assert_success(&harness.run(&["resize-window", "-A", "-t", "s1:0"])?);

    let expanded = linked_window_sizes(&harness)?;
    assert_eq!(expanded, "s1:80:20\ns2:80:20\n");

    assert_success(&harness.run(&["resize-window", "-t", "s1:0", "-x", "100", "-y", "30"])?);
    assert_success(&harness.run(&["resize-window", "-a", "-t", "s1:0"])?);

    let shrunk = linked_window_sizes(&harness)?;
    assert_eq!(shrunk, "s1:80:20\ns2:80:20\n");

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn scripted_move_window_preserves_after_placement() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("scripted-move-window-after")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "s", "-n", "zero"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "s", "-n", "one"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "s", "-n", "two"])?);
    assert_success(&harness.run(&["run-shell", "-C", "move-window -a -s s:0 -t s:2"])?);

    let windows = harness.run(&[
        "list-windows",
        "-t",
        "s",
        "-F",
        "#{window_index}:#{window_name}",
    ])?;
    assert_eq!(windows.status.code(), Some(0));
    assert_eq!(stdout(&windows), "1:one\n2:two\n3:zero\n");
    assert!(stderr(&windows).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn select_pane_keep_zoom_switches_active_pane_without_unzooming() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("select-pane-keep-zoom")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-x", "80", "-y", "24", "-s", "s"])?);
    assert_success(&harness.run(&["split-window", "-h", "-t", "s:0.0"])?);
    assert_success(&harness.run(&["resize-pane", "-Z", "-t", "s:0.0"])?);
    assert_success(&harness.run(&["select-pane", "-Z", "-t", "s:0.1"])?);

    let panes = harness.run(&[
        "list-panes",
        "-t",
        "s",
        "-F",
        "#{pane_index}:#{pane_active}:#{window_zoomed_flag}:#{pane_width}",
    ])?;
    assert_eq!(panes.status.code(), Some(0));
    assert_eq!(stdout(&panes), "0:0:1:40\n1:1:1:80\n");
    assert!(stderr(&panes).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn resize_pane_trim_flag_deletes_history_below_cursor() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("resize-pane-trim-history")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&[
        "new-session",
        "-d",
        "-x",
        "10",
        "-y",
        "5",
        "-s",
        "s",
        "sh",
        "-c",
        "printf '01\\n02\\n03\\n04\\n05\\n06\\n07\\n08\\n09\\n10\\033[3;1H'; sleep 30",
    ])?);
    std::thread::sleep(SETTLE);

    let before = capture_history_lines(&harness)?;
    assert_success(&harness.run(&["resize-pane", "-T", "-t", "s:0.0"])?);
    let after = capture_history_lines(&harness)?;

    assert_eq!(before, "01\n02\n03\n04\n05\n06\n07\n08\n09\n10\n");
    assert_ne!(after, before);
    assert_eq!(after, "01\n02\n03\n04\n05\n06\n07\n08\n");

    terminate_child(daemon.child_mut())?;
    Ok(())
}

fn linked_window_sizes(harness: &CliHarness) -> Result<String, Box<dyn Error>> {
    let output = harness.run(&[
        "list-windows",
        "-a",
        "-F",
        "#{session_name}:#{window_width}:#{window_height}",
    ])?;
    assert_eq!(output.status.code(), Some(0));
    assert!(stderr(&output).is_empty());
    let mut lines = stdout(&output)
        .lines()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    lines.sort();
    Ok(format!("{}\n", lines.join("\n")))
}

fn capture_history_lines(harness: &CliHarness) -> Result<String, Box<dyn Error>> {
    let output = harness.run(&["capture-pane", "-p", "-S", "-100", "-t", "s:0.0"])?;
    assert_eq!(output.status.code(), Some(0));
    assert!(stderr(&output).is_empty());
    Ok(stdout(&output))
}

fn wait_for_capture_contains(
    harness: &CliHarness,
    target: &str,
    needle: &str,
) -> Result<String, Box<dyn Error>> {
    wait_for_stdout_contains(harness, &["capture-pane", "-p", "-t", target], needle)
}

fn wait_for_stdout_contains(
    harness: &CliHarness,
    args: &[&str],
    needle: &str,
) -> Result<String, Box<dyn Error>> {
    wait_for_stdout_matching(harness, args, |stdout| stdout.contains(needle))
}

fn wait_for_stdout_matching(
    harness: &CliHarness,
    args: &[&str],
    mut predicate: impl FnMut(&str) -> bool,
) -> Result<String, Box<dyn Error>> {
    let deadline = Instant::now() + SETTLE * 12;

    loop {
        let output = harness.run(args)?;
        assert_eq!(output.status.code(), Some(0));
        assert!(stderr(&output).is_empty());
        let stdout = stdout(&output);
        if predicate(&stdout) || Instant::now() >= deadline {
            return Ok(stdout);
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}
