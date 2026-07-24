//! Recoverable raw pane output for terminal-emulator consumers.

use std::error::Error;
use std::fmt;

use rmux_proto::{
    PaneRawRebase as ProtoRebase, PaneRawRebaseReason as ProtoReason,
    PaneStreamEvent as ProtoEvent, PaneStreamMode, PaneTargetRef,
};

use super::pane_stream::{
    end_from_proto, lifecycle_from_proto, MappedEvent, PaneStreamEndReason,
    PaneStreamLifecycleEvent, RecoverablePaneStream,
};
use crate::handles::pane::snapshot::snapshot_from_response;
use crate::transport::TransportClient;
use crate::{PaneSnapshot, Result, RmuxError};

/// Options for opening a recoverable raw output stream.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct PaneRecoveryOptions {
    /// Whether each rebase should also carry a typed viewport snapshot.
    ///
    /// Emulator consumers normally leave this disabled because the required
    /// raw keyframe already reconstructs the pane.
    pub include_snapshot: bool,
}

/// Why the daemon rebuilt a raw stream's emulator state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PaneRecoveryRebaseReason {
    /// Initial stream state.
    Initial,
    /// The pane geometry changed.
    Resize,
    /// Scrollback was cleared.
    ClearHistory,
    /// An incomplete parser sequence expired.
    ParserStateExpired,
    /// Terminal state was reset.
    TerminalReset,
    /// Another transcript mutation invalidated continuation.
    TranscriptMutation,
    /// The stream fell behind retained raw output.
    Lag,
    /// The pane process generation changed.
    GenerationChanged,
}

/// Completeness report for a transport-bounded recovery keyframe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PaneRecoveryCoverage {
    /// Total scrollback rows retained by the daemon at capture time.
    pub history_rows_total: u64,
    /// Newest whole scrollback rows represented by the keyframe.
    pub history_rows_included: u64,
    /// Whether all bounded title, path, title-stack and hyperlink metadata was
    /// represented.
    pub metadata_complete: bool,
}

impl PaneRecoveryCoverage {
    /// Returns whether the keyframe includes every retained scrollback row.
    #[must_use]
    pub const fn history_complete(self) -> bool {
        self.history_rows_included >= self.history_rows_total
    }
}

/// Complete raw-emulator recovery state.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PaneRecoveryRebase {
    /// Stream-local epoch stamped onto following byte events.
    pub epoch: u64,
    /// Pane process generation at the atomic capture boundary.
    pub generation: u64,
    /// Cold-path invalidation revision at the boundary.
    pub invalidation_revision: u64,
    /// First output sequence not represented by `keyframe`.
    pub next_sequence: u64,
    /// Captured terminal width.
    pub cols: u16,
    /// Captured terminal height.
    pub rows: u16,
    /// ANSI bytes that reset and reconstruct a compatible emulator.
    pub keyframe: Vec<u8>,
    /// Whether the reconstructed pane is using its alternate screen.
    pub alternate: bool,
    /// Explicit report for bounded scrollback and terminal metadata.
    pub coverage: PaneRecoveryCoverage,
    /// Optional typed viewport requested through [`PaneRecoveryOptions`].
    pub snapshot: Option<PaneSnapshot>,
    /// Cause of this rebase.
    pub reason: PaneRecoveryRebaseReason,
}

/// One item from a recoverable raw pane stream.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PaneRecoveryEvent {
    /// Reset the consumer emulator and feed this authoritative keyframe.
    Rebase(PaneRecoveryRebase),
    /// Feed exact pane bytes after the current rebase.
    Bytes {
        /// Rebase epoch these bytes continue.
        epoch: u64,
        /// Monotonic per-pane output sequence.
        sequence: u64,
        /// Exact pane output bytes.
        bytes: Vec<u8>,
    },
    /// Non-terminal child lifecycle observation.
    Lifecycle(PaneStreamLifecycleEvent),
    /// Typed logical end of this stream.
    End(PaneStreamEndReason),
}

