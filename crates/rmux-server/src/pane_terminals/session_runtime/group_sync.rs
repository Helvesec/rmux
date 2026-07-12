use std::collections::BTreeMap;

use rmux_core::Session;
use rmux_proto::{RmuxError, SessionName, WindowTarget};

use super::{session_not_found, HandlerState};

impl HandlerState {
    fn synchronize_group_member<F>(
        &mut self,
        source: &Session,
        member_name: &SessionName,
        synchronize: F,
    ) -> Result<(), RmuxError>
    where
        F: FnOnce(&mut Session, &Session),
    {
        let before_pane_options = self.pane_option_slots_for_session(member_name)?;
        let before_window_indices = self
            .sessions
            .session(member_name)
            .map(|session| session.windows().keys().copied().collect::<Vec<_>>())
            .ok_or_else(|| session_not_found(member_name))?;
        let member = self
            .sessions
            .session_mut(member_name)
            .ok_or_else(|| session_not_found(member_name))?;
        synchronize(member, source);
        self.rekey_pane_options_after_session_change(&before_pane_options, member_name)?;
        for window_index in before_window_indices {
            if source.window_at(window_index).is_none() {
                let target = WindowTarget::with_window(member_name.clone(), window_index);
                let _ = self.options.remove_window(&target);
            }
        }
        Ok(())
    }

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
        self.synchronize_session_group_models_from_using(
            source_session_name,
            Session::synchronize_group_from,
        )
    }

    pub(crate) fn synchronize_session_group_models_from_with_window_selection_map(
        &mut self,
        source_session_name: &SessionName,
        index_map: &BTreeMap<u32, u32>,
    ) -> Result<Vec<SessionName>, RmuxError> {
        self.synchronize_session_group_models_from_using(source_session_name, |member, source| {
            member.synchronize_group_from_with_window_selection_map(source, index_map);
        })
    }

    fn synchronize_session_group_models_from_using<F>(
        &mut self,
        source_session_name: &SessionName,
        synchronize: F,
    ) -> Result<Vec<SessionName>, RmuxError>
    where
        F: Fn(&mut Session, &Session),
    {
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

            self.synchronize_group_member(&source_session, &member_name, &synchronize)?;
            synchronized.push(member_name);
        }

        Ok(synchronized)
    }

    pub(crate) fn synchronize_session_group_from(
        &mut self,
        source_session_name: &SessionName,
    ) -> Result<Vec<SessionName>, RmuxError> {
        let synchronized = self.synchronize_session_group_models_from(source_session_name)?;
        if synchronized.len() <= 1 {
            return Ok(synchronized);
        }

        let source_session = self
            .sessions
            .session(source_session_name)
            .cloned()
            .ok_or_else(|| session_not_found(source_session_name))?;

        self.synchronize_window_alias_options_from_session(&source_session);
        self.synchronize_pane_alias_options_from_session(&source_session)?;

        Ok(synchronized)
    }

    pub(crate) fn synchronize_session_group_from_with_window_selection_map(
        &mut self,
        source_session_name: &SessionName,
        index_map: &BTreeMap<u32, u32>,
    ) -> Result<Vec<SessionName>, RmuxError> {
        self.synchronize_session_group_from_with_window_selection_and_winlink_alert_maps(
            source_session_name,
            index_map,
            index_map,
        )
    }

    pub(crate) fn synchronize_session_group_from_with_winlink_alert_map(
        &mut self,
        source_session_name: &SessionName,
        winlink_alert_map: &BTreeMap<u32, u32>,
    ) -> Result<Vec<SessionName>, RmuxError> {
        self.synchronize_session_group_from_using(source_session_name, |member, source| {
            member.synchronize_group_from_with_winlink_alert_map(source, winlink_alert_map);
        })
    }

    pub(crate) fn synchronize_session_group_from_with_window_selection_and_winlink_alert_maps(
        &mut self,
        source_session_name: &SessionName,
        window_selection_map: &BTreeMap<u32, u32>,
        winlink_alert_map: &BTreeMap<u32, u32>,
    ) -> Result<Vec<SessionName>, RmuxError> {
        self.synchronize_session_group_from_using(source_session_name, |member, source| {
            member.synchronize_group_from_with_window_selection_and_winlink_alert_maps(
                source,
                window_selection_map,
                winlink_alert_map,
            );
        })
    }

    fn synchronize_session_group_from_using<F>(
        &mut self,
        source_session_name: &SessionName,
        synchronize: F,
    ) -> Result<Vec<SessionName>, RmuxError>
    where
        F: Fn(&mut Session, &Session),
    {
        let synchronized =
            self.synchronize_session_group_models_from_using(source_session_name, synchronize)?;
        if synchronized.len() <= 1 {
            return Ok(synchronized);
        }
        let source_session = self
            .sessions
            .session(source_session_name)
            .cloned()
            .ok_or_else(|| session_not_found(source_session_name))?;
        self.synchronize_window_alias_options_from_session(&source_session);
        self.synchronize_pane_alias_options_from_session(&source_session)?;
        Ok(synchronized)
    }
}
