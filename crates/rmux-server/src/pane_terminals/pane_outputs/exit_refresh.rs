use rmux_proto::{PaneTarget, RmuxError, SessionName, Target};

use crate::pane_terminal_lookup::pane_id_for_target;
use crate::pane_terminals::{session_not_found, HandlerState};

use super::PaneExitMetadata;

impl HandlerState {
    pub(crate) fn refresh_format_target_exit_status(
        &mut self,
        target: &Target,
    ) -> Result<(), RmuxError> {
        let Some(target) = self.format_target_active_pane(target)? else {
            return Ok(());
        };
        let _ = self.refresh_pane_exit_status(&target)?;
        Ok(())
    }

    pub(crate) fn refresh_list_panes_exit_statuses(
        &mut self,
        session_name: &SessionName,
        window_index: Option<u32>,
    ) -> Result<(), RmuxError> {
        let targets = self.pane_targets_for_listing(session_name, window_index)?;
        for target in targets {
            let _ = self.refresh_pane_exit_status(&target)?;
        }
        Ok(())
    }

    pub(crate) fn refresh_pane_exit_status(
        &mut self,
        target: &PaneTarget,
    ) -> Result<Option<PaneExitMetadata>, RmuxError> {
        let pane_id = pane_id_for_target(
            &self.sessions,
            target.session_name(),
            target.window_index(),
            target.pane_index(),
        )?;
        let runtime_session_name =
            self.runtime_session_name_for_window(target.session_name(), target.window_index());
        self.observe_runtime_pane_exit(&runtime_session_name, pane_id)
    }

    fn format_target_active_pane(&self, target: &Target) -> Result<Option<PaneTarget>, RmuxError> {
        match target {
            Target::Pane(target) => Ok(Some(target.clone())),
            Target::Window(target) => self.active_pane_target_for_window(
                target.session_name(),
                target.window_index(),
                &target.to_string(),
            ),
            Target::Session(session_name) => {
                let session = self
                    .sessions
                    .session(session_name)
                    .ok_or_else(|| session_not_found(session_name))?;
                self.active_pane_target_for_window(
                    session_name,
                    session.active_window_index(),
                    session_name.as_str(),
                )
            }
        }
    }

    fn active_pane_target_for_window(
        &self,
        session_name: &SessionName,
        window_index: u32,
        target_text: &str,
    ) -> Result<Option<PaneTarget>, RmuxError> {
        let session = self
            .sessions
            .session(session_name)
            .ok_or_else(|| session_not_found(session_name))?;
        let window = session.window_at(window_index).ok_or_else(|| {
            RmuxError::invalid_target(target_text, "window index does not exist in session")
        })?;
        Ok(window
            .active_pane()
            .map(|pane| PaneTarget::with_window(session_name.clone(), window_index, pane.index())))
    }

    fn pane_targets_for_listing(
        &self,
        session_name: &SessionName,
        window_index: Option<u32>,
    ) -> Result<Vec<PaneTarget>, RmuxError> {
        let session = self
            .sessions
            .session(session_name)
            .ok_or_else(|| session_not_found(session_name))?;
        let mut targets = Vec::new();
        for (&current_window_index, window) in session.windows() {
            if window_index.is_some_and(|index| index != current_window_index) {
                continue;
            }
            for pane in window.panes() {
                targets.push(PaneTarget::with_window(
                    session_name.clone(),
                    current_window_index,
                    pane.index(),
                ));
            }
        }
        Ok(targets)
    }
}
