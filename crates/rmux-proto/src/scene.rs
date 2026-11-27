//! Additive scene projection carried in the existing list-windows envelope.
use crate::{PaneId, RmuxError, SessionId, WindowId};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Required capability; clients must negotiate before sending a scene query.
pub use crate::capabilities::CAPABILITY_SCENE_V1;
/// Reserved transport format; cannot collide with an OS command argument.
pub const SCENE_QUERY_PREFIX: &str = "\0rmux-window-scene-v1:";

/// Read-only scene request inside ListWindowsRequest.format.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SceneQuery {
    /// Selected session-local slot.
    pub window_index: u32,
    /// Expected stable session and window, checked together under the state lock.
    pub expected_identity: Option<(SessionId, WindowId)>,
}
impl SceneQuery {
    /// Encodes the transport-only format after capability negotiation.
    pub fn encode(&self) -> Result<String, RmuxError> {
        serde_json::to_string(self)
            .map(|s| format!("{SCENE_QUERY_PREFIX}{s}"))
            .map_err(|e| RmuxError::Encode(e.to_string()))
    }
    /// Decodes the suffix of a reserved format query.
    pub fn decode(suffix: &str) -> Result<Self, RmuxError> {
        serde_json::from_str(suffix).map_err(|e| RmuxError::Decode(e.to_string()))
    }
}

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
    /// Seals the exact versioned projection, excluding its fingerprint field.
    pub fn seal(&mut self) -> Result<(), RmuxError> {
        self.revision.clear();
        let bytes = serde_json::to_vec(self).map_err(|e| RmuxError::Encode(e.to_string()))?;
        self.revision = format!("{:x}", Sha256::digest(bytes));
        Ok(())
    }
    /// Serializes this owned DTO without changing the detached binary envelope.
    pub fn encode(&self) -> Result<String, RmuxError> {
        serde_json::to_string(self).map_err(|e| RmuxError::Encode(e.to_string()))
    }
}
