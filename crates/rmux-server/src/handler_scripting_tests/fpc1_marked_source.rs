use super::fpc1_junction::{
    canonical_request as junction_canonical, fixture as junction_fixture,
    quiet_fixture as junction_quiet_fixture, stale_requester as omitted_review_stale_requester,
};
use super::frs1::{cleanup, queue, window};
use super::*;
use std::fs;

fn source_file_succeeded(response: &Response) -> bool {
    matches!(response, Response::SourceFile(result)
        if result.exit_status().is_none() && result.stderr().is_empty())
}

fn layout_notification_count(
    events: &mut tokio::sync::broadcast::Receiver<crate::handler::QueuedLifecycleEvent>,
) -> usize {
    std::iter::from_fn(|| events.try_recv().ok())
        .filter(|event| {
            matches!(
                event.event,
                rmux_core::LifecycleEvent::WindowLayoutChanged { .. }
            )
        })
        .count()
}

#[tokio::test]
async fn omitted_review_marked_source_does_not_consume_stale_current_target() {
    let mut outcomes = Vec::new();
    for stale_context in [false, true] {
        let (handler, primary, _linked, root, valid, _events) = junction_fixture().await;
        let source = window(&handler, &primary).await.pane(0).unwrap().id();
        queue(&handler, "select-pane -m -t frs-sibling-primary:0.0")
            .await
            .unwrap();
        let (stale, _stale_events) =
            omitted_review_stale_requester(&handler, &primary, &root).await;
        let pid = if stale_context {
            stale.0.id()
        } else {
            valid.0.id()
        };
        let response = handler
            .dispatch(
                pid,
                junction_canonical("swap-pane -d -t junction-decoy:0.0"),
            )
            .await
            .response;
        let actual = window(&handler, &session_name("junction-decoy"))
            .await
            .pane(0)
            .unwrap()
            .id();
        eprintln!(
            "MARKED_SOURCE stale={stale_context} expected={source} actual={actual} response={response:?}"
        );
        let success = source_file_succeeded(&response);
        cleanup(&handler).await;
        drop(stale);
        drop(valid);
        fs::remove_dir_all(root).unwrap();
        outcomes.push((stale_context, success && actual == source));
    }
    assert_eq!(
        outcomes,
        vec![(false, true), (true, true)],
        "a valid marked pane is the default source; stale unused current context must not reject it"
    );
}

