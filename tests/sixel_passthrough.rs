#![cfg(unix)]

mod common;

use std::error::Error;
use std::time::Duration;

use common::{
    assert_success, drain_attach_output_bytes, read_until_contains, read_until_contains_all,
    terminate_child, AttachedSession, CliHarness,
};
use rmux_pty::TerminalSize;

const IO_TIMEOUT: Duration = Duration::from_secs(5);
const SIXEL_SEQUENCE: &str = "\x1bPq#0!10~\x1b\\";
const OSC52_SEQUENCE: &str = "\x1b]52;c;QQ==\x07";
/// Printed through octal escapes so the shell's echo of the command line does
/// not itself contain the marker the ordering assertion searches for.
const SYNCHRONIZED_TEXT_OCTAL: &str = "\\106\\111\\116\\101\\114\\124\\105\\130\\124";
const SYNCHRONIZED_TEXT: &str = "FINALTEXT";

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

/// The child writes its final text and then a cursor-positioned SIXEL inside a
/// DEC 2026 interval, and releases the interval in a later write. Every render
/// reader resolves through the frozen presentation screen, so an image emitted
/// before the converged render lands on stale content that the following
/// erase-to-end then destroys. The client must observe the child's order.
#[test]
fn attach_pty_orders_synchronized_text_before_its_positioned_sixel() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("sixel-synchronized-order")?;
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
        &format!("printf '\\033[?2026h\\033[H{SYNCHRONIZED_TEXT_OCTAL}\\033Pq#0!10~\\033\\\\'"),
        "Enter",
    ])?);
    // Let the child's write land in its own read before the release is sent,
    // which is the boundary the defect needs and well inside the one-second
    // fail-open deadline.
    std::thread::sleep(Duration::from_millis(200));
    assert_success(&harness.run(&[
        "send-keys",
        "-t",
        "alpha:0.0",
        "printf '\\033[?2026l'",
        "Enter",
    ])?);

    let output = read_until_contains_all(
        attach.master_mut(),
        &[SYNCHRONIZED_TEXT, SIXEL_SEQUENCE],
        IO_TIMEOUT,
    )?;
    let text_at = output
        .find(SYNCHRONIZED_TEXT)
        .expect("read_until_contains_all waited for the converged text");
    let image_at = output
        .find(SIXEL_SEQUENCE)
        .expect("read_until_contains_all waited for the SIXEL");
    assert!(
        text_at < image_at,
        "the converged render must precede the image it was drawn under: text at {text_at}, \
         image at {image_at}, in {output:?}"
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
