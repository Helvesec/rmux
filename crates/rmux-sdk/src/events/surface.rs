//! Authoritative structured pane surfaces for non-emulator consumers.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;

use rmux_proto::{
    PaneStreamEvent as ProtoEvent, PaneStreamMode, PaneSurfaceFrame as ProtoFrame,
    PaneSurfaceSnapshot as ProtoSnapshot, PaneTargetRef,
};

use super::pane_stream::{
    end_from_proto, lifecycle_from_proto, MappedEvent, PaneStreamEndReason,
    PaneStreamLifecycleEvent, RecoverablePaneStream,
};
use crate::handles::pane::snapshot::{cell_from_wire, cursor_from_wire};
use crate::transport::TransportClient;
use crate::{PaneSnapshot, Result, RmuxError};

/// Complete authoritative structured pane state.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PaneSurfaceSnapshot {
    /// Visible row-major pane grid and cursor.
    pub grid: PaneSnapshot,
    /// Hyperlink IDs aligned one-for-one with `grid.cells`.
    pub cell_hyperlink_ids: Vec<Option<u32>>,
    /// Terminal title.
    pub title: String,
    /// Terminal-reported working-directory path.
    pub path: String,
    /// OSC 8 target URIs keyed by `cell_hyperlink_ids`.
    pub hyperlinks: BTreeMap<u32, String>,
    /// Application-defined OSC 10/11/12 colours.
    pub dynamic_colors: PaneSurfaceDynamicColors,
    /// Whether title, path, and visible-cell hyperlink metadata is complete.
    pub metadata_complete: bool,
    /// Raw terminal mode bitset.
    pub mode_bits: u32,
    /// Whether the alternate screen is active.
    pub alternate: bool,
    /// Inclusive top row of the scrolling region.
    pub scroll_top: u32,
    /// Inclusive bottom row of the scrolling region.
    pub scroll_bottom: u32,
    /// Number of retained scrollback rows.
    pub history_size: u64,
    /// Bytes retained by the daemon's history representation.
    pub history_bytes: u64,
}

impl PaneSurfaceSnapshot {
    /// Returns the hyperlink ID at a visible row and column.
    #[must_use]
    pub fn hyperlink_id_at(&self, row: u16, col: u16) -> Option<u32> {
        if row >= self.grid.rows || col >= self.grid.cols {
            return None;
        }
        let index = usize::from(row)
            .saturating_mul(usize::from(self.grid.cols))
            .saturating_add(usize::from(col));
        self.cell_hyperlink_ids.get(index).copied().flatten()
    }

    /// Returns the OSC 8 URI at a visible row and column.
    #[must_use]
    pub fn hyperlink_uri_at(&self, row: u16, col: u16) -> Option<&str> {
        self.hyperlink_id_at(row, col)
            .and_then(|id| self.hyperlinks.get(&id).map(String::as_str))
    }
}

/// Application-defined OSC 10/11/12 colours.
#[derive(Debug, Default, Clone, PartialEq, Eq, Hash)]
pub struct PaneSurfaceDynamicColors {
    /// OSC 10 default foreground colour.
    pub foreground: Option<String>,
    /// OSC 11 default background colour.
    pub background: Option<String>,
    /// OSC 12 cursor colour.
    pub cursor: Option<String>,
}

/// A self-contained pane surface frame.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PaneSurfaceFrame {
    /// Stream-local epoch established by the latest reset.
    pub epoch: u64,
    /// Monotonic revision of the complete surface projection.
    pub revision: u64,
    /// First raw pane-output sequence not represented by this surface.
    pub next_output_sequence: u64,
    /// Complete state; applying this frame never requires an older patch.
    pub snapshot: PaneSurfaceSnapshot,
}

/// One item from an authoritative surface stream.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PaneSurfaceEvent {
    /// Establishes a new epoch after an invalidation or initial subscribe.
    Reset(PaneSurfaceFrame),
    /// Replaces the current surface within the same epoch.
    Patch(PaneSurfaceFrame),
    /// Non-terminal child lifecycle observation.
    Lifecycle(PaneStreamLifecycleEvent),
    /// Typed logical end of this stream.
    End(PaneStreamEndReason),
}

