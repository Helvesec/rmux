use std::collections::HashMap;

use rmux_core::PaneId;
use rmux_proto::{PaneTarget, RmuxError, SessionName};

use super::pane_pipe::ActivePanePipe;
use super::HandlerState;
use crate::pane_terminal_lookup::pane_id_for_target;

impl HandlerState {
    pub(crate) fn pipe_pane(
        &mut self,
        target: PaneTarget,
        command: Option<String>,
        read_from_pipe: bool,
        write_to_pipe: bool,
        once: bool,
    ) -> Result<rmux_proto::PipePaneResponse, RmuxError> {
        let session_name = target.session_name().clone();
        let window_index = target.window_index();
        let pane_index = target.pane_index();
        let pane_id = pane_id_for_target(&self.sessions, &session_name, window_index, pane_index)?;
        let runtime_session_name =
            self.runtime_session_name_for_window(&session_name, window_index);

        let existing_pipe = self.remove_pane_pipe(&runtime_session_name, pane_id);
        let had_existing_pipe = existing_pipe.is_some();
        if let Some(pipe) = existing_pipe {
            pipe.stop();
        }

        if once && had_existing_pipe {
            return Ok(rmux_proto::PipePaneResponse { target });
        }

        let Some(command) = command.filter(|command| !command.is_empty()) else {
            return Ok(rmux_proto::PipePaneResponse { target });
        };

        let pane_master =
            self.clone_pane_master_if_alive(&session_name, window_index, pane_index)?;
        let pane_output = self.pane_output_for_target(&session_name, window_index, pane_index)?;
        let profile = self
            .terminals
            .pane_profile(&runtime_session_name, pane_id, window_index, pane_index)?
            .clone();
        let pipe = ActivePanePipe::spawn(
            &profile,
            pane_output,
            pane_master,
            &command,
            read_from_pipe,
            write_to_pipe,
        )?;
        if let Some(previous) = self.pipes.insert(&runtime_session_name, pane_id, pipe) {
            previous.stop();
        }

        Ok(rmux_proto::PipePaneResponse { target })
    }

    pub(crate) fn pane_has_pipe(
        &self,
        session_name: &SessionName,
        window_index: u32,
        pane_id: PaneId,
    ) -> bool {
        let runtime_session_name = self.runtime_session_name_for_window(session_name, window_index);
        self.pipes.contains(&runtime_session_name, pane_id)
    }

    pub(in crate::pane_terminals) fn remove_session_pipes(
        &mut self,
        session_name: &SessionName,
    ) -> HashMap<PaneId, ActivePanePipe> {
        self.pipes.remove_session(session_name)
    }

    pub(in crate::pane_terminals) fn remove_pane_pipe(
        &mut self,
        session_name: &SessionName,
        pane_id: PaneId,
    ) -> Option<ActivePanePipe> {
        self.pipes.remove(session_name, pane_id)
    }
}
