use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rmux_proto::{SessionName, TerminalSize};

use super::{current_unix_timestamp, synchronized_active_window, Session, WindowIdAllocator};
use crate::{AlertFlags, Pane, PaneId, SessionId, Window, WINLINK_ALERTFLAGS};

impl Session {
    /// Returns the stable validated session name.
    #[must_use]
    pub const fn name(&self) -> &SessionName {
        &self.name
    }

    /// Returns the named session group when the session is grouped.
    #[must_use]
    pub const fn group_name(&self) -> Option<&SessionName> {
        self.group_name.as_ref()
    }

    /// Returns the store-assigned session identity used by `$N` targets.
    #[must_use]
    pub const fn id(&self) -> SessionId {
        self.id
    }

    /// Returns the session creation timestamp as Unix seconds.
    #[must_use]
    pub const fn created_at(&self) -> i64 {
        self.created_at
    }

    /// Returns the last session activity timestamp as Unix seconds.
    #[must_use]
    pub const fn activity_at(&self) -> i64 {
        self.activity_at
    }

    /// Returns the last attached timestamp as Unix seconds.
    #[must_use]
    pub const fn last_attached_at(&self) -> Option<i64> {
        self.last_attached_at
    }

    /// Returns the session working directory when one has been assigned.
    #[must_use]
    pub fn cwd(&self) -> Option<&Path> {
        self.cwd.as_deref()
    }

    /// Returns the terminal size last applied to the session as a whole.
    #[must_use]
    pub const fn terminal_size(&self) -> TerminalSize {
        self.terminal_size
    }

    pub(crate) fn set_id(&mut self, id: SessionId) {
        self.id = id;
    }

    pub(crate) fn rebind_window_id_allocator(&mut self, allocator: WindowIdAllocator) {
        let next_after_windows = self
            .windows
            .values()
            .map(|window| window.id().as_u32().saturating_add(1))
            .max()
            .unwrap_or_else(|| allocator.peek());
        self.next_window_id = allocator;
        self.next_window_id.bump_to(next_after_windows);
    }

    /// Renames the session without rewriting any other session state.
    pub fn rename(&mut self, new_name: SessionName) {
        self.name = new_name;
    }

    /// Assigns or clears the session group name without mutating any other session state.
    pub fn set_group_name(&mut self, group_name: Option<SessionName>) {
        self.group_name = group_name;
    }

    /// Updates the session working directory.
    pub fn set_cwd(&mut self, cwd: Option<PathBuf>) {
        self.cwd = cwd;
    }

    /// Records session activity at the current time.
    pub fn touch_activity(&mut self) {
        self.activity_at = current_unix_timestamp();
    }

    /// Records that a client attached to the session at the current time.
    pub fn touch_attached(&mut self) {
        let now = current_unix_timestamp();
        self.activity_at = now;
        self.last_attached_at = Some(now);
    }

    /// Clones the session as a grouped peer with a fresh identity and timestamps.
    #[must_use]
    pub fn clone_as_group_member(
        &self,
        name: SessionName,
        group_name: SessionName,
        session_id: SessionId,
    ) -> Self {
        let now = current_unix_timestamp();
        let mut cloned = self.clone();
        cloned.id = session_id;
        cloned.name = name;
        cloned.group_name = Some(group_name);
        cloned.created_at = now;
        cloned.activity_at = now;
        cloned.last_attached_at = None;
        cloned
    }

    /// Synchronizes shared grouped-session window state from the source session while preserving the local current window when possible.
    pub fn synchronize_group_from(&mut self, source: &Session) {
        debug_assert_ne!(self.name, source.name);
        let previous_active = self.active_window;
        let previous_last = self.last_window;

        self.windows = source.windows.clone();
        self.winlink_alert_flags = source.winlink_alert_flags.clone();
        self.next_pane_id = source.next_pane_id;
        self.cwd = source.cwd.clone();

        self.active_window =
            synchronized_active_window(&self.windows, previous_active, previous_last);
        self.last_window = previous_last
            .filter(|window_index| {
                *window_index != self.active_window && self.windows.contains_key(window_index)
            })
            .or_else(|| {
                (previous_active != self.active_window
                    && self.windows.contains_key(&previous_active))
                .then_some(previous_active)
            });
    }

    /// Returns the session's active window.
    #[must_use]
    pub fn window(&self) -> &Window {
        self.window_at(self.active_window)
            .expect("active session window must exist")
    }

