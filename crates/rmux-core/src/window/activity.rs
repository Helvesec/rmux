use super::{current_unix_timestamp, Window};
use crate::PaneId;

/// Exact activity state for one pane and its containing shared window.
///
/// The fields stay private so callers can only replay a snapshot captured from
/// another occurrence of the same linked window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowPaneActivity {
    pane_id: PaneId,
    window_activity_at: i64,
    pane_activity_at: i64,
}

impl Window {
    /// Returns the window creation timestamp as Unix seconds.
    #[must_use]
    pub const fn created_at(&self) -> i64 {
        self.created_at
    }

    /// Returns the last window activity timestamp as Unix seconds.
    #[must_use]
    pub const fn activity_at(&self) -> i64 {
        self.activity_at
    }

    /// Records output activity for a specific pane in this window.
    pub fn touch_activity_for_pane(&mut self, pane_index: u32) -> bool {
        let Some(position) = self
            .panes
            .iter()
            .position(|pane| pane.index() == pane_index)
        else {
            return false;
        };
        let now = current_unix_timestamp();
        self.activity_at = now;
        self.panes[position].set_activity_at(now);
        true
    }

    /// Captures the window and pane activity timestamps for a stable pane.
    #[must_use]
    pub fn pane_activity_snapshot(&self, pane_id: PaneId) -> Option<WindowPaneActivity> {
        let pane = self.panes.iter().find(|pane| pane.id() == pane_id)?;
        Some(WindowPaneActivity {
            pane_id,
            window_activity_at: self.activity_at,
            pane_activity_at: pane.activity_at(),
        })
    }

    /// Applies activity captured from another occurrence of the same linked window.
    pub fn apply_pane_activity_snapshot(&mut self, activity: WindowPaneActivity) -> bool {
        let Some(pane) = self
            .panes
            .iter_mut()
            .find(|pane| pane.id() == activity.pane_id)
        else {
            return false;
        };
        self.activity_at = activity.window_activity_at;
        pane.set_activity_at(activity.pane_activity_at);
        true
    }
}
