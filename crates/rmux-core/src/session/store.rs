use std::collections::HashMap;

use rmux_proto::{RmuxError, SessionId, SessionName, TerminalSize};

use super::{Session, WindowIdAllocator};

/// Summary for a grouped session creation request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupedSessionCreation {
    /// The newly-created session name.
    pub session_name: SessionName,
    /// The stable group name attached to the new session.
    pub group_name: SessionName,
    /// The session used as the template for shared window state, when one existed.
    pub template_session: Option<SessionName>,
    /// The runtime-owning session for the group after creation.
    pub runtime_owner: SessionName,
}

/// In-memory storage for all sessions owned by the server.
#[derive(Debug, Default)]
pub struct SessionStore {
    sessions: HashMap<SessionName, Session>,
    next_session_id: u32,
    next_pane_id: u32,
    next_window_id: WindowIdAllocator,
    group_runtime_owners: HashMap<SessionName, SessionName>,
}

impl SessionStore {
    /// Creates an empty session store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the number of sessions currently present.
    #[must_use]
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    /// Returns whether the store contains no sessions.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// Returns whether the addressed session exists.
    #[must_use]
    pub fn contains_session(&self, session_name: &SessionName) -> bool {
        self.sessions.contains_key(session_name)
    }

    /// Returns an immutable reference to the named session.
    #[must_use]
    pub fn session(&self, session_name: &SessionName) -> Option<&Session> {
        self.sessions.get(session_name)
    }

    /// Returns the session with the store-assigned `$N` identity.
    #[must_use]
    pub fn session_by_id(&self, session_id: impl Into<SessionId>) -> Option<&Session> {
        let session_id = session_id.into();
        self.sessions
            .values()
            .find(|session| session.id() == session_id)
    }

    /// Returns an iterator over all sessions in the store.
    pub fn iter(&self) -> impl Iterator<Item = (&SessionName, &Session)> {
        self.sessions.iter()
    }

    /// Returns the stable session group name for the addressed session.
    #[must_use]
    pub fn session_group_name(&self, session_name: &SessionName) -> Option<&SessionName> {
        self.session(session_name).and_then(Session::group_name)
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
                .any(|session| session.group_name() == Some(group_name))
    }

    /// Returns grouped sessions for the named group in stable name order.
    #[must_use]
    pub fn sessions_in_group(&self, group_name: &SessionName) -> Vec<SessionName> {
        let mut sessions = self
            .sessions
            .iter()
            .filter_map(|(session_name, session)| {
                (session.group_name() == Some(group_name)).then_some(session_name.clone())
            })
            .collect::<Vec<_>>();
        sessions.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        sessions
    }

    /// Creates a session or returns `DuplicateSession` when the name exists.
    pub fn create_session(
        &mut self,
        session_name: SessionName,
        size: TerminalSize,
    ) -> Result<(), RmuxError> {
        self.create_session_with_base_index(session_name, size, 0)
    }

    /// Creates a session using the requested first window index.
    pub fn create_session_with_base_index(
        &mut self,
        session_name: SessionName,
        size: TerminalSize,
        base_index: u32,
    ) -> Result<(), RmuxError> {
        let session_id = self.allocate_session_id();
        self.create_session_with_base_index_and_id(session_name, size, base_index, session_id)
    }

    /// Creates an auto-named tmux-compatible session using the next global
    /// session id for both the `$id` and the visible session name.
    pub fn create_auto_named_session_with_base_index(
        &mut self,
        size: TerminalSize,
        base_index: u32,
    ) -> Result<SessionName, RmuxError> {
        let (session_name, session_id) = self.next_automatic_session_identity(None);
        self.create_session_with_base_index_and_id(
            session_name.clone(),
            size,
            base_index,
            session_id,
        )?;
        Ok(session_name)
    }

