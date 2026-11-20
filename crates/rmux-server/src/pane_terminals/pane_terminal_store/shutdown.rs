use super::{PaneTerminal, PaneTerminalStore};

impl PaneTerminalStore {
    pub(in crate::pane_terminals) fn shutdown_processes(&mut self) -> std::io::Result<()> {
        PaneTerminal::shutdown_all(
            self.sessions
                .values_mut()
                .flat_map(|panes| panes.values_mut()),
        )
    }
}
