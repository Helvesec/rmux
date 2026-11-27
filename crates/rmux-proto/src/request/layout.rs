use std::cell::Cell;

use serde::{de, Deserialize, Deserializer, Serialize};

use crate::{LayoutName, PaneId, SessionId, SessionName, TerminalSize, WindowId, WindowTarget};

/// Current version of the explicit RMUX window snapshot payload.
pub const RMUX_WINDOW_SNAPSHOT_VERSION: u16 = 1;

/// Target forms accepted by `select-layout`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SelectLayoutTarget {
    /// Applies to the addressed session's single V1 window.
    Session(SessionName),
    /// Applies to the addressed V1 window target.
    Window(WindowTarget),
}

/// Request payload for `select-layout`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectLayoutRequest {
    /// The exact layout target.
    pub target: SelectLayoutTarget,
    /// The requested layout.
    pub layout: LayoutName,
}

/// Request payload for `select-layout` with a custom layout string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectCustomLayoutRequest {
    /// The exact layout target.
    pub target: SelectLayoutTarget,
    /// The tmux custom layout string, including checksum.
    pub layout: String,
}

/// Request payload for `select-layout -o`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectOldLayoutRequest {
    /// The exact layout target.
    pub target: SelectLayoutTarget,
}

/// Request payload for `select-layout -E`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpreadLayoutRequest {
    /// The exact layout target.
    pub target: SelectLayoutTarget,
}

/// Request payload for `next-layout`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NextLayoutRequest {
    /// The exact window target.
    pub target: WindowTarget,
}

/// Request payload for `previous-layout`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviousLayoutRequest {
    /// The exact window target.
    pub target: WindowTarget,
}

/// Split axis used by the typed RMUX window snapshot tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RmuxWindowSnapshotSplitDirection {
    /// Children are arranged from left to right.
    LeftRight,
    /// Children are arranged from top to bottom.
    TopBottom,
}

/// Deepest chain of nested splits any decoder accepts in a snapshot tree,
/// matching the layout-string parser bound in `rmux-core`.
const MAX_SNAPSHOT_SPLIT_DEPTH: usize = 128;

/// One node in the typed tiled tree captured by a RMUX window snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RmuxWindowSnapshotCell {
    /// A leaf occupied by one stable pane identity.
    Pane {
        /// Stable pane identity occupying this leaf.
        pane_id: PaneId,
        /// Leaf width in terminal cells.
        cols: u16,
        /// Leaf height in terminal cells.
        rows: u16,
        /// Leaf left offset within the window content area.
        x: u16,
        /// Leaf top offset within the window content area.
        y: u16,
    },
    /// An internal split with ordered child cells.
    Split {
        /// Child arrangement direction.
        direction: RmuxWindowSnapshotSplitDirection,
        /// Split width in terminal cells.
        cols: u16,
        /// Split height in terminal cells.
        rows: u16,
        /// Split left offset within the window content area.
        x: u16,
        /// Split top offset within the window content area.
        y: u16,
        /// Ordered child cells in visual traversal order.
        #[serde(deserialize_with = "deserialize_bounded_children")]
        children: Vec<RmuxWindowSnapshotCell>,
    },
}

thread_local! {
    /// Splits currently being decoded on this thread. Decoding is synchronous,
    /// so the count always describes the ancestors of the cell being decoded.
    static SNAPSHOT_SPLIT_DEPTH: Cell<usize> = const { Cell::new(0) };
}

/// Holds one level of [`SNAPSHOT_SPLIT_DEPTH`] for the duration of a child list.
struct SnapshotSplitDepthGuard;

impl SnapshotSplitDepthGuard {
    fn enter<E: de::Error>() -> Result<Self, E> {
        SNAPSHOT_SPLIT_DEPTH.with(|depth| {
            let nested = depth.get() + 1;
            if nested > MAX_SNAPSHOT_SPLIT_DEPTH {
                return Err(E::custom(format_args!(
                    "window snapshot tiled tree is nested deeper than {MAX_SNAPSHOT_SPLIT_DEPTH} splits"
                )));
            }
            depth.set(nested);
            Ok(Self)
        })
    }
}

impl Drop for SnapshotSplitDepthGuard {
    fn drop(&mut self) {
        SNAPSHOT_SPLIT_DEPTH.with(|depth| depth.set(depth.get() - 1));
    }
}