    fn create_session_with_base_index_and_id(
        &mut self,
        session_name: SessionName,
        size: TerminalSize,
        base_index: u32,
        session_id: SessionId,
    ) -> Result<(), RmuxError> {
        if self.sessions.contains_key(&session_name) {
            return Err(RmuxError::DuplicateSession(session_name.to_string()));
        }
        if self.session_by_id(session_id).is_some() {
            return Err(RmuxError::Server(format!(
                "session id {} already exists",
                session_id.as_u32()
            )));
        }

        let pane_id = self.allocate_pane_id();
        let window_id = self.allocate_window_id();
        let mut session = Session::new_with_initial_window(
            session_name.clone(),
            size,
            base_index,
            pane_id,
            window_id,
        );
        session.rebind_window_id_allocator(self.next_window_id.clone());
        session.set_id(session_id);
        self.next_session_id = self
            .next_session_id
            .max(session_id.as_u32().saturating_add(1));
        self.sessions.insert(session_name, session);
        Ok(())
    }

    /// Creates a session in an existing or newly-declared session group.
    pub fn create_grouped_session_with_base_index(
        &mut self,
        session_name: SessionName,
        size: TerminalSize,
        base_index: u32,
        group_target: SessionName,
    ) -> Result<GroupedSessionCreation, RmuxError> {
        let session_id = self.allocate_session_id();
        self.create_grouped_session_with_base_index_and_id(
            session_name,
            size,
            base_index,
            group_target,
            session_id,
        )
    }

    /// Creates an auto-named grouped session using tmux's global session-id
    /// suffix rule (`group-$id`).
    pub fn create_auto_grouped_session_with_base_index(
        &mut self,
        size: TerminalSize,
        base_index: u32,
        group_target: SessionName,
    ) -> Result<GroupedSessionCreation, RmuxError> {
        let group_name = self
            .sessions
            .get(&group_target)
            .and_then(Session::group_name)
            .cloned()
            .unwrap_or_else(|| group_target.clone());
        let (session_name, session_id) = self.next_automatic_session_identity(Some(&group_name));
        self.create_grouped_session_with_base_index_and_id(
            session_name,
            size,
            base_index,
            group_target,
            session_id,
        )
    }

    fn create_grouped_session_with_base_index_and_id(
        &mut self,
        session_name: SessionName,
        size: TerminalSize,
        base_index: u32,
        group_target: SessionName,
        session_id: SessionId,
    ) -> Result<GroupedSessionCreation, RmuxError> {
        if self.sessions.contains_key(&session_name) {
            return Err(RmuxError::DuplicateSession(session_name.to_string()));
        }
        if self.session_by_id(session_id).is_some() {
            return Err(RmuxError::Server(format!(
                "session id {} already exists",
                session_id.as_u32()
            )));
        }

        enum GroupTemplate {
            Existing {
                group_name: SessionName,
                template_session: SessionName,
                runtime_owner: SessionName,
            },
            Standalone {
                group_name: SessionName,
            },
        }

        let template = if let Some(source_session) = self.sessions.get(&group_target) {
            let group_name = source_session
                .group_name()
                .cloned()
                .unwrap_or_else(|| group_target.clone());
            let runtime_owner = self
                .group_runtime_owners
                .get(&group_name)
                .cloned()
                .unwrap_or_else(|| group_target.clone());
            GroupTemplate::Existing {
                group_name,
                template_session: group_target.clone(),
                runtime_owner,
            }
        } else if let Some(runtime_owner) = self.group_runtime_owners.get(&group_target).cloned() {
            let template_session = if self.sessions.contains_key(&runtime_owner) {
                runtime_owner.clone()
            } else {
                self.sessions_in_group(&group_target)
                    .into_iter()
                    .next()
                    .ok_or_else(|| {
                        RmuxError::Server(format!(
                            "session group {group_target} has no template session"
                        ))
                    })?
            };
            GroupTemplate::Existing {
                group_name: group_target.clone(),
                template_session,
                runtime_owner,
            }
        } else {
            GroupTemplate::Standalone {
                group_name: group_target.clone(),
            }
        };

        match template {
            GroupTemplate::Existing {
                group_name,
                template_session,
                runtime_owner,
            } => {
                if self
                    .sessions
                    .get(&template_session)
                    .and_then(Session::group_name)
                    .is_none()
                {
                    let source = self
                        .sessions
                        .get_mut(&template_session)
                        .expect("template session must exist");
                    source.set_group_name(Some(group_name.clone()));
                    self.group_runtime_owners
                        .entry(group_name.clone())
                        .or_insert(template_session.clone());
                }

                let grouped = self
                    .sessions
                    .get(&template_session)
                    .expect("template session must exist")
                    .clone_as_group_member(session_name.clone(), group_name.clone(), session_id);
                let replaced = self.sessions.insert(session_name.clone(), grouped);
                debug_assert!(replaced.is_none());
                self.next_session_id = self
                    .next_session_id
                    .max(session_id.as_u32().saturating_add(1));
                Ok(GroupedSessionCreation {
                    session_name,
                    group_name,
                    template_session: Some(template_session),
                    runtime_owner,
                })
            }
            GroupTemplate::Standalone { group_name } => {
                let pane_id = self.allocate_pane_id();
                let window_id = self.allocate_window_id();
                let mut session = Session::new_with_initial_window(
                    session_name.clone(),
                    size,
                    base_index,
                    pane_id,
                    window_id,
                );
                session.rebind_window_id_allocator(self.next_window_id.clone());
                session.set_id(session_id);
                self.next_session_id = self
                    .next_session_id
                    .max(session_id.as_u32().saturating_add(1));
                session.set_group_name(Some(group_name.clone()));
                let replaced = self.sessions.insert(session_name.clone(), session);
                debug_assert!(replaced.is_none());
                self.group_runtime_owners
                    .insert(group_name.clone(), session_name.clone());
                Ok(GroupedSessionCreation {
                    session_name: session_name.clone(),
                    group_name,
                    template_session: None,
                    runtime_owner: session_name,
                })
            }
        }
    }

