use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use rmux_proto::{SessionName, TerminalSize};

use super::{current_unix_timestamp, synchronized_active_window, Session, WindowIdAllocator};
use crate::{AlertFlags, Pane, PaneId, SessionId, Window, WindowId, WINLINK_ALERTFLAGS};

fn window_index_for_id(
    windows: &BTreeMap<u32, Window>,
    window_id: WindowId,
    preferred_index: u32,
) -> Option<u32> {
    if windows
        .get(&preferred_index)
        .is_some_and(|window| window.id() == window_id)
    {
        return Some(preferred_index);
    }
    let mut matches = windows
        .iter()
        .filter_map(|(window_index, window)| (window.id() == window_id).then_some(*window_index));
    let only_match = matches.next()?;
    matches.next().is_none().then_some(only_match)
}

fn remap_local_winlink_alert_flags(
    previous_windows: &BTreeMap<u32, Window>,
    previous_flags: &BTreeMap<u32, AlertFlags>,
    synchronized_windows: &BTreeMap<u32, Window>,
    source_flags: &BTreeMap<u32, AlertFlags>,
    winlink_alert_map: Option<&BTreeMap<u32, u32>>,
) -> BTreeMap<u32, AlertFlags> {
    let mut synchronized_indices = BTreeMap::new();
    let mut claimed = BTreeSet::new();

    if let Some(winlink_alert_map) = winlink_alert_map {
        for (&previous_index, previous_window) in previous_windows {
            let Some(&next_index) = winlink_alert_map.get(&previous_index) else {
                continue;
            };
            if synchronized_windows
                .get(&next_index)
                .is_some_and(|window| window.id() == previous_window.id())
                && claimed.insert(next_index)
            {
                synchronized_indices.insert(previous_index, next_index);
            }
        }
    }

    // Reserve every surviving exact slot before falling back by WindowId. In
    // particular, a removed duplicate alias must not steal the surviving
    // alias's index merely because both aliases share one WindowId.
    for (&previous_index, previous_window) in previous_windows {
        if synchronized_indices.contains_key(&previous_index) {
            continue;
        }
        if synchronized_windows
            .get(&previous_index)
            .is_some_and(|window| window.id() == previous_window.id())
            && claimed.insert(previous_index)
        {
            synchronized_indices.insert(previous_index, previous_index);
        }
    }

    for (&previous_index, previous_window) in previous_windows {
        if synchronized_indices.contains_key(&previous_index) {
            continue;
        }
        let next_index = synchronized_windows.iter().find_map(|(&index, window)| {
            (window.id() == previous_window.id() && !claimed.contains(&index)).then_some(index)
        });
        if let Some(next_index) = next_index {
            claimed.insert(next_index);
            synchronized_indices.insert(previous_index, next_index);
        }
    }

    let mut remapped = previous_flags
        .iter()
        .filter_map(|(&previous_index, &flags)| {
            synchronized_indices
                .get(&previous_index)
                .copied()
                .map(|next_index| (next_index, flags))
        })
        .collect::<BTreeMap<_, _>>();
    for &window_index in synchronized_windows.keys() {
        remapped.entry(window_index).or_insert_with(|| {
            source_flags
                .get(&window_index)
                .copied()
                .unwrap_or_else(AlertFlags::empty)
        });
    }
    remapped
}

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

    /// Returns the internal total-order token behind `activity_at`.
    ///
    /// Targetless resolution ranks by this token so two sessions whose public
    /// whole-second timestamps collide still have a defined order. It is not a
    /// timestamp and never leaves the process.
    #[doc(hidden)]
    #[must_use]
    pub const fn recency(&self) -> super::SessionRecency {
        self.recency
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

    /// Records that a client attached to the session at the current time.
    pub fn touch_attached(&mut self) {
        let now = current_unix_timestamp();
        self.activity_at = now;
        self.last_attached_at = Some(now);
        self.recency = super::SessionRecency::next();
    }

    /// Pins the public whole-second timestamps without disturbing the internal
    /// recency order.
    ///
    /// Session times come from the wall clock, so no test can otherwise
    /// guarantee that two sessions land in the same second — which is exactly
    /// the condition targetless ranking has to survive. Regressions for it
    /// would have to race the clock or retry; this makes them deterministic
    /// instead.
    ///
    /// It is a test seam, never a runtime operation. `cfg(test)` alone cannot
    /// reach the server crate's tests, so the `test-seams` feature carries it
    /// there and nowhere else: it is off by default and only the dev-dependency
    /// edge turns it on, so the published API never gains a way to rewrite a
    /// session's public timestamps.
    #[cfg(any(test, feature = "test-seams"))]
    #[doc(hidden)]
    pub fn pin_public_times_for_tests(&mut self, seconds: i64) {
        self.created_at = seconds;
        self.activity_at = seconds;
        self.last_attached_at = self.last_attached_at.map(|_| seconds);
    }

    /// Records that an attached client interacted with the session, leaving
    /// attach history untouched.
    ///
    /// Callers must invoke this only once per accepted interaction, at the
    /// boundary where an attached client's input is admitted — not once per
    /// pane write, and not for input the server rejects.
    ///
    /// This advances the *public* `activity_at`, not only the internal recency
    /// token: `#{session_activity}` and the `list-sessions` payload report the
    /// new second. tmux 3.7b does the same — it calls `session_update_activity`
    /// while admitting a key, before deciding what the key means — so a client
    /// typing into a session is expected to change that output.
    pub fn touch_activity(&mut self) {
        self.activity_at = current_unix_timestamp();
        self.recency = super::SessionRecency::next();
    }

    /// Moves the session to its own position at the head of the recency order,
    /// leaving every public timestamp untouched.
    ///
    /// `Session` is publicly cloneable, so a session handed back to a store can
    /// carry a token a live session already holds. The order the readers rank by
    /// has to stay total, so the store re-mints on collision rather than letting
    /// two sessions share a position.
    pub(super) fn renew_recency(&mut self) {
        self.recency = super::SessionRecency::next();
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
        cloned.group_initial_window_id = self
            .windows
            .first_key_value()
            .map(|(_, window)| window.id());
        cloned.created_at = now;
        cloned.activity_at = now;
        cloned.last_attached_at = None;
        cloned.recency = super::SessionRecency::next();
        cloned
    }

    /// Synchronizes shared grouped-session window state from the source session while preserving the local current window when possible.
    pub fn synchronize_group_from(&mut self, source: &Session) {
        self.synchronize_group_from_with_optional_winlink_alert_map(source, None);
    }

    /// Synchronizes grouped windows by identity while applying an authoritative
    /// permutation to peer-local winlink alert flags.
    pub fn synchronize_group_from_with_winlink_alert_map(
        &mut self,
        source: &Session,
        winlink_alert_map: &BTreeMap<u32, u32>,
    ) {
        self.synchronize_group_from_with_optional_winlink_alert_map(
            source,
            Some(winlink_alert_map),
        );
    }

    fn synchronize_group_from_with_optional_winlink_alert_map(
        &mut self,
        source: &Session,
        winlink_alert_map: Option<&BTreeMap<u32, u32>>,
    ) {
        debug_assert_ne!(self.name, source.name);
        let previous_active = self.active_window;
        let previous_last = self.last_window;
        let previous_active_window_id = self.window_at(previous_active).map(Window::id);

        let synchronized_active = previous_active_window_id
            .and_then(|window_id| window_index_for_id(&source.windows, window_id, previous_active))
            .unwrap_or_else(|| {
                synchronized_active_window(&source.windows, previous_active, previous_last)
            });
        self.synchronize_group_windows(source, synchronized_active, winlink_alert_map);
    }

    /// Synchronizes grouped window state while remapping selected source slots explicitly.
    pub fn synchronize_group_from_with_window_selection_map(
        &mut self,
        source: &Session,
        index_map: &BTreeMap<u32, u32>,
    ) {
        self.synchronize_group_from_with_window_selection_and_winlink_alert_maps(
            source, index_map, index_map,
        );
    }

    /// Synchronizes grouped windows with independent maps for peer selection
    /// and peer-local winlink alert flags.
    pub fn synchronize_group_from_with_window_selection_and_winlink_alert_maps(
        &mut self,
        source: &Session,
        window_selection_map: &BTreeMap<u32, u32>,
        winlink_alert_map: &BTreeMap<u32, u32>,
    ) {
        debug_assert_ne!(self.name, source.name);
        let previous_active = self.active_window;
        let previous_last = self.last_window;
        let synchronized_active = window_selection_map
            .get(&previous_active)
            .copied()
            .unwrap_or(previous_active);
        let synchronized_active = if source.windows.contains_key(&synchronized_active) {
            synchronized_active
        } else {
            synchronized_active_window(&source.windows, previous_active, previous_last)
        };
        self.synchronize_group_windows(source, synchronized_active, Some(winlink_alert_map));
    }

    fn synchronize_group_windows(
        &mut self,
        source: &Session,
        active_window: u32,
        winlink_alert_map: Option<&BTreeMap<u32, u32>>,
    ) {
        let synchronized_alert_flags = remap_local_winlink_alert_flags(
            &self.windows,
            &self.winlink_alert_flags,
            &source.windows,
            &source.winlink_alert_flags,
            winlink_alert_map,
        );
        self.windows = source.windows.clone();
        self.winlink_alert_flags = synchronized_alert_flags;
        self.next_pane_id = source.next_pane_id;
        self.cwd = source.cwd.clone();
        self.active_window = active_window;
        // A peer is resynchronised, never navigated: `session.c:663-678` keeps its
        // visits on the slots it already knew, whatever now occupies them, and
        // drops only the ones that went away. The head of what is left is then
        // its return.
        self.window_stack = crate::session::visit_history::carry_occurrences(
            &self.window_stack,
            &self
                .windows
                .keys()
                .map(|&index| (index, index))
                .collect::<BTreeMap<u32, u32>>(),
            active_window,
            |slot| self.windows.contains_key(&slot),
        );
        self.last_window =
            crate::session::visit_history::live_head(&self.window_stack, active_window);
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

    /// The windows this session really visited, most recent first.
    ///
    /// The active window is never in it and a window that was never visited is
    /// never in it, which is what tmux keeps as `s->lastw`.
    #[must_use]
    pub fn window_visit_stack(&self) -> &[u32] {
        &self.window_stack
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
