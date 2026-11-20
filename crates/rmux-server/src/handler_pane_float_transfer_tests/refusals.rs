//! FMT1 refusals and their compensation: a floating destination, an injected
//! native resize failure, and the transfer that must still work afterwards.
use super::*;

#[tokio::test]
async fn a_floating_target_is_refused_without_effect_or_layout_change() {
    let (handler, name, source_id, witness_id, _) = fixture("fmt1-refuse").await;
    let before = window(&handler, &name, 0).await;
    let source_index = index_of(&before, source_id);
    let witness_index = index_of(&before, witness_id);
    let mut events = handler.subscribe_lifecycle_events();

    for source_index in [source_index, 0] {
        let response = handler
            .handle(Request::MovePane(move_request(
                pane(&name, 0, source_index),
                pane(&name, 0, witness_index),
            )))
            .await;
        match response {
            Response::Error(response) => assert!(
                response.error.to_string().contains("floating placement"),
                "{response:?}"
            ),
            other => panic!("a floating target must be refused: {other:?}"),
        }
        assert_eq!(
            window(&handler, &name, 0).await,
            before,
            "a refusal leaves the window, its stack and its zoom untouched"
        );
        assert!(
            drain_layout_targets(&mut events).is_empty(),
            "a refusal publishes no layout change"
        );
    }
    cleanup(&handler).await;
}
#[tokio::test]
async fn a_pre_batch_resize_refusal_rolls_the_floating_transfer_back_without_event() {
    // `fail_next_resize_for_test` is refused by `PaneTerminalStore::resize_session`
    // in its pre-loop, so it stops the batch BEFORE any native call:
    // `NativeResizeStop::BeforeBatch` with `completed_prefix` 0. What this
    // witnesses is therefore the transaction itself — model restore, runtime
    // kept, nothing published — and not a failure after a call. The partial and
    // residual cases, where a call really applied, need the per-call seam and
    // live in `native_resize.rs`, macOS-only and PENDING on this host.
    let (handler, name, source_id, witness_id, tile_id) = fixture("fmt1-rollback").await;
    let before = window(&handler, &name, 0).await;
    let source_index = index_of(&before, source_id);
    let tile_index = index_of(&before, tile_id);
    let source_runtime_before = runtime_witness(&handler, source_id).await;
    let witness_runtime_before = runtime_witness(&handler, witness_id).await;
    handler.state.lock().await.fail_next_resize_for_test();
    let mut events = handler.subscribe_lifecycle_events();

    let response = handler
        .handle(Request::MovePane(move_request(
            pane(&name, 0, source_index),
            pane(&name, 0, tile_index),
        )))
        .await;
    assert!(
        matches!(&response, Response::Error(error)
            if error.error.to_string().contains("injected pane terminal resize failure")),
        "expected the injected rollback path, got {response:?}"
    );

    let after = window(&handler, &name, 0).await;
    assert_eq!(
        after, before,
        "the model is restored to its pre-transfer state"
    );
    assert_eq!(stack_ids(&after), vec![source_id, witness_id]);
    assert!(
        after.floating_stack().contains(source_id),
        "the source stays floating"
    );
    assert!(
        drain_layout_targets(&mut events).is_empty(),
        "a compensated failure publishes no layout change"
    );
    let state = handler.state.lock().await;
    for pane_index in [
        index_of(&after, tile_id),
        source_index,
        index_of(&after, witness_id),
    ] {
        state
            .pane_profile_in_window(&name, 0, pane_index)
            .expect("every runtime terminal must be restored");
    }
    drop(state);
    assert_eq!(
        runtime_witness(&handler, source_id).await.pid,
        source_runtime_before.pid,
        "compensation must not respawn the source"
    );
    assert_eq!(
        runtime_witness(&handler, witness_id).await,
        witness_runtime_before,
        "the witness float is untouched by the failure and its compensation"
    );
    cleanup(&handler).await;
}
#[tokio::test]
async fn a_refused_transfer_leaves_the_next_transfer_working() {
    // A refusal must not poison the queue, the marked pane or the window.
    let (handler, name, source_id, witness_id, tile_id) = fixture("fmt1-progress").await;
    let before = window(&handler, &name, 0).await;
    let source_index = index_of(&before, source_id);

    let refused = handler
        .handle(Request::MovePane(move_request(
            pane(&name, 0, source_index),
            pane(&name, 0, index_of(&before, witness_id)),
        )))
        .await;
    assert!(matches!(refused, Response::Error(_)), "{refused:?}");
    assert_eq!(window(&handler, &name, 0).await, before);

    let accepted = handler
        .handle(Request::MovePane(move_request(
            pane(&name, 0, source_index),
            pane(&name, 0, index_of(&before, tile_id)),
        )))
        .await;
    assert!(matches!(accepted, Response::MovePane(_)), "{accepted:?}");
    let after = window(&handler, &name, 0).await;
    assert_eq!(
        pane_ids(&after),
        vec![(0, tile_id), (1, source_id), (2, witness_id)]
    );
    assert_eq!(
        geometry(&after, source_id),
        rmux_core::PaneGeometry::new(41, 0, 39, 24)
    );
    assert_eq!(stack_ids(&after), vec![witness_id]);
    cleanup(&handler).await;
}