#[tokio::test]
async fn marked_source_neighbors_ignore_stale_current_target() {
    for command in [
        "join-pane -d -t junction-decoy:0.0",
        "move-pane -d -t junction-decoy:0.0",
        "swap-window -d -t junction-decoy:0",
    ] {
        let (handler, primary, _linked, root, valid, _events) = junction_fixture().await;
        let source_window = window(&handler, &primary).await;
        let source_window_id = source_window.id();
        let source_pane_id = source_window.pane(0).unwrap().id();
        queue(&handler, "select-pane -m -t frs-sibling-primary:0.0")
            .await
            .unwrap();
        let (stale, _stale_events) =
            omitted_review_stale_requester(&handler, &primary, &root).await;

        let response = handler
            .dispatch(stale.0.id(), junction_canonical(command))
            .await
            .response;
        let decoy = window(&handler, &session_name("junction-decoy")).await;
        let used_mark = if command.starts_with("swap-window") {
            decoy.id() == source_window_id
        } else {
            decoy.panes().iter().any(|pane| pane.id() == source_pane_id)
        };
        eprintln!(
            "MARKED_NEIGHBOR command={command:?} source_window={source_window_id} \
             source_pane={source_pane_id} response={response:?} used_mark={used_mark}"
        );
        assert!(source_file_succeeded(&response), "{command}: {response:?}");
        assert!(used_mark, "{command} did not consume the valid mark");

        cleanup(&handler).await;
        drop(stale);
        drop(valid);
        fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test]
async fn missing_cleared_or_removed_mark_keeps_stale_current_refusal() {
    for mark_state in ["missing", "cleared", "removed"] {
        // This fixture compares whole Windows, name and activity_at included,
        // to prove the refusal mutates nothing. The default producers rename
        // the window automatically and stamp activity, so the witness needs the
        // quiet precondition that keeps the PTYs alive, uses the silent
        // command and names the windows explicitly.
        let (handler, primary, _linked, root, valid, _events) = junction_quiet_fixture().await;
        match mark_state {
            "missing" => {}
            "cleared" => {
                queue(&handler, "select-pane -m -t frs-sibling-primary:0.0")
                    .await
                    .unwrap();
                queue(&handler, "select-pane -M -t frs-sibling-primary:0.0")
                    .await
                    .unwrap();
            }
            "removed" => {
                queue(&handler, "select-pane -m -t frs-sibling-primary:0.1")
                    .await
                    .unwrap();
                queue(&handler, "kill-pane -t frs-sibling-primary:0.1")
                    .await
                    .unwrap();
            }
            _ => unreachable!(),
        }
        assert!(handler.state.lock().await.marked_pane_target().is_none());
        let (stale, _stale_events) =
            omitted_review_stale_requester(&handler, &primary, &root).await;
        let primary_before = window(&handler, &primary).await;
        let decoy_name = session_name("junction-decoy");
        let decoy_before = window(&handler, &decoy_name).await;
        let resize_before = handler
            .state
            .lock()
            .await
            .window_runtime_resize_count_for_test();
        let mut notifications = handler.subscribe_lifecycle_events();

        let response = handler
            .dispatch(
                stale.0.id(),
                junction_canonical("swap-pane -d -t junction-decoy:0.0"),
            )
            .await
            .response;
        let rejected = matches!(&response, Response::SourceFile(result)
            if result.exit_status() == Some(1)
                && String::from_utf8_lossy(result.stderr())
                    .contains("is not present in inherited session"));
        eprintln!("MARK_FALLBACK state={mark_state} response={response:?}");
        assert!(rejected, "{mark_state}: {response:?}");
        assert_eq!(window(&handler, &primary).await, primary_before);
        assert_eq!(window(&handler, &decoy_name).await, decoy_before);
        assert_eq!(
            handler
                .state
                .lock()
                .await
                .window_runtime_resize_count_for_test(),
            resize_before
        );
        assert_eq!(layout_notification_count(&mut notifications), 0);

        cleanup(&handler).await;
        drop(stale);
        drop(valid);
        fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test]
async fn independent_modes_do_not_consume_stale_current_target() {
    let (handler, primary, _linked, root, valid, _events) = junction_fixture().await;
    queue(&handler, "split-window -h -t junction-decoy:0.0")
        .await
        .unwrap();
    let decoy_name = session_name("junction-decoy");
    let before = window(&handler, &decoy_name).await;
    let (stale, _stale_events) = omitted_review_stale_requester(&handler, &primary, &root).await;
    for command in [
        "move-window -dr -t junction-decoy:0",
        "swap-pane -dD -t junction-decoy:0.0",
        "swap-pane -dU -t junction-decoy:0.0",
        "copy-mode -q -t junction-decoy:0.0",
    ] {
        let response = handler
            .dispatch(stale.0.id(), junction_canonical(command))
            .await
            .response;
        eprintln!("INDEPENDENT_MODE command={command:?} response={response:?}");
        assert!(source_file_succeeded(&response), "{command}: {response:?}");
    }
    let after = window(&handler, &decoy_name).await;
    assert_eq!(
        before
            .panes()
            .iter()
            .map(|pane| pane.id())
            .collect::<Vec<_>>(),
        after
            .panes()
            .iter()
            .map(|pane| pane.id())
            .collect::<Vec<_>>()
    );

    cleanup(&handler).await;
    drop(stale);
    drop(valid);
    fs::remove_dir_all(root).unwrap();
}
