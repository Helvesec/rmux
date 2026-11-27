//! FMT1 — what the transfer actually asks the OS for, recorded call by call.
//!
//! These witnesses run only where the resize outcome seam is compiled,
//! `#[cfg(all(test, target_os = "macos"))]` at
//! `crates/rmux-server/src/pane_terminals/native_resize.rs`. On Linux there is
//! no per-call observation at all: a child that records no `SIGWINCH` proves
//! its size did not change, never that no call was issued. Everything below is
//! therefore PENDING until a macOS host runs it.

use std::collections::BTreeMap;
use std::path::Path;

use rmux_proto::{Request, Response};

use super::*;
use crate::pane_terminals::ResizeCase;

async fn arm(handler: &RequestHandler, case: ResizeCase) {
    handler
        .state
        .lock()
        .await
        .arm_resize_outcome_test(Path::new("."), case);
}

async fn trace(handler: &RequestHandler) -> serde_json::Value {
    handler.state.lock().await.finish_resize_outcome_test()
}

/// One entry of the probe's recorded call list.
///
/// `injected_pre_call` separates an attempt the probe refused before the OS was
/// reached from a real `TIOCSWINSZ`; `returned_ok` says whether the call the
/// loop actually made succeeded. Both are needed to qualify a trace honestly:
/// an injected attempt is not an OS call, and a batch stops at the first one.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RecordedCall {
    pane_id: u32,
    requested: (u64, u64),
    injected_pre_call: bool,
    returned_ok: bool,
}

fn calls(trace: &serde_json::Value) -> Vec<RecordedCall> {
    trace["calls"]
        .as_array()
        .expect("the probe records a call list")
        .iter()
        .map(|call| {
            let requested = call["requested"].as_array().expect("requested size");
            RecordedCall {
                pane_id: call["pane_id"].as_u64().expect("pane id") as u32,
                requested: (
                    requested[0].as_u64().expect("cols"),
                    requested[1].as_u64().expect("rows"),
                ),
                injected_pre_call: call["injected_pre_call"]
                    .as_bool()
                    .expect("the probe marks every injected attempt"),
                returned_ok: call["returned_ok"]
                    .as_bool()
                    .expect("the loop records every outcome"),
            }
        })
        .collect()
}

/// Splits a trace at the first injected attempt.
///
/// `PaneTerminalStore::resize_session` returns as soon as one call fails, so
/// the injected attempt is the last entry of the batch that contained it. The
/// transfer transaction then restores its model snapshot and runs a second,
/// whole-session batch; everything after the boundary belongs to that one.
/// Deriving the boundary from the data rather than from a pane count is what
/// keeps the two batches from being confused with each other.
fn split_at_first_injected(recorded: &[RecordedCall]) -> (&[RecordedCall], &[RecordedCall]) {
    let boundary = recorded
        .iter()
        .position(|call| call.injected_pre_call)
        .expect("the probe injects into the first batch");
    recorded.split_at(boundary + 1)
}

/// The rectangle every live pane of the session held before the transfer, which
/// is what a successful compensation has to ask the OS to restore.
fn rectangles_before(window: &Window) -> BTreeMap<u32, (u64, u64)> {
    window
        .panes()
        .iter()
        .map(|pane| {
            (
                pane.id().as_u32(),
                (
                    u64::from(pane.geometry().cols()),
                    u64::from(pane.geometry().rows()),
                ),
            )
        })
        .collect()
}

