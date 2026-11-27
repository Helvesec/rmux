use std::io;

use super::pane_terminal_store::PaneTerminalStore;
use super::HandlerState;

/// Owns every terminal removed from the closing daemon, including linked panes.
pub(crate) struct PaneTerminalShutdown {
    terminals: PaneTerminalStore,
}

impl HandlerState {
    pub(crate) fn take_terminal_processes_for_shutdown(&mut self) -> PaneTerminalShutdown {
        // Stop readers before releasing their terminal owners. Taking the whole
        // store also covers aliases and state retained by another handler clone.
        #[cfg(unix)]
        self.pane_output_readers.clear();
        PaneTerminalShutdown {
            terminals: std::mem::take(&mut self.terminals),
        }
    }
}

impl PaneTerminalShutdown {
    pub(crate) async fn finish(self) -> io::Result<()> {
        // Join teardown while the old endpoint is reserved, off the async worker
        // cancelling readers. Unix shares a single grace across all PTYs;
        // Windows retains its existing ConPTY/Job destruction policy.
        let Self { terminals } = self;
        tokio::task::spawn_blocking(move || {
            #[cfg(unix)]
            let terminals = {
                let mut terminals = terminals;
                terminals.shutdown_processes()?;
                terminals
            };
            drop(terminals);
            Ok(())
        })
        .await
        .map_err(io::Error::other)?
    }
}
