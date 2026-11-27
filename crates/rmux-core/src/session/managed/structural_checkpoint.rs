use super::*;
use crate::window::WindowCheckpoint;

/// Bounded undo for explicitly named session families and their canonical windows.
/// Removed owners must be retained from mutation outcomes until commit or rollback.
/// This is not a live session representation and cannot be inserted as one.
pub struct ManagedStructureCheckpoint {
    sessions: Vec<ManagedSession>,
    links: BTreeMap<SessionId, BTreeMap<u32, WindowId>>,
    windows: BTreeMap<WindowId, (WindowLink, WindowCheckpoint)>,
    /// The COMPLETE order state of every captured window: the exact links it
    /// had been made through, or `None` where the store could not vouch for
    /// them. Both are restored as they were, so an undone change leaves the
    /// order it found and never a rebuilt one - and an unknown order comes back
    /// unknown instead of being invented from the restored tables.
    order: BTreeMap<WindowId, Option<Vec<WindowLink>>>,
    runtime_owners: Vec<(SessionName, Option<SessionName>)>,
    #[allow(clippy::vec_box)] // Retain canonical allocation identity through rollback.
    removed: Vec<Box<Window>>,
}

impl ManagedStructureCheckpoint {
    /// Transfers retained owners to an enclosing transaction before this one commits.
    pub fn take_removed(&mut self) -> Vec<Box<Window>> {
        std::mem::take(&mut self.removed)
    }

    /// Retains final owners without moving their allocations.
    pub fn retain_removed(&mut self, windows: impl IntoIterator<Item = Box<Window>>) {
        self.removed.extend(windows);
    }
}

impl ManagedSessionStore {
    /// Captures only the named families and aliases of their windows. Other
    /// sessions and owners are neither copied nor replaced by rollback.
    pub fn checkpoint_structure(
        &self,
        names: &[SessionName],
    ) -> Result<ManagedStructureCheckpoint, RmuxError> {
        let mut selected = std::collections::HashSet::new();
        let mut ids = std::collections::BTreeSet::new();
        for name in names {
            let session = self
                .session(name)
                .ok_or_else(|| RmuxError::SessionNotFound(name.to_string()))?;
            ids.extend(session.windows().map(|(_, window)| window.id()));
            selected.extend(self.session_group_members(name));
        }
        for session in self.iter() {
            if session
                .windows()
                .any(|(_, window)| ids.contains(&window.id()))
            {
                selected.insert(session.name().clone());
            }
        }
        let mut checkpoint = ManagedStructureCheckpoint {
            sessions: Vec::new(),
            links: BTreeMap::new(),
            windows: BTreeMap::new(),
            order: BTreeMap::new(),
            runtime_owners: Vec::new(),
            removed: Vec::new(),
        };
        let mut groups = std::collections::HashSet::new();
        let mut selected = selected.into_iter().collect::<Vec<_>>();
        selected.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        for name in selected {
            let session = &self.sessions[&name];
            checkpoint.sessions.push(session.clone());
            checkpoint
                .links
                .insert(session.id, self.index_links(&name)?);
            if let Some(group) = &session.group_name {
                groups.insert(group.clone());
            }
            for (index, window) in self.windows.session_windows(session.id) {
                checkpoint.windows.entry(window.id()).or_insert_with(|| {
                    (
                        WindowLink {
                            session_id: session.id,
                            window_index: index,
                        },
                        window.checkpoint(),
                    )
                });
                ids.insert(window.id());
            }
        }
        // Taken after the walk, so every captured identity is here exactly once,
        // with the whole of its order - the links it holds in sessions this
        // checkpoint did not select included.
        for id in ids {
            checkpoint
                .order
                .insert(id, self.windows.window_link_order(id).map(<[_]>::to_vec));
        }
        checkpoint.runtime_owners = groups
            .into_iter()
            .map(|group| {
                let owner = self.group_runtime_owners.get(&group).cloned();
                (group, owner)
            })
            .collect();
        Ok(checkpoint)
    }

    /// Restores original owners, links, navigation and window contents. Identity
    /// allocation counters deliberately remain monotonic after a failed attempt.
    pub fn restore_structure_checkpoint(
        &mut self,
        mut checkpoint: ManagedStructureCheckpoint,
    ) -> Result<(), RmuxError> {
        self.windows
            .restore_links(
                &checkpoint.links,
                &mut checkpoint.removed,
                &checkpoint.order,
            )
            .map_err(|error| RmuxError::Server(format!("structural rollback failed: {error}")))?;
        for (_, (link, window)) in checkpoint.windows {
            self.windows
                .resolve_mut(link)
                .expect("restored links")
                .restore_checkpoint(window);
        }
        for session in checkpoint.sessions {
            self.sessions.insert(session.name.clone(), session);
        }
        for (group, owner) in checkpoint.runtime_owners {
            if let Some(owner) = owner {
                self.group_runtime_owners.insert(group, owner);
            } else {
                self.group_runtime_owners.remove(&group);
            }
        }
        Ok(())
    }
}