#[tokio::test]
async fn the_transfer_asks_for_the_final_size_of_every_live_pane_of_its_session() {
    // The exact call set, recorded rather than inferred. Two facts it pins:
    // the source and the destination are asked for their FINAL rectangles and
    // for no intermediate one, and the untouched floating witness is asked for
    // the rectangle it already had.
    //
    // The whole-session scope is the decided contract, not a gap: the transfer
    // resizes its session through `resize_terminals`
    // (`pane_terminals/session_mutation.rs`), and FMT1 does not introduce
    // `set-pane-placement`'s scoped policy. A call at an unchanged size to an
    // intact float is not its exclusion from the batch, and an absent SIGWINCH
    // never establishes an absent call — which is exactly why this witness
    // records the call instead of inferring it from the child.
    let (handler, name, source_id, witness_id, tile_id) = fixture("fmt1-native-scope").await;
    let before = window(&handler, &name, 0).await;
    let source_index = index_of(&before, source_id);
    let tile_index = index_of(&before, tile_id);
    arm(&handler, ResizeCase::Record).await;

    let response = handler
        .handle(Request::MovePane(move_request(
            pane(&name, 0, source_index),
            pane(&name, 0, tile_index),
        )))
        .await;
    assert!(matches!(response, Response::MovePane(_)), "{response:?}");

    let recorded = calls(&trace(&handler).await);
    let requested = |pane_id: PaneId| {
        recorded
            .iter()
            .filter(|call| call.pane_id == pane_id.as_u32())
            .map(|call| call.requested)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        requested(source_id),
        vec![(39, 24)],
        "the moved pane is asked once, for its final rectangle"
    );
    assert_eq!(
        requested(tile_id),
        vec![(40, 24)],
        "the destination is asked once, for its final rectangle"
    );
    assert_eq!(
        requested(witness_id),
        vec![(12, 5)],
        "the untouched float is asked for the rectangle it already had"
    );
    assert!(
        recorded
            .iter()
            .all(|call| !call.injected_pre_call && call.returned_ok),
        "a successful transfer injects nothing and every call reaches the OS: {recorded:?}"
    );
    assert_eq!(
        recorded.len(),
        3,
        "one call per live pane of the session, no repeats: {recorded:?}"
    );
    cleanup(&handler).await;
}

/// What the transaction must do with the batch that follows the failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Compensation {
    /// Every pane of the pre-transfer set is asked for its old rectangle, and
    /// every one of those calls reaches the OS and succeeds.
    Restores,
    /// The probe refuses the first compensation call, so the batch stops there
    /// and the remaining panes are never attempted.
    FailsOnItsFirstCall,
}

