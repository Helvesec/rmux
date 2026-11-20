use super::super::store::GroupedSessionCreation;
use super::*;

pub(super) fn checked_id(value: u64, kind: &str) -> Result<u32, RmuxError> {
    u32::try_from(value)
        .map_err(|_| RmuxError::Server(format!("managed {kind} identity space exhausted")))
}

impl ManagedSessionStore {
    /// Creates a session with globally distinct initial session/window/pane IDs
    /// within this store. Rejected creation consumes no identity.
    pub fn create_session(
        &mut self,
        name: SessionName,
        size: TerminalSize,
    ) -> Result<SessionId, RmuxError> {
        self.create_session_with_base_index(name, size, 0)
    }

    /// Creates a session whose initial window occupies the requested local index.
    pub fn create_session_with_base_index(
        &mut self,
        name: SessionName,
        size: TerminalSize,
        base_index: u32,
    ) -> Result<SessionId, RmuxError> {
        self.create_standalone(name, size, base_index, self.next_session_id)
    }

    fn create_standalone(
        &mut self,
        name: SessionName,
        size: TerminalSize,
        base_index: u32,
        next: u64,
    ) -> Result<SessionId, RmuxError> {
        if self.sessions.contains_key(&name) {
            return Err(RmuxError::DuplicateSession(name.to_string()));
        }
        let identity = checked_id(next, "session")?;
        let window_id = checked_id(self.next_window_id, "window")?;
        let pane_id = checked_id(self.next_pane_id, "pane")?;
        let id = SessionId::new(identity);
        let now = current_unix_timestamp();
        let metadata = ManagedSession {
            id,
            name: name.clone(),
            group_name: None,
            terminal_size: size,
            active_window: base_index,
            last_window: None,
            window_stack: Vec::new(),
            group_initial_window_id: None,
            winlink_alert_flags: BTreeMap::from([(base_index, AlertFlags::empty())]),
            created_at: now,
            activity_at: now,
            last_attached_at: None,
            recency: SessionRecency::next(),
            cwd: None,
        };
        self.windows
            .introduce(
                WindowLink {
                    session_id: id,
                    window_index: base_index,
                },
                Window::new_with_initial_pane(size, PaneId::new(pane_id), WindowId::new(window_id)),
            )
            .expect("fresh managed identities and session slot");
        self.sessions.insert(name, metadata);
        self.next_session_id = next + 1;
        self.next_window_id += 1;
        self.next_pane_id += 1;
        Ok(id)
    }

    /// Creates a peer from an existing session/group, or declares a new group.
    /// Rejects a standalone target whose name is already an existing group key.
    pub fn create_grouped_session_with_base_index(
        &mut self,
        name: SessionName,
        size: TerminalSize,
        base_index: u32,
        target: SessionName,
    ) -> Result<GroupedSessionCreation, RmuxError> {
        self.create_grouped(name, size, base_index, target, self.next_session_id)
    }

    /// Selects new-session's first shared window after group enrollment.
    /// Initialization is not a visit: the fresh peer keeps its empty history.
    pub fn select_initial_grouped_window(&mut self, name: &SessionName) -> Result<(), RmuxError> {
        let session = self
            .sessions
            .get_mut(name)
            .ok_or_else(|| RmuxError::SessionNotFound(name.to_string()))?;
        debug_assert!(session.group_name.is_some());
        debug_assert!(session.last_window.is_none() && session.window_stack.is_empty());
        session.active_window = self
            .windows
            .session_windows(session.id)
            .next()
            .expect("new grouped session has a shared window")
            .0;
        Ok(())
    }

