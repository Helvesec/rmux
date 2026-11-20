use super::creation::checked_id;
use super::index_ops::{first_gap, occupied};
use super::index_state::LocalState;
use super::*;

/// Newly allocated canonical window and the links introduced for its group.
#[derive(Debug)]
#[must_use]
pub struct ManagedWindowCreation {
    /// Addressed session's new slot.
    pub window_index: u32,
    /// Independent canonical window identity.
    pub window_id: WindowId,
    /// Independent initial pane identity.
    pub pane_id: PaneId,
    /// Structural changes for later event consumers.
    pub change: ManagedIndexChange,
}

/// Canonical naming outcome, shared by every alias regardless of group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedWindowNameChange {
    /// Canonical window identity.
    pub window_id: WindowId,
    /// Previous name.
    pub previous_name: Option<String>,
    /// Resulting name.
    pub name: Option<String>,
    /// Whether automatic naming remains enabled.
    pub automatic_rename: bool,
}

impl ManagedSessionStore {
    /// Creates at the lowest free local index without selecting the new window.
    pub fn create_window(
        &mut self,
        name: &SessionName,
        size: TerminalSize,
    ) -> Result<ManagedWindowCreation, RmuxError> {
        self.create_window_at_or_above(name, size, 0)
    }

    /// Creates at the first free index at or above the requested minimum.
    pub fn create_window_at_or_above(
        &mut self,
        name: &SessionName,
        size: TerminalSize,
        minimum: u32,
    ) -> Result<ManagedWindowCreation, RmuxError> {
        let index = first_gap(&self.index_links(name)?, name, minimum)?;
        self.insert_window_with_initial_pane(name, index, size)
    }

    /// Inserts at an explicit vacant slot. Rejections consume no identities.
    pub fn insert_window_with_initial_pane(
        &mut self,
        name: &SessionName,
        index: u32,
        size: TerminalSize,
    ) -> Result<ManagedWindowCreation, RmuxError> {
        let mut links = self.index_links(name)?;
        if links.contains_key(&index) {
            return Err(occupied(name, index));
        }
        let window_id = WindowId::new(checked_id(self.next_window_id, "window")?);
        let pane_id = PaneId::new(checked_id(self.next_pane_id, "pane")?);
        let occurrences: std::collections::BTreeMap<u32, u32> =
            links.keys().map(|&i| (i, i)).collect();
        links.insert(index, window_id);
        let mut local = LocalState::from_session(&self.sessions[name]);
        local.alerts.insert(index, AlertFlags::empty());
        let window = Window::new_with_initial_pane(size, pane_id, window_id);
        // The window that appears is new and unvisited; every other slot keeps
        // the visits it holds, in this session and in its peers alike.
        let history = super::index_state::identity_map(&links);
        let plan = self.link_made_plan(&WindowTarget::with_window(name.clone(), index));
        let change = self.commit_indices(
            name,
            links,
            super::index_state::IndexMaps {
                occurrences,
                history,
                link_order: plan,
            },
            local,
            None,
            Some(window),
        )?;
        self.next_window_id += 1;
        self.next_pane_id += 1;
        Ok(ManagedWindowCreation {
            window_index: index,
            window_id,
            pane_id,
            change,
        })
    }

    /// Adds a group-coherent alias to an existing owner, with a typed link outcome.
    pub fn insert_alias(
        &mut self,
        target: &WindowTarget,
        id: WindowId,
    ) -> Result<ManagedIndexChange, RmuxError> {
        let name = target.session_name();
        let index = target.window_index();
        let mut links = self.index_links(name)?;
        if links.contains_key(&index) {
            return Err(occupied(name, index));
        }
        let occurrences: std::collections::BTreeMap<u32, u32> =
            links.keys().map(|&i| (i, i)).collect();
        links.insert(index, id);
        let mut local = LocalState::from_session(&self.sessions[name]);
        local.alerts.insert(index, AlertFlags::empty());
        let history = super::index_state::identity_map(&links);
        let plan = self.link_made_plan(target);
        self.commit_indices(
            name,
            links,
            super::index_state::IndexMaps {
                occurrences,
                history,
                link_order: plan,
            },
            local,
            None,
            None,
        )
    }

    /// Renames the canonical owner and disables automatic naming for all aliases.
    pub fn rename_window(
        &mut self,
        target: &WindowTarget,
        name: String,
    ) -> Result<ManagedWindowNameChange, RmuxError> {
        self.change_window_name(target, name, false)
    }

    /// Applies an automatic name only while the canonical owner permits it.
    pub fn set_automatic_window_name(
        &mut self,
        target: &WindowTarget,
        name: String,
    ) -> Result<ManagedWindowNameChange, RmuxError> {
        self.change_window_name(target, name, true)
    }

    /// Enables automatic naming on the addressed canonical window.
    pub fn enable_automatic_window_name(&mut self, target: &WindowTarget) -> Result<(), RmuxError> {
        let link = self.target_link(target)?;
        self.windows
            .resolve_mut(link)
            .expect("validated link")
            .enable_automatic_rename();
        Ok(())
    }

    fn change_window_name(
        &mut self,
        target: &WindowTarget,
        name: String,
        automatic: bool,
    ) -> Result<ManagedWindowNameChange, RmuxError> {
        let link = self.target_link(target)?;
        let window = self.windows.resolve_mut(link).expect("validated link");
        let previous_name = window.name().map(str::to_owned);
        if !automatic {
            window.set_name(name);
        } else if window.automatic_rename() {
            window.set_automatic_name(name);
        }
        Ok(ManagedWindowNameChange {
            window_id: window.id(),
            previous_name,
            name: window.name().map(str::to_owned),
            automatic_rename: window.automatic_rename(),
        })
    }
}
