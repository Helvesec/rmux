use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use rmux_proto::{RmuxError, SessionName, TerminalSize, WindowTarget};

use super::{current_unix_timestamp, invalid_window_target, SessionRecency};
use crate::{
    AlertFlags, PaneId, SessionId, Window, WindowId, WindowLayoutContext, WindowLink,
    WindowSelection, WindowStateStore, WindowSwitch,
};

mod activity;
mod checkpoint;
mod structural_checkpoint;
pub use structural_checkpoint::ManagedStructureCheckpoint;
mod creation;
mod group_navigation;
mod navigation;
pub use checkpoint::ManagedWindowCheckpoint;
pub use group_navigation::ManagedGroupNavigation;
mod geometry;
mod index_ops;
mod index_state;
mod link_reshape;
mod window_creation;
mod window_occurrences;
pub use index_state::{ManagedIndexChange, ManagedSessionIndexChange};
pub use window_creation::{ManagedWindowCreation, ManagedWindowNameChange};
mod groups;
mod panes;
mod presentation;
mod preview;
pub use presentation::ManagedSessionPresentation;
mod respawn;
mod transfer_break;
mod transfer_indices;
mod transfers;
pub use transfer_break::ManagedPaneBreak;
pub use transfers::ManagedPaneTransfer;
mod removal;
pub use panes::{ManagedPaneCreation, ManagedPaneRemoval, ManagedPaneSplit};
use removal::DetachSource;
pub use removal::{ManagedSessionRemoval, ManagedWindowRemoval};
mod view;
pub use view::ManagedSessionView;

/// Session-local state only. Window owners and link identities live in the
/// canonical store; this table carries only per-occurrence alerts/navigation.
#[derive(Debug, Clone)]
#[cfg_attr(any(test, feature = "test-seams"), derive(PartialEq, Eq))]
struct ManagedSession {
    id: SessionId,
    name: SessionName,
    group_name: Option<SessionName>,
    terminal_size: TerminalSize,
    active_window: u32,
    last_window: Option<u32>,
    /// Windows this session really visited, most recent first, never the active one.
    window_stack: Vec<u32>,
    group_initial_window_id: Option<WindowId>,
    winlink_alert_flags: BTreeMap<u32, AlertFlags>,
    created_at: i64,
    activity_at: i64,
    last_attached_at: Option<i64>,
    recency: SessionRecency,
    cwd: Option<PathBuf>,
}

/// Managed sessions with exactly one canonical owner for every linked window.
///
/// This feature-branch API is not yet used by live server consumers. It never
/// constructs, retains or clones standalone sessions. Mutable access is limited
/// to operation contexts so callers cannot replace owners or invalidate links.
#[derive(Debug, Default)]
pub struct ManagedSessionStore {
    sessions: HashMap<SessionName, ManagedSession>,
    windows: WindowStateStore,
    // A wider counter represents exhaustion without recycling the final ID.
    next_session_id: u64,
    next_window_id: u64,
    next_pane_id: u64,
    group_runtime_owners: HashMap<SessionName, SessionName>,
}

impl ManagedSessionStore {
    /// Detached metadata for format loops. No live process or terminal is copied.
    /// The caller must retain this only as an immutable rendering input.
    #[must_use]
    pub fn render_snapshot(&self) -> Self {
        Self {
            sessions: self.sessions.clone(),
            windows: self.windows.render_snapshot(),
            next_session_id: self.next_session_id,
            next_window_id: self.next_window_id,
            next_pane_id: self.next_pane_id,
            group_runtime_owners: self.group_runtime_owners.clone(),
        }
    }

    /// Creates an empty managed store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Next session identity counter, including the exhausted sentinel.
    pub fn next_session_id(&self) -> u64 {
        self.next_session_id
    }

    /// Returns the number of managed sessions.
    #[must_use]
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    /// Returns whether there are no sessions.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// Whether a named session exists.
    pub fn contains_session(&self, name: &SessionName) -> bool {
        self.sessions.contains_key(name)
    }

