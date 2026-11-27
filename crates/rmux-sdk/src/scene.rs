//! Owned scene snapshots and coalesced observation differences.
//! A revision describes content, not time. Polling can miss intermediate states;
//! after lag/reconnect call `SceneState::resynchronize` with a fresh guarded snapshot.
//! Pane grids are captured separately and are not an atomic transcript snapshot.
use crate::{PaneId, SessionId, WindowId};
use serde::{Deserialize, Serialize};

/// Signed canonical rectangle in content coordinates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SceneRect {
    /// Left column.
    pub x: i32,
    /// Top row.
    pub y: i32,
    /// Width.
    pub cols: i32,
    /// Height.
    pub rows: i32,
}

/// Already clipped and occluded raw pane-grid row span in terminal coordinates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SceneSpan {
    /// Terminal column.
    pub x: u16,
    /// Terminal row including status offset.
    pub y: u16,
    /// Row in the raw pane snapshot.
    pub source_row: usize,
    /// Column in the raw pane snapshot.
    pub source_col: usize,
    /// Number of visible columns.
    pub cols: u16,
}

/// Canonical hit region; frontmost visible region wins.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SceneHitRegion {
    /// Content, frame, or pane-border-status; unknown kinds are ignored.
    pub kind: String,
    /// Signed source rectangle.
    pub rect: SceneRect,
    /// Viewport-clipped rectangle before front-layer occlusion.
    pub clip: SceneRect,
}

/// One stable pane in rear-to-front scene order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScenePane {
    /// Stable pane identity.
    pub id: PaneId,
    /// Presentation slot; never an identity.
    pub index: u32,
    /// Whether this pane is floating.
    pub floating: bool,
    /// Canonical z ordering key.
    pub layer: u32,
    /// Canonical focus.
    pub focused: bool,
    /// Canonical zoom visibility.
    pub visible: bool,
    /// Signed pane application rectangle.
    pub content: SceneRect,
    /// Signed outer frame.
    pub frame: SceneRect,
    /// Clipped content rectangle.
    pub content_clip: SceneRect,
    /// Clipped outer frame.
    pub frame_clip: SceneRect,
    /// Canonical hit regions.
    pub hit_regions: Vec<SceneHitRegion>,
    /// Visible raw grid spans; excludes front panes and frames, without scrollbar decoration.
    pub spans: Vec<SceneSpan>,
}

/// One already occlusion-tested floating frame cell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SceneBorder {
    /// Terminal column.
    pub x: u16,
    /// Terminal row.
    pub y: u16,
    /// Canonical frame glyph.
    pub glyph: char,
    /// Stable owner.
    pub pane_id: PaneId,
    /// Whether the frame owns focus.
    pub active: bool,
}

/// Owned read-only projection of one resolved window, with no internal layout tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SceneSnapshot {
    /// Projection schema; currently 1.
    pub schema: u16,
    /// Stable selected session identity.
    pub session_id: SessionId,
    /// Stable selected window identity.
    pub window_id: WindowId,
    /// Session-local selected slot.
    pub window_index: u32,
    /// SHA256 content fingerprint, not a monotonic event sequence or mutation token.
    pub revision: String,
    /// Terminal viewport columns.
    pub cols: u16,
    /// Terminal viewport rows.
    pub rows: u16,
    /// Content origin below a top status bar.
    pub content_y_offset: u16,
    /// Canonical active pane identity.
    pub active_pane_id: Option<PaneId>,
    /// Panes in canonical rear-to-front order.
    pub panes: Vec<ScenePane>,
    /// Canonical visible floating frame cells.
    pub borders: Vec<SceneBorder>,
}

impl SceneSnapshot {
    pub(crate) fn decode(json: &[u8]) -> crate::Result<Self> {
        let snapshot: Self = serde_json::from_slice(json).map_err(|e| {
            crate::RmuxError::from_proto(rmux_proto::RmuxError::Decode(e.to_string()))
        })?;
        if snapshot.schema != 1 {
            return Err(crate::RmuxError::unsupported(
                "scene.window.v1",
                "unsupported scene schema",
            ));
        }
        Ok(snapshot)
    }
}

/// Differences between observed snapshots, never an assertion about unseen mutations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SceneUpdate {
    /// Full baseline replacement, including first observation or explicit recovery.
    pub reset: bool,
    /// Stable pane IDs newly observed.
    pub created: Vec<PaneId>,
    /// Stable pane IDs whose resolved representation changed.
    pub changed: Vec<PaneId>,
    /// Stable pane IDs no longer present.
    pub removed: Vec<PaneId>,
    /// Selected pane, session, or window changed since the last observation.
    pub selection_changed: bool,
}

/// Pure consumer-owned state; does not own or mutate daemon placement.
#[derive(Debug, Clone, Default)]
pub struct SceneState {
    current: Option<SceneSnapshot>,
}
impl SceneState {
    /// Last complete observed snapshot.
    pub fn snapshot(&self) -> Option<&SceneSnapshot> {
        self.current.as_ref()
    }
    /// Replaces the baseline after a gap; no intermediate events are fabricated.
    pub fn resynchronize(&mut self, snapshot: SceneSnapshot) -> SceneUpdate {
        self.current = None;
        self.apply(snapshot)
    }
    /// Folds one full observation. Different session/window identities reset the baseline.
    pub fn apply(&mut self, snapshot: SceneSnapshot) -> SceneUpdate {
        let old = self.current.as_ref().filter(|old| {
            (old.session_id, old.window_id) == (snapshot.session_id, snapshot.window_id)
        });
        let mut update = SceneUpdate {
            reset: old.is_none(),
            created: Vec::new(),
            changed: Vec::new(),
            removed: Vec::new(),
            selection_changed: old.is_none_or(|old| {
                old.active_pane_id != snapshot.active_pane_id
                    || old.window_index != snapshot.window_index
            }),
        };
        for pane in &snapshot.panes {
            match old.and_then(|old| old.panes.iter().find(|p| p.id == pane.id)) {
                None => update.created.push(pane.id),
                Some(previous) if previous != pane => update.changed.push(pane.id),
                _ => {}
            }
        }
        if let Some(old) = old {
            update.removed = old
                .panes
                .iter()
                .filter(|p| !snapshot.panes.iter().any(|n| n.id == p.id))
                .map(|p| p.id)
                .collect();
        }
        self.current = Some(snapshot);
        update
    }
}
