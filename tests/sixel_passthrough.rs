#![cfg(unix)]

mod common;

use std::error::Error;
use std::time::Duration;

use common::{
    assert_success, drain_attach_output_bytes, read_until_contains, terminate_child,
    AttachedSession, CliHarness,
};
use rmux_pty::TerminalSize;

const IO_TIMEOUT: Duration = Duration::from_secs(5);
const SIXEL_SEQUENCE: &str = "\x1bPq#0!10~\x1b\\";
const OSC52_SEQUENCE: &str = "\x1b]52;c;QQ==\x07";

#[test]
fn attach_pty_forwards_sixel_when_passthrough_all_is_enabled() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("sixel-attach-pty")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["set-option", "-g", "allow-passthrough", "all"])?);

    let mut attach = AttachedSession::spawn_with_env(
        &harness,
        "alpha",
        TerminalSize::new(100, 30),
        &[("TERM", "foot")],
    )?;
    attach.wait_for_raw_mode(IO_TIMEOUT)?;
    let _ = read_until_contains(attach.master_mut(), "[alpha]", IO_TIMEOUT)?;

    assert_success(&harness.run(&[
        "send-keys",
        "-t",
        "alpha:0.0",
        "printf '\\033Pq#0!10~\\033\\\\'",
        "Enter",
    ])?);
    let output = read_until_contains(attach.master_mut(), SIXEL_SEQUENCE, IO_TIMEOUT)?;
    assert!(
        output.contains(SIXEL_SEQUENCE),
        "attached PTY did not receive the raw SIXEL DCS sequence: {output:?}"
    );

    assert_success(&harness.run(&["kill-session", "-t", "alpha"])?);
    let status = attach.wait_for_exit(IO_TIMEOUT)?;
    assert_eq!(status.code(), Some(0));
    attach.assert_restored()?;

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn attach_pty_forwards_dcs_passthrough_when_passthrough_all_is_enabled(
) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("dcs-passthrough-attach-pty")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["set-option", "-g", "allow-passthrough", "all"])?);

    let mut attach = AttachedSession::spawn_with_env(
        &harness,
        "alpha",
        TerminalSize::new(100, 30),
        &[("TERM", "xterm-256color")],
    )?;
    attach.wait_for_raw_mode(IO_TIMEOUT)?;
    let _ = read_until_contains(attach.master_mut(), "[alpha]", IO_TIMEOUT)?;

    assert_success(&harness.run(&[
        "send-keys",
        "-t",
        "alpha:0.0",
        "printf '\\033Ptmux;\\033]52;c;QQ==\\007\\033\\\\'",
        "Enter",
    ])?);
    let output = read_until_contains(attach.master_mut(), OSC52_SEQUENCE, IO_TIMEOUT)?;
    assert!(
        output.contains(OSC52_SEQUENCE),
        "attached PTY did not receive the raw OSC52 passthrough sequence: {output:?}"
    );

    assert_success(&harness.run(&["kill-session", "-t", "alpha"])?);
    let status = attach.wait_for_exit(IO_TIMEOUT)?;
    assert_eq!(status.code(), Some(0));
    attach.assert_restored()?;

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn attach_pty_forwards_pane_osc52_when_clipboard_is_enabled() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("osc52-attach-pty")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["set-option", "-g", "set-clipboard", "on"])?);

    let mut attach = AttachedSession::spawn_with_env(
        &harness,
        "alpha",
        TerminalSize::new(100, 30),
        &[("TERM", "xterm-256color")],
    )?;
    attach.wait_for_raw_mode(IO_TIMEOUT)?;
    let _ = read_until_contains(attach.master_mut(), "[alpha]", IO_TIMEOUT)?;

    assert_success(&harness.run(&[
        "send-keys",
        "-t",
        "alpha:0.0",
        "printf '\\033]52;c;QQ==\\007'",
        "Enter",
    ])?);
    let mut output = read_until_contains(attach.master_mut(), OSC52_SEQUENCE, IO_TIMEOUT)?;
    std::thread::sleep(Duration::from_millis(150));
    output.push_str(&String::from_utf8_lossy(&drain_attach_output_bytes(
        attach.master_mut(),
    )?));
    assert!(
        output.contains(OSC52_SEQUENCE),
        "attached PTY did not receive pane-emitted OSC52 sequence: {output:?}"
    );
    assert_eq!(
        output.match_indices(OSC52_SEQUENCE).count(),
        1,
        "the active pane output ring must be the only OSC52 relay path: {output:?}"
    );

    assert_success(&harness.run(&["kill-session", "-t", "alpha"])?);
    let status = attach.wait_for_exit(IO_TIMEOUT)?;
    assert_eq!(status.code(), Some(0));
    attach.assert_restored()?;

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn attach_pty_suppresses_pane_osc52_when_clipboard_is_disabled() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("osc52-disabled-attach-pty")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["set-option", "-g", "set-clipboard", "off"])?);

    let mut attach = AttachedSession::spawn_with_env(
        &harness,
        "alpha",
        TerminalSize::new(100, 30),
        &[("TERM", "xterm-256color")],
    )?;
    attach.wait_for_raw_mode(IO_TIMEOUT)?;
    let _ = read_until_contains(attach.master_mut(), "[alpha]", IO_TIMEOUT)?;

    assert_success(&harness.run(&[
        "send-keys",
        "-t",
        "alpha:0.0",
        "printf '\\033]52;c;QQ==\\007'",
        "Enter",
    ])?);
    std::thread::sleep(Duration::from_millis(250));
    let output =
        String::from_utf8_lossy(&drain_attach_output_bytes(attach.master_mut())?).into_owned();
    assert!(
        !output.contains(OSC52_SEQUENCE),
        "disabled set-clipboard should suppress pane-emitted OSC52: {output:?}"
    );

    assert_success(&harness.run(&["kill-session", "-t", "alpha"])?);
    let status = attach.wait_for_exit(IO_TIMEOUT)?;
    assert_eq!(status.code(), Some(0));
    attach.assert_restored()?;

    terminate_child(daemon.child_mut())?;
    Ok(())
}