/// Client-side protocol state for applying [`PaneRecoveryEvent`] safely.
///
/// Feed a rebase keyframe after resetting the consumer emulator, then feed
/// each accepted byte event. A later rebase atomically replaces the prior
/// epoch; callers never stitch state across epochs.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PaneRecoveryState {
    epoch: Option<u64>,
    generation: Option<u64>,
    invalidation_revision: Option<u64>,
    next_sequence: Option<u64>,
    ended: Option<PaneStreamEndReason>,
}

impl PaneRecoveryState {
    /// Applies one event while enforcing epoch, generation, and sequence
    /// invariants.
    ///
    /// This method does not copy the keyframe or byte payload. After success,
    /// the caller can feed the bytes borrowed from the original event directly
    /// into its emulator.
    pub fn apply(
        &mut self,
        event: &PaneRecoveryEvent,
    ) -> std::result::Result<(), PaneRecoveryApplyError> {
        if let Some(reason) = self.ended {
            return Err(PaneRecoveryApplyError::AlreadyEnded(reason));
        }
        match event {
            PaneRecoveryEvent::Rebase(rebase) => self.apply_rebase(rebase),
            PaneRecoveryEvent::Bytes {
                epoch, sequence, ..
            } => self.apply_bytes(*epoch, *sequence),
            PaneRecoveryEvent::Lifecycle(PaneStreamLifecycleEvent::ProcessExited {
                output_sequence,
            }) => self.apply_lifecycle(*output_sequence),
            PaneRecoveryEvent::End(reason) => {
                self.ended = Some(*reason);
                Ok(())
            }
        }
    }

    /// Current stream-local epoch after the first rebase.
    #[must_use]
    pub const fn epoch(&self) -> Option<u64> {
        self.epoch
    }

    /// Current pane process generation after the first rebase.
    #[must_use]
    pub const fn generation(&self) -> Option<u64> {
        self.generation
    }

    /// Next exact output sequence expected from this epoch.
    #[must_use]
    pub const fn next_sequence(&self) -> Option<u64> {
        self.next_sequence
    }

    /// Terminal reason after an end event.
    #[must_use]
    pub const fn ended(&self) -> Option<PaneStreamEndReason> {
        self.ended
    }

    fn apply_rebase(
        &mut self,
        rebase: &PaneRecoveryRebase,
    ) -> std::result::Result<(), PaneRecoveryApplyError> {
        if let Some(current_epoch) = self.epoch {
            if rebase.epoch <= current_epoch {
                return Err(PaneRecoveryApplyError::StaleRebase {
                    current_epoch,
                    received_epoch: rebase.epoch,
                });
            }
        }
        if let Some(current_generation) = self.generation {
            if rebase.generation < current_generation {
                return Err(PaneRecoveryApplyError::GenerationRegressed {
                    current_generation,
                    received_generation: rebase.generation,
                });
            }
        }
        if let Some(current_revision) = self.invalidation_revision {
            if rebase.invalidation_revision < current_revision {
                return Err(PaneRecoveryApplyError::InvalidationRegressed {
                    current_revision,
                    received_revision: rebase.invalidation_revision,
                });
            }
        }
        if let Some(current_sequence) = self.next_sequence {
            if rebase.next_sequence < current_sequence {
                return Err(PaneRecoveryApplyError::SequenceRegressed {
                    current_sequence,
                    received_sequence: rebase.next_sequence,
                });
            }
        }
        self.epoch = Some(rebase.epoch);
        self.generation = Some(rebase.generation);
        self.invalidation_revision = Some(rebase.invalidation_revision);
        self.next_sequence = Some(rebase.next_sequence);
        Ok(())
    }

    fn apply_bytes(
        &mut self,
        epoch: u64,
        sequence: u64,
    ) -> std::result::Result<(), PaneRecoveryApplyError> {
        let current_epoch = self.epoch.ok_or(PaneRecoveryApplyError::BeforeRebase)?;
        if epoch != current_epoch {
            return Err(PaneRecoveryApplyError::EpochMismatch {
                current_epoch,
                received_epoch: epoch,
            });
        }
        let expected_sequence = self
            .next_sequence
            .ok_or(PaneRecoveryApplyError::BeforeRebase)?;
        if sequence != expected_sequence {
            return Err(PaneRecoveryApplyError::SequenceMismatch {
                expected_sequence,
                received_sequence: sequence,
            });
        }
        self.next_sequence = Some(
            sequence
                .checked_add(1)
                .ok_or(PaneRecoveryApplyError::SequenceExhausted)?,
        );
        Ok(())
    }

