use std::collections::HashMap;
#[cfg(unix)]
use std::os::fd::BorrowedFd;
#[cfg(unix)]
use std::path::PathBuf;

use rmux_core::PaneId;
use rmux_proto::{PaneTarget, RmuxError, SessionName};
use rmux_pty::PtyMaster;

use crate::pane_terminal_lookup::{missing_pane_terminal, pane_id_for_target};
use crate::terminal::TerminalProfile;

use super::{
    pane_terminal_geometry_for_session, session_not_found, HandlerState, PaneExitMetadata,
};

impl HandlerState {
    pub(crate) fn set_pane_input_disabled(
        &mut self,
        target: &PaneTarget,
        disabled: bool,
    ) -> Result<(), RmuxError> {
        let pane_id = pane_id_for_target(
            &self.sessions,
            target.session_name(),
            target.window_index(),
            target.pane_index(),
        )?;
        if disabled {
            self.input_disabled_panes.insert(pane_id);
        } else {
            self.input_disabled_panes.remove(&pane_id);
        }
        Ok(())
    }

    pub(crate) fn pane_input_is_disabled(&self, pane_id: PaneId) -> bool {
        self.input_disabled_panes.contains(&pane_id)
    }

    pub(crate) fn window_index_for_pane_id(
        &self,
        session_name: &SessionName,
        pane_id: PaneId,
    ) -> Option<u32> {
        self.sessions
            .session(session_name)
            .and_then(|session| session.window_index_for_pane_id(pane_id))
    }

    pub(crate) fn pane_target_for_runtime_pane(
        &self,
        runtime_session_name: &SessionName,
        pane_id: PaneId,
    ) -> Option<PaneTarget> {
        let mut sessions = self
            .sessions
            .iter()
            .map(|(session_name, _)| session_name.clone())
            .collect::<Vec<_>>();
        sessions.sort_by(|left, right| left.as_str().cmp(right.as_str()));

        for session_name in sessions {
            let Some(window_index) = self.window_index_for_pane_id(&session_name, pane_id) else {
                continue;
            };
            if self.runtime_session_name_for_window(&session_name, window_index)
                != *runtime_session_name
            {
                continue;
            }
            let pane_index = self
                .sessions
                .session(&session_name)
                .and_then(|session| session.window_at(window_index))
                .and_then(|window| {
                    window
                        .panes()
                        .iter()
                        .find(|pane| pane.id() == pane_id)
                        .map(|pane| pane.index())
                })?;
            return Some(PaneTarget::with_window(
                session_name,
                window_index,
                pane_index,
            ));
        }

        None
    }

    pub(crate) fn resolve_pane_event_runtime_session(
        &self,
        event_session_name: &SessionName,
        pane_id: PaneId,
        generation: Option<u64>,
    ) -> Option<SessionName> {
        if self.pane_output_generation_matches(event_session_name, pane_id, generation)
            && self
                .pane_target_for_runtime_pane(event_session_name, pane_id)
                .is_some()
        {
            return Some(event_session_name.clone());
        }

        self.runtime_session_candidates()
            .into_iter()
            .filter(|candidate| candidate != event_session_name)
            .find(|candidate| {
                self.pane_output_generation_matches(candidate, pane_id, generation)
                    && self
                        .pane_target_for_runtime_pane(candidate, pane_id)
                        .is_some()
            })
    }

