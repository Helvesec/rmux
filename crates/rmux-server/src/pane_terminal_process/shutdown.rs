use std::io;
use std::time::Instant;

use super::{poll_exit_until, PaneProcess, PaneTerminal, GRACEFUL_TERMINATION_TIMEOUT};

impl PaneTerminal {
    /// Close all terminals first, then allow one shared interval to reap their
    /// leaders. Handling HUP is allowed: shutdown never escalates to KILL.
    pub(crate) fn shutdown_all<'a>(
        terminals: impl Iterator<Item = &'a mut Self>,
    ) -> io::Result<()> {
        let mut children: Vec<_> = terminals
            .filter_map(|terminal| terminal.process.take())
            .map(PaneProcess::into_closed_child)
            .collect();
        poll_exit_until(
            Instant::now() + GRACEFUL_TERMINATION_TIMEOUT,
            std::thread::sleep,
            || {
                children.retain_mut(|child| matches!(child.try_wait(), Ok(None)));
                children.is_empty()
            },
        );
        for child in children {
            crate::unix_child_reaper::reap(child);
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "shutdown_tests.rs"]
mod tests;
