//! FMT1 transfers that succeed: the floating source reaches its tiled
//! destination and takes its identity, runtime, options and transcript with it.
use super::*;

#[tokio::test]
async fn move_pane_and_join_pane_carry_a_floating_source_into_a_tiled_target() {
    // `-h` reaches the handler as SplitDirection::Horizontal and is inverted by
    // `join_pane_internal_direction`, producing the receipt's 40/39 side share.
    for use_join in [false, true] {
        let label = if use_join { "fmt1-join" } else { "fmt1-move" };
        let (handler, name, source_id, witness_id, tile_id) = fixture(label).await;
        let before = window(&handler, &name, 0).await;
        let source_index = index_of(&before, source_id);
        let source_before = runtime_witness(&handler, source_id).await;
        let witness_before = runtime_witness(&handler, witness_id).await;
        let mut events = handler.subscribe_lifecycle_events();

        let moved = if use_join {
            let response = handler
                .handle(Request::JoinPane(join_request(
                    pane(&name, 0, source_index),
                    pane(&name, 0, 0),
                )))
                .await;
            match response {
                Response::JoinPane(JoinPaneResponse { target }) => target,
                other => panic!("{other:?}"),
            }
        } else {
            let response = handler
                .handle(Request::MovePane(move_request(
                    pane(&name, 0, source_index),
                    pane(&name, 0, 0),
                )))
                .await;
            match response {
                Response::MovePane(MovePaneResponse { target }) => target,
                other => panic!("{other:?}"),
            }
        };

        let after = window(&handler, &name, 0).await;
        assert_eq!(moved.window_index(), 0);
        assert_eq!(after.pane(moved.pane_index()).unwrap().id(), source_id);
        assert_eq!(
            pane_ids(&after),
            vec![(0, tile_id), (1, source_id), (2, witness_id)],
            "the acquired receipt keeps %0/%1/%2 at 0/1/2"
        );
        assert_eq!(
            geometry(&after, tile_id),
            rmux_core::PaneGeometry::new(0, 0, 40, 24)
        );
        assert_eq!(
            geometry(&after, source_id),
            rmux_core::PaneGeometry::new(41, 0, 39, 24)
        );
        assert_eq!(
            geometry(&after, witness_id),
            rmux_core::PaneGeometry::new(50, 4, 12, 5),
            "the untouched witness float keeps its rectangle"
        );
        assert_eq!(stack_ids(&after), vec![witness_id]);
        assert_eq!(after.active_pane_index(), index_of(&after, source_id));
        assert!(!after.is_zoomed());
        // The runtime followed the pane: same OS process, same runtime
        // generation, no exit; only the native size moved to its final value.
        let source_after = runtime_witness(&handler, source_id).await;
        assert_eq!(source_after.pid, source_before.pid);
        assert_eq!(source_after.generation, source_before.generation);
        assert!(!source_after.exited);
        assert_eq!(
            source_after.dimensions,
            rmux_proto::TerminalSize { cols: 39, rows: 24 },
            "a pane whose native size changes receives the final size"
        );
        assert_eq!(
            runtime_witness(&handler, witness_id).await,
            witness_before,
            "an unchanged witness float receives no runtime update at all"
        );
        assert!(!drain_layout_targets(&mut events).is_empty());
        cleanup(&handler).await;
    }
}
#[tokio::test]
async fn a_floating_source_crosses_windows_and_keeps_its_runtime() {
    let (handler, name, source_id, witness_id, tile_id) = fixture("fmt1-cross").await;
    let created = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: name.clone(),
            name: None,
            detached: true,
            environment: None,
            command: None,
            start_directory: None,
            target_window_index: None,
            insert_at_target: false,
            process_command: None,
        })))
        .await;
    assert!(matches!(created, Response::NewWindow(_)), "{created:?}");
    handler.wait_for_initial_panes_for_test().await;
    let far = window(&handler, &name, 1).await;
    let far_id = far.pane(0).unwrap().id();
    let source_index = index_of(&window(&handler, &name, 0).await, source_id);
    let source_before = runtime_witness(&handler, source_id).await;
    let witness_before = runtime_witness(&handler, witness_id).await;
    let tile_before = runtime_witness(&handler, tile_id).await;

    let response = handler
        .handle(Request::MovePane(move_request(
            pane(&name, 0, source_index),
            pane(&name, 1, 0),
        )))
        .await;
    let moved = match response {
        Response::MovePane(MovePaneResponse { target }) => target,
        other => panic!("{other:?}"),
    };

    let destination = window(&handler, &name, 1).await;
    let source_window = window(&handler, &name, 0).await;
    assert_eq!(moved.window_index(), 1);
    assert_eq!(pane_ids(&destination), vec![(0, far_id), (1, source_id)]);
    assert_eq!(
        geometry(&destination, far_id),
        rmux_core::PaneGeometry::new(0, 0, 40, 24)
    );
    assert_eq!(
        geometry(&destination, source_id),
        rmux_core::PaneGeometry::new(41, 0, 39, 24)
    );
    assert!(destination.floating_stack().is_empty());
    // The source window keeps its tile at full size and renumbers the witness.
    assert_eq!(
        pane_ids(&source_window),
        vec![(0, tile_id), (1, witness_id)]
    );
    assert_eq!(
        geometry(&source_window, tile_id),
        rmux_core::PaneGeometry::new(0, 0, 80, 24)
    );
    assert_eq!(
        geometry(&source_window, witness_id),
        rmux_core::PaneGeometry::new(50, 4, 12, 5)
    );
    assert_eq!(stack_ids(&source_window), vec![witness_id]);
    let source_after = runtime_witness(&handler, source_id).await;
    assert_eq!(source_after.pid, source_before.pid, "no respawn");
    assert_eq!(source_after.generation, source_before.generation);
    assert!(!source_after.exited);
    assert_eq!(
        source_after.dimensions,
        rmux_proto::TerminalSize { cols: 39, rows: 24 }
    );
    assert_eq!(
        runtime_witness(&handler, witness_id).await,
        witness_before,
        "the witness float of the source window is left alone"
    );
    assert_eq!(
        runtime_witness(&handler, tile_id).await,
        tile_before,
        "the source tile already owned the whole window and is not resized"
    );
    cleanup(&handler).await;
}
#[tokio::test]
async fn two_winlinks_of_one_window_take_one_floating_transfer() {
    let (handler, name, source_id, witness_id, tile_id) = fixture("fmt1-alias").await;
    let linked = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(name.clone(), 0),
            target: WindowTarget::with_window(name.clone(), 1),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(linked, Response::LinkWindow(_)), "{linked:?}");
    handler.wait_for_initial_panes_for_test().await;
    let owner = window(&handler, &name, 0).await;
    assert_eq!(window(&handler, &name, 1).await.id(), owner.id());
    let source_index = index_of(&owner, source_id);
    let mut events = handler.subscribe_lifecycle_events();

    // Source addressed through slot 0, destination through the alias slot 1.
    let response = handler
        .handle(Request::MovePane(move_request(
            pane(&name, 0, source_index),
            pane(&name, 1, 0),
        )))
        .await;
    assert!(matches!(response, Response::MovePane(_)), "{response:?}");

    let first = window(&handler, &name, 0).await;
    let second = window(&handler, &name, 1).await;
    assert_eq!(first, second, "one window, two winlinks, one mutation");
    assert_eq!(
        pane_ids(&first),
        vec![(0, tile_id), (1, source_id), (2, witness_id)]
    );
    assert_eq!(stack_ids(&first), vec![witness_id]);
    assert_eq!(
        geometry(&first, source_id),
        rmux_core::PaneGeometry::new(41, 0, 39, 24)
    );
    let targets = drain_layout_targets(&mut events);
    assert!(
        targets
            .iter()
            .all(|target| target.session_name() == &name && target.window_index() < 2),
        "{targets:?}"
    );
    assert!(!targets.is_empty());
    cleanup(&handler).await;
}
#[tokio::test]
async fn the_last_floating_pane_transfer_tears_down_its_source_window() {
    let (handler, name, source_id, witness_id, tile_id) = fixture("fmt1-teardown").await;
    let created = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: name.clone(),
            name: None,
            detached: true,
            environment: None,
            command: None,
            start_directory: None,
            target_window_index: None,
            insert_at_target: false,
            process_command: None,
        })))
        .await;
    assert!(matches!(created, Response::NewWindow(_)), "{created:?}");
    handler.wait_for_initial_panes_for_test().await;
    let far_id = window(&handler, &name, 1).await.pane(0).unwrap().id();
    // Leave the source window holding nothing but the floating source.
    let before = window(&handler, &name, 0).await;
    queue(
        &handler,
        &format!("kill-pane -t {name}:0.{}", index_of(&before, witness_id)),
    )
    .await
    .unwrap();
    let before = window(&handler, &name, 0).await;
    queue(
        &handler,
        &format!("kill-pane -t {name}:0.{}", index_of(&before, tile_id)),
    )
    .await
    .unwrap();
    let stranded = window(&handler, &name, 0).await;
    assert_eq!(stranded.pane_count(), 1);
    assert_eq!(stack_ids(&stranded), vec![source_id]);
    assert!(stranded.layout_dump().is_empty());
    let source_before = runtime_witness(&handler, source_id).await;

    let response = handler
        .handle(Request::MovePane(move_request(
            pane(&name, 0, index_of(&stranded, source_id)),
            pane(&name, 1, 0),
        )))
        .await;
    let moved = match response {
        Response::MovePane(MovePaneResponse { target }) => target,
        other => panic!("{other:?}"),
    };

    let state = handler.state.lock().await;
    let session = state.sessions.session(&name).unwrap();
    assert!(
        session.window_at(0).is_none(),
        "the emptied source window is torn down"
    );
    let destination = session.window_at(moved.window_index()).unwrap();
    assert_eq!(pane_ids(destination), vec![(0, far_id), (1, source_id)]);
    assert!(destination.floating_stack().is_empty());
    drop(state);
    let source_after = runtime_witness(&handler, source_id).await;
    assert_eq!(
        source_after.pid, source_before.pid,
        "the teardown must not kill the transferred runtime"
    );
    assert_eq!(source_after.generation, source_before.generation);
    assert!(!source_after.exited);
    cleanup(&handler).await;
}
#[tokio::test]
async fn a_marked_floating_source_is_used_when_s_is_omitted_and_the_mark_is_cleared() {
    let (handler, name, source_id, witness_id, tile_id) = fixture("fmt1-marked").await;
    let before = window(&handler, &name, 0).await;
    let source_index = index_of(&before, source_id);
    queue(
        &handler,
        &format!("select-pane -m -t {name}:0.{source_index}"),
    )
    .await
    .unwrap();
    assert_eq!(
        handler
            .state
            .lock()
            .await
            .marked_pane_target()
            .map(|target| target.pane_index()),
        Some(source_index),
        "`select-pane -m` marks the floating source"
    );

    queue(&handler, &format!("move-pane -h -t {name}:0.0"))
        .await
        .unwrap();

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
    assert_eq!(
        handler.state.lock().await.marked_pane_target(),
        None,
        "tmux 3.7b clears the mark once the marked source has moved"
    );
    cleanup(&handler).await;
}
#[tokio::test]
async fn size_and_full_size_flags_follow_the_inherited_rules_for_a_floating_source() {
    let (handler, name, source_id, witness_id, tile_id) = fixture("fmt1-size").await;
    let before = window(&handler, &name, 0).await;
    let source_index = index_of(&before, source_id);

    queue(
        &handler,
        &format!("move-pane -h -l 20 -s {name}:0.{source_index} -t {name}:0.0"),
    )
    .await
    .unwrap();

    let after = window(&handler, &name, 0).await;
    assert_eq!(
        geometry(&after, source_id).cols(),
        20,
        "`-l` sizes the moved pane"
    );
    assert_eq!(geometry(&after, tile_id).cols(), 59);
    assert_eq!(
        geometry(&after, witness_id),
        rmux_core::PaneGeometry::new(50, 4, 12, 5)
    );
    assert_eq!(stack_ids(&after), vec![witness_id]);
    cleanup(&handler).await;
}
#[tokio::test]
async fn a_floating_source_crosses_sessions_through_the_cross_session_transaction() {
    // A non-grouped destination session takes the other transfer transaction
    // (`join_pane_across_sessions`), so it needs its own witness.
    let (handler, name, source_id, witness_id, tile_id) = fixture("fmt1-xsession").await;
    let destination = session_name("fmt1-xdest");
    queue(
        &handler,
        &format!("new-session -d -s {destination} -n far -x80 -y24"),
    )
    .await
    .unwrap();
    handler.wait_for_initial_panes_for_test().await;
    let far = window(&handler, &destination, 0).await;
    let far_id = far.pane(0).unwrap().id();
    let source_index = index_of(&window(&handler, &name, 0).await, source_id);
    let source_before = runtime_witness(&handler, source_id).await;
    let witness_before = runtime_witness(&handler, witness_id).await;

    let response = handler
        .handle(Request::MovePane(move_request(
            pane(&name, 0, source_index),
            pane(&destination, 0, 0),
        )))
        .await;
    let moved = match response {
        Response::MovePane(MovePaneResponse { target }) => target,
        other => panic!("{other:?}"),
    };

    assert_eq!(moved.session_name(), &destination);
    let landed = window(&handler, &destination, 0).await;
    assert_eq!(pane_ids(&landed), vec![(0, far_id), (1, source_id)]);
    assert_eq!(
        geometry(&landed, source_id),
        rmux_core::PaneGeometry::new(41, 0, 39, 24)
    );
    assert!(landed.floating_stack().is_empty());
    let origin = window(&handler, &name, 0).await;
    assert_eq!(pane_ids(&origin), vec![(0, tile_id), (1, witness_id)]);
    assert_eq!(stack_ids(&origin), vec![witness_id]);
    let source_after = runtime_witness(&handler, source_id).await;
    assert_eq!(source_after.pid, source_before.pid, "no respawn");
    assert_eq!(source_after.generation, source_before.generation);
    assert!(!source_after.exited);
    assert_eq!(
        source_after.dimensions,
        rmux_proto::TerminalSize { cols: 39, rows: 24 }
    );
    assert_eq!(
        runtime_witness(&handler, witness_id).await,
        witness_before,
        "the witness float stays out of the cross-session resize set"
    );
    cleanup(&handler).await;
}
#[tokio::test]
async fn a_floating_source_crosses_grouped_sessions_as_one_window_mutation() {
    let (handler, name, source_id, witness_id, tile_id) = fixture("fmt1-group").await;
    let grouped = session_name("fmt1-groupmate");
    queue(&handler, &format!("new-session -d -s {grouped} -t {name}"))
        .await
        .unwrap();
    handler.wait_for_initial_panes_for_test().await;
    let owner = window(&handler, &name, 0).await;
    assert_eq!(window(&handler, &grouped, 0).await.id(), owner.id());
    let source_index = index_of(&owner, source_id);

    // Source through the original session, destination through the group mate.
    let response = handler
        .handle(Request::MovePane(move_request(
            pane(&name, 0, source_index),
            pane(&grouped, 0, 0),
        )))
        .await;
    assert!(matches!(response, Response::MovePane(_)), "{response:?}");

    let first = window(&handler, &name, 0).await;
    let second = window(&handler, &grouped, 0).await;
    assert_eq!(first.id(), second.id());
    assert_eq!(
        pane_ids(&first),
        vec![(0, tile_id), (1, source_id), (2, witness_id)]
    );
    assert_eq!(pane_ids(&second), pane_ids(&first));
    assert_eq!(stack_ids(&first), vec![witness_id]);
    assert_eq!(stack_ids(&second), vec![witness_id]);
    assert_eq!(
        geometry(&first, source_id),
        rmux_core::PaneGeometry::new(41, 0, 39, 24)
    );
    cleanup(&handler).await;
}
#[tokio::test]
async fn pane_options_and_transcript_follow_a_floating_source_across_sessions() {
    // The transfer must carry more than the process: the pane's explicit
    // options and its transcript are what make it the *same* terminal.
    let (handler, name, source_id, _witness_id, _tile_id) = fixture("fmt1-carry").await;
    let destination = session_name("fmt1-carry-dest");
    queue(
        &handler,
        &format!("new-session -d -s {destination} -n far -x80 -y24"),
    )
    .await
    .unwrap();
    handler.wait_for_initial_panes_for_test().await;
    let source_index = index_of(&window(&handler, &name, 0).await, source_id);
    queue(
        &handler,
        &format!("set-option -p -t {name}:0.{source_index} @fmt1.carried kept"),
    )
    .await
    .unwrap();
    queue(
        &handler,
        &format!("send-keys -t {name}:0.{source_index} FMT1-TRANSCRIPT Enter"),
    )
    .await
    .unwrap();
    handler
        .wait_for_pane_startup_to_finish_for_test(&pane(&name, 0, source_index))
        .await;
    let before = wait_for_transcript(
        &handler,
        &format!("{name}:0.{source_index}"),
        "FMT1-TRANSCRIPT",
    )
    .await;
    assert!(
        before.contains("FMT1-TRANSCRIPT"),
        "precondition: the transcript holds the marker, got {before:?}"
    );

    let response = handler
        .handle(Request::MovePane(move_request(
            pane(&name, 0, source_index),
            pane(&destination, 0, 0),
        )))
        .await;
    let moved = match response {
        Response::MovePane(MovePaneResponse { target }) => target,
        other => panic!("{other:?}"),
    };

    let options = queue(
        &handler,
        &format!(
            "show-options -p -t {}:{}.{} @fmt1.carried",
            moved.session_name(),
            moved.window_index(),
            moved.pane_index()
        ),
    )
    .await
    .unwrap();
    assert!(
        String::from_utf8_lossy(&options.stdout).contains("kept"),
        "the pane's explicit options follow it, got {:?}",
        String::from_utf8_lossy(&options.stdout)
    );
    let after = wait_for_transcript(
        &handler,
        &format!(
            "{}:{}.{}",
            moved.session_name(),
            moved.window_index(),
            moved.pane_index()
        ),
        "FMT1-TRANSCRIPT",
    )
    .await;
    assert!(
        after.contains("FMT1-TRANSCRIPT"),
        "the transcript follows the pane, got {after:?}"
    );
    cleanup(&handler).await;
}
