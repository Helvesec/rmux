use rmux_proto::{RmuxError, SessionName};

use crate::{Pane, SessionId, SessionStore, Window};

use super::matching::{
    parse_prefixed_id, target_not_found, unique_match, MatchMode, ResolvedParts,
};
use super::TargetFindFlags;

impl SessionStore {
    pub(super) fn resolve_session_part(
        &self,
        value: &str,
        flags: TargetFindFlags,
    ) -> Result<SessionName, RmuxError> {
        if let Some(session_id) = parse_prefixed_id(value, '$')? {
            return self
                .session_by_id(SessionId::new(session_id))
                .map(|session| session.name().clone())
                .ok_or_else(|| target_not_found(value, "session"));
        }

        if let Ok(name) = SessionName::new(value.to_owned())
            && let Some(session) = self.session(&name) {
                return Ok(session.name().clone());
            }

        if flags.contains(TargetFindFlags::EXACT_SESSION) {
            return Err(target_not_found(value, "session"));
        }

        if let Some(session_name) = self.unique_session_match(value, MatchMode::Prefix)? {
            return Ok(session_name);
        }
        if let Some(session_name) = self.unique_session_match(value, MatchMode::Pattern)? {
            return Ok(session_name);
        }

        Err(target_not_found(value, "session"))
    }

    pub(super) fn default_session_name(
        &self,
        flags: TargetFindFlags,
    ) -> Result<SessionName, RmuxError> {
        let sessions = self.iter().collect::<Vec<_>>();
        let preferred = if flags.contains(TargetFindFlags::PREFER_UNATTACHED) {
            sessions
                .iter()
                .copied()
                .filter(|(_, session)| session.last_attached_at().is_none())
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let candidates = if preferred.is_empty() {
            sessions
        } else {
            preferred
        };
        let (session_name, _) = candidates
            .into_iter()
            .max_by(|(left_name, left), (right_name, right)| {
                left.activity_at()
                    .cmp(&right.activity_at())
                    .then(left.created_at().cmp(&right.created_at()))
                    .then(right_name.as_str().cmp(left_name.as_str()))
            })
            .ok_or_else(|| RmuxError::Server("no current target".to_owned()))?;
        Ok(session_name.clone())
    }

    pub(super) fn parts_for_session_current(
        &self,
        session_name: &SessionName,
    ) -> Result<ResolvedParts, RmuxError> {
        let session = self
            .session(session_name)
            .ok_or_else(|| target_not_found(session_name.as_str(), "session"))?;
        let window_index = session.active_window_index();
        let pane_index = session
            .window_at(window_index)
            .and_then(Window::active_pane)
            .map(Pane::index)
            .ok_or_else(|| target_not_found(&format!("{session_name}:{window_index}"), "pane"))?;
        Ok(ResolvedParts::pane(
            session_name.clone(),
            window_index,
            pane_index,
        ))
    }

    fn unique_session_match(
        &self,
        value: &str,
        mode: MatchMode,
    ) -> Result<Option<SessionName>, RmuxError> {
        let mut matches = self
            .iter()
            .filter(|(session_name, _)| match mode {
                MatchMode::Exact => session_name.as_str() == value,
                MatchMode::Prefix => session_name.as_str().starts_with(value),
                MatchMode::Pattern => crate::fnmatch(value, session_name.as_str()),
            })
            .map(|(session_name, _)| session_name.clone())
            .collect::<Vec<_>>();
        matches.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        unique_match(matches, value, "session")
    }
}