    fn runtime_session_candidates(&self) -> Vec<SessionName> {
        let mut candidates = self
            .sessions
            .iter()
            .flat_map(|(session_name, session)| {
                session
                    .windows()
                    .keys()
                    .map(|window_index| {
                        self.runtime_session_name_for_window(session_name, *window_index)
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        candidates.dedup();
        candidates
    }

    pub(crate) fn contains_session_terminals(&self, session_name: &SessionName) -> bool {
        let runtime_session_name = self.runtime_session_name(session_name);
        if self.terminals.contains_session(&runtime_session_name) {
            return true;
        }
        #[cfg(windows)]
        {
            if self.starting_panes.contains_key(&runtime_session_name) {
                return true;
            }
        }
        false
    }

    pub(in crate::pane_terminals) fn session_pane_terminal_geometries_by_runtime(
        &self,
        session_name: &SessionName,
    ) -> Result<HashMap<SessionName, Vec<crate::pane_terminal_lookup::SessionPane>>, RmuxError>
    {
        let session = self
            .sessions
            .session(session_name)
            .ok_or_else(|| RmuxError::SessionNotFound(session_name.to_string()))?;

        let mut panes_by_runtime = HashMap::new();
        for (window_index, window) in session.windows() {
            let runtime_session_name =
                self.runtime_session_name_for_window(session_name, *window_index);
            let panes = panes_by_runtime
                .entry(runtime_session_name)
                .or_insert_with(Vec::new);
            panes.extend(window.panes().iter().map(|pane| {
                crate::pane_terminal_lookup::SessionPane {
                    id: pane.id(),
                    window_index: *window_index,
                    index: pane.index(),
                    geometry: pane_terminal_geometry_for_session(
                        session,
                        &self.options,
                        *window_index,
                        pane.geometry(),
                    ),
                }
            }));
        }

        Ok(panes_by_runtime)
    }

    pub(crate) fn ensure_panes_exist(
        &self,
        session_name: &SessionName,
        pane_ids: &[PaneId],
    ) -> Result<(), RmuxError> {
        let runtime_session_name = self.runtime_session_name(session_name);
        if self
            .terminals
            .ensure_panes_exist(&runtime_session_name, pane_ids)
            .is_ok()
        {
            return Ok(());
        }
        #[cfg(windows)]
        {
            let all_present = pane_ids.iter().copied().all(|pane_id| {
                self.terminals
                    .ensure_panes_exist(&runtime_session_name, &[pane_id])
                    .is_ok()
                    || self
                        .starting_panes
                        .get(&runtime_session_name)
                        .is_some_and(|panes| panes.contains_key(&pane_id))
            });
            if all_present {
                return Ok(());
            }
        }
        self.terminals
            .ensure_panes_exist(&runtime_session_name, pane_ids)
    }

    pub(crate) fn ensure_window_panes_exist(
        &self,
        session_name: &SessionName,
        window_index: u32,
        pane_ids: &[PaneId],
    ) -> Result<(), RmuxError> {
        let runtime_session_name = self.runtime_session_name_for_window(session_name, window_index);
        match self
            .terminals
            .ensure_panes_exist(&runtime_session_name, pane_ids)
        {
            Ok(()) => Ok(()),
            Err(error) => {
                #[cfg(windows)]
                {
                    let all_starting = pane_ids.iter().all(|pane_id| {
                        self.starting_panes
                            .get(&runtime_session_name)
                            .is_some_and(|panes| panes.contains_key(pane_id))
                    });
                    if all_starting {
                        return Ok(());
                    }
                }
                Err(error)
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn remove_pane_terminal(
        &mut self,
        session_name: &SessionName,
        pane_id: PaneId,
    ) -> bool {
        let runtime_session_name = self.runtime_session_name(session_name);
        self.remove_pane_terminal_from_runtime(&runtime_session_name, pane_id)
    }

    pub(in crate::pane_terminals) fn remove_pane_terminal_from_runtime(
        &mut self,
        runtime_session_name: &SessionName,
        pane_id: PaneId,
    ) -> bool {
        if let Some(pipe) = self.remove_pane_pipe(runtime_session_name, pane_id) {
            pipe.stop();
        }
        self.remove_pane_output(runtime_session_name, pane_id);
        if let Some(dead_panes) = self.dead_panes.get_mut(runtime_session_name) {
            let _ = dead_panes.remove(&pane_id);
        }
        self.clear_attached_submitted_line(runtime_session_name, pane_id);
        self.clear_marked_pane_if_id(pane_id);
        self.remove_pane_lifecycle(pane_id);
        let Some(terminal) = self.terminals.remove_pane(runtime_session_name, pane_id) else {
            return false;
        };
        terminal.terminate_in_background();
        true
    }

    #[cfg(unix)]
    pub(crate) fn pane_master_fd(
        &self,
        session_name: &SessionName,
        window_index: u32,
        pane_index: u32,
    ) -> Result<BorrowedFd<'_>, RmuxError> {
        let pane_id = pane_id_for_target(&self.sessions, session_name, window_index, pane_index)?;
        let runtime_session_name = self.runtime_session_name_for_window(session_name, window_index);
        self.terminals
            .pane_master_fd(&runtime_session_name, pane_id, window_index, pane_index)
    }

    pub(crate) fn clone_pane_master_if_alive(
        &mut self,
        session_name: &SessionName,
        window_index: u32,
        pane_index: u32,
    ) -> Result<PtyMaster, RmuxError> {
        let pane_id = pane_id_for_target(&self.sessions, session_name, window_index, pane_index)?;
        let runtime_session_name = self.runtime_session_name_for_window(session_name, window_index);
        self.terminals.clone_pane_master_if_alive(
            &runtime_session_name,
            pane_id,
            window_index,
            pane_index,
        )
    }

    pub(crate) fn clone_pane_master(
        &self,
        session_name: &SessionName,
        window_index: u32,
        pane_index: u32,
    ) -> Result<PtyMaster, RmuxError> {
        let pane_id = pane_id_for_target(&self.sessions, session_name, window_index, pane_index)?;
        let runtime_session_name = self.runtime_session_name_for_window(session_name, window_index);
        self.terminals
            .clone_pane_master(&runtime_session_name, pane_id, window_index, pane_index)
    }

    #[cfg(test)]
    pub(crate) fn start_pane_input_capture_for_test(&self, target: &PaneTarget) {
        self.pane_input_captures
            .lock()
            .expect("pane input capture mutex")
            .insert(target.to_string(), Vec::new());
    }

    #[cfg(test)]
    pub(crate) fn append_pane_input_capture_for_test(
        &self,
        target: &PaneTarget,
        bytes: &[u8],
    ) -> bool {
        let mut captures = self
            .pane_input_captures
            .lock()
            .expect("pane input capture mutex");
        let Some(captured) = captures.get_mut(&target.to_string()) else {
            return false;
        };
        captured.extend_from_slice(bytes);
        true
    }

    #[cfg(test)]
    pub(crate) fn pane_input_capture_for_test(&self, target: &PaneTarget) -> Option<Vec<u8>> {
        self.pane_input_captures
            .lock()
            .expect("pane input capture mutex")
            .get(&target.to_string())
            .cloned()
    }

    pub(crate) fn pane_pid_in_window(
        &self,
        session_name: &SessionName,
        window_index: u32,
        pane_index: u32,
    ) -> Result<u32, RmuxError> {
        let pane_id = pane_id_for_target(&self.sessions, session_name, window_index, pane_index)?;
        let runtime_session_name = self.runtime_session_name_for_window(session_name, window_index);
        #[cfg(windows)]
        if self.pane_is_starting_in_window(session_name, window_index, pane_index) {
            return Err(RmuxError::Server(format!(
                "pane {session_name}:{window_index}.{pane_index} is still starting"
            )));
        }
        self.terminals
            .pane_pid(&runtime_session_name, pane_id, window_index, pane_index)
    }

    #[cfg(unix)]
    pub(crate) fn pane_tty_path_in_window(
        &self,
        session_name: &SessionName,
        window_index: u32,
        pane_index: u32,
    ) -> Result<PathBuf, RmuxError> {
        let pane_id = pane_id_for_target(&self.sessions, session_name, window_index, pane_index)?;
        let runtime_session_name = self.runtime_session_name_for_window(session_name, window_index);
        self.terminals
            .pane_tty_path(&runtime_session_name, pane_id, window_index, pane_index)
    }

    pub(crate) fn pane_exit_metadata(
        &self,
        session_name: &SessionName,
        pane_id: PaneId,
    ) -> Option<PaneExitMetadata> {
        let window_index = self.window_index_for_pane_id(session_name, pane_id)?;
        let runtime_session_name = self.runtime_session_name_for_window(session_name, window_index);
        self.dead_panes
            .get(&runtime_session_name)
            .and_then(|panes| panes.get(&pane_id))
            .copied()
    }

    pub(crate) fn pane_is_dead(&self, session_name: &SessionName, pane_id: PaneId) -> bool {
        self.pane_exit_metadata(session_name, pane_id).is_some()
    }

    pub(crate) fn pane_output_generation_matches(
        &self,
        session_name: &SessionName,
        pane_id: PaneId,
        generation: Option<u64>,
    ) -> bool {
        match generation {
            None => true,
            Some(generation) => self
                .pane_output_generations
                .get(session_name)
                .and_then(|panes| panes.get(&pane_id))
                .is_some_and(|current| *current == generation),
        }
    }

    pub(crate) fn active_pane_master(
        &self,
        session_name: &SessionName,
    ) -> Result<PtyMaster, RmuxError> {
        let session = self
            .sessions
            .session(session_name)
            .ok_or_else(|| session_not_found(session_name))?;
        let window_index = session.active_window_index();
        let pane_index = session.active_pane_index();
        let pane_id = session
            .active_pane()
            .map(|pane| pane.id())
            .ok_or_else(|| missing_pane_terminal(session_name, window_index, pane_index))?;
        let runtime_session_name = self.runtime_session_name_for_window(session_name, window_index);

        self.terminals
            .clone_pane_master(&runtime_session_name, pane_id, window_index, pane_index)
    }

    #[cfg(test)]
    pub(crate) fn pane_profile(
        &self,
        session_name: &SessionName,
        pane_index: u32,
    ) -> Result<&TerminalProfile, RmuxError> {
        self.pane_profile_in_window(session_name, 0, pane_index)
    }

    pub(crate) fn pane_profile_in_window(
        &self,
        session_name: &SessionName,
        window_index: u32,
        pane_index: u32,
    ) -> Result<&TerminalProfile, RmuxError> {
        let pane_id = pane_id_for_target(&self.sessions, session_name, window_index, pane_index)?;
        let runtime_session_name = self.runtime_session_name_for_window(session_name, window_index);
        #[cfg(windows)]
        {
            if let Some(profile) =
                self.starting_pane_profile_in_window(session_name, window_index, pane_index)
            {
                return Ok(profile);
            }
        }
        self.terminals
            .pane_profile(&runtime_session_name, pane_id, window_index, pane_index)
    }

    pub(crate) fn pane_runtime_window_name_in_window(
        &self,
        session_name: &SessionName,
        window_index: u32,
        pane_index: u32,
    ) -> Result<Option<String>, RmuxError> {
        let pane_id = pane_id_for_target(&self.sessions, session_name, window_index, pane_index)?;
        let runtime_session_name = self.runtime_session_name_for_window(session_name, window_index);
        #[cfg(windows)]
        {
            if let Some(name) = self.starting_pane_runtime_window_name_in_window(
                session_name,
                window_index,
                pane_index,
            ) {
                return Ok(Some(name.to_owned()));
            }
        }
        self.terminals
            .pane_runtime_window_name(&runtime_session_name, pane_id, window_index, pane_index)
            .map(|value| value.map(str::to_owned))
    }

    #[cfg(test)]
    pub(crate) fn fail_next_resize_for_test(&mut self) {
        self.terminals.fail_next_resize_for_test();
    }
}
