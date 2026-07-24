//! Authoritative structured pane surfaces for non-emulator consumers.

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
    /// Terminal title.
    pub title: String,
    /// Terminal-reported working-directory path.
    pub path: String,
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
    /// The frame's row-major grid shape was invalid.
    InvalidShape(crate::PaneSnapshotShapeError),
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
            Self::InvalidShape(error) => write!(formatter, "invalid surface grid: {error}"),
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
        .map_err(PaneSurfaceApplyError::InvalidShape)
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
    Ok(PaneSurfaceSnapshot {
        grid,
        title: value.title,
        path: value.path,
        mode_bits: value.mode_bits,
        alternate: value.alternate,
        scroll_top: value.scroll_top,
        scroll_bottom: value.scroll_bottom,
        history_size: value.history_size,
        history_bytes: value.history_bytes,
    })
}

fn wrong_projection() -> RmuxError {
    RmuxError::protocol(rmux_proto::RmuxError::Server(
        "rmux daemon sent a raw event to a surface stream".to_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PaneCursor, PaneSnapshot};

    fn frame(epoch: u64, revision: u64) -> PaneSurfaceFrame {
        PaneSurfaceFrame {
            epoch,
            revision,
            next_output_sequence: revision,
            snapshot: PaneSurfaceSnapshot {
                grid: PaneSnapshot::new(0, 0, Vec::new(), PaneCursor::default())
                    .expect("zero-sized grid")
                    .with_revision(revision),
                title: String::new(),
                path: String::new(),
                mode_bits: 0,
                alternate: false,
                scroll_top: 0,
                scroll_bottom: 0,
                history_size: 0,
                history_bytes: 0,
            },
        }
    }

    #[test]
    fn reducer_requires_reset_then_monotone_same_epoch_patches() {
        let mut state = PaneSurfaceState::default();
        assert_eq!(
            state.apply(&PaneSurfaceEvent::Patch(frame(1, 1))),
            Err(PaneSurfaceApplyError::PatchBeforeReset)
        );
        assert_eq!(state.apply(&PaneSurfaceEvent::Reset(frame(1, 1))), Ok(true));
        assert_eq!(
            state.apply(&PaneSurfaceEvent::Patch(frame(1, 1))),
            Err(PaneSurfaceApplyError::StaleRevision {
                current_revision: 1,
                received_revision: 1,
            })
        );
        assert_eq!(
            state.apply(&PaneSurfaceEvent::Patch(frame(2, 2))),
            Err(PaneSurfaceApplyError::EpochMismatch {
                current_epoch: 1,
                received_epoch: 2,
            })
        );
        assert_eq!(state.apply(&PaneSurfaceEvent::Patch(frame(1, 2))), Ok(true));
        assert_eq!(state.frame(), Some(&frame(1, 2)));
    }

    #[test]
    fn reducer_accepts_new_epoch_reset_and_rejects_events_after_end() {
        let mut state = PaneSurfaceState::default();
        state
            .apply(&PaneSurfaceEvent::Reset(frame(1, 1)))
            .expect("initial reset");
        state
            .apply(&PaneSurfaceEvent::Reset(frame(2, 2)))
            .expect("new epoch reset");
        state
            .apply(&PaneSurfaceEvent::End(PaneStreamEndReason::PaneRemoved))
            .expect("end");
        assert_eq!(state.ended(), Some(PaneStreamEndReason::PaneRemoved));
        assert_eq!(
            state.apply(&PaneSurfaceEvent::Lifecycle(
                PaneStreamLifecycleEvent::ProcessExited {
                    output_sequence: None,
                }
            )),
            Err(PaneSurfaceApplyError::AlreadyEnded(
                PaneStreamEndReason::PaneRemoved
            ))
        );
    }
}