    /// Returns the explicitly addressed window when it exists.
    #[must_use]
    pub fn window_at(&self, window_index: u32) -> Option<&Window> {
        self.windows.get(&window_index)
    }

    /// Returns all windows keyed by window index.
    #[must_use]
    pub const fn windows(&self) -> &BTreeMap<u32, Window> {
        &self.windows
    }

    pub(crate) fn window_mut(&mut self) -> &mut Window {
        self.window_at_mut(self.active_window)
            .expect("active session window must exist")
    }

    /// Returns the addressed window as a mutable reference when it exists.
    pub fn window_at_mut(&mut self, window_index: u32) -> Option<&mut Window> {
        self.windows.get_mut(&window_index)
    }

    /// Returns the persistent alert flags for the addressed window slot.
    #[must_use]
    pub fn winlink_alert_flags(&self, window_index: u32) -> AlertFlags {
        self.winlink_alert_flags
            .get(&window_index)
            .copied()
            .unwrap_or_else(AlertFlags::empty)
    }

    /// Returns the combined session alert flags across all alerted windows.
    #[must_use]
    pub fn session_alert_flags(&self) -> AlertFlags {
        self.winlink_alert_flags
            .values()
            .copied()
            .fold(AlertFlags::empty(), |flags, winlink_flags| {
                flags.union(winlink_flags)
            })
    }

    /// Returns the alerted window indexes in display order.
    #[must_use]
    pub fn alerted_window_indexes(&self) -> Vec<u32> {
        self.winlink_alert_flags
            .iter()
            .filter_map(|(window_index, flags)| {
                flags
                    .intersects(WINLINK_ALERTFLAGS)
                    .then_some(*window_index)
            })
            .collect()
    }

    /// Returns whether any window in the session currently carries an alert.
    #[must_use]
    pub fn has_alerts(&self) -> bool {
        self.winlink_alert_flags
            .values()
            .any(|flags| flags.intersects(WINLINK_ALERTFLAGS))
    }

    /// Adds persistent alert flags to the addressed session winlink.
    pub fn add_winlink_alert_flags(&mut self, window_index: u32, flags: AlertFlags) -> bool {
        if !self.windows.contains_key(&window_index) {
            return false;
        }

        let entry = self
            .winlink_alert_flags
            .entry(window_index)
            .or_insert_with(AlertFlags::empty);
        let changed = !entry.contains(flags);
        entry.insert(flags);
        changed
    }

    /// Clears the selected persistent alert flags from the addressed session winlink.
    pub fn clear_winlink_alert_flags(&mut self, window_index: u32, flags: AlertFlags) -> bool {
        let Some(entry) = self.winlink_alert_flags.get_mut(&window_index) else {
            return false;
        };
        if !entry.intersects(flags) {
            return false;
        }

        entry.remove(flags);
        true
    }

    /// Clears all persistent alert flags from the addressed session winlink.
    pub fn clear_all_winlink_alert_flags(&mut self, window_index: u32) -> bool {
        self.clear_winlink_alert_flags(window_index, WINLINK_ALERTFLAGS)
    }

    /// Returns the active window index.
    #[must_use]
    pub const fn active_window_index(&self) -> u32 {
        self.active_window
    }

    /// Returns the previously active window index when one exists.
    #[must_use]
    pub const fn last_window_index(&self) -> Option<u32> {
        self.last_window
    }

    /// Returns the active pane index owned by the session.
    #[must_use]
    pub fn active_pane_index(&self) -> u32 {
        self.window().active_pane_index()
    }

    /// Returns the stable internal identity for the active pane.
    #[must_use]
    pub fn active_pane_id(&self) -> Option<PaneId> {
        self.active_pane().map(Pane::id)
    }

    /// Returns the active pane when the session invariant is satisfied.
    #[must_use]
    pub fn active_pane(&self) -> Option<&Pane> {
        self.window().active_pane()
    }

    /// Returns the stable internal identity for a pane in the active window.
    #[must_use]
    pub fn pane_id(&self, pane_index: u32) -> Option<PaneId> {
        self.pane_id_in_window(self.active_window, pane_index)
    }

    /// Returns the stable internal identity for a pane in the addressed window.
    #[must_use]
    pub fn pane_id_in_window(&self, window_index: u32, pane_index: u32) -> Option<PaneId> {
        self.window_at(window_index)
            .and_then(|window| window.pane_id(pane_index))
    }
}