    /// Returns the next available grouped-session name using tmux's `group-N` prefix shape.
    #[must_use]
    pub fn next_grouped_session_name(&self, group_name: &SessionName) -> SessionName {
        for suffix in 1_u32.. {
            let candidate = SessionName::new(format!("{group_name}-{suffix}"))
                .expect("generated grouped session name must be valid");
            if !self.contains_session(&candidate) {
                return candidate;
            }
        }

        unreachable!("u32 loop must eventually yield an unused grouped session name")
    }

    fn next_automatic_session_identity(
        &self,
        prefix: Option<&SessionName>,
    ) -> (SessionName, SessionId) {
        let mut session_id = self.next_session_id;

        loop {
            let candidate = match prefix {
                Some(prefix) => SessionName::new(format!("{prefix}-{session_id}"))
                    .expect("generated grouped session name must be valid"),
                None => SessionName::new(session_id.to_string())
                    .expect("generated default session name must be valid"),
            };
            if !self.contains_session(&candidate) {
                return (candidate, SessionId::new(session_id));
            }
            session_id = session_id
                .checked_add(1)
                .expect("u32 loop must eventually yield an unused session id");
        }
    }

    /// Removes a session or returns `SessionNotFound` when it is absent.
    pub fn remove_session(&mut self, session_name: &SessionName) -> Result<Session, RmuxError> {
        let removed = self
            .sessions
            .remove(session_name)
            .ok_or_else(|| RmuxError::SessionNotFound(session_name.to_string()))?;
        self.repair_group_runtime_owner(removed.group_name().cloned());
        Ok(removed)
    }

    /// Inserts an existing session back into the store without rewriting it.
    pub fn insert_existing_session(&mut self, session: Session) -> Result<(), RmuxError> {
        let mut session = session;
        let session_name = session.name().clone();
        if self.sessions.contains_key(&session_name) {
            return Err(RmuxError::DuplicateSession(session_name.to_string()));
        }

        if self
            .sessions
            .values()
            .any(|existing| existing.id() == session.id())
        {
            session.set_id(self.allocate_session_id());
        }
        session.rebind_window_id_allocator(self.next_window_id.clone());
        self.next_session_id = self
            .next_session_id
            .max(session.id().as_u32().saturating_add(1));
        self.bump_next_pane_id_from_session(&session);
        if let Some(group_name) = session.group_name().cloned() {
            match self.group_runtime_owners.get(&group_name) {
                Some(owner) if self.sessions.contains_key(owner) || owner == &session_name => {}
                _ => {
                    self.group_runtime_owners
                        .insert(group_name, session_name.clone());
                }
            }
        }

        let replaced = self.sessions.insert(session_name, session);
        debug_assert!(replaced.is_none());
        Ok(())
    }

