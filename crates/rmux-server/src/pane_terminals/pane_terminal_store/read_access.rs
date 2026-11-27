use super::*;
#[cfg(unix)]
use std::path::PathBuf;

impl PaneTerminalStore {
    pub(in crate::pane_terminals) fn pane_pid(
        &self,
        session_name: &SessionName,
        pane_id: PaneId,
        window_index: u32,
        pane_index: u32,
    ) -> Result<u32, RmuxError> {
        let terminal = self
            .sessions
            .get(session_name)
            .and_then(|panes| panes.get(&pane_id))
            .ok_or_else(|| missing_pane_terminal(session_name, window_index, pane_index))?;
        Ok(terminal.pid())
    }

    #[cfg(unix)]
    pub(in crate::pane_terminals) fn pane_tty_path(
        &self,
        session_name: &SessionName,
        pane_id: PaneId,
        window_index: u32,
        pane_index: u32,
    ) -> Result<PathBuf, RmuxError> {
        let terminal = self
            .sessions
            .get(session_name)
            .and_then(|panes| panes.get(&pane_id))
            .ok_or_else(|| missing_pane_terminal(session_name, window_index, pane_index))?;
        terminal
            .tty_path()
            .ok_or_else(|| missing_pane_terminal(session_name, window_index, pane_index))
    }

    pub(in crate::pane_terminals) fn pane_profile(
        &self,
        session_name: &SessionName,
        pane_id: PaneId,
        window_index: u32,
        pane_index: u32,
    ) -> Result<&TerminalProfile, RmuxError> {
        let session_panes = self
            .sessions
            .get(session_name)
            .ok_or_else(|| RmuxError::SessionNotFound(session_name.to_string()))?;
        let pane = session_panes
            .get(&pane_id)
            .ok_or_else(|| missing_pane_terminal(session_name, window_index, pane_index))?;

        Ok(pane.profile())
    }

    pub(in crate::pane_terminals) fn session_base_environment(
        &self,
        session_name: &SessionName,
    ) -> Option<SessionBaseEnvironment> {
        self.session_environments.get(session_name).cloned()
    }

    pub(in crate::pane_terminals) fn pane_base_environment(
        &self,
        session_name: &SessionName,
        pane_id: PaneId,
    ) -> Option<SessionBaseEnvironment> {
        self.sessions
            .get(session_name)?
            .get(&pane_id)
            .map(|pane| SessionBaseEnvironment::from_profile(pane.profile()))
    }

    pub(in crate::pane_terminals) fn pane_runtime_window_name(
        &self,
        session_name: &SessionName,
        pane_id: PaneId,
        window_index: u32,
        pane_index: u32,
    ) -> Result<Option<&str>, RmuxError> {
        let session_panes = self
            .sessions
            .get(session_name)
            .ok_or_else(|| RmuxError::SessionNotFound(session_name.to_string()))?;
        let pane = session_panes
            .get(&pane_id)
            .ok_or_else(|| missing_pane_terminal(session_name, window_index, pane_index))?;

        Ok(pane.runtime_window_name())
    }
}
