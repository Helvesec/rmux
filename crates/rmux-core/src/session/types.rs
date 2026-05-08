use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use rmux_proto::{PaneSplitSize, PaneTarget, SplitDirection};

use crate::{PaneId, WindowId};

#[derive(Clone, Default)]
pub(crate) struct WindowIdAllocator {
    next: Arc<AtomicU32>,
}

impl WindowIdAllocator {
    pub(crate) fn new(next: u32) -> Self {
        Self {
            next: Arc::new(AtomicU32::new(next)),
        }
    }

    pub(crate) fn peek(&self) -> u32 {
        self.next.load(Ordering::Relaxed)
    }

    pub(crate) fn allocate(&self) -> WindowId {
        let next = self
            .next
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                (value != u32::MAX).then_some(value + 1)
            })
            .expect("window id space exhausted");
        assert_ne!(next, u32::MAX, "window id space exhausted");
        WindowId::new(next)
    }

    pub(crate) fn bump_to(&self, next: u32) {
        let _ = self
            .next
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                (current < next).then_some(next)
            });
    }
}

impl std::fmt::Debug for WindowIdAllocator {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WindowIdAllocator")
            .field("next", &self.peek())
            .finish()
    }
}

impl PartialEq for WindowIdAllocator {
    fn eq(&self, other: &Self) -> bool {
        self.peek() == other.peek()
    }
}

impl Eq for WindowIdAllocator {}

/// The result of removing one pane from a session window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KillPaneOutcome {
    removed_pane_ids: Vec<PaneId>,
    window_destroyed: bool,
}

impl KillPaneOutcome {
    pub(super) fn new(removed_pane_ids: Vec<PaneId>, window_destroyed: bool) -> Self {
        Self {
            removed_pane_ids,
            window_destroyed,
        }
    }

    /// Returns the stable pane identities removed by the operation.
    #[must_use]
    pub fn removed_pane_ids(&self) -> &[PaneId] {
        &self.removed_pane_ids
    }

    /// Returns whether removing the pane also destroyed its window.
    #[must_use]
    pub const fn window_destroyed(&self) -> bool {
        self.window_destroyed
    }
}

/// A pane address within a single session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionPaneTarget {
    pub(super) window_index: u32,
    pub(super) pane_index: u32,
}

impl SessionPaneTarget {
    /// Creates a pane address from its containing window and pane indexes.
    #[must_use]
    pub const fn new(window_index: u32, pane_index: u32) -> Self {
        Self {
            window_index,
            pane_index,
        }
    }

    /// Returns the containing window index.
    #[must_use]
    pub const fn window_index(&self) -> u32 {
        self.window_index
    }

    /// Returns the pane index inside the containing window.
    #[must_use]
    pub const fn pane_index(&self) -> u32 {
        self.pane_index
    }
}

impl From<&PaneTarget> for SessionPaneTarget {
    fn from(value: &PaneTarget) -> Self {
        Self::new(value.window_index(), value.pane_index())
    }
}

/// Behavioral flags for pane swaps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneSwapOptions {
    pub(super) detached: bool,
    pub(super) preserve_zoom: bool,
}

impl PaneSwapOptions {
    /// Creates a pane-swap option set.
    #[must_use]
    pub const fn new(detached: bool, preserve_zoom: bool) -> Self {
        Self {
            detached,
            preserve_zoom,
        }
    }
}

/// Behavioral flags for pane joins and moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneJoinOptions {
    pub(super) direction: SplitDirection,
    pub(super) detached: bool,
    pub(super) before: bool,
    pub(super) full_size: bool,
    pub(super) size: Option<PaneSplitSize>,
}

impl PaneJoinOptions {
    /// Creates a pane-join option set.
    #[must_use]
    pub const fn new(
        direction: SplitDirection,
        detached: bool,
        before: bool,
        full_size: bool,
        size: Option<PaneSplitSize>,
    ) -> Self {
        Self {
            direction,
            detached,
            before,
            full_size,
            size,
        }
    }
}

/// Destination and behavioral flags for breaking a pane into its own window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BreakPaneOptions {
    pub(super) target_window_index: Option<u32>,
    pub(super) name: Option<String>,
    pub(super) detached: bool,
    pub(super) after: bool,
    pub(super) before: bool,
}

impl BreakPaneOptions {
    /// Creates a pane-break option set.
    #[must_use]
    pub fn new(
        target_window_index: Option<u32>,
        name: Option<String>,
        detached: bool,
        after: bool,
        before: bool,
    ) -> Self {
        Self {
            target_window_index,
            name,
            detached,
            after,
            before,
        }
    }
}
