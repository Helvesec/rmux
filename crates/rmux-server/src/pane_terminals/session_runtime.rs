use rmux_proto::{RmuxError, SessionName};

use super::{session_not_found, HandlerState};

impl HandlerState {
    pub(in crate::pane_terminals) fn runtime_session_name(
        &self,
        session_name: &SessionName,
    ) -> SessionName {
        self.sessions
            .runtime_owner(session_name)
            .unwrap_or_else(|| session_name.clone())
    }

    pub(crate) fn synchronize_session_group_from(
        &mut self,
        source_session_name: &SessionName,
    ) -> Result<Vec<SessionName>, RmuxError> {
        let source_session = self
            .sessions
            .session(source_session_name)
            .cloned()
            .ok_or_else(|| session_not_found(source_session_name))?;
        let group_members = self.sessions.session_group_members(source_session_name);
        if group_members.len() <= 1 {
            return Ok(group_members);
        }

        let mut synchronized = Vec::with_capacity(group_members.len());
        for member_name in group_members {
            if member_name == *source_session_name {
                synchronized.push(member_name);
                continue;
            }

            let member = self
                .sessions
                .session_mut(&member_name)
                .ok_or_else(|| session_not_found(&member_name))?;
            member.synchronize_group_from(&source_session);
            synchronized.push(member_name);
        }

        Ok(synchronized)
    }

    pub(crate) fn rename_session(
        &mut self,
        session_name: &SessionName,
        new_name: &SessionName,
    ) -> Result<(), RmuxError> {
        let mut completed = Vec::new();
        let runtime_session_name = self.runtime_session_name(session_name);

        self.sessions
            .rename_session(session_name, new_name.clone())?;
        completed.push(RenameSessionStep::Sessions);

        if let Err(error) = self.options.rename_session(session_name, new_name.clone()) {
            self.rollback_session_rename(&completed, session_name, new_name, &error)?;
            return Err(error);
        }
        completed.push(RenameSessionStep::Options);

        if let Err(error) = self
            .environment
            .rename_session(session_name, new_name.clone())
        {
            self.rollback_session_rename(&completed, session_name, new_name, &error)?;
            return Err(error);
        }
        completed.push(RenameSessionStep::Environment);

        if let Err(error) = self.hooks.rename_session(session_name, new_name.clone()) {
            self.rollback_session_rename(&completed, session_name, new_name, &error)?;
            return Err(error);
        }
        completed.push(RenameSessionStep::Hooks);

        if runtime_session_name == *session_name {
            if let Err(error) = self.terminals.rename_session(session_name, new_name) {
                self.rollback_session_rename(&completed, session_name, new_name, &error)?;
                return Err(error);
            }
            completed.push(RenameSessionStep::Terminals);
        }

        if runtime_session_name == *session_name
            && let Err(error) = self.rename_runtime_session_state(session_name, new_name) {
                self.rollback_session_rename(&completed, session_name, new_name, &error)?;
                return Err(error);
            }

        if let Some(pixels) = self.attached_terminal_pixels.remove(session_name) {
            self.attached_terminal_pixels
                .insert(new_name.clone(), pixels);
        }

        Ok(())
    }

    pub(crate) fn remove_session_terminals(
        &mut self,
        session_name: &SessionName,
        current_runtime_owner: Option<&SessionName>,
        next_runtime_owner: Option<&SessionName>,
    ) -> Result<bool, RmuxError> {
        let Some(current_runtime_owner) = current_runtime_owner else {
            return Ok(false);
        };
        if current_runtime_owner != session_name {
            return Ok(true);
        }

        if let Some(next_runtime_owner) = next_runtime_owner {
            self.terminals
                .rename_session(session_name, next_runtime_owner)?;
            self.rename_runtime_session_state(session_name, next_runtime_owner)?;
            self.sync_pane_lifecycle_dimensions_for_session(next_runtime_owner);
            return Ok(true);
        }

        if self.session_has_marked_pane(session_name) {
            self.clear_marked_pane();
        }

        for pipe in self.remove_session_pipes(session_name).into_values() {
            pipe.stop();
        }
        self.remove_session_pane_outputs(session_name);
        let _ = self.dead_panes.remove(session_name);
        let _ = self.attached_submitted_rows.remove(session_name);
        let _ = self.attached_terminal_pixels.remove(session_name);
        self.auto_named_windows
            .retain(|(tracked_session, _)| tracked_session != session_name);
        let mut removed_terminals = self.terminals.remove_session(session_name);
        if let Some(panes) = removed_terminals.as_mut() {
            for terminal in panes.values_mut() {
                terminal.terminate_with_bounded_grace();
            }
            for pane_id in panes.keys() {
                self.remove_pane_lifecycle(*pane_id);
            }
        }
        Ok(removed_terminals.is_some())
    }

