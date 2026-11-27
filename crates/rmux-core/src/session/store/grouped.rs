//! Creation of sessions inside tmux session groups.

use rmux_proto::{RmuxError, SessionId, SessionName, TerminalSize};

use super::{Session, SessionStore};

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

impl SessionStore {
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
        let prefix = self.automatic_group_prefix(&group_target);
        let (session_name, session_id) = self.next_automatic_session_identity(Some(&prefix));
        self.create_grouped_session_with_base_index_and_id(
            session_name,
            size,
            base_index,
            group_target,
            session_id,
        )
    }

    /// Prefix of an automatic grouped name, as in tmux 3.7b `cmd-new-session.c`:
    /// the stored group or session name as is, or, for a group that does not
    /// exist yet, the target text escaped like a new name (`clean_name`) while
    /// the group itself keeps that text.
    fn automatic_group_prefix(&self, group_target: &SessionName) -> SessionName {
        if let Some(group) = self
            .sessions
            .get(group_target)
            .and_then(Session::group_name)
        {
            return group.clone();
        }
        if self.contains_session(group_target) || self.contains_group(group_target) {
            return group_target.clone();
        }
        SessionName::new(group_target.as_str()).expect("a stored session name is never empty")
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
            let candidate = SessionName::from_canonical(format!("{group_name}-{suffix}"))
                .expect("generated grouped session name must be valid");
            if !self.contains_session(&candidate) {
                return candidate;
            }
        }

        unreachable!("u32 loop must eventually yield an unused grouped session name")
    }
}
