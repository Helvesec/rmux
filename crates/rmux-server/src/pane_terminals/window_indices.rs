use std::collections::BTreeMap;

use rmux_proto::{OptionName, RmuxError, SessionName};

use super::{session_not_found, HandlerState};

impl HandlerState {
    pub(in crate::pane_terminals) fn session_base_index(&self, session_name: &SessionName) -> u32 {
        self.options
            .resolve(Some(session_name), OptionName::BaseIndex)
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(0)
    }

    pub(in crate::pane_terminals) fn renumber_windows_if_enabled(
        &mut self,
        session_name: &SessionName,
    ) -> Result<BTreeMap<u32, u32>, RmuxError> {
        if self
            .options
            .resolve(Some(session_name), OptionName::RenumberWindows)
            != Some("on")
        {
            return Ok(BTreeMap::new());
        }

        self.reindex_windows_from_base(session_name)
    }

    pub(in crate::pane_terminals) fn renumber_transfer_source_sessions(
        &mut self,
        session_names: &[SessionName],
    ) -> Result<(), RmuxError> {
        let mut session_names = session_names.to_vec();
        session_names.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        session_names.dedup();

        for session_name in &session_names {
            if self
                .sessions
                .session(session_name)
                .is_some_and(|session| session.windows().next().is_some())
            {
                let _ = self.renumber_windows_if_enabled(session_name)?;
            }
        }
        for session_name in session_names {
            if self
                .sessions
                .session(&session_name)
                .is_some_and(|session| session.windows().next().is_some())
            {
                self.synchronize_session_group_from(&session_name)?;
                self.sync_pane_lifecycle_dimensions_for_session(&session_name);
            }
        }
        Ok(())
    }

    pub(in crate::pane_terminals) fn reindex_windows_from_base(
        &mut self,
        session_name: &SessionName,
    ) -> Result<BTreeMap<u32, u32>, RmuxError> {
        let base_index = self.session_base_index(session_name);
        let session = self
            .sessions
            .session(session_name)
            .ok_or_else(|| session_not_found(session_name))?;
        let index_map = session
            .windows()
            .enumerate()
            .map(|(offset, (index, _))| {
                let offset = u32::try_from(offset)
                    .map_err(|_| RmuxError::Server("window index space exhausted".to_owned()))?;
                let destination = base_index
                    .checked_add(offset)
                    .ok_or_else(|| RmuxError::Server("window index space exhausted".to_owned()))?;
                Ok((index, destination))
            })
            .collect::<Result<BTreeMap<_, _>, RmuxError>>()?;
        let members = self.sessions.session_group_members(session_name);
        // Stage fallible ancillary rekeys before the atomic canonical commit.
        // These stores contain no window owners or pane layout state.
        let mut options = self.options.clone();
        let mut hooks = self.hooks.clone();
        for member in &members {
            options.remap_session_window_indices(member, &index_map)?;
            hooks.remap_session_window_indices(member, &index_map)?;
        }
        let change = self
            .sessions
            .reindex_windows_from(session_name, base_index)?;
        self.options = options;
        self.hooks = hooks;
        for member_change in change.sessions {
            let name = self
                .sessions
                .session_by_id(member_change.session_id)
                .expect("reindex retains session identity")
                .name()
                .clone();
            self.remap_window_indexed_state(&name, &member_change.index_map);
        }
        Ok(index_map)
    }

    pub(in crate::pane_terminals) fn remap_reindexed_window_metadata(
        &mut self,
        session_name: &SessionName,
        index_map: &std::collections::BTreeMap<u32, u32>,
    ) -> Result<(), RmuxError> {
        self.options
            .remap_session_window_indices(session_name, index_map)?;
        self.hooks
            .remap_session_window_indices(session_name, index_map)?;
        self.remap_window_indexed_state(session_name, index_map);
        Ok(())
    }
}