/// Client-side reducer for [`PaneSurfaceEvent`].
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PaneSurfaceState {
    frame: Option<PaneSurfaceFrame>,
    ended: Option<PaneStreamEndReason>,
}

impl PaneSurfaceState {
    /// Applies one event while enforcing epoch, revision, and shape invariants.
    ///
    /// Returns `Ok(true)` when the authoritative frame changed and `Ok(false)`
    /// for lifecycle/end events.
    pub fn apply(
        &mut self,
        event: &PaneSurfaceEvent,
    ) -> std::result::Result<bool, PaneSurfaceApplyError> {
        if let Some(reason) = self.ended {
            return Err(PaneSurfaceApplyError::AlreadyEnded(reason));
        }
        match event {
            PaneSurfaceEvent::Reset(frame) => {
                validate_frame(frame)?;
                if self
                    .frame
                    .as_ref()
                    .is_some_and(|current| frame.epoch <= current.epoch)
                {
                    return Err(PaneSurfaceApplyError::StaleReset {
                        current_epoch: self.frame.as_ref().map_or(0, |value| value.epoch),
                        received_epoch: frame.epoch,
                    });
                }
                if let Some(current) = self.frame.as_ref() {
                    if frame.revision <= current.revision {
                        return Err(PaneSurfaceApplyError::StaleRevision {
                            current_revision: current.revision,
                            received_revision: frame.revision,
                        });
                    }
                    validate_output_boundary(current, frame)?;
                }
                self.frame = Some(frame.clone());
                Ok(true)
            }
            PaneSurfaceEvent::Patch(frame) => {
                validate_frame(frame)?;
                let current = self
                    .frame
                    .as_ref()
                    .ok_or(PaneSurfaceApplyError::PatchBeforeReset)?;
                if frame.epoch != current.epoch {
                    return Err(PaneSurfaceApplyError::EpochMismatch {
                        current_epoch: current.epoch,
                        received_epoch: frame.epoch,
                    });
                }
                if frame.revision <= current.revision {
                    return Err(PaneSurfaceApplyError::StaleRevision {
                        current_revision: current.revision,
                        received_revision: frame.revision,
                    });
                }
                validate_output_boundary(current, frame)?;
                self.frame = Some(frame.clone());
                Ok(true)
            }
            PaneSurfaceEvent::Lifecycle(_) => Ok(false),
            PaneSurfaceEvent::End(reason) => {
                self.ended = Some(*reason);
                Ok(false)
            }
        }
    }

    /// Returns the latest complete frame.
    #[must_use]
    pub const fn frame(&self) -> Option<&PaneSurfaceFrame> {
        self.frame.as_ref()
    }

    /// Returns the terminal reason after an end event.
    #[must_use]
    pub const fn ended(&self) -> Option<PaneStreamEndReason> {
        self.ended
    }
}

/// A rejected surface-state transition.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum PaneSurfaceApplyError {
    /// A patch arrived before any reset.
    PatchBeforeReset,
    /// A reset did not advance the stream epoch.
    StaleReset {
        /// Current reducer epoch.
        current_epoch: u64,
        /// Rejected reset epoch.
        received_epoch: u64,
    },
    /// A patch addressed a different epoch.
    EpochMismatch {
        /// Current reducer epoch.
        current_epoch: u64,
        /// Rejected patch epoch.
        received_epoch: u64,
    },
    /// A patch did not advance the surface revision.
    StaleRevision {
        /// Current reducer revision.
        current_revision: u64,
        /// Rejected patch revision.
        received_revision: u64,
    },
    /// A frame moved behind output already represented by the surface.
    OutputSequenceRegressed {
        /// First output sequence not represented by the current frame.
        current_sequence: u64,
        /// First output sequence not represented by the rejected frame.
        received_sequence: u64,
    },
    /// The frame's row-major grid shape was invalid.
    InvalidShape(crate::PaneSnapshotShapeError),
    /// Hyperlink IDs were not aligned one-for-one with the grid cells.
    InvalidHyperlinkShape {
        /// Number of IDs required by the grid.
        expected: usize,
        /// Number of IDs carried by the surface.
        actual: usize,
    },
    /// An event arrived after a terminal end.
    AlreadyEnded(PaneStreamEndReason),
}

