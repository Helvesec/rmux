//! Alternative views of one pane: choose the view your application needs.
//! Cancellation waits below retain cursor owners; explicit drop releases them.

use std::future::Future;
use std::time::Duration;

use rmux_sdk::{
    Pane, PaneLagNotice, PaneRecoveryEvent, PaneRecoveryRebase, PaneRecoveryState, PaneSnapshot,
    PaneStreamEndReason, PaneStreamLifecycleEvent, PaneSurfaceEvent, PaneSurfaceState,
    PaneSurfaceStream,
};

use crate::AppResult;

#[derive(Debug, PartialEq, Eq)]
pub enum Observation {
    Cancelled,
    Closed,
    Ended(PaneStreamEndReason),
}

/// Instructions for the application's terminal emulator; payloads stay borrowed.
#[derive(Debug, PartialEq, Eq)]
pub enum RawAction<'a> {
    Reset(&'a PaneRecoveryRebase),
    Feed(&'a [u8]),
    ChildExited,
    Ended(PaneStreamEndReason),
    Unsupported,
}

pub fn raw_action(event: &PaneRecoveryEvent) -> RawAction<'_> {
    match event {
        PaneRecoveryEvent::Rebase(rebase) => RawAction::Reset(rebase),
        PaneRecoveryEvent::Bytes { bytes, .. } => RawAction::Feed(bytes),
        PaneRecoveryEvent::Lifecycle(PaneStreamLifecycleEvent::ProcessExited { .. }) => {
            RawAction::ChildExited
        }
        PaneRecoveryEvent::End(reason) => RawAction::Ended(*reason),
        _ => RawAction::Unsupported,
    }
}

