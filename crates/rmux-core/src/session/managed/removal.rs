use super::*;

/// Removed session identity and final owners, ready for runtime cleanup.
#[derive(Debug)]
#[must_use]
pub struct ManagedSessionRemoval {
    /// Stable identity of the removed session.
    pub session_id: SessionId,
    /// Each final owner appears exactly once, even with duplicate aliases.
    pub windows: Vec<Box<Window>>,
    /// New group runtime owner, if this removal transfers ownership to a peer.
    pub runtime_owner_transfer: Option<SessionName>,
}

impl ManagedSessionRemoval {
    /// Stable identity retained for runtime cleanup.
    pub fn id(&self) -> SessionId {
        self.session_id
    }
}

/// Link cleanup from slot unlink or canonical window destruction.
#[derive(Debug)]
#[must_use]
pub struct ManagedWindowRemoval {
    /// Identity of the unlinked window.
    pub window_id: WindowId,
    /// Final owner, only if no aliases survive anywhere.
    pub window: Option<Box<Window>>,
    /// Removed occurrences in stable session-name/index order.
    pub links: Vec<WindowLink>,
    /// Sessions removed when their final slot was unlinked, in the same order.
    pub removed_sessions: Vec<SessionId>,
}

/// The member a group is synchronised from when one of its sessions loses a
/// window link and survives (`session_detach`, `session.c:340-358`).
#[derive(Debug, Clone, Copy)]
pub(super) enum DetachSource<'a> {
    /// `unlink-window` detaches the slot from the session it addressed
    /// (`cmd-kill-window.c`, `server_unlink_window`).
    Addressed(&'a SessionName),
    /// A destroyed window: `server_kill_window` walks the sessions by name
    /// (`server-fn.c`), so the group's first member by name detaches first and
    /// every other member follows it.
    FirstByName,
    /// A detach this store does not model (a moved window): the order of the
    /// windows the group keeps becomes unknown.
    Unmodelled,
}

impl ManagedSessionStore {
    /// Removes only this session's links, transferring final owners without cloning.
    pub fn remove_session(
        &mut self,
        name: &SessionName,
    ) -> Result<ManagedSessionRemoval, RmuxError> {
        let session = self
            .sessions
            .get(name)
            .ok_or_else(|| RmuxError::SessionNotFound(name.to_string()))?;
        let id = session.id;
        let runtime_owner_transfer = self.runtime_owner_transfer_target(name);
        let indices = self
            .windows
            .session_windows(id)
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        let mut windows = Vec::new();
        for window_index in indices {
            if let Some(window) = self
                .windows
                .unlink(WindowLink {
                    session_id: id,
                    window_index,
                })
                .expect("validated session links")
            {
                windows.push(window);
            }
        }
        let removed = self.sessions.remove(name).expect("validated session");
        self.repair_group_runtime_owner(removed.group_name);
        Ok(ManagedSessionRemoval {
            session_id: id,
            windows,
            runtime_owner_transfer,
        })
    }

    /// Unlinks the addressed slot throughout its group, removing every empty
    /// session. Local duplicate and outside-group aliases remain untouched.
    /// All peer tables are validated before any links or metadata change.
    pub fn remove_window(
        &mut self,
        target: &WindowTarget,
    ) -> Result<ManagedWindowRemoval, RmuxError> {
        let source = DetachSource::Addressed(target.session_name());
        let link = self.target_link(target)?;
        let window_id = self.windows.resolve(link).expect("validated link").id();
        let before = self.index_links(target.session_name())?;
        let members = self.session_group_members(target.session_name());
        for member in &members {
            if self.index_links(member)? != before {
                return Err(RmuxError::Server(
                    "managed group link tables differ".to_owned(),
                ));
            }
        }
        let mut result = ManagedWindowRemoval {
            window_id,
            window: None,
            links: Vec::with_capacity(members.len()),
            removed_sessions: Vec::new(),
        };
        for member in members {
            let target = WindowTarget::with_window(member, link.window_index);
            let link = self.target_link(&target).expect("validated group slot");
            let (window, removed_session) = self.remove_occurrence(&target, link, source);
            result.links.push(link);
            result.removed_sessions.extend(removed_session);
            if window.is_some() {
                debug_assert!(result.window.is_none(), "one canonical owner");
                result.window = window;
            }
        }
        Ok(result)
    }

    // Unlike unlink, final-pane destruction removes every occurrence globally.
    // Validate all affected group tables before the first mutation; then use the
    // same local navigation/removal machinery and transfer the Box exactly once.
    pub(super) fn destroy_pane_window(
        &mut self,
        target: &WindowTarget,
    ) -> Result<ManagedWindowRemoval, RmuxError> {
        self.destroy_pane_window_with_session_teardown(target, false)
    }

