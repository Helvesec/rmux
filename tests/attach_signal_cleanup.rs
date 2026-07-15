#![cfg(unix)]

mod common;

use std::error::Error;
use std::os::unix::process::ExitStatusExt;
use std::time::Duration;

use common::{assert_success, AttachedSession, CliHarness};
use rmux_pty::TerminalSize;

const SIGNAL_EXIT_TIMEOUT: Duration = Duration::from_secs(5);

#[test]
fn sigterm_during_attach_restores_terminal_before_termination() -> Result<(), Box<dyn Error>> {
    assert_attach_signal_cleanup(libc::SIGTERM, "sigterm")
}

#[test]
fn sighup_during_attach_restores_terminal_before_termination() -> Result<(), Box<dyn Error>> {
    assert_attach_signal_cleanup(libc::SIGHUP, "sighup")
}

fn assert_attach_signal_cleanup(signal: i32, label: &str) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new(&format!("attach-{label}-cleanup"))?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-x", "80", "-y", "24"])?);

    let mut attach = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(80, 24))?;
    attach.wait_for_raw_mode(SIGNAL_EXIT_TIMEOUT)?;

    let pid = i32::try_from(attach.child_mut().id())?;
    // SAFETY: `pid` is the live attach child owned by this test and `signal` is
    // restricted by the two callers to SIGTERM or SIGHUP.
    if unsafe { libc::kill(pid, signal) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }

    let status = attach.wait_for_exit(SIGNAL_EXIT_TIMEOUT)?;
    assert_eq!(
        status.signal(),
        Some(signal),
        "attach must preserve termination by the received signal: {status}"
    );
    attach.assert_restored()?;
    Ok(())
}