/// `emulator` must reset/resize on Reset, then feed keyframe; Feed is exact bytes.
/// Rebase includes epoch/generation/cursor, coverage and an optional owned snapshot.
/// An error returns immediately: a raw subscription cannot retry a rejected poll.
pub async fn observe_raw(
    pane: &Pane,
    cancel: impl Future<Output = ()>,
    mut emulator: impl FnMut(RawAction<'_>),
) -> AppResult<Observation> {
    let mut stream = pane.recover_output().await?;
    let mut state = PaneRecoveryState::default();
    tokio::pin!(cancel);
    let outcome = loop {
        let event = tokio::select! {
            biased;
            () = &mut cancel => break Observation::Cancelled,
            event = stream.next() => event?,
        };
        let Some(event) = event else {
            break Observation::Closed;
        };
        state.apply(&event)?; // Optional replay/cursor state; stream already validates.
        let action = raw_action(&event);
        if action == RawAction::Unsupported {
            return Err(
                std::io::Error::new(std::io::ErrorKind::Unsupported, "unknown raw event").into(),
            );
        }
        emulator(action);
        if let Some(reason) = state.ended() {
            break Observation::Ended(reason);
        }
    };
    drop(stream); // Best-effort unsubscribe; no pane/process/daemon shutdown.
    Ok(outcome)
}

/// Request the optional viewport without constructing a non-exhaustive DTO literal.
pub async fn recovery_with_viewport(pane: &Pane) -> rmux_sdk::Result<rmux_sdk::PaneRecoveryStream> {
    let mut options = rmux_sdk::PaneRecoveryOptions::default();
    options.include_snapshot = true;
    pane.recover_output_with(options).await
}

/// Caller owns the stream: a projection poll error returns without dropping it.
/// The caller may correct pane state and explicitly call again on this same owner.
/// Reducer errors propagate separately through AppResult; they are not retried.
pub async fn observe_surface(
    stream: &mut PaneSurfaceStream,
    state: &mut PaneSurfaceState,
    cancel: impl Future<Output = ()>,
    mut display: impl FnMut(&rmux_sdk::PaneSurfaceFrame),
) -> AppResult<Observation> {
    tokio::pin!(cancel);
    loop {
        let event = tokio::select! {
            biased;
            () = &mut cancel => return Ok(Observation::Cancelled),
            event = stream.next() => event?,
        };
        let Some(event) = event else {
            return Ok(Observation::Closed);
        };
        if state.apply(&event)? {
            if let Some(frame) = state.frame() {
                // Reset and Patch replace the full frame, including hyperlink metadata.
                display(frame);
            }
        }
        if let PaneSurfaceEvent::End(reason) = event {
            return Ok(Observation::Ended(reason));
        }
    }
}

/// An explicit retry after the application has identified a recoverable projection
/// error and arranged a pane-state change. Not a blanket retry of protocol errors.
/// A cancelled wait keeps `stream` available; drop it when observation is finished.
pub async fn retry_surface_after_change(
    stream: &mut PaneSurfaceStream,
    state: &mut PaneSurfaceState,
    changed: impl Future<Output = ()>,
    cancel: impl Future<Output = ()>,
    display: impl FnMut(&rmux_sdk::PaneSurfaceFrame),
) -> AppResult<Observation> {
    tokio::pin!(cancel);
    tokio::select! {
        biased;
        () = &mut cancel => return Ok(Observation::Cancelled),
        () = changed => {}
    }
    observe_surface(stream, state, cancel, display).await
}

pub async fn open_surface(pane: &Pane) -> rmux_sdk::Result<PaneSurfaceStream> {
    pane.surface_stream().await
}

/// Protect a separately captured initial grid from older queued render updates.
fn advances_image(current: u64, incoming: u64) -> bool {
    incoming > current
}

/// Opens render before taking ONE initial snapshot. Render setup consumes its own
/// reset, so waiting on next() alone need not paint anything on a quiet pane.
pub async fn observe_render(
    rmux: &rmux_sdk::Rmux,
    session: rmux_sdk::SessionName,
    pane_id: rmux_sdk::PaneId,
    cancel: impl Future<Output = ()>,
    mut display: impl FnMut(&PaneSnapshot),
    mut report_lag: impl FnMut(&PaneLagNotice),
) -> AppResult<Observation> {
    // Bind both operations to the same logical pane even if its slot moves.
    let pane = rmux.pane_by_id(session, pane_id).await?;
    let mut stream = pane
        .render_stream()
        .await?
        .with_debounce(Duration::from_millis(16));
    let initial = pane.snapshot().await?;
    let mut revision = initial.revision;
    display(&initial);
    tokio::pin!(cancel);
    let outcome = loop {
        let update = tokio::select! {
            biased;
            () = &mut cancel => break Observation::Cancelled,
            update = stream.next() => update?,
        };
        let Some(update) = update else {
            break Observation::Closed;
        };
        // Lag remains meaningful even for an image that is already displayed.
        if let Some(lag) = update.lag() {
            report_lag(lag);
        }
        let snapshot = update.into_snapshot();
        if advances_image(revision, snapshot.revision) {
            revision = snapshot.revision;
            display(&snapshot);
        }
        // No snapshot RPC here: updates come from the shared surface projection.
    };
    drop(stream); // Releases both internal subscriptions, not the child process.
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_exit_is_not_logical_end() {
        for output_sequence in [None, Some(7)] {
            let event = PaneRecoveryEvent::Lifecycle(PaneStreamLifecycleEvent::ProcessExited {
                output_sequence,
            });
            assert_eq!(raw_action(&event), RawAction::ChildExited);
        }
        let end = PaneRecoveryEvent::End(PaneStreamEndReason::PaneRemoved);
        assert_eq!(
            raw_action(&end),
            RawAction::Ended(PaneStreamEndReason::PaneRemoved)
        );
    }

    #[test]
    fn raw_delivery_preserves_bytes_including_empty_payloads() {
        for bytes in [vec![], vec![0xff, 0, b'\n']] {
            let event = PaneRecoveryEvent::Bytes {
                epoch: 2,
                sequence: 7,
                bytes: bytes.clone(),
            };
            assert_eq!(raw_action(&event), RawAction::Feed(&bytes));
        }
    }

    #[test]
    fn initial_render_image_does_not_regress_to_queued_revision() {
        assert!(!advances_image(12, 11));
        assert!(!advances_image(12, 12));
        assert!(advances_image(12, 13));
        assert!(!advances_image(u64::MAX, 0));
    }
}