impl fmt::Display for PaneSurfaceApplyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PatchBeforeReset => formatter.write_str("surface patch arrived before reset"),
            Self::StaleReset {
                current_epoch,
                received_epoch,
            } => write!(
                formatter,
                "surface reset epoch {received_epoch} does not advance current epoch {current_epoch}"
            ),
            Self::EpochMismatch {
                current_epoch,
                received_epoch,
            } => write!(
                formatter,
                "surface patch epoch {received_epoch} does not match current epoch {current_epoch}"
            ),
            Self::StaleRevision {
                current_revision,
                received_revision,
            } => write!(
                formatter,
                "surface revision {received_revision} does not advance current revision {current_revision}"
            ),
            Self::OutputSequenceRegressed {
                current_sequence,
                received_sequence,
            } => write!(
                formatter,
                "surface output boundary regressed from {current_sequence} to {received_sequence}"
            ),
            Self::InvalidShape(error) => write!(formatter, "invalid surface grid: {error}"),
            Self::InvalidHyperlinkShape { expected, actual } => write!(
                formatter,
                "surface carries {actual} hyperlink IDs for {expected} grid cells"
            ),
            Self::AlreadyEnded(reason) => {
                write!(formatter, "surface stream already ended with {reason:?}")
            }
        }
    }
}

impl Error for PaneSurfaceApplyError {}

/// Opaque stream of authoritative structured pane surfaces.
///
/// Construction goes through [`crate::Pane::surface_stream`]. Rendering is
/// shared once per pane inside the daemon, independent of viewer count.
pub struct PaneSurfaceStream {
    inner: RecoverablePaneStream<PaneSurfaceEvent>,
}

impl PaneSurfaceStream {
    pub(crate) async fn open(transport: TransportClient, target: PaneTargetRef) -> Result<Self> {
        Ok(Self {
            inner: RecoverablePaneStream::open(
                transport,
                target,
                PaneStreamMode::Surface,
                false,
                map_event,
            )
            .await?,
        })
    }

    /// Returns the next surface event, waiting for pane activity when needed.
    pub async fn next(&mut self) -> Result<Option<PaneSurfaceEvent>> {
        self.inner.next().await
    }

    /// Performs at most one daemon cursor round trip and returns ready events.
    pub async fn poll_once(&mut self) -> Result<Vec<PaneSurfaceEvent>> {
        self.inner.poll_once().await
    }
}

impl fmt::Debug for PaneSurfaceStream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PaneSurfaceStream")
            .finish_non_exhaustive()
    }
}

fn validate_frame(frame: &PaneSurfaceFrame) -> std::result::Result<(), PaneSurfaceApplyError> {
    frame
        .snapshot
        .grid
        .validate_shape()
        .map_err(PaneSurfaceApplyError::InvalidShape)?;
    let expected = frame.snapshot.grid.cells.len();
    let actual = frame.snapshot.cell_hyperlink_ids.len();
    if actual != expected {
        return Err(PaneSurfaceApplyError::InvalidHyperlinkShape { expected, actual });
    }
    Ok(())
}

fn validate_output_boundary(
    current: &PaneSurfaceFrame,
    received: &PaneSurfaceFrame,
) -> std::result::Result<(), PaneSurfaceApplyError> {
    if received.next_output_sequence < current.next_output_sequence {
        return Err(PaneSurfaceApplyError::OutputSequenceRegressed {
            current_sequence: current.next_output_sequence,
            received_sequence: received.next_output_sequence,
        });
    }
    Ok(())
}