/// Bounds the recursion of every decoder, bincode included: bincode 1 has no
/// recursion limit, and a frame far below its size cap can nest deep enough
/// to overflow the decoding thread's stack.
fn deserialize_bounded_children<'de, D>(
    deserializer: D,
) -> Result<Vec<RmuxWindowSnapshotCell>, D::Error>
where
    D: Deserializer<'de>,
{
    let _depth = SnapshotSplitDepthGuard::enter::<D::Error>()?;
    Vec::deserialize(deserializer)
}

/// One floating pane placement in rear-to-front stack order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RmuxFloatingPaneSnapshot {
    /// Stable pane identity for the floating placement.
    pub pane_id: PaneId,
    /// Signed left coordinate in the window content area.
    pub x: i32,
    /// Signed top coordinate in the window content area.
    pub y: i32,
    /// Signed width in terminal cells; must be positive at restore admission.
    pub cols: i32,
    /// Signed height in terminal cells; must be positive at restore admission.
    pub rows: i32,
    /// Whether the floating pane is visible.
    pub visible: bool,
}

/// Explicit RMUX snapshot for one live canonical window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RmuxPaneRuntimeIdentity {
    /// Pane whose runtime was captured.
    pub pane_id: PaneId,
    /// Random birth identity, renewed on respawn and across daemon incarnations.
    pub incarnation: [u8; 32],
}

/// Layout behavior retained alongside the exact geometry, excluding legacy old-layout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RmuxWindowLayoutMetadata {
    /// Current named layout family.
    pub layout: LayoutName,
    /// Cursor used by next-layout and previous-layout.
    pub last_layout: Option<LayoutName>,
    /// Whether absolute resizing preserves a custom split tree.
    pub custom_layout: bool,
    /// Explicit main-pane width, when set.
    pub requested_main_width: Option<u16>,
    /// Explicit main-pane height, when set.
    pub requested_main_height: Option<u16>,
}

/// Explicit RMUX snapshot for one live canonical window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RmuxWindowSnapshot {
    /// Snapshot schema version.
    pub version: u16,
    /// Stable session identity of the exported target occurrence.
    pub session_id: SessionId,
    /// Session/window occurrence used at export time.
    pub target: WindowTarget,
    /// Stable canonical window identity.
    pub window_id: WindowId,
    /// Window content size used by every layout coordinate.
    pub size: TerminalSize,
    /// Whether the source window was zoomed; FLS1 snapshots must restore only when false.
    pub zoomed: bool,
    /// Typed tiled layout tree, absent only when every live pane is floating.
    pub tiled: Option<RmuxWindowSnapshotCell>,
    /// Floating placements in rear-to-front order.
    pub floating: Vec<RmuxFloatingPaneSnapshot>,
    /// Stable pane identity selected when the snapshot was exported.
    pub active_pane: PaneId,
    /// Stable pane identity selected immediately before `active_pane`, when present.
    pub last_pane: Option<PaneId>,
    /// Runtime birth identities, validated by the owning daemon before mutation.
    pub runtimes: Vec<RmuxPaneRuntimeIdentity>,
    /// Metadata needed to preserve subsequent layout behavior.
    pub metadata: RmuxWindowLayoutMetadata,
}

/// Request payload for `export-window-snapshot`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportWindowSnapshotRequest {
    /// Exact window target to snapshot.
    pub target: WindowTarget,
}

/// Request payload for `restore-window-snapshot`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreWindowSnapshotRequest {
    /// Exact window target that must still match the snapshot guard.
    pub target: WindowTarget,
    /// Typed RMUX window snapshot to restore.
    pub snapshot: RmuxWindowSnapshot,
}

