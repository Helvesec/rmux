//! Daemon pane-stream event mapping and admission for raw recovery streams.

use rmux_proto::{
    PaneRawRebase as ProtoRebase, PaneRawRebaseReason as ProtoReason, PaneStreamEvent as ProtoEvent,
};

use super::{
    PaneRecoveryCoverage, PaneRecoveryEvent, PaneRecoveryRebase, PaneRecoveryRebaseReason,
};
use crate::events::pane_stream::{
    end_from_proto, lifecycle_from_proto, unsupported_stream_variant, MappedEvent,
};
use crate::handles::pane::snapshot::snapshot_from_response;
use crate::{Result, RmuxError};

/// A raw recovery stream opens on an authoritative initial keyframe, or on the
/// typed end the daemon substitutes for one.
///
/// Bytes, a lifecycle observation, a surface frame, or a rebase that resumes an
/// established epoch would leave the consumer emulator without the state those
/// events continue. A terminal end continues nothing and carries no bytes, so
/// it needs neither keyframe nor emulator: it is the entire stream. The daemon
/// replaces the opening event with one when observation permission is revoked
/// while the subscribe request is in flight, and refusing it would report a
/// deliberate authorization decision as server drift. Which terminal reasons
/// are transportable stays [`end_from_proto`]'s single fail-closed allowlist,
/// so a new one needs no second gate here.
pub(super) fn admit_initial_raw_event(event: &ProtoEvent) -> Result<()> {
    match event {
        ProtoEvent::RawRebase(rebase) if rebase.reason != ProtoReason::Initial => {
            Err(invalid_recovery_admission(format!(
                "raw recovery stream opened with a {:?} rebase instead of an initial one",
                rebase.reason
            )))
        }
        ProtoEvent::RawRebase(_) | ProtoEvent::End(_) => Ok(()),
        _ => Err(invalid_recovery_admission(format!(
            "raw recovery stream opened with a {} event instead of a rebase or a typed end",
            stream_event_kind(event)
        ))),
    }
}

const fn stream_event_kind(event: &ProtoEvent) -> &'static str {
    match event {
        ProtoEvent::RawRebase(_) => "raw-rebase",
        ProtoEvent::RawBytes(_) => "raw-bytes",
        ProtoEvent::SurfaceReset(_) => "surface-reset",
        ProtoEvent::SurfacePatch(_) => "surface-patch",
        ProtoEvent::Lifecycle(_) => "lifecycle",
        ProtoEvent::End(_) => "end",
        _ => "unsupported",
    }
}

pub(super) fn map_event(event: ProtoEvent) -> Result<MappedEvent<PaneRecoveryEvent>> {
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
            lifecycle_from_proto(event)?,
        ))),
        ProtoEvent::End(reason) => Ok(MappedEvent::terminal(PaneRecoveryEvent::End(
            end_from_proto(reason)?,
        ))),
        ProtoEvent::SurfaceReset(_) | ProtoEvent::SurfacePatch(_) => Err(wrong_projection()),
        _ => Err(unsupported_stream_variant("event")),
    }
}

pub(super) fn rebase_from_proto(value: ProtoRebase) -> Result<PaneRecoveryRebase> {
    let snapshot = value.snapshot.map(snapshot_from_response).transpose()?;
    // A keyframe that carries no geometry or no bytes cannot reset and
    // reconstruct an emulator, and a typed snapshot describes the very grid
    // the keyframe paints.
    if value.cols == 0 || value.rows == 0 {
        return Err(invalid_recovery_admission(format!(
            "raw recovery rebase reported an empty {}x{} keyframe geometry",
            value.cols, value.rows
        )));
    }
    if value.keyframe.is_empty() {
        return Err(invalid_recovery_admission(
            "raw recovery rebase carried no keyframe bytes".to_owned(),
        ));
    }
    if let Some(snapshot) = snapshot.as_ref() {
        if snapshot.cols != value.cols || snapshot.rows != value.rows {
            return Err(invalid_recovery_admission(format!(
                "raw recovery rebase snapshot geometry {}x{} does not match its {}x{} keyframe",
                snapshot.cols, snapshot.rows, value.cols, value.rows
            )));
        }
    }
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
        reason: reason_from_proto(value.reason)?,
    })
}

fn reason_from_proto(reason: ProtoReason) -> Result<PaneRecoveryRebaseReason> {
    Ok(match reason {
        ProtoReason::Initial => PaneRecoveryRebaseReason::Initial,
        ProtoReason::Resize => PaneRecoveryRebaseReason::Resize,
        ProtoReason::ClearHistory => PaneRecoveryRebaseReason::ClearHistory,
        ProtoReason::ParserStateExpired => PaneRecoveryRebaseReason::ParserStateExpired,
        ProtoReason::TerminalReset => PaneRecoveryRebaseReason::TerminalReset,
        ProtoReason::TranscriptMutation => PaneRecoveryRebaseReason::TranscriptMutation,
        ProtoReason::Lag => PaneRecoveryRebaseReason::Lag,
        ProtoReason::GenerationChanged => PaneRecoveryRebaseReason::GenerationChanged,
        _ => return Err(unsupported_stream_variant("raw-rebase reason")),
    })
}

fn wrong_projection() -> RmuxError {
    RmuxError::from_proto(rmux_proto::RmuxError::Server(
        "rmux daemon sent a surface event to a raw recovery stream".to_owned(),
    ))
}

fn invalid_recovery_admission(detail: String) -> RmuxError {
    RmuxError::from_proto(rmux_proto::RmuxError::Server(format!(
        "rmux daemon {detail}"
    )))
}