fn map_event(event: ProtoEvent) -> Result<MappedEvent<PaneSurfaceEvent>> {
    match event {
        ProtoEvent::SurfaceReset(frame) => Ok(MappedEvent::live(PaneSurfaceEvent::Reset(
            frame_from_proto(*frame)?,
        ))),
        ProtoEvent::SurfacePatch(frame) => Ok(MappedEvent::live(PaneSurfaceEvent::Patch(
            frame_from_proto(*frame)?,
        ))),
        ProtoEvent::Lifecycle(event) => Ok(MappedEvent::live(PaneSurfaceEvent::Lifecycle(
            lifecycle_from_proto(event),
        ))),
        ProtoEvent::End(reason) => Ok(MappedEvent::terminal(PaneSurfaceEvent::End(
            end_from_proto(reason),
        ))),
        ProtoEvent::RawRebase(_) | ProtoEvent::RawBytes(_) => Err(wrong_projection()),
    }
}

fn frame_from_proto(value: ProtoFrame) -> Result<PaneSurfaceFrame> {
    Ok(PaneSurfaceFrame {
        epoch: value.epoch,
        revision: value.revision,
        next_output_sequence: value.next_output_sequence,
        snapshot: snapshot_from_proto(value.snapshot)?,
    })
}

fn snapshot_from_proto(value: ProtoSnapshot) -> Result<PaneSurfaceSnapshot> {
    let mut hyperlinks = BTreeMap::new();
    for hyperlink in value.hyperlinks {
        if hyperlink.id == 0 {
            return Err(invalid_surface_metadata(
                "pane surface hyperlink ID must be non-zero",
            ));
        }
        if hyperlinks.insert(hyperlink.id, hyperlink.uri).is_some() {
            return Err(invalid_surface_metadata(
                "pane surface hyperlink IDs must be unique",
            ));
        }
    }
    let cell_hyperlink_ids = value
        .cells
        .iter()
        .map(|cell| (cell.link != 0).then_some(cell.link))
        .collect::<Vec<_>>();
    let grid = PaneSnapshot {
        cols: value.cols,
        rows: value.rows,
        cells: value.cells.into_iter().map(cell_from_wire).collect(),
        cursor: cursor_from_wire(value.cursor),
        revision: value.revision,
    };
    grid.validate_shape().map_err(|error| {
        RmuxError::protocol(rmux_proto::RmuxError::Server(format!(
            "pane surface response had malformed row-major cell shape: {error}"
        )))
    })?;
    if value.metadata_complete
        && cell_hyperlink_ids
            .iter()
            .filter_map(|id| *id)
            .any(|id| !hyperlinks.contains_key(&id))
    {
        return Err(invalid_surface_metadata(
            "complete pane surface metadata omitted a visible hyperlink",
        ));
    }
    Ok(PaneSurfaceSnapshot {
        grid,
        cell_hyperlink_ids,
        title: value.title,
        path: value.path,
        hyperlinks,
        dynamic_colors: PaneSurfaceDynamicColors {
            foreground: value.dynamic_colors.foreground,
            background: value.dynamic_colors.background,
            cursor: value.dynamic_colors.cursor,
        },
        metadata_complete: value.metadata_complete,
        mode_bits: value.mode_bits,
        alternate: value.alternate,
        scroll_top: value.scroll_top,
        scroll_bottom: value.scroll_bottom,
        history_size: value.history_size,
        history_bytes: value.history_bytes,
    })
}

fn invalid_surface_metadata(message: &str) -> RmuxError {
    RmuxError::protocol(rmux_proto::RmuxError::Server(message.to_owned()))
}

fn wrong_projection() -> RmuxError {
    RmuxError::protocol(rmux_proto::RmuxError::Server(
        "rmux daemon sent a raw event to a surface stream".to_owned(),
    ))
}

#[cfg(test)]
#[path = "surface_tests.rs"]
mod tests;