    pub(super) fn destroy_pane_window_with_session_teardown(
        &mut self,
        target: &WindowTarget,
        defer_session_removal: bool,
    ) -> Result<ManagedWindowRemoval, RmuxError> {
        let window_id = self.window(target)?.id();
        let mut names = self.sessions.keys().cloned().collect::<Vec<_>>();
        names.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        let mut targets = Vec::new();
        for name in names {
            let links = self.index_links(&name)?;
            if !links.values().any(|id| *id == window_id) {
                continue;
            }
            for member in self.session_group_members(&name) {
                if self.index_links(&member)? != links {
                    return Err(RmuxError::Server(
                        "managed group link tables differ".to_owned(),
                    ));
                }
            }
            for (index, id) in links {
                if id == window_id {
                    targets.push(WindowTarget::with_window(name.clone(), index));
                }
            }
        }
        let mut result = ManagedWindowRemoval {
            window_id,
            window: None,
            links: Vec::new(),
            removed_sessions: Vec::new(),
        };
        for target in targets {
            let link = self
                .target_link(&target)
                .expect("validated global occurrence");
            let (window, session) = self.remove_occurrence_with_session_teardown(
                &target,
                link,
                defer_session_removal,
                DetachSource::FirstByName,
            );
            result.links.push(link);
            result.removed_sessions.extend(session);
            if window.is_some() {
                debug_assert!(result.window.is_none(), "one canonical owner");
                result.window = window;
            }
        }
        Ok(result)
    }

    // Only called after validating the complete group. Never expose a local
    // structural mutation on managed sessions through the public API.
    pub(super) fn remove_occurrence(
        &mut self,
        target: &WindowTarget,
        link: WindowLink,
        source: DetachSource<'_>,
    ) -> (Option<Box<Window>>, Option<SessionId>) {
        self.remove_occurrence_with_session_teardown(target, link, false, source)
    }

    pub(super) fn remove_occurrence_with_session_teardown(
        &mut self,
        target: &WindowTarget,
        link: WindowLink,
        defer_session_removal: bool,
        source: DetachSource<'_>,
    ) -> (Option<Box<Window>>, Option<SessionId>) {
        let session = &self.sessions[target.session_name()];
        let remaining = self
            .windows
            .session_windows(session.id)
            .filter(|(index, _)| *index != link.window_index)
            .map(|(index, window)| (index, window.id()))
            .collect::<BTreeMap<_, _>>();
        let replacement = if session.active_window == link.window_index {
            session
                .last_window
                .filter(|index| remaining.contains_key(index))
                .or_else(|| {
                    session.group_initial_window_id.and_then(|id| {
                        remaining
                            .iter()
                            .find_map(|(index, candidate)| (*candidate == id).then_some(*index))
                    })
                })
                .or_else(|| {
                    remaining
                        .range(..link.window_index)
                        .next_back()
                        .or_else(|| remaining.last_key_value())
                        .map(|(index, _)| *index)
                })
        } else {
            None
        };
        let window = self.windows.unlink(link).expect("validated link");
        let members = self.session_group_members(target.session_name());
        if !remaining.is_empty() && members.len() > 1 {
            // A PARTIAL detach from a member of a group runs
            // `session_group_synchronize_from` (`session.c:340-358`), which for
            // every other member empties its winlink list and creates it again
            // from the source (`:612-626`, `:633-689`); each recreated winlink
            // is appended at the tail of its own window's list. So the windows
            // that SURVIVE have their names rebuilt even though their links do
            // not change here.
            //
            // Destroying a whole session is not that: it leaves the group and
            // then removes only its own winlinks (`session.c:195-229`), which is
            // why that path - reached with no window left here - keeps the
            // survivors' order exactly as it was.
            let synchronised = match source {
                DetachSource::Addressed(name) => self.group_synchronised_from(name),
                DetachSource::FirstByName => self.group_synchronised_from(&members[0]),
                DetachSource::Unmodelled => None,
            };
            match synchronised {
                Some((source, peers)) => self.windows.synchronise_group_from(source, &peers),
                None => self.windows.forget_link_order(remaining.values().copied()),
            }
        }
        if remaining.is_empty() {
            if defer_session_removal {
                return (window, Some(link.session_id));
            }
            let removed = self
                .sessions
                .remove(target.session_name())
                .expect("validated session");
            self.repair_group_runtime_owner(removed.group_name);
            return (window, Some(link.session_id));
        }
        self.metadata_mut(target.session_name())
            .expect("validated session")
            .winlink_alert_flags
            .remove(&link.window_index);
        if let Some(index) = replacement {
            self.select_window(&WindowTarget::with_window(
                target.session_name().clone(),
                index,
            ))
            .expect("surviving replacement");
        }
        let session = self
            .metadata_mut(target.session_name())
            .expect("validated session");
        if session.last_window == Some(link.window_index) {
            // `session_last` takes the head of the stack that survived
            // (`session.c:461-471`), so the return keeps working while the
            // history still names a live window; only an empty one leaves none.
            session.last_window = None;
        }
        // The occurrence is gone: tmux drops it from the stack the same way
        // (`session.c:350` through `window.c:255-260`), so nothing dead and no
        // index taken over by another window can be recalled later.
        crate::session::visit_history::forget_occurrence(
            &mut session.window_stack,
            link.window_index,
        );
        if session.last_window.is_none() {
            session.last_window = session
                .window_stack
                .first()
                .copied()
                .filter(|head| *head != session.active_window);
        }
        (window, None)
    }
}
