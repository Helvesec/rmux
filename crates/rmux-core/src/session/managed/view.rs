use rmux_proto::TerminalSize;
use std::path::Path;

use super::ManagedSession;
use crate::{
    AlertFlags, Pane, PaneId, SessionId, SessionName, SessionRecency, Window, WindowId, WindowLink,
    WindowStateStore,
};

/// A read-only borrow of session metadata and its canonical window owners.
/// Copying a view copies references only; it never copies session/window state.
#[derive(Debug, Clone, Copy)]
pub struct ManagedSessionView<'a> {
    pub(super) viewport_offset: Option<(u16, u16)>,
    pub(super) metadata: &'a ManagedSession,
    pub(super) windows: &'a WindowStateStore,
    pub(super) overrides: Option<&'a std::collections::BTreeMap<u32, Window>>,
}

impl<'a> ManagedSessionView<'a> {
    /// Client presentation offset; canonical views have no viewport override.
    pub fn viewport_offset(&self) -> Option<(u16, u16)> {
        self.viewport_offset
    }

    /// Stable session identity.
    pub fn id(&self) -> SessionId {
        self.metadata.id
    }
    /// Validated session name.
    pub fn name(&self) -> &'a SessionName {
        &self.metadata.name
    }
    /// Stable group name, independent of session renames.
    pub fn group_name(&self) -> Option<&'a SessionName> {
        self.metadata.group_name.as_ref()
    }
    /// Outer terminal geometry, local to this session.
    pub fn terminal_size(&self) -> TerminalSize {
        self.metadata.terminal_size
    }
    /// Session creation time in Unix seconds.
    pub fn created_at(&self) -> i64 {
        self.metadata.created_at
    }
    /// Last accepted activity time in Unix seconds.
    pub fn activity_at(&self) -> i64 {
        self.metadata.activity_at
    }
    /// Last attachment time, if attached previously.
    pub fn last_attached_at(&self) -> Option<i64> {
        self.metadata.last_attached_at
    }
    /// Process-local recency order.
    pub fn recency(&self) -> SessionRecency {
        self.metadata.recency
    }
    /// Session working directory.
    pub fn cwd(&self) -> Option<&'a Path> {
        self.metadata.cwd.as_deref()
    }
    /// Session-local active index.
    pub fn active_window_index(&self) -> u32 {
        self.metadata.active_window
    }
    /// Session-local previous index.
    pub fn last_window_index(&self) -> Option<u32> {
        self.metadata.last_window
    }

    /// The windows this session really visited, most recent first.
    #[must_use]
    pub fn window_visit_stack(&self) -> &'a [u32] {
        &self.metadata.window_stack
    }
    /// The active canonical window.
    pub fn window(&self) -> &'a Window {
        self.window_at(self.active_window_index())
            .expect("managed session has an active link")
    }
    /// The addressed canonical window, if linked into this session.
    pub fn window_at(&self, index: u32) -> Option<&'a Window> {
        if let Some(window) = self.overrides.and_then(|windows| windows.get(&index)) {
            return Some(window);
        }
        self.windows
            .resolve(WindowLink {
                session_id: self.id(),
                window_index: index,
            })
            .ok()
    }
    /// Borrowed canonical windows in local display order, including aliases.
    pub fn windows(&self) -> impl DoubleEndedIterator<Item = (u32, &'a Window)> + 'a {
        let overrides = self.overrides;
        self.windows
            .session_windows(self.id())
            .map(move |(index, window)| {
                (
                    index,
                    overrides
                        .and_then(|windows| windows.get(&index))
                        .unwrap_or(window),
                )
            })
    }
    /// First free local slot, without materializing canonical windows.
    pub(crate) fn lowest_available_window_index_at_or_above(
        &self,
        minimum: u32,
    ) -> Result<u32, rmux_proto::RmuxError> {
        let mut next = minimum;
        for (index, _) in self.windows() {
            if index < next {
                continue;
            }
            if index > next {
                break;
            }
            next = next.checked_add(1).ok_or_else(|| {
                rmux_proto::RmuxError::Server(format!(
                    "window index space exhausted for session {}",
                    self.name()
                ))
            })?;
        }
        Ok(next)
    }

    /// Unzoomed detached layout projection for format readers. The canonical
    /// owner and every alias remain unchanged.
    pub fn layout_window_snapshot(&self, index: u32) -> Option<Window> {
        let mut window = self.window_at(index)?.clone();
        if window.is_zoomed() {
            let active = window.active_pane_index();
            crate::WindowLayoutContext::new(self.name().clone(), index, &mut window)
                .toggle_zoom(active)
                .ok()?;
        }
        Some(window)
    }

    /// The stable identity behind a local occurrence.
    pub fn window_id(&self, index: u32) -> Option<WindowId> {
        self.window_at(index).map(Window::id)
    }
    /// Active pane in the active canonical window.
    pub fn active_pane(&self) -> Option<&'a Pane> {
        self.window().active_pane()
    }
    /// Active pane index in the selected canonical window.
    pub fn active_pane_index(&self) -> u32 {
        self.window().active_pane_index()
    }
    /// Stable active pane identity.
    pub fn active_pane_id(&self) -> Option<PaneId> {
        self.active_pane().map(Pane::id)
    }
    /// Stable pane identity at an exact occurrence.
    pub fn pane_id_in_window(&self, window_index: u32, pane_index: u32) -> Option<PaneId> {
        self.window_at(window_index)?.pane(pane_index).map(Pane::id)
    }
    /// First local occurrence containing a stable pane identity.
    pub fn window_index_for_pane_id(&self, pane_id: PaneId) -> Option<u32> {
        self.windows().find_map(|(index, window)| {
            window
                .panes()
                .iter()
                .any(|pane| pane.id() == pane_id)
                .then_some(index)
        })
    }
    /// Alerts belonging to one session-local occurrence.
    pub fn winlink_alert_flags(&self, index: u32) -> AlertFlags {
        self.metadata
            .winlink_alert_flags
            .get(&index)
            .copied()
            .unwrap_or_else(AlertFlags::empty)
    }
    /// Alerted occurrence indexes in display order.
    pub fn alerted_window_indexes(&self) -> Vec<u32> {
        self.metadata
            .winlink_alert_flags
            .iter()
            .filter_map(|(index, flags)| {
                flags
                    .intersects(crate::WINLINK_ALERTFLAGS)
                    .then_some(*index)
            })
            .collect()
    }

    /// Union of this session's local occurrence alerts.
    pub fn session_alert_flags(&self) -> AlertFlags {
        self.metadata
            .winlink_alert_flags
            .values()
            .copied()
            .fold(AlertFlags::empty(), AlertFlags::union)
    }
}
