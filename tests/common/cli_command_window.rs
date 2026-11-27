use std::error::Error;
use std::process::{Command, Output, Stdio};

use super::{acquire_cli_command_lock, CliCommandLock, CliHarness};

/// Commands of one [`CliHarness`] that hold the cross-process CLI command lock
/// together, from before a test arms a deadline until it has acted on it.
///
/// A test that arms a time-bounded wait (a background `wait-pane` or
/// `send-keys --wait`, or a pane with a fixed lifetime) and must then act on
/// the daemon before that deadline cannot let each action queue for the lock
/// behind the commands of other test processes: under parallel load that
/// queueing alone exceeds the deadline. Inside a window the lock is taken
/// once, before the deadline exists, so the deadline only measures the daemon.
/// Drop the window before waiting for the outcome, so other tests can proceed.
pub(crate) struct CliCommandWindow<'harness> {
    harness: &'harness CliHarness,
    _lock: CliCommandLock,
}

impl CliHarness {
    /// Takes the CLI command lock for a window of commands. The exclusive
    /// borrow keeps [`CliHarness::run`], which would wait on this very lock,
    /// out of the window.
    pub(crate) fn command_window(&mut self) -> Result<CliCommandWindow<'_>, Box<dyn Error>> {
        Ok(CliCommandWindow {
            harness: self,
            _lock: acquire_cli_command_lock()?,
        })
    }
}

impl CliCommandWindow<'_> {
    /// Runs one CLI command to completion under the window's lock.
    pub(crate) fn run(&self, args: &[&str]) -> Result<Output, Box<dyn Error>> {
        Ok(self
            .base_command()
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()?)
    }

    /// The harness command, for a background command armed inside the window.
    pub(crate) fn base_command(&self) -> Command {
        self.harness.base_command()
    }
}