#[tokio::test]
async fn a_native_resize_failure_after_a_real_call_is_compensated_over_the_same_panes() {
    // The R1 witness injected before the batch; this one lets the first call
    // reach the OS and refuses the second, so the compensation runs after real
    // OS work.
    //
    // `resize_session` records the attempt and returns at once, so the first
    // batch is two entries — one real call, then the injected one — and never
    // the whole session. The transaction then restores its model snapshot and
    // runs a second, whole-session batch. `TransferPartial` injects only at
    // call 2, so that second batch is three successful calls (2 + 3);
    // `TransferResidual` injects at call 3 as well, which is the first
    // compensation call, so the second batch is one refused attempt (2 + 1).
    for (case, label, compensation_contract, expected_total) in [
        (
            ResizeCase::TransferPartial,
            "partial",
            Compensation::Restores,
            5,
        ),
        (
            ResizeCase::TransferResidual,
            "residual",
            Compensation::FailsOnItsFirstCall,
            3,
        ),
    ] {
        let (handler, name, source_id, witness_id, tile_id) =
            fixture(&format!("fmt1-native-{label}")).await;
        let before = window(&handler, &name, 0).await;
        let restore = rectangles_before(&before);
        let source_index = index_of(&before, source_id);
        let tile_index = index_of(&before, tile_id);
        let witness_runtime = runtime_witness(&handler, witness_id).await;
        let source_runtime = runtime_witness(&handler, source_id).await;
        let mut events = handler.subscribe_lifecycle_events();
        arm(&handler, case).await;

        let response = handler
            .handle(Request::MovePane(move_request(
                pane(&name, 0, source_index),
                pane(&name, 0, tile_index),
            )))
            .await;
        let error = match response {
            Response::Error(response) => response.error.to_string(),
            other => panic!("{label}: a failed native resize must fail the transfer: {other:?}"),
        };

        // The model is restored before the compensation batch runs, in both
        // cases, and a failed transfer publishes no success.
        assert_eq!(
            window(&handler, &name, 0).await,
            before,
            "{label}: the model is restored whatever the OS did"
        );
        assert!(
            drain_layout_targets(&mut events).is_empty(),
            "{label}: a compensated failure publishes no layout change"
        );

        let recorded = calls(&trace(&handler).await);
        assert_eq!(
            recorded.len(),
            expected_total,
            "{label}: the seam's injection schedule moved: {recorded:?}"
        );
        let (initial, compensation) = split_at_first_injected(&recorded);

        // The batch that failed: one call that really reached the OS and
        // applied, then the attempt the probe refused, which ended the batch.
        assert_eq!(
            initial.len(),
            2,
            "{label}: the batch stops at its first failure: {initial:?}"
        );
        assert!(
            !initial[0].injected_pre_call && initial[0].returned_ok,
            "{label}: the first call must be a real OS call that succeeded: {initial:?}"
        );
        assert!(
            initial[1].injected_pre_call && !initial[1].returned_ok,
            "{label}: the second must be the injected attempt, never an OS call: {initial:?}"
        );
        for call in initial {
            assert!(
                restore.contains_key(&call.pane_id),
                "{label}: the failing batch only addresses live panes: {call:?}"
            );
        }

        match compensation_contract {
            Compensation::Restores => {
                assert_eq!(
                    compensation
                        .iter()
                        .map(|call| (call.pane_id, call.requested))
                        .collect::<BTreeMap<_, _>>(),
                    restore,
                    "{label}: compensation asks the complete pre-transfer set for its old \
                     rectangles: {compensation:?}"
                );
                assert_eq!(
                    compensation.len(),
                    restore.len(),
                    "{label}: one compensation call per pane, no repeats: {compensation:?}"
                );
                assert!(
                    compensation
                        .iter()
                        .all(|call| !call.injected_pre_call && call.returned_ok),
                    "{label}: every compensation call reaches the OS and succeeds: {compensation:?}"
                );
                assert!(
                    error.contains("failed to resize pane terminal"),
                    "{label}: a successful compensation propagates the original failure: {error}"
                );
                assert!(
                    !error.contains("failed to roll back"),
                    "{label}: nothing rolled back badly, so nothing may say so: {error}"
                );
            }
            Compensation::FailsOnItsFirstCall => {
                assert_eq!(
                    compensation.len(),
                    1,
                    "{label}: the compensation stops at its own first failure: {compensation:?}"
                );
                assert!(
                    compensation[0].injected_pre_call && !compensation[0].returned_ok,
                    "{label}: that entry is the refused attempt, not an OS call: {compensation:?}"
                );
                assert_eq!(
                    restore.get(&compensation[0].pane_id).copied(),
                    Some(compensation[0].requested),
                    "{label}: it was already asking for a pre-transfer rectangle: {compensation:?}"
                );
                // The remaining panes were never attempted. This witness says
                // what was asked for and what was refused; it claims nothing
                // about the size the OS is left holding.
                assert!(
                    error.contains("failed to roll back session"),
                    "{label}: a failed compensation must name itself: {error}"
                );
                assert!(
                    error.contains("failed to resize pane terminal"),
                    "{label}: and must still quote the failure it was compensating: {error}"
                );
            }
        }

        // Identity is unaffected either way: no respawn, and the untouched
        // float keeps its whole lifecycle record.
        assert_eq!(
            runtime_witness(&handler, source_id).await.pid,
            source_runtime.pid,
            "{label}: compensation never respawns the source"
        );
        assert_eq!(
            runtime_witness(&handler, witness_id).await,
            witness_runtime,
            "{label}: the untouched float keeps its whole lifecycle record"
        );
        cleanup(&handler).await;
    }
}