    fn create_grouped(
        &mut self,
        name: SessionName,
        size: TerminalSize,
        base_index: u32,
        target: SessionName,
        next: u64,
    ) -> Result<GroupedSessionCreation, RmuxError> {
        if self.sessions.contains_key(&name) {
            return Err(RmuxError::DuplicateSession(name.to_string()));
        }
        let id = SessionId::new(checked_id(next, "session")?);
        let template = if self.sessions.contains_key(&target) {
            Some(target.clone())
        } else {
            self.group_runtime_owners.get(&target).cloned()
        };
        let Some(template_name) = template else {
            self.create_standalone(name.clone(), size, base_index, next)?;
            self.sessions
                .get_mut(&name)
                .expect("created session")
                .group_name = Some(target.clone());
            self.group_runtime_owners
                .insert(target.clone(), name.clone());
            return Ok(GroupedSessionCreation {
                session_name: name.clone(),
                group_name: target,
                template_session: None,
                runtime_owner: name,
            });
        };
        let source = self.sessions.get(&template_name).ok_or_else(|| {
            RmuxError::Server(format!("session group {target} has no template session"))
        })?;
        let group = source.group_name.clone().unwrap_or_else(|| target.clone());
        // Session names take precedence over group keys. A renamed group's old
        // name may now address a different standalone session; enrolling that
        // session would mix unrelated link tables under the existing group.
        if source.group_name.is_none() && self.contains_group(&group) {
            return Err(RmuxError::Message(format!(
                "standalone session {template_name} conflicts with existing session group {group}"
            )));
        }
        let owner = self
            .group_runtime_owners
            .get(&group)
            .cloned()
            .unwrap_or_else(|| template_name.clone());
        let links = self
            .windows
            .session_windows(source.id)
            .map(|(index, window)| (index, window.id()))
            .collect::<Vec<_>>();
        let now = current_unix_timestamp();
        let metadata = ManagedSession {
            id,
            name: name.clone(),
            group_name: Some(group.clone()),
            terminal_size: source.terminal_size,
            active_window: source.active_window,
            // With no visit of its own there is no window to return to either.
            last_window: None,
            // A new peer of the group has visited none of these links yet
            // (`session.c:121`, `session.c:671-679`, `cmd-new-session.c:324-326`);
            // the clone and the checkpoint of the SAME session keep theirs.
            window_stack: Vec::new(),
            group_initial_window_id: links.first().map(|(_, id)| *id),
            winlink_alert_flags: source.winlink_alert_flags.clone(),
            cwd: source.cwd.clone(),
            created_at: now,
            activity_at: now,
            last_attached_at: None,
            recency: SessionRecency::next(),
        };
        for (window_index, window_id) in links {
            self.windows
                .link(
                    WindowLink {
                        session_id: id,
                        window_index,
                    },
                    window_id,
                )
                .expect("fresh session links to existing owners");
        }
        self.sessions
            .get_mut(&template_name)
            .expect("validated template")
            .group_name = Some(group.clone());
        self.group_runtime_owners
            .entry(group.clone())
            .or_insert(owner.clone());
        self.sessions.insert(name.clone(), metadata);
        self.next_session_id = next + 1;
        Ok(GroupedSessionCreation {
            session_name: name,
            group_name: group,
            template_session: Some(template_name),
            runtime_owner: owner,
        })
    }

    /// Creates a numeric name using the next unused global session ID.
    pub fn create_auto_named_session_with_base_index(
        &mut self,
        size: TerminalSize,
        base_index: u32,
    ) -> Result<SessionName, RmuxError> {
        let (name, next) = self.automatic_identity(None)?;
        self.create_standalone(name.clone(), size, base_index, next)?;
        Ok(name)
    }

    /// Creates a grouped name with the global session ID suffix.
    pub fn create_auto_grouped_session_with_base_index(
        &mut self,
        size: TerminalSize,
        base_index: u32,
        target: SessionName,
    ) -> Result<GroupedSessionCreation, RmuxError> {
        let prefix = self.automatic_group_prefix(&target)?;
        let (name, next) = self.automatic_identity(Some(&prefix))?;
        self.create_grouped(name, size, base_index, target, next)
    }

    /// Prefix of an automatic grouped name, as in tmux 3.7b `cmd-new-session.c`:
    /// the stored group or session name as is, or, for a group that does not
    /// exist yet, the target text escaped like a new name (`clean_name`) while
    /// the group itself keeps that text.
    fn automatic_group_prefix(&self, target: &SessionName) -> Result<SessionName, RmuxError> {
        if let Some(group) = self.session_group_name(target) {
            return Ok(group.clone());
        }
        if self.sessions.contains_key(target) || self.contains_group(target) {
            return Ok(target.clone());
        }
        SessionName::new(target.as_str()).map_err(|error| RmuxError::Message(error.to_string()))
    }

    fn automatic_identity(
        &self,
        prefix: Option<&SessionName>,
    ) -> Result<(SessionName, u64), RmuxError> {
        let mut next = self.next_session_id;
        loop {
            let id = checked_id(next, "session")?;
            let text = prefix.map_or_else(|| id.to_string(), |prefix| format!("{prefix}-{id}"));
            // The prefix is an already stored name; tmux appends `-<id>` to it as is.
            let name = SessionName::from_canonical(text)
                .map_err(|error| RmuxError::Message(error.to_string()))?;
            if !self.sessions.contains_key(&name) {
                return Ok((name, next));
            }
            next += 1;
        }
    }
}
