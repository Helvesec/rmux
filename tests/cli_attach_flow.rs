#![cfg(unix)]

mod common;

use std::error::Error;
use std::time::Duration;

use common::{
    assert_success, drain_attach_output, drain_attach_output_bytes, read_until_contains,
    read_until_contains_all, terminate_child, AttachedSession, CliHarness,
};
use rmux_core::{input::InputParser, Screen};
use rmux_proto::TerminalSize as ScreenTerminalSize;
use rmux_pty::TerminalSize;

const IO_TIMEOUT: Duration = Duration::from_secs(5);

type PaneWidthRow = (usize, usize, bool);
type TestResult<T> = Result<T, Box<dyn Error>>;

fn list_pane_widths(harness: &CliHarness, target: &str) -> TestResult<Vec<PaneWidthRow>> {
    let output = harness.run(&[
        "list-panes",
        "-t",
        target,
        "-F",
        "#{pane_index}:#{pane_width}:#{pane_active}",
    ])?;
    common::stdout(&output)
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            let mut parts = line.split(':');
            let pane_index = parts
                .next()
                .ok_or_else(|| format!("missing pane index in {line:?}"))?
                .parse::<usize>()?;
            let pane_width = parts
                .next()
                .ok_or_else(|| format!("missing pane width in {line:?}"))?
                .parse::<usize>()?;
            let pane_active = parts
                .next()
                .ok_or_else(|| format!("missing pane_active in {line:?}"))?
                == "1";
            if parts.next().is_some() {
                return Err(format!("unexpected pane format {line:?}").into());
            }
            Ok((pane_index, pane_width, pane_active))
        })
        .collect()
}

fn list_pane_geometry(harness: &CliHarness, target: &str) -> Result<Vec<String>, Box<dyn Error>> {
    let output = harness.run(&[
        "list-panes",
        "-t",
        target,
        "-F",
        "#{pane_index}:#{pane_width}:#{pane_height}:#{pane_left}:#{pane_top}:#{pane_active}",
    ])?;
    Ok(common::stdout(&output)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect())
}

fn window_layout(harness: &CliHarness, target: &str) -> Result<String, Box<dyn Error>> {
    let output = harness.run(&["list-windows", "-t", target, "-F", "#{window_layout}"])?;
    Ok(common::stdout(&output).trim().to_owned())
}

fn send_shell_marker(
    harness: &CliHarness,
    target: &str,
    marker: &str,
) -> Result<(), Box<dyn Error>> {
    assert_success(&harness.run(&["send-keys", "-t", target, "C-c"])?);
    std::thread::sleep(Duration::from_millis(30));
    let escaped = marker
        .as_bytes()
        .iter()
        .map(|byte| format!("\\{byte:03o}"))
        .collect::<String>();
    let command = format!("printf '{escaped}\\012'");
    assert_success(&harness.run(&["send-keys", "-t", target, &command, "Enter"])?);
    Ok(())
}

fn capture_attach_transcript(
    screen: &mut Screen,
    parser: &mut InputParser,
    bytes: &[u8],
) -> Result<String, Box<dyn Error>> {
    parser.parse(bytes, screen);
    Ok(String::from_utf8(screen.capture_transcript(
        Default::default(),
        Default::default(),
    ))?)
}

fn transcript_without_status_line(transcript: &str) -> String {
    let mut lines = transcript.lines().collect::<Vec<_>>();
    let _ = lines.pop();
    if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n"))
    }
}

fn line_has_isolated_left_chevron(line: &str) -> bool {
    line.contains('│')
        && line
            .chars()
            .collect::<Vec<_>>()
            .windows(3)
            .any(|window| matches!(window, [' ', '<', ' ']))
}

fn apply_quiescent_attach_output(
    attach: &mut AttachedSession,
    screen: &mut Screen,
    parser: &mut InputParser,
    wait: Duration,
) -> Result<String, Box<dyn Error>> {
    std::thread::sleep(wait);
    let bytes = drain_attach_output_bytes(attach.master_mut())?;
    capture_attach_transcript(screen, parser, &bytes)
}

