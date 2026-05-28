#![cfg(unix)]

mod common;

use std::error::Error;
use std::time::Duration;

use common::{assert_success, read_until_contains, terminate_child, AttachedSession, CliHarness};
use rmux_pty::TerminalSize;

const IO_TIMEOUT: Duration = Duration::from_secs(5);
const SIXEL_SEQUENCE: &str = "\x1bPq#0!10~\x1b\\";

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