    /// Borrows metadata and canonical windows for a named session.
    pub fn session(&self, name: &SessionName) -> Option<ManagedSessionView<'_>> {
        self.sessions.get(name).map(|metadata| ManagedSessionView {
            viewport_offset: None,
            metadata,
            windows: &self.windows,
            overrides: None,
        })
    }

    /// Borrows a session by its stable identity.
    pub fn session_by_id(&self, id: SessionId) -> Option<ManagedSessionView<'_>> {
        self.sessions
            .values()
            .find(|session| session.id == id)
            .map(|metadata| ManagedSessionView {
                viewport_offset: None,
                metadata,
                windows: &self.windows,
                overrides: None,
            })
    }

    /// Iterates borrowed sessions in unspecified order.
    pub fn iter(&self) -> impl Iterator<Item = ManagedSessionView<'_>> {
        self.sessions.values().map(|metadata| ManagedSessionView {
            viewport_offset: None,
            metadata,
            windows: &self.windows,
            overrides: None,
        })
    }

    /// Adds an alias without selecting it or copying window state. Both the
    /// destination session and canonical window must already exist. Group peers
    /// receive the same slot; use `insert_alias` for the typed structural outcome.
    pub fn link_window(&mut self, target: &WindowTarget, id: WindowId) -> Result<(), RmuxError> {
        let _ = self.insert_alias(target, id)?;
        Ok(())
    }

    /// Borrows an addressed canonical window.
    pub fn window(&self, target: &WindowTarget) -> Result<&Window, RmuxError> {
        self.windows
            .resolve(self.target_link(target)?)
            .map_err(|_| invalid_window_target(target.session_name(), target.window_index()))
    }

    /// Borrows the existing layout context for an addressed canonical window.
    pub fn window_layout_context_mut(
        &mut self,
        target: &WindowTarget,
    ) -> Result<WindowLayoutContext<'_>, RmuxError> {
        let link = self.target_link(target)?;
        Ok(self
            .windows
            .layout_context(link, target.session_name().clone())
            .expect("validated link"))
    }

    /// Borrows pane selection without changing session-local window navigation.
    pub fn window_selection_mut(
        &mut self,
        target: &WindowTarget,
    ) -> Result<WindowSelection<'_>, RmuxError> {
        let link = self.target_link(target)?;
        let session = &self.sessions[target.session_name()];
        Ok(WindowSelection::new(
            &session.name,
            link.window_index,
            self.windows.resolve_mut(link).expect("validated link"),
        ))
    }

    /// Borrows shared window mutation together with only the addressed session's
    /// local navigation, alerts and outer terminal geometry.
    pub fn window_switch_mut(
        &mut self,
        target: &WindowTarget,
    ) -> Result<WindowSwitch<'_>, RmuxError> {
        let link = self.target_link(target)?;
        let session = self
            .sessions
            .get_mut(target.session_name())
            .expect("validated session");
        Ok(WindowSwitch {
            session_name: &session.name,
            window_index: link.window_index,
            terminal_size: &mut session.terminal_size,
            window: self.windows.resolve_mut(link).expect("validated link"),
            winlink_alert_flags: &mut session.winlink_alert_flags,
            active_window: &mut session.active_window,
            last_window: &mut session.last_window,
            window_stack: &mut session.window_stack,
            group_initial_window_id: &mut session.group_initial_window_id,
        })
    }

    /// Selects a local window with the same navigation logic as standalone sessions.
    pub fn select_window(&mut self, target: &WindowTarget) -> Result<(), RmuxError> {
        self.window_switch_mut(target)?.select_window();
        Ok(())
    }

    /// Selects this session's previously active occurrence.
    pub fn last_window(&mut self, name: &SessionName) -> Result<u32, RmuxError> {
        let index = self
            .metadata_mut(name)?
            .last_window
            .ok_or_else(|| RmuxError::Message("no last window".to_owned()))?;
        self.select_window(&WindowTarget::with_window(name.clone(), index))?;
        Ok(index)
    }

    /// Renames metadata without changing session identity or any links.
    pub fn rename_session(
        &mut self,
        name: &SessionName,
        new_name: SessionName,
    ) -> Result<(), RmuxError> {
        if !self.sessions.contains_key(name) {
            return Err(RmuxError::SessionNotFound(name.to_string()));
        }
        if name == &new_name {
            return Ok(());
        }
        if self.sessions.contains_key(&new_name) {
            return Err(RmuxError::DuplicateSession(new_name.to_string()));
        }
        let mut session = self.sessions.remove(name).expect("validated session");
        session.name = new_name.clone();
        if let Some(group) = &session.group_name {
            if self.group_runtime_owners.get(group) == Some(name) {
                self.group_runtime_owners
                    .insert(group.clone(), new_name.clone());
            }
        }
        self.sessions.insert(new_name, session);
        Ok(())
    }

    /// Adds alerts to one occurrence, independent of every alias.
    pub fn add_winlink_alert_flags(
        &mut self,
        target: &WindowTarget,
        flags: AlertFlags,
    ) -> Result<bool, RmuxError> {
        self.target_link(target)?;
        let entry = self
            .sessions
            .get_mut(target.session_name())
            .expect("validated session")
            .winlink_alert_flags
            .get_mut(&target.window_index())
            .expect("every link has local alerts");
        let changed = !entry.contains(flags);
        entry.insert(flags);
        Ok(changed)
    }

    /// Clears selected alerts on one occurrence only.
    pub fn clear_winlink_alert_flags(
        &mut self,
        target: &WindowTarget,
        flags: AlertFlags,
    ) -> Result<bool, RmuxError> {
        self.target_link(target)?;
        let entry = self
            .sessions
            .get_mut(target.session_name())
            .expect("validated session")
            .winlink_alert_flags
            .get_mut(&target.window_index())
            .expect("every link has local alerts");
        let changed = entry.intersects(flags);
        entry.remove(flags);
        Ok(changed)
    }

    /// Assigns the session working directory without touching its windows.
    pub fn set_cwd(&mut self, name: &SessionName, cwd: Option<PathBuf>) -> Result<(), RmuxError> {
        self.metadata_mut(name)?.cwd = cwd;
        Ok(())
    }

    /// Records an accepted interaction, preserving attach history.
    pub fn touch_activity(&mut self, name: &SessionName) -> Result<(), RmuxError> {
        let session = self.metadata_mut(name)?;
        session.activity_at = current_unix_timestamp();
        session.recency = SessionRecency::next();
        Ok(())
    }

    /// Records an attach in session-local metadata.
    pub fn touch_attached(&mut self, name: &SessionName) -> Result<(), RmuxError> {
        self.touch_activity(name)?;
        let session = self.metadata_mut(name)?;
        session.last_attached_at = Some(session.activity_at);
        Ok(())
    }

    /// Advances the allocator for fixtures that assert a specific sparse identity.
    #[cfg(any(test, feature = "test-seams"))]
    #[doc(hidden)]
    pub fn advance_next_pane_id_for_tests(&mut self, id: PaneId) {
        assert!(u64::from(id.as_u32()) >= self.next_pane_id);
        self.next_pane_id = u64::from(id.as_u32());
    }

    /// Pins public timestamps for deterministic same-second recency tests.
    #[cfg(any(test, feature = "test-seams"))]
    #[doc(hidden)]
    pub fn pin_public_times_for_tests(
        &mut self,
        name: &SessionName,
        seconds: i64,
    ) -> Result<(), RmuxError> {
        let session = self.metadata_mut(name)?;
        session.created_at = seconds;
        session.activity_at = seconds;
        session.last_attached_at = session.last_attached_at.map(|_| seconds);
        Ok(())
    }

    /// Sets pending alerts without draining them, for teardown fixtures.
    #[cfg(any(test, feature = "test-seams"))]
    #[doc(hidden)]
    pub fn queue_window_alerts_for_tests(
        &mut self,
        target: &WindowTarget,
        flags: AlertFlags,
    ) -> Result<(), RmuxError> {
        let link = self.target_link(target)?;
        self.windows
            .resolve_mut(link)
            .expect("validated link")
            .queue_alerts(flags);
        Ok(())
    }

    fn metadata_mut(&mut self, name: &SessionName) -> Result<&mut ManagedSession, RmuxError> {
        self.sessions
            .get_mut(name)
            .ok_or_else(|| RmuxError::SessionNotFound(name.to_string()))
    }

    fn target_link(&self, target: &WindowTarget) -> Result<WindowLink, RmuxError> {
        let session = self
            .sessions
            .get(target.session_name())
            .ok_or_else(|| RmuxError::SessionNotFound(target.session_name().to_string()))?;
        let link = WindowLink {
            session_id: session.id,
            window_index: target.window_index(),
        };
        self.windows
            .resolve(link)
            .map_err(|_| invalid_window_target(&session.name, link.window_index))?;
        Ok(link)
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
#[path = "managed/wstack1_tests.rs"]
mod wstack1_tests;

/// Detached assertion data, never accepted by a managed mutation API.
#[cfg(any(test, feature = "test-seams"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedSessionTestSnapshot {
    metadata: ManagedSession,
    windows: BTreeMap<u32, Window>,
}

#[cfg(any(test, feature = "test-seams"))]
impl ManagedSessionView<'_> {
    /// Captures every metadata field and linked window for rollback assertions.
    pub fn test_snapshot(self) -> ManagedSessionTestSnapshot {
        ManagedSessionTestSnapshot {
            metadata: self.metadata.clone(),
            windows: self.windows().map(|(i, w)| (i, w.clone())).collect(),
        }
    }
}

#[cfg(any(test, feature = "test-seams"))]
impl ManagedSessionTestSnapshot {
    /// Reads detached window assertion data.
    pub fn window_at(&self, index: u32) -> Option<&Window> {
        self.windows.get(&index)
    }
    /// Reads detached navigation assertion data.
    pub fn last_window_index(&self) -> Option<u32> {
        self.metadata.last_window
    }
}
