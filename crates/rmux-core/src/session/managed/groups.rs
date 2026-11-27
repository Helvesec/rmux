use super::*;
use std::collections::HashSet;

impl ManagedSessionStore {
    /// Returns the stable session group name for the addressed session.
    #[must_use]
    pub fn session_group_name(&self, session_name: &SessionName) -> Option<&SessionName> {
        self.session(session_name)
            .and_then(|session| session.group_name())
    }

    /// Returns the runtime-owning session for the addressed session.
    #[must_use]
    pub fn runtime_owner(&self, session_name: &SessionName) -> Option<SessionName> {
        let session = self.session(session_name)?;
        match session.group_name() {
            Some(group_name) => self.group_runtime_owners.get(group_name).cloned(),
            None => Some(session_name.clone()),
        }
    }

    /// Returns the next runtime-owning session when the current runtime owner is removed.
    #[must_use]
    pub fn runtime_owner_transfer_target(&self, session_name: &SessionName) -> Option<SessionName> {
        if self.runtime_owner(session_name).as_ref() != Some(session_name) {
            return None;
        }

        let group_name = self.session_group_name(session_name)?;
        self.sessions_in_group(group_name)
            .into_iter()
            .find(|candidate| candidate != session_name)
    }

    /// Returns the grouped peer sessions for the addressed session in stable name order.
    #[must_use]
    pub fn session_group_members(&self, session_name: &SessionName) -> Vec<SessionName> {
        let Some(group_name) = self.session_group_name(session_name).cloned() else {
            return vec![session_name.clone()];
        };
        self.sessions_in_group(&group_name)
    }

    /// Returns the number of sessions sharing the addressed session's group.
    #[must_use]
    pub fn session_group_size(&self, session_name: &SessionName) -> usize {
        self.session_group_members(session_name).len()
    }

    /// Returns whether a named session group exists.
    #[must_use]
    pub fn contains_group(&self, group_name: &SessionName) -> bool {
        self.group_runtime_owners.contains_key(group_name)
            || self
                .sessions
                .values()
                .any(|session| session.group_name.as_ref() == Some(group_name))
    }

    /// The sessions a window is linked into, in the order those links were MADE.
    ///
    /// This is the concrete link order the window store keeps, not a membership
    /// and not the canonicalised runtime slots: a session that links the same
    /// window twice is named twice, and an outsider linked between a creation
    /// and a JOIN is named between them. The names are resolved HERE, at the
    /// reading, so a rename is answered under the new name without the order
    /// moving - the identity in the order is the SessionId.
    ///
    /// `None` is an order this store cannot vouch for, and its reader must fall
    /// back rather than invent one.
    #[must_use]
    pub fn window_link_order(&self, target: &WindowTarget) -> Option<Vec<SessionName>> {
        let id = self.window(target).ok()?.id();
        let made = self.windows.window_link_order(id)?;
        let mut names = Vec::with_capacity(made.len());
        for link in made {
            let name = self
                .sessions
                .iter()
                .find(|(_, session)| session.id == link.session_id)
                .map(|(name, _)| name.clone())?;
            names.push(name);
        }
        Some(names)
    }

    /// Current sessions in the window's actual winlink order, for presentation.
    ///
    /// Keep the link's local index until after selection: an inactive first
    /// alias must not mask a later, current alias of the same session. Unknown
    /// chronology has the same `None` contract as `window_link_order`.
    #[must_use]
    pub fn window_active_sessions_in_link_order(
        &self,
        target: &WindowTarget,
    ) -> Option<Vec<SessionName>> {
        let id = self.window(target).ok()?.id();
        let links = self.windows.window_link_order(id)?;
        let mut seen = HashSet::new();
        let mut names = Vec::new();
        for link in links {
            let session = self.session_by_id(link.session_id)?;
            if session.active_window_index() == link.window_index && seen.insert(link.session_id) {
                names.push(session.name().clone());
            }
        }
        Some(names)
    }

    /// Returns grouped sessions for the named group in stable name order.
    #[must_use]
    pub fn sessions_in_group(&self, group_name: &SessionName) -> Vec<SessionName> {
        let mut sessions = self
            .sessions
            .iter()
            .filter_map(|(session_name, session)| {
                (session.group_name.as_ref() == Some(group_name)).then_some(session_name.clone())
            })
            .collect::<Vec<_>>();
        sessions.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        sessions
    }

    pub(super) fn repair_group_runtime_owner(&mut self, group: Option<SessionName>) {
        let Some(group) = group else {
            return;
        };
        let members = self.sessions_in_group(&group);
        if members.is_empty() {
            self.group_runtime_owners.remove(&group);
        } else if !self
            .group_runtime_owners
            .get(&group)
            .is_some_and(|owner| members.contains(owner))
        {
            self.group_runtime_owners.insert(group, members[0].clone());
        }
    }
}