/// Decodes an explicit RMUX window snapshot JSON payload.
///
/// This is the only JSON syntax accepted by `restore-window-snapshot`; tmux
/// `#{window_layout}` strings are intentionally a separate legacy format.
pub fn decode_rmux_window_snapshot_json(
    input: &str,
) -> Result<RmuxWindowSnapshot, serde_json::Error> {
    serde_json::from_str(input)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot_with_float(x: i32, y: i32) -> RmuxWindowSnapshot {
        let session_name = SessionName::new("alpha").expect("valid session name");
        RmuxWindowSnapshot {
            version: RMUX_WINDOW_SNAPSHOT_VERSION,
            session_id: SessionId::new(1),
            target: WindowTarget::with_window(session_name, 0),
            window_id: WindowId::new(1),
            size: TerminalSize { cols: 80, rows: 24 },
            zoomed: false,
            tiled: Some(RmuxWindowSnapshotCell::Pane {
                pane_id: PaneId::new(1),
                cols: 80,
                rows: 24,
                x: 0,
                y: 0,
            }),
            floating: vec![RmuxFloatingPaneSnapshot {
                pane_id: PaneId::new(2),
                x,
                y,
                cols: 30,
                rows: 10,
                visible: true,
            }],
            active_pane: PaneId::new(1),
            last_pane: Some(PaneId::new(2)),
            runtimes: vec![],
            metadata: RmuxWindowLayoutMetadata {
                layout: LayoutName::EvenHorizontal,
                last_layout: Some(LayoutName::EvenHorizontal),
                custom_layout: false,
                requested_main_width: None,
                requested_main_height: None,
            },
        }
    }

    #[test]
    fn distinct_signed_float_rectangles_encode_distinct_json() {
        let first = serde_json::to_string(&snapshot_with_float(-2, 4)).expect("snapshot encodes");
        let second = serde_json::to_string(&snapshot_with_float(2, -4)).expect("snapshot encodes");

        assert_ne!(first, second);
        assert!(first.contains("\"x\":-2"));
        assert!(second.contains("\"y\":-4"));
    }

    /// Stack given to every depth test, far below the 8 MiB daemon workers.
    const SMALL_STACK_BYTES: usize = 512 * 1024;

    /// Forges a valid detached `RestoreWindowSnapshot` frame whose tiled tree
    /// nests `splits` single-child splits above one pane leaf. The tree is
    /// spliced as bytes: building and dropping it in memory would recurse as
    /// deeply as decoding it.
    fn nested_restore_frame(splits: usize) -> Vec<u8> {
        let leaf = RmuxWindowSnapshotCell::Pane {
            pane_id: PaneId::new(0x5a5a_5a5a),
            cols: 0x5a5b,
            rows: 0x5a5c,
            x: 0x5a5d,
            y: 0x5a5e,
        };
        let mut snapshot = snapshot_with_float(0, 0);
        snapshot.tiled = Some(leaf.clone());
        let request =
            crate::Request::RestoreWindowSnapshot(Box::new(RestoreWindowSnapshotRequest {
                target: snapshot.target.clone(),
                snapshot,
            }));
        let payload = bincode::serialize(&request).expect("request encodes");
        let leaf_bytes = bincode::serialize(&leaf).expect("leaf encodes");
        let leaf_at = payload
            .windows(leaf_bytes.len())
            .position(|window| window == leaf_bytes)
            .expect("leaf is encoded inline");

        let mut split_prefix = bincode::serialize(&RmuxWindowSnapshotCell::Split {
            direction: RmuxWindowSnapshotSplitDirection::LeftRight,
            cols: 80,
            rows: 24,
            x: 0,
            y: 0,
            children: Vec::new(),
        })
        .expect("split encodes");
        let empty_len = split_prefix.split_off(split_prefix.len() - 8);
        assert_eq!(empty_len, 0_u64.to_le_bytes(), "children length is a u64");
        split_prefix.extend_from_slice(&1_u64.to_le_bytes());

        let mut forged = payload[..leaf_at].to_vec();
        for _ in 0..splits {
            forged.extend_from_slice(&split_prefix);
        }
        forged.extend_from_slice(&payload[leaf_at..]);

        let mut frame = vec![crate::envelope::RMUX_FRAME_MAGIC];
        crate::envelope::encode_varint_u32(crate::envelope::RMUX_WIRE_VERSION, &mut frame);
        let length = u32::try_from(forged.len()).expect("forged payload fits a frame");
        frame.extend_from_slice(&length.to_le_bytes());
        frame.extend_from_slice(&forged);
        frame
    }

    /// Decodes `frame` as the daemon does, on a small stack, and returns the
    /// number of splits above the decoded leaf.
    fn decode_split_depth_on_small_stack(frame: Vec<u8>) -> Result<usize, crate::RmuxError> {
        std::thread::Builder::new()
            .stack_size(SMALL_STACK_BYTES)
            .spawn(move || {
                let mut decoder = crate::FrameDecoder::new();
                decoder.push_bytes(&frame);
                let request = decoder
                    .next_frame::<crate::Request>()?
                    .expect("frame is complete");
                let crate::Request::RestoreWindowSnapshot(request) = request else {
                    panic!("forged frame decodes as restore-window-snapshot");
                };
                let mut cell = request.snapshot.tiled.as_ref().expect("tiled tree");
                let mut splits = 0;
                while let RmuxWindowSnapshotCell::Split { children, .. } = cell {
                    splits += 1;
                    cell = &children[0];
                }
                Ok(splits)
            })
            .expect("decoder thread starts")
            .join()
            .expect("decoder thread does not panic")
    }

    #[test]
    fn deeply_nested_snapshot_frame_is_rejected_without_overflowing_the_stack() {
        let frame = nested_restore_frame(100_000);
        assert!(frame.len() < crate::DEFAULT_MAX_DETACHED_FRAME_LENGTH);

        let error = decode_split_depth_on_small_stack(frame).expect_err("deep tree is rejected");

        let crate::RmuxError::Decode(message) = &error else {
            panic!("expected a decode error, got {error:?}");
        };
        assert!(message.contains("nested"), "{message}");
    }

    #[test]
    fn snapshot_split_depth_limit_is_inclusive() {
        let deepest =
            decode_split_depth_on_small_stack(nested_restore_frame(MAX_SNAPSHOT_SPLIT_DEPTH));
        assert_eq!(deepest, Ok(MAX_SNAPSHOT_SPLIT_DEPTH));

        let error =
            decode_split_depth_on_small_stack(nested_restore_frame(MAX_SNAPSHOT_SPLIT_DEPTH + 1))
                .expect_err("one split past the limit is rejected");
        assert!(matches!(error, crate::RmuxError::Decode(_)), "{error:?}");
    }
}

