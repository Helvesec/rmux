use std::io;
use std::time::Instant;

use super::{
    poll_exit_until, PaneProcess, PaneTerminal, Signal, GRACEFUL_TERMINATION_TIMEOUT,
    HARD_TERMINATION_TIMEOUT,
};

/// One live process tree that the daemon shutdown still has to reap.
struct PendingReap<'a> {
    process: &'a mut PaneProcess,
    settled: bool,
}

impl PendingReap<'_> {
    /// Observes exit without blocking. As in `PaneProcess::wait_for_exit`, a
    /// failed observation means the child can no longer be waited on, so it is
    /// settled rather than treated as still running.
    fn observe(&mut self) {
        match self.process.try_wait_for_terminal_exit() {
            Ok(Some(status)) => {
                self.process.exit_status = Some(status);
                self.settled = true;
            }
            Ok(None) => {}
            Err(_) => self.settled = true,
        }
    }
}

impl PaneTerminal {
    /// Reaps every live pane process with one grace and one hard wait for the
    /// whole daemon, independent of its pane count. Returns `TimedOut` when some
    /// tree outlived the hard wait after `KILL`; the wait never blocks beyond it.
    pub(crate) fn shutdown_all<'a>(
        terminals: impl Iterator<Item = &'a mut Self>,
    ) -> io::Result<()> {
        let mut pending: Vec<PendingReap<'_>> = terminals
            .filter_map(|terminal| terminal.process.as_mut())
            .filter(|process| process.exit_status.is_none())
            .map(|process| PendingReap {
                process,
                settled: false,
            })
            .collect();

        // No child is waited on until every process tree received HUP.
        for entry in &mut pending {
            entry.process.termination_attempted = true;
            entry.process.signal_process_tree(Signal::HUP);
        }
        if reap_until(&mut pending, Instant::now() + GRACEFUL_TERMINATION_TIMEOUT) {
            return Ok(());
        }

        // Escalate every remaining tree before the single shared hard wait.
        for entry in pending.iter().filter(|entry| !entry.settled) {
            entry.process.signal_process_tree(Signal::KILL);
        }
        if reap_until(&mut pending, Instant::now() + HARD_TERMINATION_TIMEOUT) {
            return Ok(());
        }

        let unreaped = pending.iter().filter(|entry| !entry.settled).count();
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!("{unreaped} pane process(es) outlived the hard termination timeout"),
        ))
    }
}

/// Polls every unsettled entry until all are settled or `deadline` passes.
/// Returns whether every entry settled.
fn reap_until(pending: &mut [PendingReap<'_>], deadline: Instant) -> bool {
    poll_exit_until(deadline, std::thread::sleep, || {
        for entry in pending.iter_mut().filter(|entry| !entry.settled) {
            entry.observe();
        }
        pending.iter().all(|entry| entry.settled)
    })
}

#[cfg(test)]
#[path = "shutdown_tests.rs"]
mod tests;