    /// Returns a mutable reference to the named session.
    #[must_use]
    pub fn session_mut(&mut self, session_name: &SessionName) -> Option<&mut Session> {
        self.sessions.get_mut(session_name)
    }

    /// Returns the next session id that will be allocated.
    #[must_use]
    pub const fn next_session_id(&self) -> SessionId {
        SessionId::new(self.next_session_id)
    }

    /// Returns the next globally visible pane id that will be allocated.
    #[must_use]
    pub const fn next_pane_id(&self) -> crate::PaneId {
        crate::PaneId::new(self.next_pane_id)
    }

    /// Renames the addressed session by updating both the stored key and the session's internal name.
    pub fn rename_session(
        &mut self,
        session_name: &SessionName,
        new_name: SessionName,
    ) -> Result<(), RmuxError> {
        if !self.sessions.contains_key(session_name) {
            return Err(RmuxError::SessionNotFound(session_name.to_string()));
        }
        if self.sessions.contains_key(&new_name) {
            return Err(RmuxError::DuplicateSession(new_name.to_string()));
        }

        let mut sessions = std::mem::take(&mut self.sessions);
        let mut session = sessions
            .remove(session_name)
            .expect("prevalidated session must exist");
        let previous_group_name = session.group_name().cloned();
        session.rename(new_name.clone());
        if let Some(group_name) = previous_group_name
            && self.group_runtime_owners.get(&group_name) == Some(session_name) {
                self.group_runtime_owners
                    .insert(group_name, new_name.clone());
            }
        let replaced = sessions.insert(new_name, session);
        debug_assert!(replaced.is_none());
        self.sessions = sessions;
        Ok(())
    }

    fn allocate_session_id(&mut self) -> SessionId {
        let mut next_session_id = self.next_session_id;

        loop {
            let session_id = SessionId::new(next_session_id);
            if self.session_by_id(session_id).is_none() {
                self.next_session_id = next_session_id.saturating_add(1);
                return session_id;
            }

            assert_ne!(next_session_id, u32::MAX, "session id space exhausted");
            next_session_id += 1;
        }
    }

    /// Allocates the next globally visible pane id for runtime-created panes.
    pub fn allocate_pane_id(&mut self) -> crate::PaneId {
        let pane_id = crate::PaneId::new(self.next_pane_id);
        self.next_pane_id = self.next_pane_id.saturating_add(1);
        pane_id
    }

    fn allocate_window_id(&mut self) -> crate::WindowId {
        self.next_window_id.allocate()
    }

    fn bump_next_pane_id_from_session(&mut self, session: &Session) {
        let next_after_session = session
            .windows()
            .values()
            .flat_map(|window| window.panes().iter())
            .map(|pane| pane.id().as_u32().saturating_add(1))
            .max()
            .unwrap_or(self.next_pane_id);
        self.next_pane_id = self.next_pane_id.max(next_after_session);
    }

    fn repair_group_runtime_owner(&mut self, group_name: Option<SessionName>) {
        let Some(group_name) = group_name else {
            return;
        };
        let mut sessions = self.sessions_in_group(&group_name);
        if sessions.is_empty() {
            self.group_runtime_owners.remove(&group_name);
            return;
        }
        let owner = self
            .group_runtime_owners
            .get(&group_name)
            .cloned()
            .filter(|owner| sessions.contains(owner))
            .unwrap_or_else(|| {
                sessions
                    .drain(..)
                    .next()
                    .expect("non-empty grouped session list")
            });
        self.group_runtime_owners.insert(group_name, owner);
    }
}