    fn rename_runtime_session_state(
        &mut self,
        session_name: &SessionName,
        new_name: &SessionName,
    ) -> Result<(), RmuxError> {
        if !self.transcripts.contains_key(session_name) {
            return Err(RmuxError::Server(format!(
                "missing pane transcripts for session {session_name}"
            )));
        }
        if self.transcripts.contains_key(new_name) {
            return Err(RmuxError::Server(format!(
                "pane transcripts already exist for session {new_name}"
            )));
        }
        if !self.pane_outputs.contains_key(session_name) {
            return Err(RmuxError::Server(format!(
                "missing pane output channels for session {session_name}"
            )));
        }
        if self.pane_outputs.contains_key(new_name) {
            return Err(RmuxError::Server(format!(
                "pane output channels already exist for session {new_name}"
            )));
        }

        let mut transcripts = std::mem::take(&mut self.transcripts);
        let mut pane_outputs = std::mem::take(&mut self.pane_outputs);
        let mut dead_panes = std::mem::take(&mut self.dead_panes);
        let mut attached_submitted_rows = std::mem::take(&mut self.attached_submitted_rows);

        let session_transcripts = transcripts
            .remove(session_name)
            .expect("prevalidated pane transcripts must exist");
        let session_outputs = pane_outputs
            .remove(session_name)
            .expect("prevalidated pane outputs must exist");
        let session_dead_panes = dead_panes.remove(session_name).unwrap_or_default();
        let session_attached_rows = attached_submitted_rows
            .remove(session_name)
            .unwrap_or_default();

        debug_assert!(transcripts
            .insert(new_name.clone(), session_transcripts)
            .is_none());
        debug_assert!(pane_outputs
            .insert(new_name.clone(), session_outputs)
            .is_none());
        if !session_dead_panes.is_empty() {
            debug_assert!(dead_panes
                .insert(new_name.clone(), session_dead_panes)
                .is_none());
        }
        if !session_attached_rows.is_empty() {
            debug_assert!(attached_submitted_rows
                .insert(new_name.clone(), session_attached_rows)
                .is_none());
        }
        let auto_named_windows = std::mem::take(&mut self.auto_named_windows)
            .into_iter()
            .map(|(tracked_session, window_index)| {
                if tracked_session == *session_name {
                    (new_name.clone(), window_index)
                } else {
                    (tracked_session, window_index)
                }
            })
            .collect();

        self.transcripts = transcripts;
        self.pane_outputs = pane_outputs;
        self.dead_panes = dead_panes;
        self.attached_submitted_rows = attached_submitted_rows;
        self.auto_named_windows = auto_named_windows;
        self.pipes.rename_session(session_name, new_name)?;
        Ok(())
    }

    fn rollback_session_rename(
        &mut self,
        completed: &[RenameSessionStep],
        session_name: &SessionName,
        new_name: &SessionName,
        source_error: &RmuxError,
    ) -> Result<(), RmuxError> {
        for step in completed.iter().rev().copied() {
            let rollback = match step {
                RenameSessionStep::Sessions => {
                    self.sessions.rename_session(new_name, session_name.clone())
                }
                RenameSessionStep::Options => {
                    self.options.rename_session(new_name, session_name.clone())
                }
                RenameSessionStep::Environment => self
                    .environment
                    .rename_session(new_name, session_name.clone()),
                RenameSessionStep::Hooks => {
                    self.hooks.rename_session(new_name, session_name.clone())
                }
                RenameSessionStep::Terminals => {
                    self.terminals.rename_session(new_name, session_name)
                }
            };

            if let Err(rollback_error) = rollback {
                return Err(RmuxError::Server(format!(
                    "failed to roll back session rename from {session_name} to {new_name} after {source_error}: {rollback_error}"
                )));
            }
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
enum RenameSessionStep {
    Sessions,
    Options,
    Environment,
    Hooks,
    Terminals,
}
