use std::collections::BTreeMap;

use rmux_proto::{RmuxError, SessionName};

use super::{session_not_found, HandlerState};

impl HandlerState {
    pub(crate) fn remap_session_group_window_metadata(
        &mut self,
        session_name: &SessionName,
        index_map: &BTreeMap<u32, u32>,
    ) -> Result<(), RmuxError> {
        self.remap_reindexed_window_metadata(session_name, index_map)?;
        for member_name in self
            .sessions
            .session_group_members(session_name)
            .into_iter()
            .filter(|member_name| member_name != session_name)
        {
            // The source winlink options are copied to its aliases during the
            // subsequent group synchronization. Rekeying peer options here as
            // well would make pane-option rekeying run twice. Hooks and the
            // canonical link/automatic-name indices have no such later copy,
            // so they must follow the same insertion map now.
            self.hooks
                .remap_session_window_indices(&member_name, index_map)?;
            self.remap_window_indexed_state(&member_name, index_map);
        }
        Ok(())
    }

    pub(crate) fn synchronize_session_group_models_from(
        &mut self,
        source_session_name: &SessionName,
    ) -> Result<Vec<SessionName>, RmuxError> {
        self.sessions
            .session(source_session_name)
            .ok_or_else(|| session_not_found(source_session_name))?;
        // Managed lifecycle commits update the whole group's link tables and
        // preserve each peer's local navigation in the same core operation.
        Ok(self.sessions.session_group_members(source_session_name))
    }

    pub(crate) fn synchronize_session_group_models_from_with_window_selection_map(
        &mut self,
        source_session_name: &SessionName,
        _index_map: &BTreeMap<u32, u32>,
    ) -> Result<Vec<SessionName>, RmuxError> {
        self.synchronize_session_group_models_from(source_session_name)
    }

    pub(crate) fn synchronize_session_group_from(
        &mut self,
        source_session_name: &SessionName,
    ) -> Result<Vec<SessionName>, RmuxError> {
        let synchronized = self.synchronize_session_group_models_from(source_session_name)?;
        if synchronized.len() > 1 {
            self.synchronize_window_alias_options_from_name(source_session_name)?;
            self.synchronize_pane_alias_options_from_name(source_session_name)?;
        }
        Ok(synchronized)
    }

    pub(crate) fn synchronize_session_group_from_with_window_selection_map(
        &mut self,
        source_session_name: &SessionName,
        _index_map: &BTreeMap<u32, u32>,
    ) -> Result<Vec<SessionName>, RmuxError> {
        self.synchronize_session_group_from(source_session_name)
    }

    pub(crate) fn synchronize_session_group_from_with_winlink_alert_map(
        &mut self,
        source_session_name: &SessionName,
        _winlink_alert_map: &BTreeMap<u32, u32>,
    ) -> Result<Vec<SessionName>, RmuxError> {
        self.synchronize_session_group_from(source_session_name)
    }

    pub(crate) fn synchronize_session_group_from_with_window_selection_and_winlink_alert_maps(
        &mut self,
        source_session_name: &SessionName,
        _window_selection_map: &BTreeMap<u32, u32>,
        _winlink_alert_map: &BTreeMap<u32, u32>,
    ) -> Result<Vec<SessionName>, RmuxError> {
        self.synchronize_session_group_from(source_session_name)
    }
}