/// Explicit canonical snapshot version, including attachment-dependent resize behavior.
pub const RMUX_WINDOW_SNAPSHOT_V2_VERSION: u16 = 2;

/// Geometry-free root attachment metadata. Tiled structure and rectangles remain
/// in `tiled`; float rectangles and z order remain in `floating`. Attachment
/// order is independent of z order and pane display slots.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct RmuxWindowResizeTopology {
    /// Root split axis while floating siblings are attached. None without floats.
    pub root_direction: Option<RmuxWindowSnapshotSplitDirection>,
    /// Stable floating IDs in attachment order, oldest first.
    pub floating_attachment_order: Vec<PaneId>,
}

/// V2 canonical content may exceed `size`, which is the fixed viewport.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RmuxWindowSnapshotV2 {
    /// Snapshot schema version.
    pub version: u16,
    /// Stable session identity of the exported target occurrence.
    pub session_id: SessionId,
    /// Session/window occurrence used at export time.
    pub target: WindowTarget,
    /// Stable canonical window identity.
    pub window_id: WindowId,
    /// Window content size used by every layout coordinate.
    pub size: TerminalSize,
    /// Whether the source window was zoomed; FLS1 snapshots must restore only when false.
    pub zoomed: bool,
    /// Typed tiled layout tree, absent only when every live pane is floating.
    pub tiled: Option<RmuxWindowSnapshotCell>,
    /// Floating placements in rear-to-front order.
    pub floating: Vec<RmuxFloatingPaneSnapshot>,
    /// Stable pane identity selected when the snapshot was exported.
    pub active_pane: PaneId,
    /// Stable pane identity selected immediately before `active_pane`, when present.
    pub last_pane: Option<PaneId>,
    /// Runtime birth identities, validated by the owning daemon before mutation.
    pub runtimes: Vec<RmuxPaneRuntimeIdentity>,
    /// Metadata needed to preserve subsequent layout behavior.
    pub metadata: RmuxWindowLayoutMetadata,
    /// Root attachment semantics, distinct from visual stacking.
    pub resize_topology: RmuxWindowResizeTopology,
}

/// Opt-in export of a canonical V2 snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportWindowSnapshotV2Request {
    /// Target canonical window occurrence.
    pub target: WindowTarget,
}
/// Transactional restore of a canonical V2 snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreWindowSnapshotV2Request {
    /// Target canonical window occurrence.
    pub target: WindowTarget,
    /// Explicit V2 state including live identity guards.
    pub snapshot: RmuxWindowSnapshotV2,
}
/// Decode explicit V2 JSON. Version validation precedes product mutation.
pub fn decode_rmux_window_snapshot_v2_json(
    json: &str,
) -> Result<RmuxWindowSnapshotV2, serde_json::Error> {
    serde_json::from_str(json)
}

/// Parsed absolute-axis sequence. Each axis is admitted immediately before
/// its effect, preserving tmux's valid-x/invalid-y partial-success behavior.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResizePaneAxesRequest {
    /// Exact pane occurrence selected by the command parser.
    pub target: crate::PaneTarget,
    /// Width admitted before the height phase.
    pub columns: Option<u16>,
    /// Height validation result, evaluated only after width succeeds.
    pub rows: Result<Option<u16>, crate::RmuxError>,
}
