use super::fpc1_junction::{
    fixture as junction_fixture, quiet_fixture as junction_quiet_fixture,
    quiet_fixture as materialized_source_fixture, stale_requester as junction_stale_requester,
};
use super::frs1::{cleanup, queue, window};
use super::*;
use std::fs;

fn source_file_request(path: &std::path::Path, target: Option<PaneTarget>) -> Request {
    Request::SourceFile(Box::new(SourceFileRequest {
        paths: vec![path.to_string_lossy().into_owned()],
        quiet: false,
        parse_only: false,
        verbose: false,
        expand_paths: false,
        target,
        caller_cwd: None,
        stdin: None,
    }))
}

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
async fn marked_legacy_and_source_file_boundaries_ignore_stale_unused_current() {
    for path in ["legacy-marked", "legacy-tilde", "source-file"] {
        let (handler, primary, _linked, root, valid, _events) = junction_fixture().await;
        queue(&handler, "select-pane -m -t frs-sibling-primary:0.0")
            .await
            .unwrap();
        let (stale, _stale_events) = junction_stale_requester(&handler, &primary, &root).await;
        let source_id = window(&handler, &primary).await.pane(0).unwrap().id();
        let response = if path == "source-file" {
            let file = root.join("marked-boundary.conf");
            fs::write(&file, "swap-pane -d -t junction-decoy:0.0\n").unwrap();
            handler
                .dispatch(stale.0.id(), source_file_request(&file, None))
                .await
                .response
        } else {
            let target = if path == "legacy-marked" {
                "{marked}"
            } else {
                "~"
            };
            handler
                .dispatch(
                    stale.0.id(),
                    Request::ResolveTarget(rmux_proto::ResolveTargetRequest {
                        target: Some(target.to_owned()),
                        target_type: rmux_proto::ResolveTargetType::Pane,
                        window_index: false,
                        prefer_unattached: false,
                    }),
                )
                .await
                .response
        };
        let correct = match &response {
            Response::ResolveTarget(result) => match &result.target {
                Target::Pane(target) => {
                    let state = handler.state.lock().await;
                    state
                        .sessions
                        .session(target.session_name())
                        .unwrap()
                        .window_at(target.window_index())
                        .unwrap()
                        .pane(target.pane_index())
                        .unwrap()
                        .id()
                        == source_id
                }
                _ => false,
            },
            Response::SourceFile(_) => {
                source_file_succeeded(&response)
                    && window(&handler, &session_name("junction-decoy"))
                        .await
                        .pane(0)
                        .unwrap()
                        .id()
                        == source_id
            }
            _ => false,
        };
        eprintln!("MARKED_PUBLIC path={path} correct={correct} response={response:?}");
        assert!(correct, "{path}: {response:?}");

        cleanup(&handler).await;
        drop(stale);
        drop(valid);
        fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test]
async fn cli_materialized_source_preserves_marked_priority_and_stale_fallback() {
    for mark_state in ["valid", "cleared"] {
        let (handler, primary, _linked, root, valid, _events) = materialized_source_fixture().await;
        queue(&handler, "select-pane -m -t frs-sibling-primary:0.0")
            .await
            .unwrap();
        if mark_state == "cleared" {
            queue(&handler, "select-pane -M -t frs-sibling-primary:0.0")
                .await
                .unwrap();
        }
        let (stale, _stale_events) = junction_stale_requester(&handler, &primary, &root).await;
        let primary_before = window(&handler, &primary).await;
        let decoy_name = session_name("junction-decoy");
        let decoy_before = window(&handler, &decoy_name).await;
        let source_id = primary_before.pane(0).unwrap().id();
        let resize_before = handler
            .state
            .lock()
            .await
            .window_runtime_resize_count_for_test();
        let mut notifications = handler.subscribe_lifecycle_events();

        // Exact two-request sequence emitted by the public source-file CLI.
        let resolution = handler
            .dispatch(
                stale.0.id(),
                Request::ResolveTarget(rmux_proto::ResolveTargetRequest {
                    target: Some(source_id.to_string()),
                    target_type: rmux_proto::ResolveTargetType::Pane,
                    window_index: false,
                    prefer_unattached: false,
                }),
            )
            .await
            .response;
        let target = match &resolution {
            Response::ResolveTarget(result) => match &result.target {
                Target::Pane(target) => Some(target.clone()),
                _ => None,
            },
            _ => None,
        };
        let file = root.join("materialized-source.conf");
        fs::write(&file, "swap-pane -d -t junction-decoy:0.0\n").unwrap();
        let response = handler
            .dispatch(stale.0.id(), source_file_request(&file, target))
            .await
            .response;

        if mark_state == "valid" {
            assert!(source_file_succeeded(&response), "{response:?}");
            assert_eq!(
                window(&handler, &decoy_name).await.pane(0).unwrap().id(),
                source_id
            );
        } else {
            assert!(
                format!("{response:?}").contains("is not present in inherited session"),
                "{response:?}"
            );
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
        }
        eprintln!(
            "MATERIALIZED_PUBLIC mark={mark_state} resolution={resolution:?} response={response:?}"
        );

        cleanup(&handler).await;
        drop(stale);
        drop(valid);
        fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test]
async fn materialized_source_snapshot_excludes_independent_automatic_rename() {
    let (handler, primary, _linked, root, requester, _events) = materialized_source_fixture().await;
    let mut snapshots = Vec::new();
    {
        let mut state = handler.state.lock().await;
        for name in [primary, session_name("junction-decoy")] {
            let target = WindowTarget::with_window(name.clone(), 0);
            // A late foreground-command observation can reach this same
            // production callback from pane startup or pane alerts, without
            // ResolveTarget or SourceFile ever executing. Keep it deterministic
            // and hold the state lock across the entire observation.
            state
                .options
                .set(
                    rmux_proto::ScopeSelector::Window(target.clone()),
                    rmux_proto::OptionName::AutomaticRenameFormat,
                    "late-startup-command".to_owned(),
                    rmux_proto::SetOptionMode::Replace,
                )
                .unwrap();
            let before = state.sessions.window(&target).unwrap().clone();
            let changed = RequestHandler::sync_automatic_window_name_for_window_target_locked(
                &mut state,
                &target,
                before.id(),
            )
            .is_some();
            let after = state.sessions.window(&target).unwrap().clone();
            snapshots.push((name, changed, before, after));
        }
    }
    cleanup(&handler).await;
    drop(requester);
    fs::remove_dir_all(root).unwrap();
    for (name, changed, before, after) in snapshots {
        eprintln!(
            "MATERIALIZED_IDLE window={name} automatic_rename={changed} before={:?} after={:?}",
            before.name(),
            after.name()
        );
        assert_eq!(
            after, before,
            "{name}: independent startup rename changed the witness"
        );
        assert!(
            !changed,
            "{name}: snapshot fixture admitted automatic rename"
        );
    }
}

#[tokio::test]
async fn source_file_current_consumer_keeps_stale_refusal_with_valid_mark() {
    // Same contract as the marked-source refusal: whole Windows are compared,
    // so the witness needs the quiet precondition the public-review neighbour
    // below already uses.
    let (handler, primary, _linked, root, valid, _events) = junction_quiet_fixture().await;
    queue(&handler, "select-pane -m -t frs-sibling-primary:0.0")
        .await
        .unwrap();
    let (stale, _stale_events) = junction_stale_requester(&handler, &primary, &root).await;
    let primary_before = window(&handler, &primary).await;
    let decoy_name = session_name("junction-decoy");
    let decoy_before = window(&handler, &decoy_name).await;
    let resize_before = handler
        .state
        .lock()
        .await
        .window_runtime_resize_count_for_test();
    let mut notifications = handler.subscribe_lifecycle_events();
    let file = root.join("current-consumer.conf");
    fs::write(&file, "link-window -d -t junction-decoy:2\n").unwrap();

    let response = handler
        .dispatch(stale.0.id(), source_file_request(&file, None))
        .await
        .response;
    assert!(
        matches!(&response, Response::SourceFile(result)
            if result.exit_status() == Some(1)
                && String::from_utf8_lossy(result.stderr())
                    .contains("is not present in inherited session")),
        "{response:?}"
    );
    assert_eq!(window(&handler, &primary).await, primary_before);
    assert_eq!(window(&handler, &decoy_name).await, decoy_before);
    assert!(handler
        .state
        .lock()
        .await
        .sessions
        .session(&decoy_name)
        .unwrap()
        .window_at(2)
        .is_none());
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

#[tokio::test]
async fn public_review_single_unrelated_attach_must_not_replace_stale_inherited_source() {
    let mut outcomes = Vec::new();
    for attach in [false, true] {
        let (handler, primary, _linked, root, valid, _events) =
            super::fpc1_junction::quiet_fixture().await;
        let (stale, _events2) = junction_stale_requester(&handler, &primary, &root).await;
        assert!(handler.state.lock().await.marked_pane_target().is_none());
        let before = window(&handler, &primary).await;
        let decoy_before = window(&handler, &session_name("junction-decoy")).await;
        let source_id = before.pane(0).unwrap().id();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<crate::pane_io::AttachControl>();
        if attach {
            handler
                .register_attach(std::process::id(), primary.clone(), tx)
                .await;
        }
        let resolution = handler
            .dispatch(
                stale.0.id(),
                Request::ResolveTarget(rmux_proto::ResolveTargetRequest {
                    target: Some(source_id.to_string()),
                    target_type: rmux_proto::ResolveTargetType::Pane,
                    window_index: false,
                    prefer_unattached: false,
                }),
            )
            .await
            .response;
        let target = match &resolution {
            Response::ResolveTarget(result) => match &result.target {
                Target::Pane(target) => Some(target.clone()),
                _ => None,
            },
            _ => None,
        };
        let target_before = target.clone();
        let file = root.join("unrelated-attach.conf");
        fs::write(&file, "swap-pane -d -t junction-decoy:0.0\n").unwrap();
        let response = handler
            .dispatch(stale.0.id(), source_file_request(&file, target))
            .await
            .response;
        let after = window(&handler, &session_name("junction-decoy")).await;
        let refused = matches!(&response, Response::Error(_))
            || matches!(&response, Response::SourceFile(result) if result.exit_status() == Some(1));
        let unchanged = after == decoy_before && window(&handler, &primary).await == before;
        eprintln!(
            "SINGLE_ATTACH attach={attach} requester={} resolution={resolution:?} \
             target_sent={target_before:?} response={response:?} expected_destination={} \
             actual_destination={} refused={refused} unchanged={unchanged}",
            stale.0.id(),
            decoy_before.pane(0).unwrap().id(),
            after.pane(0).unwrap().id()
        );
        cleanup(&handler).await;
        drop(stale);
        drop(valid);
        fs::remove_dir_all(root).unwrap();
        outcomes.push((attach, refused && unchanged));
    }
    assert_eq!(
        outcomes,
        vec![(false, true), (true, true)],
        "an unrelated sole attached client cannot replace an explicitly inherited but stale \
         SessionId/PaneId when current is required"
    );
}

#[tokio::test]
async fn source_file_keeps_requester_attach_and_valid_inherited_context() {
    for context in ["requester-attach", "valid-inherited"] {
        let (handler, primary, _linked, root, valid, _events) = junction_fixture().await;
        let (stale, _stale_events) = junction_stale_requester(&handler, &primary, &root).await;
        assert!(handler.state.lock().await.marked_pane_target().is_none());
        let source_id = window(&handler, &primary).await.pane(0).unwrap().id();
        let decoy_name = session_name("junction-decoy");
        let (requester_pid, attach_pid, attach_session) = if context == "requester-attach" {
            (stale.0.id(), stale.0.id(), primary.clone())
        } else {
            (valid.0.id(), std::process::id(), decoy_name.clone())
        };
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<crate::pane_io::AttachControl>();
        handler
            .register_attach(attach_pid, attach_session, tx)
            .await;
        let file = root.join(format!("{context}.conf"));
        fs::write(&file, "swap-pane -d -t junction-decoy:0.0\n").unwrap();

        let response = handler
            .dispatch(requester_pid, source_file_request(&file, None))
            .await
            .response;
        let actual = window(&handler, &decoy_name).await.pane(0).unwrap().id();
        eprintln!(
            "SOURCE_CONTEXT context={context} requester={requester_pid} attach={attach_pid} \
             response={response:?} expected_destination={source_id} actual_destination={actual}"
        );
        assert!(source_file_succeeded(&response), "{context}: {response:?}");
        assert_eq!(actual, source_id, "{context}");

        cleanup(&handler).await;
        drop(stale);
        drop(valid);
        fs::remove_dir_all(root).unwrap();
    }
}
