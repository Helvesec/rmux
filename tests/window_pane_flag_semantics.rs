#![cfg(unix)]

mod common;

use std::error::Error;
use std::ffi::OsString;
use std::fs::{self, File};
use std::os::unix::ffi::OsStringExt;
use std::process::Stdio;
use std::time::{Duration, Instant};

use common::{assert_success, stderr, stdout, terminate_child, CliHarness};

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
    std::thread::sleep(SETTLE);

    let panes = harness.run(&["list-panes", "-t", "s", "-F", "#{pane_index}:#{pane_id}"])?;
    assert_eq!(panes.status.code(), Some(0));
    assert!(stdout(&panes).contains("1:%"));
    assert!(stderr(&panes).is_empty());

    let capture = harness.run(&["capture-pane", "-p", "-t", "s:0.1"])?;
    assert_eq!(capture.status.code(), Some(0));
    let payload_count = stdout(&capture).matches("hello").count();
    assert_eq!(
        payload_count,
        1,
        "capture should contain exactly one stdin payload: {:?}",
        stdout(&capture)
    );
    assert!(
        stdout(&capture).contains("hello\nworld"),
        "LF stdin should render as separate terminal lines without indentation: {:?}",
        stdout(&capture)
    );
    assert!(stderr(&capture).is_empty());

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

    let dead_metadata = harness.run(&[
        "list-panes",
        "-t",
        "s",
        "-F",
        "#{pane_index}:dead=#{pane_dead}:status=#{pane_dead_status}:time=#{pane_dead_time}:cmd=#{pane_current_command}",
    ])?;
    assert_eq!(dead_metadata.status.code(), Some(0));
    let dead_metadata_stdout = stdout(&dead_metadata);
    assert!(
        ["bash", "zsh", "sh"]
            .iter()
            .any(|shell| dead_metadata_stdout
                .contains(&format!("1:dead=1:status=:time=:cmd={shell}"))),
        "split-window -I synthetic pane should not expose exit status or time: {:?}",
        dead_metadata_stdout
    );
    assert!(stderr(&dead_metadata).is_empty());

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
    let deadline = Instant::now() + SETTLE * 12;

    loop {
        let output = harness.run(&["capture-pane", "-p", "-t", target])?;
        assert_eq!(output.status.code(), Some(0));
        assert!(stderr(&output).is_empty());
        let stdout = stdout(&output);
        if stdout.contains(needle) || Instant::now() >= deadline {
            return Ok(stdout);
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}
