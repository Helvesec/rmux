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

    pub(in crate::pane_terminals) fn reindex_windows_from_base(
        &mut self,
        session_name: &SessionName,
    ) -> Result<BTreeMap<u32, u32>, RmuxError> {
        let base_index = self.session_base_index(session_name);
        let previous_session = self
            .sessions
            .session(session_name)
            .cloned()
            .ok_or_else(|| session_not_found(session_name))?;
        let previous_options = self.options.clone();
        let previous_hooks = self.hooks.clone();
        let previous_auto_named_windows = self.auto_named_windows.clone();
        let previous_window_link_slots = self.window_link_slots.clone();
        let previous_window_link_groups = self.window_link_groups.clone();
        let previous_window_link_occurrences = self.window_link_occurrences.clone();

        let session = self
            .sessions
            .session_mut(session_name)
            .ok_or_else(|| session_not_found(session_name))?;
        let index_map = session.reindex_windows_from(base_index)?;
        if let Err(error) = self.remap_reindexed_window_metadata(session_name, &index_map) {
            self.replace_session(session_name, previous_session)?;
            self.options = previous_options;
            self.hooks = previous_hooks;
            self.auto_named_windows = previous_auto_named_windows;
            self.window_link_slots = previous_window_link_slots;
            self.window_link_groups = previous_window_link_groups;
            self.window_link_occurrences = previous_window_link_occurrences;
            return Err(error);
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