fn wait_for_attach_transcript_matching<F>(
    attach: &mut AttachedSession,
    screen: &mut Screen,
    parser: &mut InputParser,
    timeout: Duration,
    description: &str,
    mut matches: F,
) -> Result<String, Box<dyn Error>>
where
    F: FnMut(&str) -> bool,
{
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let bytes = drain_attach_output_bytes(attach.master_mut())?;
        let transcript = capture_attach_transcript(screen, parser, &bytes)?;
        let content = transcript_without_status_line(&transcript);
        if matches(&content) {
            return Ok(transcript);
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!("timed out waiting for {description}, got:\n{content}").into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn wait_for_pane_count(
    harness: &CliHarness,
    target: &str,
    expected: usize,
) -> Result<Vec<String>, Box<dyn Error>> {
    let deadline = std::time::Instant::now() + IO_TIMEOUT;
    loop {
        let pane_indexes =
            common::stdout(&harness.run(&["list-panes", "-t", target, "-F", "#{pane_index}"])?)
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>();
        if pane_indexes.len() == expected {
            return Ok(pane_indexes);
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!(
                "timed out waiting for {expected} panes in {target}, got {:?}",
                pane_indexes
            )
            .into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn wait_for_layout_change(
    harness: &CliHarness,
    target: &str,
    previous_layout: &str,
) -> Result<String, Box<dyn Error>> {
    let deadline = std::time::Instant::now() + IO_TIMEOUT;
    loop {
        let current = window_layout(harness, target)?;
        if current != previous_layout {
            return Ok(current);
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!(
                "timed out waiting for window layout change in {target}, still {previous_layout}"
            )
            .into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn active_pane_target(harness: &CliHarness, session_name: &str) -> Result<String, Box<dyn Error>> {
    let output = harness.run(&[
        "list-panes",
        "-t",
        session_name,
        "-F",
        "#{pane_index}:#{pane_active}",
    ])?;
    let panes = common::stdout(&output);
    let pane_index = panes
        .lines()
        .find_map(|line| {
            let (pane_index, active) = line.split_once(':')?;
            (active == "1").then_some(pane_index)
        })
        .ok_or_else(|| format!("missing active pane in {session_name}"))?;
    Ok(format!("{session_name}:0.{pane_index}"))
}

fn wait_for_mode_tree_exit(harness: &CliHarness, target: &str) -> Result<(), Box<dyn Error>> {
    let deadline = std::time::Instant::now() + IO_TIMEOUT;
    loop {
        let output = harness.run(&[
            "display-message",
            "-p",
            "-t",
            target,
            "#{pane_in_mode}|#{pane_mode}",
        ])?;
        if common::stdout(&output).trim_end() == "0|" {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!("timed out waiting for mode-tree to exit in {target}").into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn wait_for_mode_tree_exit_collecting_output(
    harness: &CliHarness,
    attach: &mut AttachedSession,
    target: &str,
) -> Result<Vec<u8>, Box<dyn Error>> {
    let deadline = std::time::Instant::now() + IO_TIMEOUT;
    let mut output_bytes = Vec::new();
    loop {
        output_bytes.extend(drain_attach_output_bytes(attach.master_mut())?);
        let output = harness.run(&[
            "display-message",
            "-p",
            "-t",
            target,
            "#{pane_in_mode}|#{pane_mode}",
        ])?;
        if common::stdout(&output).trim_end() == "0|" {
            return Ok(output_bytes);
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!("timed out waiting for mode-tree to exit in {target}").into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn wait_for_mode_tree_enter(harness: &CliHarness, target: &str) -> Result<(), Box<dyn Error>> {
    let deadline = std::time::Instant::now() + IO_TIMEOUT;
    loop {
        let output = harness.run(&[
            "display-message",
            "-p",
            "-t",
            target,
            "#{pane_in_mode}|#{pane_mode}",
        ])?;
        if common::stdout(&output).trim_end() == "1|tree-mode" {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!("timed out waiting for mode-tree to enter in {target}").into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn assert_mode_tree_exited_with_shell_name(output: &std::process::Output) {
    let rendered = common::stdout(output);
    assert!(
        rendered == "bash|0|\n" || rendered == "zsh|0|\n" || rendered == "sh|0|\n",
        "expected shell window name with no active mode, got: {rendered:?}"
    );
}

#[test]
fn attach_session_binary_uses_the_real_attach_stream_and_restores_terminal(
) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("attach-flow")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);

    let mut attach = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(80, 24))?;
    attach.wait_for_raw_mode(IO_TIMEOUT)?;
    assert!(
        attach.child_mut().try_wait()?.is_none(),
        "attach-session should remain in the streaming attach loop"
    );

    assert_success(&harness.run(&["send-keys", "-t", "alpha:0.0", "printf READY", "Enter"])?);
    let screen = read_until_contains(attach.master_mut(), "READY", IO_TIMEOUT)?;
    assert!(
        screen.contains("READY"),
        "attach output should contain pane data, got: {screen:?}"
    );

    assert_success(&harness.run(&["kill-session", "-t", "alpha"])?);
    let status = attach.wait_for_exit(IO_TIMEOUT)?;
    assert_eq!(status.code(), Some(0));
    attach.assert_restored()?;

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn attach_session_detach_emits_a_single_attach_stop_sequence() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("attach-flow-detach")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);

    let mut attach = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(80, 24))?;
    attach.wait_for_raw_mode(IO_TIMEOUT)?;
    let _ = read_until_contains(attach.master_mut(), "[alpha]", IO_TIMEOUT)?;

    attach.send_bytes(b"\x02d")?;
    let (status, output) = attach.wait_for_exit_with_output(IO_TIMEOUT)?;
    assert_eq!(status.code(), Some(0));
    attach.assert_restored()?;

    assert!(
        output
            .windows(b"[detached (from session alpha)]\r\n".len())
            .any(|window| window == b"[detached (from session alpha)]\r\n"),
        "detach output should contain the detached banner, got: {:?}",
        String::from_utf8_lossy(&output)
    );
    assert_eq!(
        output
            .windows(b"\x1b[?1049l".len())
            .filter(|window| *window == b"\x1b[?1049l")
            .count(),
        1,
        "detach output should contain exactly one alternate-screen exit, got: {:?}",
        String::from_utf8_lossy(&output)
    );

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn inactive_pane_output_refreshes_the_real_attached_client() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("attach-flow-inactive-pane-output")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["split-window", "-h", "-t", "alpha:0.0"])?);
    assert_success(&harness.run(&["select-pane", "-t", "alpha:0.0"])?);

    let mut attach = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(100, 30))?;
    attach.wait_for_raw_mode(IO_TIMEOUT)?;
    let _ = read_until_contains(attach.master_mut(), "tester@RMUXHOST", IO_TIMEOUT)?;
    std::thread::sleep(Duration::from_millis(200));
    drain_attach_output(attach.master_mut())?;

    assert_success(&harness.run(&["send-keys", "-t", "alpha:0.1", "printf INACTIVE", "Enter"])?);
    let refreshed = read_until_contains(attach.master_mut(), "INACTIVE", IO_TIMEOUT)?;
    assert!(
        refreshed.contains("INACTIVE"),
        "inactive pane output should redraw the attached client, got: {refreshed:?}"
    );

    assert_success(&harness.run(&["kill-session", "-t", "alpha"])?);
    let status = attach.wait_for_exit(IO_TIMEOUT)?;
    assert_eq!(status.code(), Some(0));
    attach.assert_restored()?;

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn attach_session_survives_manual_mate_terminal_drag_sequence() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("attach-flow-mouse-drag")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);

    let mut attach = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(80, 24))?;
    attach.wait_for_raw_mode(IO_TIMEOUT)?;
    let _ = read_until_contains(attach.master_mut(), "[alpha]", IO_TIMEOUT)?;
    let _ = read_until_contains(attach.master_mut(), "tester@RMUXHOST", IO_TIMEOUT)?;

    attach.send_bytes(b"\x1b[<0;7;1M\x1b[<32;9;1M\x1b[<32;10;1M")?;
    let rendered = read_until_contains(attach.master_mut(), "[0/0]", IO_TIMEOUT)?;
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        attach.child_mut().try_wait()?.is_none(),
        "attach client should stay alive after the captured mate-terminal drag sequence"
    );
    assert!(
        rendered.contains("\x1b[0;30;43m"),
        "post-drag attach output should contain the copy-mode selection style, got: {rendered:?}"
    );
    let replay = {
        let mut screen = Screen::new(ScreenTerminalSize { cols: 80, rows: 24 }, 0);
        let mut parser = InputParser::new();
        parser.parse(rendered.as_bytes(), &mut screen);
        String::from_utf8(screen.capture_transcript(Default::default(), Default::default()))?
    };
    assert!(
        replay.contains("tester@RMUXHOST"),
        "post-drag attach screen should still show the prompt, got: {replay:?}"
    );

    assert_success(&harness.run(&["kill-session", "-t", "alpha"])?);
    let (status, output) = attach.wait_for_exit_with_output(IO_TIMEOUT)?;
    assert_eq!(status.code(), Some(0));
    attach.assert_restored()?;
    assert!(
        !String::from_utf8_lossy(&output).contains("Broken pipe"),
        "attach output should not report a broken pipe: {:?}",
        String::from_utf8_lossy(&output)
    );

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn attach_session_survives_live_mouse_binding_runtime_errors() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("attach-flow-mouse-binding-error")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&[
        "bind-key",
        "-n",
        "MouseDown1Pane",
        "select-pane",
        "-t",
        "missing:9.9",
    ])?);

    let mut attach = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(80, 24))?;
    attach.wait_for_raw_mode(IO_TIMEOUT)?;
    let _ = read_until_contains(attach.master_mut(), "[alpha]", IO_TIMEOUT)?;

    attach.send_bytes(b"\x1b[<0;7;1M\x1b[<0;7;1m")?;
    std::thread::sleep(Duration::from_millis(200));

    assert!(
        attach.child_mut().try_wait()?.is_none(),
        "attach client should stay alive after a mouse binding runtime error"
    );

    let messages = harness.run(&["show-messages"])?;
    let rendered_messages = common::stdout(&messages);
    assert!(
        rendered_messages.contains("can't find session: missing"),
        "show-messages should record the attached mouse binding error, got: {rendered_messages:?}"
    );

    assert_success(&harness.run(&["kill-session", "-t", "alpha"])?);
    let (status, output) = attach.wait_for_exit_with_output(IO_TIMEOUT)?;
    assert_eq!(status.code(), Some(0));
    attach.assert_restored()?;
    assert!(
        !String::from_utf8_lossy(&output).contains("Broken pipe"),
        "attach output should not report a broken pipe after a mouse binding error: {:?}",
        String::from_utf8_lossy(&output)
    );

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn attach_session_display_panes_renders_overlay_to_the_real_client() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("attach-flow-display-panes")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["split-window", "-h", "-t", "alpha:0.0"])?);
    assert_success(&harness.run(&["set", "-g", "display-panes-time", "5000"])?);

    let mut attach = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(80, 24))?;
    attach.wait_for_raw_mode(IO_TIMEOUT)?;
    let _ = read_until_contains(attach.master_mut(), "tester@RMUXHOST", IO_TIMEOUT)?;
    std::thread::sleep(Duration::from_millis(200));
    drain_attach_output(attach.master_mut())?;

    assert_success(&harness.run(&["display-panes", "-t", "alpha"])?);

    let overlay = read_until_contains_all(attach.master_mut(), &["\x1b[?25l", "x"], IO_TIMEOUT)?;
    assert!(
        overlay.contains("\x1b[41m") || overlay.contains("\x1b[44m"),
        "display-panes overlay should contain tmux-style pane colours, got: {overlay:?}"
    );
    assert!(
        overlay.contains("40x23")
            || overlay.contains("39x23")
            || overlay.contains("40x24")
            || overlay.contains("39x24"),
        "display-panes overlay should contain pane size labels, got: {overlay:?}"
    );

    assert_success(&harness.run(&["kill-session", "-t", "alpha"])?);
    let status = attach.wait_for_exit(IO_TIMEOUT)?;
    assert_eq!(status.code(), Some(0));
    attach.assert_restored()?;

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn prefix_q_renders_display_panes_overlay_on_the_real_attached_client() -> Result<(), Box<dyn Error>>
{
    let harness = CliHarness::new("attach-flow-prefix-q")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["split-window", "-h", "-t", "alpha:0.0"])?);
    assert_success(&harness.run(&["set", "-g", "display-panes-time", "5000"])?);

    let mut attach = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(80, 24))?;
    attach.wait_for_raw_mode(IO_TIMEOUT)?;
    let _ = read_until_contains(attach.master_mut(), "tester@RMUXHOST", IO_TIMEOUT)?;
    std::thread::sleep(Duration::from_millis(200));
    drain_attach_output(attach.master_mut())?;

    attach.send_bytes(b"\x02q")?;

    let overlay = read_until_contains_all(attach.master_mut(), &["\x1b[?25l", "x"], IO_TIMEOUT)?;
    assert!(
        overlay.contains("\x1b[41m") || overlay.contains("\x1b[44m"),
        "prefix q should render the display-panes overlay, got: {overlay:?}"
    );

    assert_success(&harness.run(&["kill-session", "-t", "alpha"])?);
    let status = attach.wait_for_exit(IO_TIMEOUT)?;
    assert_eq!(status.code(), Some(0));
    attach.assert_restored()?;

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn prefix_n_and_p_on_single_window_show_status_without_closing_attach() -> Result<(), Box<dyn Error>>
{
    let harness = CliHarness::new("attach-flow-prefix-window-edge")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);

    let mut attach = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(80, 24))?;
    attach.wait_for_raw_mode(IO_TIMEOUT)?;
    let _ = read_until_contains(attach.master_mut(), "tester@RMUXHOST", IO_TIMEOUT)?;
    std::thread::sleep(Duration::from_millis(200));
    drain_attach_output(attach.master_mut())?;

    attach.send_bytes(b"\x02n")?;
    let next_message = read_until_contains(attach.master_mut(), "No next window", IO_TIMEOUT)?;
    assert!(
        next_message.contains("No next window"),
        "prefix n should show tmux's attached status message, got: {next_message:?}"
    );
    assert!(
        attach.child_mut().try_wait()?.is_none(),
        "prefix n on a single-window session must not close the attached client"
    );

    attach.send_bytes(b"\x02p")?;
    let previous_message =
        read_until_contains(attach.master_mut(), "No previous window", IO_TIMEOUT)?;
    assert!(
        previous_message.contains("No previous window"),
        "prefix p should show tmux's attached status message, got: {previous_message:?}"
    );
    assert!(
        attach.child_mut().try_wait()?.is_none(),
        "prefix p on a single-window session must not close the attached client"
    );

    assert_success(&harness.run(&[
        "send-keys",
        "-t",
        "alpha:0.0",
        "printf STILL_ATTACHED",
        "Enter",
    ])?);
    let alive_output = read_until_contains(attach.master_mut(), "STILL_ATTACHED", IO_TIMEOUT)?;
    assert!(
        alive_output.contains("STILL_ATTACHED"),
        "attach stream should remain usable after status errors, got: {alive_output:?}"
    );

    assert_success(&harness.run(&["kill-session", "-t", "alpha"])?);
    let status = attach.wait_for_exit(IO_TIMEOUT)?;
    assert_eq!(status.code(), Some(0));
    attach.assert_restored()?;

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn prefix_x_during_display_panes_opens_kill_pane_prompt() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("attach-flow-display-panes-prefix-x")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["split-window", "-h", "-t", "alpha:0.0"])?);
    assert_success(&harness.run(&["select-pane", "-t", "alpha:0.1"])?);
    assert_success(&harness.run(&["set", "-g", "display-panes-time", "5000"])?);

    let mut attach = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(80, 24))?;
    attach.wait_for_raw_mode(IO_TIMEOUT)?;
    let _ = read_until_contains(attach.master_mut(), "tester@RMUXHOST", IO_TIMEOUT)?;
    std::thread::sleep(Duration::from_millis(200));
    drain_attach_output(attach.master_mut())?;

    attach.send_bytes(b"\x02q")?;
    let overlay = read_until_contains(attach.master_mut(), "\x1b[?25l", IO_TIMEOUT)?;
    assert!(
        overlay.contains("\x1b[?25l"),
        "prefix q should enter display-panes before the prefix x transition, got: {overlay:?}"
    );

    attach.send_bytes(b"\x02x")?;
    let prompt = read_until_contains(attach.master_mut(), "kill-pane 1? (y/n)", IO_TIMEOUT)?;
    assert!(
        prompt.contains("kill-pane 1? (y/n)"),
        "prefix x during display-panes should open the normal kill-pane prompt, got: {prompt:?}"
    );
    assert!(
        attach.child_mut().try_wait()?.is_none(),
        "prefix x during display-panes must not close the attached client before confirmation"
    );

    attach.send_bytes(b"n")?;
    std::thread::sleep(Duration::from_millis(100));
    drain_attach_output(attach.master_mut())?;

    assert_success(&harness.run(&["kill-session", "-t", "alpha"])?);
    let status = attach.wait_for_exit(IO_TIMEOUT)?;
    assert_eq!(status.code(), Some(0));
    attach.assert_restored()?;

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn prefix_split_when_too_small_reports_no_space_without_closing_attach(
) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("attach-flow-prefix-split-no-space")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);

    let mut attach = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(40, 8))?;
    attach.wait_for_raw_mode(IO_TIMEOUT)?;
    let _ = read_until_contains(attach.master_mut(), "tester@RMUXHOST", IO_TIMEOUT)?;
    std::thread::sleep(Duration::from_millis(200));
    drain_attach_output(attach.master_mut())?;

    let mut captured = String::new();
    for _ in 0..20 {
        attach.send_bytes(b"\x02%")?;
        std::thread::sleep(Duration::from_millis(75));
        let chunk = drain_attach_output_bytes(attach.master_mut())?;
        captured.push_str(&String::from_utf8_lossy(&chunk));
        if captured.contains("No space for new pane") {
            break;
        }
    }

    assert!(
        captured.contains("No space for new pane"),
        "prefix % should show tmux's attached no-space status message, got: {captured:?}"
    );
    assert!(
        attach.child_mut().try_wait()?.is_none(),
        "prefix % at pane saturation must not close the attached client"
    );

    assert_success(&harness.run(&["kill-session", "-t", "alpha"])?);
    let status = attach.wait_for_exit(IO_TIMEOUT)?;
    assert_eq!(status.code(), Some(0));
    attach.assert_restored()?;

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn choose_tree_moves_down_one_item_per_down_key() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("attach-flow-choose-tree-down")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "alpha", "-n", "beta"])?);
    assert_success(&harness.run(&["send-keys", "-t", "alpha:1.0", "printf beta0", "Enter"])?);
    assert_success(&harness.run(&["split-window", "-h", "-t", "alpha:1.0"])?);
    assert_success(&harness.run(&["send-keys", "-t", "alpha:1.1", "printf beta1", "Enter"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "alpha", "-n", "gamma"])?);
    assert_success(&harness.run(&["send-keys", "-t", "alpha:2.0", "printf gamma0", "Enter"])?);
    assert_success(&harness.run(&["select-window", "-t", "alpha:0"])?);

    let mut attach = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(100, 30))?;
    attach.wait_for_raw_mode(IO_TIMEOUT)?;
    let _ = read_until_contains(attach.master_mut(), "[alpha]", IO_TIMEOUT)?;
    std::thread::sleep(Duration::from_millis(200));
    drain_attach_output(attach.master_mut())?;

    attach.send_bytes(b"\x02w")?;
    let tree = read_until_contains(attach.master_mut(), "alpha: 3 windows", IO_TIMEOUT)?;
    assert!(
        tree.contains("1: beta") && tree.contains("2: gamma"),
        "choose-tree should render all windows, got: {tree:?}"
    );
    wait_for_mode_tree_enter(&harness, "alpha:0.0")?;
    drain_attach_output(attach.master_mut())?;

    attach.send_bytes(b"\x1b[B")?;
    let after_down = read_until_contains(attach.master_mut(), "beta1", IO_TIMEOUT)?;
    assert!(
        after_down.contains("beta1"),
        "one Down key in choose-tree should move from 0 to beta, got: {after_down:?}"
    );

    assert_success(&harness.run(&["kill-session", "-t", "alpha"])?);
    let status = attach.wait_for_exit(IO_TIMEOUT)?;
    assert_eq!(status.code(), Some(0));
    attach.assert_restored()?;

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn choose_tree_filter_prompt_uses_tmux_parenthesized_label() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("attach-flow-choose-tree-filter-prompt")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "alpha", "-n", "beta"])?);

    let mut attach = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(100, 30))?;
    attach.wait_for_raw_mode(IO_TIMEOUT)?;
    let _ = read_until_contains(attach.master_mut(), "[alpha]", IO_TIMEOUT)?;
    std::thread::sleep(Duration::from_millis(200));
    drain_attach_output(attach.master_mut())?;

    attach.send_bytes(b"\x02w")?;
    let _ = read_until_contains(attach.master_mut(), "alpha: 2 windows", IO_TIMEOUT)?;
    drain_attach_output(attach.master_mut())?;

    attach.send_bytes(b"f")?;
    let prompt = read_until_contains(attach.master_mut(), "(filter) ", IO_TIMEOUT)?;
    assert!(
        prompt.contains("(filter) "),
        "choose-tree filter prompt should match tmux, got: {prompt:?}"
    );

    attach.send_bytes(b"\x1b\x1b")?;
    std::thread::sleep(Duration::from_millis(200));
    drain_attach_output(attach.master_mut())?;

    assert_success(&harness.run(&["kill-session", "-t", "alpha"])?);
    let status = attach.wait_for_exit(IO_TIMEOUT)?;
    assert_eq!(status.code(), Some(0));
    attach.assert_restored()?;

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn prefix_w_x_opens_the_real_choose_tree_kill_prompt() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("attach-flow-choose-tree-kill-prompt")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "alpha", "-n", "beta"])?);
    assert_success(&harness.run(&["select-window", "-t", "alpha:0"])?);

    let mut attach = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(100, 30))?;
    attach.wait_for_raw_mode(IO_TIMEOUT)?;
    let _ = read_until_contains(attach.master_mut(), "[alpha]", IO_TIMEOUT)?;
    std::thread::sleep(Duration::from_millis(200));
    drain_attach_output(attach.master_mut())?;

    attach.send_bytes(b"\x02w")?;
    let _ = read_until_contains(attach.master_mut(), "alpha: 2 windows", IO_TIMEOUT)?;
    drain_attach_output(attach.master_mut())?;

    attach.send_bytes(b"x")?;
    let prompt = read_until_contains(attach.master_mut(), "Kill window 0?", IO_TIMEOUT)?;
    assert!(
        prompt.contains("Kill window 0?"),
        "choose-tree x should open the tmux-style kill prompt for the current window, got: {prompt:?}"
    );

    attach.send_bytes(b"y")?;
    std::thread::sleep(Duration::from_millis(250));
    drain_attach_output(attach.master_mut())?;

    let windows = harness.run(&["list-windows", "-t", "alpha", "-F", "#{window_index}"])?;
    let window_lines = common::stdout(&windows)
        .lines()
        .filter(|line| !line.is_empty())
        .count();
    assert_eq!(
        window_lines, 1,
        "confirming choose-tree x should kill the selected window"
    );

    assert_success(&harness.run(&["kill-session", "-t", "alpha"])?);
    let status = attach.wait_for_exit(IO_TIMEOUT)?;
    assert_eq!(status.code(), Some(0));
    attach.assert_restored()?;

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn prefix_w_then_prefix_comma_opens_rename_window_prompt() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("attach-flow-choose-tree-prefix-comma")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["split-window", "-h", "-t", "alpha:0.0"])?);
    assert_success(&harness.run(&["split-window", "-h", "-t", "alpha:0.0"])?);
    assert_success(&harness.run(&["split-window", "-v", "-t", "alpha:0.0"])?);

    let mut attach = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(100, 30))?;
    attach.wait_for_raw_mode(IO_TIMEOUT)?;
    let _ = read_until_contains(attach.master_mut(), "tester@RMUXHOST", IO_TIMEOUT)?;
    std::thread::sleep(Duration::from_millis(200));
    drain_attach_output(attach.master_mut())?;

    attach.send_bytes(b"\x02w")?;
    let _ = read_until_contains(attach.master_mut(), "sort: index", IO_TIMEOUT)?;
    std::thread::sleep(Duration::from_millis(100));
    drain_attach_output(attach.master_mut())?;

    attach.send_bytes(b"\x02,")?;
    let prompt = read_until_contains(attach.master_mut(), "(rename-window) ", IO_TIMEOUT)?;
    assert!(
        prompt.contains("(rename-window) "),
        "prefix comma inside choose-tree should open the normal rename-window prompt, got: {prompt:?}"
    );

    assert_success(&harness.run(&["kill-session", "-t", "alpha"])?);
    let status = attach.wait_for_exit(IO_TIMEOUT)?;
    assert_eq!(status.code(), Some(0));
    attach.assert_restored()?;

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn prefix_w_then_prefix_x_uses_the_normal_kill_pane_binding() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("attach-flow-choose-tree-prefix-x")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["split-window", "-h", "-t", "alpha:0.0"])?);
    assert_success(&harness.run(&["split-window", "-h", "-t", "alpha:0.0"])?);
    assert_success(&harness.run(&["split-window", "-v", "-t", "alpha:0.0"])?);

    let active_pane = common::stdout(&harness.run(&[
        "list-panes",
        "-t",
        "alpha:0",
        "-F",
        "#{pane_index} #{pane_active}",
    ])?)
    .lines()
    .find_map(|line| {
        let mut parts = line.split_whitespace();
        let pane = parts.next()?;
        let active = parts.next()?;
        (active == "1").then_some(pane.to_owned())
    })
    .ok_or("missing active pane after split sequence")?;

    let mut attach = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(100, 30))?;
    attach.wait_for_raw_mode(IO_TIMEOUT)?;
    let _ = read_until_contains(attach.master_mut(), "tester@RMUXHOST", IO_TIMEOUT)?;
    std::thread::sleep(Duration::from_millis(200));
    drain_attach_output(attach.master_mut())?;

    attach.send_bytes(b"\x02w")?;
    let _ = read_until_contains(attach.master_mut(), "sort: index", IO_TIMEOUT)?;
    std::thread::sleep(Duration::from_millis(100));
    drain_attach_output(attach.master_mut())?;

    attach.send_bytes(b"\x02x")?;
    let prompt = read_until_contains(attach.master_mut(), "kill-pane", IO_TIMEOUT)?;
    let expected = format!("kill-pane {active_pane}? (y/n)");
    assert!(
        prompt.contains(&expected),
        "prefix x inside choose-tree should open the normal kill-pane prompt, expected {expected:?}, got: {prompt:?}"
    );

    assert_success(&harness.run(&["kill-session", "-t", "alpha"])?);
    let status = attach.wait_for_exit(IO_TIMEOUT)?;
    assert_eq!(status.code(), Some(0));
    attach.assert_restored()?;

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn prefix_w_prefix_x_confirmed_clears_choose_tree_when_host_pane_dies() -> Result<(), Box<dyn Error>>
{
    let harness = CliHarness::new("attach-flow-choose-tree-prefix-x-confirm")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&[
        "send-keys",
        "-t",
        "alpha:0.0",
        "cd /tmp/rmux-workspace",
        "Enter",
    ])?);

    let mut attach = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(100, 30))?;
    attach.wait_for_raw_mode(IO_TIMEOUT)?;
    let initial = read_until_contains(attach.master_mut(), "tester@RMUXHOST", IO_TIMEOUT)?;
    std::thread::sleep(Duration::from_millis(200));
    let mut initial_bytes = initial.into_bytes();
    initial_bytes.extend(drain_attach_output_bytes(attach.master_mut())?);

    let mut screen = Screen::new(
        ScreenTerminalSize {
            cols: 100,
            rows: 30,
        },
        0,
    );
    let mut parser = InputParser::new();
    let _ = capture_attach_transcript(&mut screen, &mut parser, &initial_bytes)?;

    attach.send_bytes(b"\x02%\x02%\x02\"")?;
    let pane_indexes = wait_for_pane_count(&harness, "alpha", 4)?;
    let _ = apply_quiescent_attach_output(
        &mut attach,
        &mut screen,
        &mut parser,
        Duration::from_millis(300),
    )?;
    for pane_index in &pane_indexes {
        assert_success(&harness.run(&[
            "send-keys",
            "-t",
            &format!("alpha:0.{pane_index}"),
            "export PS1='tester@RMUXHOST:~$ '",
            "Enter",
            "clear",
            "Enter",
        ])?);
    }
    std::thread::sleep(Duration::from_millis(300));
    drain_attach_output(attach.master_mut())?;

    let last_marker = pane_indexes
        .iter()
        .take(3)
        .nth(2)
        .map(|pane_index| format!("P{pane_index}"))
        .ok_or("missing pane indexes after split sequence")?;
    let marker_refs = pane_indexes
        .iter()
        .take(3)
        .map(|pane_index| format!("P{pane_index}"))
        .collect::<Vec<_>>();
    let marker_ref_slices = marker_refs.iter().map(String::as_str).collect::<Vec<_>>();
    for pane_index in pane_indexes.iter().take(3) {
        let target = format!("alpha:0.{pane_index}");
        let marker = format!("P{pane_index}");
        send_shell_marker(&harness, &target, &marker)?;
    }
    let marker_output = read_until_contains(attach.master_mut(), &last_marker, IO_TIMEOUT)?;
    std::thread::sleep(Duration::from_millis(200));
    let mut marker_bytes = marker_output.into_bytes();
    marker_bytes.extend(drain_attach_output_bytes(attach.master_mut())?);
    let _ = capture_attach_transcript(&mut screen, &mut parser, &marker_bytes)?;
    let _ = wait_for_attach_transcript_matching(
        &mut attach,
        &mut screen,
        &mut parser,
        IO_TIMEOUT,
        "pane markers before choose-tree kill prompt",
        |content| {
            marker_ref_slices
                .iter()
                .all(|marker| content.contains(marker))
        },
    )?;

    attach.send_bytes(b"\x02w")?;
    let _ = read_until_contains(attach.master_mut(), "sort: index", IO_TIMEOUT)?;
    std::thread::sleep(Duration::from_millis(100));
    let tree_bytes = drain_attach_output_bytes(attach.master_mut())?;
    let _ = capture_attach_transcript(&mut screen, &mut parser, &tree_bytes)?;

    attach.send_bytes(b"\x02x")?;
    let prompt = read_until_contains(attach.master_mut(), "kill-pane", IO_TIMEOUT)?;
    let _ = capture_attach_transcript(&mut screen, &mut parser, prompt.as_bytes())?;
    assert!(
        prompt.contains("kill-pane"),
        "prefix x inside choose-tree should target a pane, got: {prompt:?}"
    );

    attach.send_bytes(b"y")?;
    wait_for_pane_count(&harness, "alpha", 3)?;
    let deadline = std::time::Instant::now() + IO_TIMEOUT;
    let mut redraw_bytes = Vec::new();
    let tree_after_kill = loop {
        std::thread::sleep(Duration::from_millis(50));
        let next_bytes = drain_attach_output_bytes(attach.master_mut())?;
        if !next_bytes.is_empty() {
            redraw_bytes.extend_from_slice(&next_bytes);
            let transcript = capture_attach_transcript(&mut screen, &mut parser, &next_bytes)?;
            if !transcript.contains("sort: index")
                && !transcript.contains("┌ 0 (sort: index)")
                && !transcript.contains("└────────────────")
                && (transcript.contains("tester@RMUXHOST:~$")
                    || transcript.contains("P1")
                    || transcript.contains("P2")
                    || transcript.contains("P3"))
            {
                break transcript;
            }
        }
        if std::time::Instant::now() >= deadline {
            break String::from_utf8(
                screen.capture_transcript(Default::default(), Default::default()),
            )?;
        }
    };
    let redraw_len = redraw_bytes.len();
    let redraw_debug = String::from_utf8_lossy(&redraw_bytes);
    let clear_count = redraw_debug.matches("\x1b[2J").count();
    let sort_count = redraw_debug.matches("sort: index").count();
    let clear_pos = redraw_debug.find("\x1b[2J");
    let first_sort_pos = redraw_debug.find("sort: index");
    let last_sort_pos = redraw_debug.rfind("sort: index");
    let mode_state = common::stdout(&harness.run(&[
        "list-panes",
        "-t",
        "alpha:0",
        "-F",
        "#{pane_index}:#{pane_active}:#{pane_in_mode}:#{pane_mode}:#{window_panes}",
    ])?);
    let captured_pane = common::stdout(&harness.run(&["capture-pane", "-p", "-t", "alpha:0"])?);

    assert!(
        mode_state
            .lines()
            .all(|line| line.ends_with(":0::3")),
        "killing a pane from choose-tree should leave tree-mode in every remaining pane and keep three panes, got {mode_state:?}"
    );
    assert!(
        !tree_after_kill.contains("sort: index")
            && !tree_after_kill.contains("┌ 0 (sort: index)")
            && !tree_after_kill.contains("└────────────────"),
        "confirming kill-pane from choose-tree should clear the stale choose-tree overlay, mode={mode_state:?}, capture={captured_pane:?}, redraw_len={redraw_len}, clear_count={clear_count}, sort_count={sort_count}, clear_pos={clear_pos:?}, first_sort_pos={first_sort_pos:?}, last_sort_pos={last_sort_pos:?}, got:\n{tree_after_kill}"
    );
    assert!(
        tree_after_kill.contains("tester@RMUXHOST:~$")
            || tree_after_kill.contains("P1")
            || tree_after_kill.contains("P2")
            || tree_after_kill.contains("P3"),
        "after clearing choose-tree, the live pane layout should be visible, mode={mode_state:?}, got:\n{tree_after_kill}"
    );
    let has_cropped_prompt = tree_after_kill.lines().any(|line| {
        line.starts_with("oPC:~$")
            || line.starts_with("er@RMUXHOST:~$")
            || line.starts_with("iner@RMUXHOST:~$")
            || line.contains("│oPC:~$")
            || line.contains("│er@RMUXHOST:~$")
            || line.contains("│iner@RMUXHOST:~$")
    });
    assert!(
        !has_cropped_prompt,
        "clearing choose-tree should remove the stale cropped preview, mode={mode_state:?}, got:\n{tree_after_kill}"
    );

    assert_success(&harness.run(&["kill-session", "-t", "alpha"])?);
    let status = attach.wait_for_exit(IO_TIMEOUT)?;
    assert_eq!(status.code(), Some(0));
    attach.assert_restored()?;

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn prefix_w_then_prefix_q_renders_display_panes_overlay() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("attach-flow-choose-tree-prefix-q")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["split-window", "-h", "-t", "alpha:0.0"])?);
    assert_success(&harness.run(&["split-window", "-h", "-t", "alpha:0.0"])?);
    assert_success(&harness.run(&["split-window", "-v", "-t", "alpha:0.0"])?);
    assert_success(&harness.run(&["set", "-g", "display-panes-time", "5000"])?);

    let mut attach = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(100, 30))?;
    attach.wait_for_raw_mode(IO_TIMEOUT)?;
    let _ = read_until_contains(attach.master_mut(), "tester@RMUXHOST", IO_TIMEOUT)?;
    std::thread::sleep(Duration::from_millis(200));
    drain_attach_output(attach.master_mut())?;

    attach.send_bytes(b"\x02w")?;
    let _ = read_until_contains(attach.master_mut(), "sort: index", IO_TIMEOUT)?;
    std::thread::sleep(Duration::from_millis(100));
    drain_attach_output(attach.master_mut())?;

    attach.send_bytes(b"\x02q")?;
    let deadline = std::time::Instant::now() + IO_TIMEOUT;
    let mut overlay = String::new();
    loop {
        std::thread::sleep(Duration::from_millis(50));
        overlay.push_str(&String::from_utf8_lossy(&drain_attach_output_bytes(
            attach.master_mut(),
        )?));
        if overlay.contains("\x1b[41m")
            || overlay.contains("\x1b[44m")
            || overlay.contains("\x1b[42m")
            || overlay.contains("\x1b[45m")
        {
            break;
        }
        if std::time::Instant::now() >= deadline {
            break;
        }
    }
    assert!(
        overlay.contains("\x1b[41m")
            || overlay.contains("\x1b[44m")
            || overlay.contains("\x1b[42m")
            || overlay.contains("\x1b[45m"),
        "prefix q inside choose-tree should render the display-panes overlay, got: {overlay:?}"
    );

    assert_success(&harness.run(&["kill-session", "-t", "alpha"])?);
    let status = attach.wait_for_exit(IO_TIMEOUT)?;
    assert_eq!(status.code(), Some(0));
    attach.assert_restored()?;

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn compact_prefix_wq_closes_choose_tree_when_keys_arrive_in_one_burst() -> Result<(), Box<dyn Error>>
{
    let harness = CliHarness::new("attach-flow-choose-tree-compact-close")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "alpha", "-n", "beta"])?);

    let mut attach = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(100, 30))?;
    attach.wait_for_raw_mode(IO_TIMEOUT)?;
    let _ = read_until_contains(attach.master_mut(), "tester@RMUXHOST", IO_TIMEOUT)?;
    std::thread::sleep(Duration::from_millis(200));
    drain_attach_output(attach.master_mut())?;

    attach.send_bytes(b"\x02wq")?;
    wait_for_mode_tree_exit(&harness, "alpha:0.0")?;

    let output = harness.run(&[
        "display-message",
        "-p",
        "-t",
        "alpha:0.0",
        "#{window_name}|#{pane_in_mode}|#{pane_mode}",
    ])?;
    assert_mode_tree_exited_with_shell_name(&output);

    assert_success(&harness.run(&["kill-session", "-t", "alpha"])?);
    let status = attach.wait_for_exit(IO_TIMEOUT)?;
    assert_eq!(status.code(), Some(0));
    attach.assert_restored()?;

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn compact_prefix_tq_exits_clock_mode_when_keys_arrive_in_one_burst() -> Result<(), Box<dyn Error>>
{
    let harness = CliHarness::new("attach-flow-clock-compact-close")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);

    let mut attach = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(100, 30))?;
    attach.wait_for_raw_mode(IO_TIMEOUT)?;
    let _ = read_until_contains(attach.master_mut(), "tester@RMUXHOST", IO_TIMEOUT)?;
    std::thread::sleep(Duration::from_millis(200));
    drain_attach_output(attach.master_mut())?;

    attach.send_bytes(b"\x02tq")?;
    wait_for_mode_tree_exit(&harness, "alpha:0.0")?;

    let output = harness.run(&[
        "display-message",
        "-p",
        "-t",
        "alpha:0.0",
        "#{window_name}|#{pane_in_mode}|#{pane_mode}",
    ])?;
    assert_mode_tree_exited_with_shell_name(&output);

    assert_success(&harness.run(&["kill-session", "-t", "alpha"])?);
    let status = attach.wait_for_exit(IO_TIMEOUT)?;
    assert_eq!(status.code(), Some(0));
    attach.assert_restored()?;

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn prefix_w_then_prefix_q_timeout_restores_choose_tree_overlay() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("attach-flow-choose-tree-prefix-q-timeout")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["split-window", "-h", "-t", "alpha:0.0"])?);
    assert_success(&harness.run(&["split-window", "-h", "-t", "alpha:0.0"])?);
    assert_success(&harness.run(&["split-window", "-v", "-t", "alpha:0.0"])?);
    assert_success(&harness.run(&["set", "-g", "display-panes-time", "200"])?);

    let mut attach = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(100, 30))?;
    attach.wait_for_raw_mode(IO_TIMEOUT)?;
    let initial = read_until_contains(attach.master_mut(), "tester@RMUXHOST", IO_TIMEOUT)?;
    std::thread::sleep(Duration::from_millis(200));
    let mut initial_bytes = initial.into_bytes();
    initial_bytes.extend(drain_attach_output_bytes(attach.master_mut())?);

    let mut screen = Screen::new(
        ScreenTerminalSize {
            cols: 100,
            rows: 30,
        },
        0,
    );
    let mut parser = InputParser::new();
    let _ = capture_attach_transcript(&mut screen, &mut parser, &initial_bytes)?;

    let active_pane = active_pane_target(&harness, "alpha")?;
    attach.send_bytes(b"\x02w")?;
    let tree_output = read_until_contains(attach.master_mut(), "sort: index", IO_TIMEOUT)?;
    let mut tree_bytes = tree_output.into_bytes();
    tree_bytes.extend(drain_attach_output_bytes(attach.master_mut())?);
    let _ = capture_attach_transcript(&mut screen, &mut parser, &tree_bytes)?;
    wait_for_mode_tree_enter(&harness, &active_pane)?;
    let baseline_tree = wait_for_attach_transcript_matching(
        &mut attach,
        &mut screen,
        &mut parser,
        IO_TIMEOUT,
        "choose-tree baseline",
        |content| {
            content.contains("sort: index")
                && content.contains("(0) - alpha: 1 windows (attached)")
                && content.contains("└─> + 0: [tmux]*Z")
                && content.contains("│ 0 │")
                && content.contains("│ 3 │")
        },
    )?;
    let baseline_tree_content = transcript_without_status_line(&baseline_tree);
    assert!(
        baseline_tree_content.contains("sort: index"),
        "choose-tree baseline should contain the mode-tree header, got:\n{baseline_tree_content}"
    );

    attach.send_bytes(b"\x02q")?;
    let overlay = read_until_contains_all(attach.master_mut(), &["\x1b[?25l", "x"], IO_TIMEOUT)?;
    let mut overlay_bytes = overlay.into_bytes();
    overlay_bytes.extend(drain_attach_output_bytes(attach.master_mut())?);
    let _ = capture_attach_transcript(&mut screen, &mut parser, &overlay_bytes)?;

    let restored_tree = wait_for_attach_transcript_matching(
        &mut attach,
        &mut screen,
        &mut parser,
        IO_TIMEOUT,
        "choose-tree overlay after display-panes timeout",
        |content| {
            content.contains("sort: index")
                && content.contains("(0) - alpha: 1 windows (attached)")
                && content.contains("└─> + 0: [tmux]*Z")
                && content.contains("│ 0 │")
                && content.contains("│ 3 │")
        },
    )?;
    let restored_tree_content = transcript_without_status_line(&restored_tree);
    assert!(
        restored_tree_content.contains("sort: index")
            && restored_tree_content.contains("(0) - alpha: 1 windows (attached)")
            && restored_tree_content.contains("└─> + 0: [tmux]*Z")
            && restored_tree_content.contains("│ 0 │")
            && restored_tree_content.contains("│ 3 │"),
        "display-panes timeout inside choose-tree should restore the choose-tree overlay\n--- baseline ---\n{baseline_tree_content}\n--- restored ---\n{restored_tree_content}"
    );

    attach.send_bytes(b"\x1b\x1b")?;
    std::thread::sleep(Duration::from_millis(200));
    drain_attach_output(attach.master_mut())?;

    assert_success(&harness.run(&["kill-session", "-t", "alpha"])?);
    let status = attach.wait_for_exit(IO_TIMEOUT)?;
    assert_eq!(status.code(), Some(0));
    attach.assert_restored()?;

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn prefix_w_marks_the_host_pane_as_tmux_mode_for_status_formats() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("attach-flow-choose-tree-pane-mode")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "alpha", "-n", "beta"])?);
    assert_success(&harness.run(&["select-window", "-t", "alpha:0"])?);

    let mut attach = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(100, 30))?;
    attach.wait_for_raw_mode(IO_TIMEOUT)?;
    let _ = read_until_contains(attach.master_mut(), "[alpha]", IO_TIMEOUT)?;
    std::thread::sleep(Duration::from_millis(200));
    drain_attach_output(attach.master_mut())?;

    attach.send_bytes(b"\x02w")?;
    let _ = read_until_contains(attach.master_mut(), "alpha: 2 windows", IO_TIMEOUT)?;
    drain_attach_output(attach.master_mut())?;

    let output = harness.run(&[
        "display-message",
        "-p",
        "-t",
        "alpha:0.0",
        "#{window_name}|#{pane_in_mode}|#{pane_mode}",
    ])?;
    assert_eq!(common::stdout(&output), "[tmux]|1|tree-mode\n");
    let windows = harness.run(&[
        "list-windows",
        "-t",
        "alpha",
        "-F",
        "#{window_index}:#{window_name}",
    ])?;
    assert!(
        common::stdout(&windows)
            .lines()
            .any(|line| line == "0:[tmux]"),
        "choose-tree should update the stored automatic window name, got: {:?}",
        common::stdout(&windows)
    );

    attach.send_bytes(b"\x1b\x1b")?;
    std::thread::sleep(Duration::from_millis(200));
    drain_attach_output(attach.master_mut())?;

    assert_success(&harness.run(&["kill-session", "-t", "alpha"])?);
    let status = attach.wait_for_exit(IO_TIMEOUT)?;
    assert_eq!(status.code(), Some(0));
    attach.assert_restored()?;

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn choose_tree_q_restores_the_four_pane_layout_on_the_real_attached_client(
) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("attach-flow-choose-tree-restore")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    let mut attach = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(100, 30))?;
    attach.wait_for_raw_mode(IO_TIMEOUT)?;
    let initial = read_until_contains(attach.master_mut(), "tester@RMUXHOST", IO_TIMEOUT)?;
    std::thread::sleep(Duration::from_millis(200));
    let mut initial_bytes = initial.into_bytes();
    initial_bytes.extend(drain_attach_output_bytes(attach.master_mut())?);

    let mut screen = Screen::new(
        ScreenTerminalSize {
            cols: 100,
            rows: 30,
        },
        0,
    );
    let mut parser = InputParser::new();
    let _ = capture_attach_transcript(&mut screen, &mut parser, &initial_bytes)?;

    attach.send_bytes(b"\x02%\x02%\x02\"")?;
    let pane_count_deadline = std::time::Instant::now() + IO_TIMEOUT;
    loop {
        let output = harness.run(&["list-panes", "-t", "alpha", "-F", "#{pane_index}"])?;
        if common::stdout(&output).lines().count() == 4 {
            break;
        }
        if std::time::Instant::now() >= pane_count_deadline {
            return Err("timed out waiting for four panes after prefix % % \"".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let pane_indexes =
        common::stdout(&harness.run(&["list-panes", "-t", "alpha", "-F", "#{pane_index}"])?)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>();
    let _ = apply_quiescent_attach_output(
        &mut attach,
        &mut screen,
        &mut parser,
        Duration::from_millis(300),
    )?;
    for pane_index in pane_indexes.iter().take(3) {
        let target = format!("alpha:0.{pane_index}");
        let marker = format!("P{pane_index}");
        send_shell_marker(&harness, &target, &marker)?;
    }
    let marker = pane_indexes
        .iter()
        .take(3)
        .nth(2)
        .map(|pane_index| format!("P{pane_index}"))
        .ok_or("missing pane indexes after split sequence")?;
    let marker_refs = pane_indexes
        .iter()
        .take(3)
        .map(|pane_index| format!("P{pane_index}"))
        .collect::<Vec<_>>();
    let marker_ref_slices = marker_refs.iter().map(String::as_str).collect::<Vec<_>>();
    let marker_output = read_until_contains(attach.master_mut(), &marker, IO_TIMEOUT)?;
    std::thread::sleep(Duration::from_millis(200));
    let mut layout_bytes = marker_output.into_bytes();
    layout_bytes.extend(drain_attach_output_bytes(attach.master_mut())?);
    let _ = capture_attach_transcript(&mut screen, &mut parser, &layout_bytes)?;
    let _ = wait_for_attach_transcript_matching(
        &mut attach,
        &mut screen,
        &mut parser,
        IO_TIMEOUT,
        "four pane marker baseline",
        |content| {
            marker_ref_slices
                .iter()
                .all(|marker| content.contains(marker))
        },
    )?;
    assert!(
        common::stdout(&harness.run(&["list-panes", "-t", "alpha", "-F", "#{pane_index}"])?)
            .lines()
            .count()
            == 4,
        "expected four panes after the interactive split sequence"
    );
    let baseline_geometry = list_pane_geometry(&harness, "alpha")?;

    let active_pane = active_pane_target(&harness, "alpha")?;
    attach.send_bytes(b"\x02w")?;
    let _ = read_until_contains(attach.master_mut(), "windows", IO_TIMEOUT)?;
    wait_for_mode_tree_enter(&harness, &active_pane)?;
    std::thread::sleep(Duration::from_millis(100));
    let tree_bytes = drain_attach_output_bytes(attach.master_mut())?;
    let _ = capture_attach_transcript(&mut screen, &mut parser, &tree_bytes)?;

    attach.send_bytes(b"q")?;
    let mut after_q_bytes =
        wait_for_mode_tree_exit_collecting_output(&harness, &mut attach, &active_pane)?;
    let deadline = std::time::Instant::now() + IO_TIMEOUT;
    let restored = loop {
        after_q_bytes.extend(drain_attach_output_bytes(attach.master_mut())?);
        let transcript = capture_attach_transcript(&mut screen, &mut parser, &after_q_bytes)?;
        after_q_bytes.clear();
        let panes = transcript_without_status_line(&transcript);
        if marker_ref_slices
            .iter()
            .all(|marker| panes.contains(marker))
            && !panes.contains("sort: index")
        {
            break transcript;
        }
        if std::time::Instant::now() >= deadline {
            panic!("choose-tree q did not settle back to panes before timeout, got:\n{panes}");
        }
        std::thread::sleep(Duration::from_millis(50));
        after_q_bytes = drain_attach_output_bytes(attach.master_mut())?;
    };

    let restored_panes = transcript_without_status_line(&restored);
    let restored_geometry = list_pane_geometry(&harness, "alpha")?;
    assert_eq!(
        restored_geometry, baseline_geometry,
        "choose-tree q should restore the exact pane geometry"
    );
    assert!(
        marker_ref_slices
            .iter()
            .all(|marker| restored_panes.contains(marker))
            && !restored_panes.contains("sort: index"),
        "choose-tree q should restore pane content instead of mode-tree content\n--- restored ---\n{restored_panes}"
    );

    let settled = apply_quiescent_attach_output(
        &mut attach,
        &mut screen,
        &mut parser,
        Duration::from_millis(1200),
    )?;
    let settled_panes = transcript_without_status_line(&settled);
    let settled_geometry = list_pane_geometry(&harness, "alpha")?;
    assert_eq!(
        settled_geometry, baseline_geometry,
        "choose-tree q should not alter pane geometry after queued redraws settle"
    );
    assert!(
        !settled_panes.contains("sort: index")
            && !settled_panes.contains("┌ 0 (sort: index)")
            && !settled_panes.contains("└─> + 0: [tmux]*Z"),
        "choose-tree q should not leave stale mode-tree content after queued redraws settle\n--- settled ---\n{settled_panes}"
    );

    assert_success(&harness.run(&["kill-session", "-t", "alpha"])?);
    let status = attach.wait_for_exit(IO_TIMEOUT)?;
    assert_eq!(status.code(), Some(0));
    attach.assert_restored()?;

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn choose_tree_q_after_resize_restores_the_resized_four_pane_layout() -> Result<(), Box<dyn Error>>
{
    let harness = CliHarness::new("attach-flow-choose-tree-resize-restore")?;
    let mut daemon = harness.start_hidden_daemon()?;

    let initial_size = TerminalSize::new(100, 30);
    let resized_size = TerminalSize::new(120, 40);
    let initial_screen_size = ScreenTerminalSize {
        cols: 100,
        rows: 30,
    };
    let resized_screen_size = ScreenTerminalSize {
        cols: 120,
        rows: 40,
    };

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "beta"])?);

    let mut alpha = AttachedSession::spawn(&harness, "alpha", initial_size)?;
    alpha.wait_for_raw_mode(IO_TIMEOUT)?;
    let alpha_initial = read_until_contains(alpha.master_mut(), "tester@RMUXHOST", IO_TIMEOUT)?;
    std::thread::sleep(Duration::from_millis(200));
    let mut alpha_initial_bytes = alpha_initial.into_bytes();
    alpha_initial_bytes.extend(drain_attach_output_bytes(alpha.master_mut())?);

    let mut alpha_screen = Screen::new(initial_screen_size, 0);
    let mut alpha_parser = InputParser::new();
    let _ = capture_attach_transcript(&mut alpha_screen, &mut alpha_parser, &alpha_initial_bytes)?;

    alpha.send_bytes(b"\x02%\x02%\x02\"")?;
    let alpha_indexes = wait_for_pane_count(&harness, "alpha", 4)?;
    let _ = apply_quiescent_attach_output(
        &mut alpha,
        &mut alpha_screen,
        &mut alpha_parser,
        Duration::from_millis(300),
    )?;
    for pane_index in &alpha_indexes {
        assert_success(&harness.run(&[
            "send-keys",
            "-t",
            &format!("alpha:0.{pane_index}"),
            "export PS1='p$ '",
            "Enter",
            "clear",
            "Enter",
            &format!("echo A{pane_index}"),
            "Enter",
        ])?);
    }
    alpha.send_bytes(b"\x02r")?;
    let alpha_marker = alpha_indexes
        .last()
        .map(|pane_index| format!("A{pane_index}"))
        .ok_or("missing alpha pane indexes after split sequence")?;
    let alpha_markers = alpha_indexes
        .iter()
        .map(|pane_index| format!("A{pane_index}"))
        .collect::<Vec<_>>();
    let alpha_layout_output = read_until_contains(alpha.master_mut(), &alpha_marker, IO_TIMEOUT)?;
    std::thread::sleep(Duration::from_millis(200));
    let mut alpha_layout_bytes = alpha_layout_output.into_bytes();
    alpha_layout_bytes.extend(drain_attach_output_bytes(alpha.master_mut())?);
    let _ = capture_attach_transcript(&mut alpha_screen, &mut alpha_parser, &alpha_layout_bytes)?;

    let alpha_layout_before_resize = window_layout(&harness, "alpha")?;
    let alpha_active_pane = active_pane_target(&harness, "alpha")?;
    alpha.send_bytes(b"\x02w")?;
    let _ = read_until_contains(alpha.master_mut(), "windows", IO_TIMEOUT)?;
    std::thread::sleep(Duration::from_millis(100));
    let tree_bytes = drain_attach_output_bytes(alpha.master_mut())?;
    let _ = capture_attach_transcript(&mut alpha_screen, &mut alpha_parser, &tree_bytes)?;

    alpha.resize(resized_size)?;
    let _alpha_layout_after_resize =
        wait_for_layout_change(&harness, "alpha", &alpha_layout_before_resize)?;
    alpha_screen.resize(resized_screen_size);
    std::thread::sleep(Duration::from_millis(300));
    let alpha_resize_output = read_until_contains(alpha.master_mut(), &alpha_marker, IO_TIMEOUT)?;
    let mut alpha_resize_bytes = alpha_resize_output.into_bytes();
    alpha_resize_bytes.extend(drain_attach_output_bytes(alpha.master_mut())?);
    let _ = capture_attach_transcript(&mut alpha_screen, &mut alpha_parser, &alpha_resize_bytes)?;
    std::thread::sleep(Duration::from_millis(1000));
    drain_attach_output(alpha.master_mut())?;
    alpha.send_bytes(b"q")?;
    let mut pending_after_q =
        wait_for_mode_tree_exit_collecting_output(&harness, &mut alpha, &alpha_active_pane)?;
    let alpha_marker_refs = alpha_markers.iter().map(String::as_str).collect::<Vec<_>>();
    let alpha_command_refs = alpha_indexes
        .iter()
        .map(|pane_index| format!("p$ echo A{pane_index}"))
        .collect::<Vec<_>>();
    let deadline = std::time::Instant::now() + IO_TIMEOUT;
    let alpha_restored = loop {
        pending_after_q.extend(drain_attach_output_bytes(alpha.master_mut())?);
        assert!(
            !pending_after_q.is_empty(),
            "dismissing choose-tree after a resize should emit a redraw"
        );
        let transcript =
            capture_attach_transcript(&mut alpha_screen, &mut alpha_parser, &pending_after_q)?;
        pending_after_q.clear();
        let panes = transcript_without_status_line(&transcript);
        if alpha_marker_refs
            .iter()
            .all(|marker| panes.contains(marker))
            && alpha_command_refs
                .iter()
                .all(|command| panes.contains(command))
            && !panes.contains("sort: index")
        {
            break transcript;
        }
        if std::time::Instant::now() >= deadline {
            panic!(
                "choose-tree q after resize did not settle back to panes before timeout, got:\n{panes}"
            );
        }
        std::thread::sleep(Duration::from_millis(50));
        pending_after_q = drain_attach_output_bytes(alpha.master_mut())?;
    };

    let mut beta = AttachedSession::spawn(&harness, "beta", initial_size)?;
    beta.wait_for_raw_mode(IO_TIMEOUT)?;
    let beta_initial = read_until_contains(beta.master_mut(), "tester@RMUXHOST", IO_TIMEOUT)?;
    std::thread::sleep(Duration::from_millis(200));
    let mut beta_initial_bytes = beta_initial.into_bytes();
    beta_initial_bytes.extend(drain_attach_output_bytes(beta.master_mut())?);

    let mut beta_screen = Screen::new(initial_screen_size, 0);
    let mut beta_parser = InputParser::new();
    let _ = capture_attach_transcript(&mut beta_screen, &mut beta_parser, &beta_initial_bytes)?;

    beta.send_bytes(b"\x02%\x02%\x02\"")?;
    let beta_indexes = wait_for_pane_count(&harness, "beta", 4)?;
    let _ = apply_quiescent_attach_output(
        &mut beta,
        &mut beta_screen,
        &mut beta_parser,
        Duration::from_millis(300),
    )?;
    for pane_index in &beta_indexes {
        assert_success(&harness.run(&[
            "send-keys",
            "-t",
            &format!("beta:0.{pane_index}"),
            "export PS1='p$ '",
            "Enter",
            "clear",
            "Enter",
            &format!("echo A{pane_index}"),
            "Enter",
        ])?);
    }
    beta.send_bytes(b"\x02r")?;
    let beta_marker = beta_indexes
        .last()
        .map(|pane_index| format!("A{pane_index}"))
        .ok_or("missing beta pane indexes after split sequence")?;
    let beta_markers = beta_indexes
        .iter()
        .map(|pane_index| format!("A{pane_index}"))
        .collect::<Vec<_>>();
    let beta_layout_output = read_until_contains(beta.master_mut(), &beta_marker, IO_TIMEOUT)?;
    std::thread::sleep(Duration::from_millis(200));
    let mut beta_layout_bytes = beta_layout_output.into_bytes();
    beta_layout_bytes.extend(drain_attach_output_bytes(beta.master_mut())?);
    let _ = capture_attach_transcript(&mut beta_screen, &mut beta_parser, &beta_layout_bytes)?;

    let beta_layout_before_resize = window_layout(&harness, "beta")?;
    beta.resize(resized_size)?;
    let _beta_layout_after_resize =
        wait_for_layout_change(&harness, "beta", &beta_layout_before_resize)?;
    beta_screen.resize(resized_screen_size);
    std::thread::sleep(Duration::from_millis(1000));
    let beta_marker_refs = beta_markers.iter().map(String::as_str).collect::<Vec<_>>();
    let beta_command_refs = beta_indexes
        .iter()
        .map(|pane_index| format!("p$ echo A{pane_index}"))
        .collect::<Vec<_>>();
    let deadline = std::time::Instant::now() + IO_TIMEOUT;
    let beta_expected = loop {
        let beta_resize_bytes = drain_attach_output_bytes(beta.master_mut())?;
        if !beta_resize_bytes.is_empty() {
            let transcript =
                capture_attach_transcript(&mut beta_screen, &mut beta_parser, &beta_resize_bytes)?;
            let panes = transcript_without_status_line(&transcript);
            if beta_marker_refs.iter().all(|marker| panes.contains(marker))
                && beta_command_refs
                    .iter()
                    .all(|command| panes.contains(command))
                && !panes.contains("[beta]")
            {
                break transcript;
            }
        }
        if std::time::Instant::now() >= deadline {
            let transcript = beta_screen.capture_transcript(Default::default(), Default::default());
            let panes = String::from_utf8_lossy(&transcript);
            panic!("plain resized layout did not settle before timeout, got:\n{panes}");
        }
        std::thread::sleep(Duration::from_millis(50));
    };

    assert_eq!(
        list_pane_geometry(&harness, "alpha")?,
        list_pane_geometry(&harness, "beta")?,
        "choose-tree resize should not change the underlying pane geometry"
    );
    let alpha_panes = transcript_without_status_line(&alpha_restored);
    let beta_panes = transcript_without_status_line(&beta_expected);
    assert_eq!(
        alpha_panes, beta_panes,
        "choose-tree q after resize should restore the same resized layout as a plain resize\n--- expected ---\n{beta_panes}\n--- actual ---\n{alpha_panes}"
    );

    assert_success(&harness.run(&["kill-session", "-t", "alpha"])?);
    let alpha_status = alpha.wait_for_exit(IO_TIMEOUT)?;
    assert_eq!(alpha_status.code(), Some(0));
    alpha.assert_restored()?;

    assert_success(&harness.run(&["kill-session", "-t", "beta"])?);
    let beta_status = beta.wait_for_exit(IO_TIMEOUT)?;
    assert_eq!(beta_status.code(), Some(0));
    beta.assert_restored()?;

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn choose_tree_preview_gutter_uses_tmux_margin_when_columns_overflow() -> Result<(), Box<dyn Error>>
{
    let harness = CliHarness::new("attach-flow-choose-tree-preview-gutter")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    let mut attach = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(80, 24))?;
    attach.wait_for_raw_mode(IO_TIMEOUT)?;
    let initial = read_until_contains(attach.master_mut(), "tester@RMUXHOST", IO_TIMEOUT)?;
    std::thread::sleep(Duration::from_millis(200));
    let mut initial_bytes = initial.into_bytes();
    initial_bytes.extend(drain_attach_output_bytes(attach.master_mut())?);

    let mut screen = Screen::new(ScreenTerminalSize { cols: 80, rows: 24 }, 0);
    let mut parser = InputParser::new();
    let _ = capture_attach_transcript(&mut screen, &mut parser, &initial_bytes)?;

    attach.send_bytes(b"\x02%\x02%\x02\"")?;
    let pane_count_deadline = std::time::Instant::now() + IO_TIMEOUT;
    loop {
        let output = harness.run(&["list-panes", "-t", "alpha", "-F", "#{pane_index}"])?;
        if common::stdout(&output).lines().count() == 4 {
            break;
        }
        if std::time::Instant::now() >= pane_count_deadline {
            return Err("timed out waiting for four panes after prefix % % \"".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let pane_indexes =
        common::stdout(&harness.run(&["list-panes", "-t", "alpha", "-F", "#{pane_index}"])?)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>();
    let _ = apply_quiescent_attach_output(
        &mut attach,
        &mut screen,
        &mut parser,
        Duration::from_millis(300),
    )?;
    for pane_index in pane_indexes.iter().take(3) {
        let target = format!("alpha:0.{pane_index}");
        let marker = format!("P{pane_index}");
        send_shell_marker(&harness, &target, &marker)?;
    }
    let marker = pane_indexes
        .iter()
        .take(3)
        .nth(2)
        .map(|pane_index| format!("P{pane_index}"))
        .ok_or("missing pane indexes after split sequence")?;
    let marker_output = read_until_contains(attach.master_mut(), &marker, IO_TIMEOUT)?;
    std::thread::sleep(Duration::from_millis(200));
    let mut marker_bytes = marker_output.into_bytes();
    marker_bytes.extend(drain_attach_output_bytes(attach.master_mut())?);
    let _ = capture_attach_transcript(&mut screen, &mut parser, &marker_bytes)?;
    let marker_refs = pane_indexes
        .iter()
        .take(3)
        .map(|pane_index| format!("P{pane_index}"))
        .collect::<Vec<_>>();
    let marker_ref_slices = marker_refs.iter().map(String::as_str).collect::<Vec<_>>();
    let _ = wait_for_attach_transcript_matching(
        &mut attach,
        &mut screen,
        &mut parser,
        IO_TIMEOUT,
        "pane markers before choose-tree preview",
        |content| {
            marker_ref_slices
                .iter()
                .all(|marker| content.contains(marker))
        },
    )?;

    attach.send_bytes(b"\x02w")?;
    let _ = read_until_contains(attach.master_mut(), "sort: index", IO_TIMEOUT)?;
    std::thread::sleep(Duration::from_millis(100));
    let tree_bytes = drain_attach_output_bytes(attach.master_mut())?;
    let _ = capture_attach_transcript(&mut screen, &mut parser, &tree_bytes)?;
    let tree = wait_for_attach_transcript_matching(
        &mut attach,
        &mut screen,
        &mut parser,
        IO_TIMEOUT,
        "choose-tree preview with left chevron",
        |content| {
            content.contains("sort: index") && content.lines().any(line_has_isolated_left_chevron)
        },
    )?;
    let Some(chevron_line) = tree
        .lines()
        .find(|line| line_has_isolated_left_chevron(line))
    else {
        return Err(format!(
            "choose-tree preview did not render an isolated left chevron:\n{tree}"
        )
        .into());
    };
    assert!(
        !chevron_line.contains("│<") && !chevron_line.contains("<│"),
        "choose-tree preview gutter should not glue the left chevron to the box border, got:\n{tree}"
    );

    assert_success(&harness.run(&["kill-session", "-t", "alpha"])?);
    let status = attach.wait_for_exit(IO_TIMEOUT)?;
    assert_eq!(status.code(), Some(0));
    attach.assert_restored()?;

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn prefix_x_kills_the_current_pane_on_the_real_attached_client() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("attach-flow-prefix-x")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["split-window", "-h", "-t", "alpha:0.0"])?);
    assert_success(&harness.run(&["select-pane", "-t", "alpha:0.1"])?);

    let mut attach = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(80, 24))?;
    attach.wait_for_raw_mode(IO_TIMEOUT)?;
    let _ = read_until_contains(attach.master_mut(), "tester@RMUXHOST", IO_TIMEOUT)?;
    std::thread::sleep(Duration::from_millis(200));
    drain_attach_output(attach.master_mut())?;

    attach.send_bytes(b"\x02x")?;
    let prompt = read_until_contains(attach.master_mut(), "kill-pane", IO_TIMEOUT)?;
    assert!(
        prompt.contains("kill-pane 1? (y/n)"),
        "prefix x should open the kill-pane confirmation prompt, got: {prompt:?}"
    );

    attach.send_bytes(b"y")?;
    std::thread::sleep(Duration::from_millis(250));
    drain_attach_output(attach.master_mut())?;

    let panes = harness.run(&["list-panes", "-t", "alpha:0", "-F", "#{pane_index}"])?;
    let pane_lines = common::stdout(&panes)
        .lines()
        .filter(|line| !line.is_empty())
        .count();
    assert_eq!(
        pane_lines, 1,
        "confirming prefix x should kill the active pane"
    );

    assert_success(&harness.run(&["kill-session", "-t", "alpha"])?);
    let status = attach.wait_for_exit(IO_TIMEOUT)?;
    assert_eq!(status.code(), Some(0));
    attach.assert_restored()?;

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn prefix_ctrl_arrow_resizes_the_real_attached_pane() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("attach-flow-prefix-ctrl-arrow")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "leftcase"])?);
    assert_success(&harness.run(&["split-window", "-h", "-t", "leftcase:0.0"])?);
    assert_success(&harness.run(&["select-pane", "-t", "leftcase:0.1"])?);

    let mut left_attach = AttachedSession::spawn(&harness, "leftcase", TerminalSize::new(80, 24))?;
    left_attach.wait_for_raw_mode(IO_TIMEOUT)?;
    let _ = read_until_contains(left_attach.master_mut(), "tester@RMUXHOST", IO_TIMEOUT)?;
    std::thread::sleep(Duration::from_millis(200));
    drain_attach_output(left_attach.master_mut())?;

    let before_left = list_pane_widths(&harness, "leftcase:0")?;
    left_attach.send_bytes(b"\x02\x1b[1;5D")?;
    std::thread::sleep(Duration::from_millis(250));
    drain_attach_output(left_attach.master_mut())?;
    let after_left = list_pane_widths(&harness, "leftcase:0")?;
    assert!(
        after_left[0].1 < before_left[0].1 && after_left[1].1 > before_left[1].1,
        "prefix Ctrl-Left should expand the active right pane leftwards, before={before_left:?} after={after_left:?}"
    );

    assert_success(&harness.run(&["kill-session", "-t", "leftcase"])?);
    let left_status = left_attach.wait_for_exit(IO_TIMEOUT)?;
    assert_eq!(left_status.code(), Some(0));
    left_attach.assert_restored()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "rightcase"])?);
    assert_success(&harness.run(&["split-window", "-h", "-t", "rightcase:0.0"])?);
    assert_success(&harness.run(&["select-pane", "-t", "rightcase:0.0"])?);

    let mut right_attach =
        AttachedSession::spawn(&harness, "rightcase", TerminalSize::new(80, 24))?;
    right_attach.wait_for_raw_mode(IO_TIMEOUT)?;
    let _ = read_until_contains(right_attach.master_mut(), "tester@RMUXHOST", IO_TIMEOUT)?;
    std::thread::sleep(Duration::from_millis(200));
    drain_attach_output(right_attach.master_mut())?;

    let before_right = list_pane_widths(&harness, "rightcase:0")?;
    right_attach.send_bytes(b"\x02\x1b[1;5C")?;
    std::thread::sleep(Duration::from_millis(250));
    drain_attach_output(right_attach.master_mut())?;
    let after_right = list_pane_widths(&harness, "rightcase:0")?;
    assert!(
        after_right[0].1 > before_right[0].1 && after_right[1].1 < before_right[1].1,
        "prefix Ctrl-Right should expand the active left pane rightwards, before={before_right:?} after={after_right:?}"
    );

    assert_success(&harness.run(&["kill-session", "-t", "rightcase"])?);
    let right_status = right_attach.wait_for_exit(IO_TIMEOUT)?;
    assert_eq!(right_status.code(), Some(0));
    right_attach.assert_restored()?;

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn prefix_meta_digits_select_layouts_on_the_real_attached_client() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("attach-flow-prefix-meta-digits")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["split-window", "-v", "-t", "alpha:0.0"])?);
    assert_success(&harness.run(&["split-window", "-v", "-t", "alpha:0.0"])?);

    let mut attach = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(80, 24))?;
    attach.wait_for_raw_mode(IO_TIMEOUT)?;
    let _ = read_until_contains(attach.master_mut(), "tester@RMUXHOST", IO_TIMEOUT)?;
    std::thread::sleep(Duration::from_millis(200));
    drain_attach_output(attach.master_mut())?;

    for (bytes, expected_layout, starting_layout) in [
        (b"\x02\x1b1".as_slice(), "even-horizontal", "tiled"),
        (b"\x02\x1b2".as_slice(), "even-vertical", "tiled"),
        (b"\x02\x1b3".as_slice(), "main-horizontal", "tiled"),
        (b"\x02\x1b4".as_slice(), "main-vertical", "tiled"),
        (b"\x02\x1b5".as_slice(), "tiled", "even-horizontal"),
    ] {
        assert_success(&harness.run(&["select-layout", "-t", "alpha:0", expected_layout])?);
        let expected_dump = window_layout(&harness, "alpha")?;
        std::thread::sleep(Duration::from_millis(100));
        drain_attach_output(attach.master_mut())?;

        assert_success(&harness.run(&["select-layout", "-t", "alpha:0", starting_layout])?);
        std::thread::sleep(Duration::from_millis(300));
        drain_attach_output(attach.master_mut())?;
        attach.send_bytes(bytes)?;
        let deadline = std::time::Instant::now() + IO_TIMEOUT;
        let actual_dump = loop {
            drain_attach_output(attach.master_mut())?;
            let actual_dump = window_layout(&harness, "alpha")?;
            if actual_dump == expected_dump || std::time::Instant::now() >= deadline {
                break actual_dump;
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        assert_eq!(
            actual_dump, expected_dump,
            "prefix meta digit {:?} should match select-layout {expected_layout}, got {actual_dump:?}",
            bytes
        );
    }

    assert_success(&harness.run(&["kill-session", "-t", "alpha"])?);
    let status = attach.wait_for_exit(IO_TIMEOUT)?;
    assert_eq!(status.code(), Some(0));
    attach.assert_restored()?;

    terminate_child(daemon.child_mut())?;
    Ok(())
}