    fn apply_lifecycle(
        &mut self,
        output_sequence: Option<u64>,
    ) -> std::result::Result<(), PaneRecoveryApplyError> {
        let Some(output_sequence) = output_sequence else {
            return self
                .next_sequence
                .map(|_| ())
                .ok_or(PaneRecoveryApplyError::BeforeRebase);
        };
        self.apply_bytes(
            self.epoch.ok_or(PaneRecoveryApplyError::BeforeRebase)?,
            output_sequence,
        )
    }
}

/// A rejected raw-recovery state transition.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum PaneRecoveryApplyError {
    /// Bytes or lifecycle arrived before the initial rebase.
    BeforeRebase,
    /// A rebase did not advance the stream epoch.
    StaleRebase {
        /// Current reducer epoch.
        current_epoch: u64,
        /// Rejected epoch.
        received_epoch: u64,
    },
    /// A rebase moved to an older process generation.
    GenerationRegressed {
        /// Current process generation.
        current_generation: u64,
        /// Rejected process generation.
        received_generation: u64,
    },
    /// A rebase moved to an older invalidation revision.
    InvalidationRegressed {
        /// Current invalidation revision.
        current_revision: u64,
        /// Rejected invalidation revision.
        received_revision: u64,
    },
    /// A rebase moved behind already-consumed output.
    SequenceRegressed {
        /// Next sequence before the rejected rebase.
        current_sequence: u64,
        /// Rejected rebase sequence.
        received_sequence: u64,
    },
    /// Bytes addressed a different epoch.
    EpochMismatch {
        /// Current reducer epoch.
        current_epoch: u64,
        /// Rejected byte epoch.
        received_epoch: u64,
    },
    /// Bytes were not the exact next output event.
    SequenceMismatch {
        /// Expected sequence.
        expected_sequence: u64,
        /// Rejected sequence.
        received_sequence: u64,
    },
    /// The output sequence space was exhausted.
    SequenceExhausted,
    /// An event arrived after a terminal end.
    AlreadyEnded(PaneStreamEndReason),
}

impl fmt::Display for PaneRecoveryApplyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BeforeRebase => formatter.write_str("raw recovery event arrived before rebase"),
            Self::StaleRebase {
                current_epoch,
                received_epoch,
            } => write!(
                formatter,
                "raw recovery rebase epoch {received_epoch} does not advance {current_epoch}"
            ),
            Self::GenerationRegressed {
                current_generation,
                received_generation,
            } => write!(
                formatter,
                "raw recovery generation regressed from {current_generation} to {received_generation}"
            ),
            Self::InvalidationRegressed {
                current_revision,
                received_revision,
            } => write!(
                formatter,
                "raw recovery invalidation revision regressed from {current_revision} to {received_revision}"
            ),
            Self::SequenceRegressed {
                current_sequence,
                received_sequence,
            } => write!(
                formatter,
                "raw recovery rebase sequence {received_sequence} is behind {current_sequence}"
            ),
            Self::EpochMismatch {
                current_epoch,
                received_epoch,
            } => write!(
                formatter,
                "raw recovery byte epoch {received_epoch} does not match {current_epoch}"
            ),
            Self::SequenceMismatch {
                expected_sequence,
                received_sequence,
            } => write!(
                formatter,
                "raw recovery expected output sequence {expected_sequence}, got {received_sequence}"
            ),
            Self::SequenceExhausted => formatter.write_str("raw recovery sequence space exhausted"),
            Self::AlreadyEnded(reason) => {
                write!(formatter, "raw recovery stream already ended with {reason:?}")
            }
        }
    }
}

impl Error for PaneRecoveryApplyError {}

