#![cfg(unix)]
mod common;

use common::{
    assert_success, drain_attach_output, read_until_contains, read_until_contains_all,
    terminate_child, AttachedSession, CliHarness,
};
use rmux_pty::TerminalSize;
use std::error::Error;
use std::time::Duration;

#[test]
fn display_panes_cli_targets_a_client_in_another_session() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("display-panes-cross-session")?;
    let mut daemon = harness.start_hidden_daemon()?;
    for session in ["alpha", "beta"] {
        assert_success(&harness.run(&["new-session", "-d", "-s", session])?);
    }
    assert_success(&harness.run(&["split-window", "-h", "-t", "alpha:0.0"])?);
    assert_success(&harness.run(&["set", "-g", "display-panes-time", "5000"])?);
    let timeout = Duration::from_secs(10);
    let mut alpha = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(80, 24))?;
    alpha.wait_for_raw_mode(timeout)?;
    let _ = read_until_contains(alpha.master_mut(), "[alpha]", timeout)?;
    let mut beta = AttachedSession::spawn(&harness, "beta", TerminalSize::new(80, 24))?;
    beta.wait_for_raw_mode(timeout)?;
    let _ = read_until_contains(beta.master_mut(), "[beta]", timeout)?;
    drain_attach_output(alpha.master_mut())?;
    drain_attach_output(beta.master_mut())?;
    let client = alpha.child_mut().id().to_string();
    assert_success(&harness.run(&["display-panes", "-b", "-t", &client])?);
    let overlay = read_until_contains_all(alpha.master_mut(), &["\x1b[?25l", "x"], timeout)?;
    assert!(overlay.contains("\x1b[41m") || overlay.contains("\x1b[44m"));
    for session in ["alpha", "beta"] {
        assert_success(&harness.run(&["kill-session", "-t", session])?);
    }
    assert!(alpha.wait_for_exit(timeout)?.success());
    assert!(beta.wait_for_exit(timeout)?.success());
    alpha.assert_restored()?;
    beta.assert_restored()?;
    terminate_child(daemon.child_mut())?;
    Ok(())
}