/// Opaque stream of automatically recoverable raw pane output.
///
/// Construction goes through [`crate::Pane::recover_output`]. A lag, resize,
/// clear, parser expiry, or process-generation change produces an in-band
/// [`PaneRecoveryEvent::Rebase`] on the same subscription.
pub struct PaneRecoveryStream {
    inner: RecoverablePaneStream<PaneRecoveryEvent>,
}

impl PaneRecoveryStream {
    pub(crate) async fn open(
        transport: TransportClient,
        target: PaneTargetRef,
        options: PaneRecoveryOptions,
    ) -> Result<Self> {
        Ok(Self {
            inner: RecoverablePaneStream::open(
                transport,
                target,
                PaneStreamMode::Raw,
                options.include_snapshot,
                map_event,
            )
            .await?,
        })
    }

    /// Returns the next raw event, waiting for pane activity when necessary.
    pub async fn next(&mut self) -> Result<Option<PaneRecoveryEvent>> {
        self.inner.next().await
    }

    /// Performs at most one daemon cursor round trip and returns ready events.
    pub async fn poll_once(&mut self) -> Result<Vec<PaneRecoveryEvent>> {
        self.inner.poll_once().await
    }
}

impl std::fmt::Debug for PaneRecoveryStream {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PaneRecoveryStream")
            .finish_non_exhaustive()
    }
}

fn map_event(event: ProtoEvent) -> Result<MappedEvent<PaneRecoveryEvent>> {
    match event {
        ProtoEvent::RawRebase(rebase) => Ok(MappedEvent::live(PaneRecoveryEvent::Rebase(
            rebase_from_proto(*rebase)?,
        ))),
        ProtoEvent::RawBytes(bytes) => Ok(MappedEvent::live(PaneRecoveryEvent::Bytes {
            epoch: bytes.epoch,
            sequence: bytes.sequence,
            bytes: bytes.bytes,
        })),
        ProtoEvent::Lifecycle(event) => Ok(MappedEvent::live(PaneRecoveryEvent::Lifecycle(
            lifecycle_from_proto(event),
        ))),
        ProtoEvent::End(reason) => Ok(MappedEvent::terminal(PaneRecoveryEvent::End(
            end_from_proto(reason),
        ))),
        ProtoEvent::SurfaceReset(_) | ProtoEvent::SurfacePatch(_) => Err(wrong_projection()),
    }
}

fn rebase_from_proto(value: ProtoRebase) -> Result<PaneRecoveryRebase> {
    let snapshot = value.snapshot.map(snapshot_from_response).transpose()?;
    Ok(PaneRecoveryRebase {
        epoch: value.epoch,
        generation: value.generation,
        invalidation_revision: value.invalidation_revision,
        next_sequence: value.next_sequence,
        cols: value.cols,
        rows: value.rows,
        keyframe: value.keyframe,
        alternate: value.alternate,
        coverage: PaneRecoveryCoverage {
            history_rows_total: value.coverage.history_rows_total,
            history_rows_included: value.coverage.history_rows_included,
            metadata_complete: value.coverage.metadata_complete,
        },
        snapshot,
        reason: reason_from_proto(value.reason),
    })
}

const fn reason_from_proto(reason: ProtoReason) -> PaneRecoveryRebaseReason {
    match reason {
        ProtoReason::Initial => PaneRecoveryRebaseReason::Initial,
        ProtoReason::Resize => PaneRecoveryRebaseReason::Resize,
        ProtoReason::ClearHistory => PaneRecoveryRebaseReason::ClearHistory,
        ProtoReason::ParserStateExpired => PaneRecoveryRebaseReason::ParserStateExpired,
        ProtoReason::TerminalReset => PaneRecoveryRebaseReason::TerminalReset,
        ProtoReason::TranscriptMutation => PaneRecoveryRebaseReason::TranscriptMutation,
        ProtoReason::Lag => PaneRecoveryRebaseReason::Lag,
        ProtoReason::GenerationChanged => PaneRecoveryRebaseReason::GenerationChanged,
    }
}

fn wrong_projection() -> RmuxError {
    RmuxError::protocol(rmux_proto::RmuxError::Server(
        "rmux daemon sent a surface event to a raw recovery stream".to_owned(),
    ))
}

#[cfg(test)]
#[path = "recovery_tests.rs"]
mod tests;
