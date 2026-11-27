use super::*;

#[tokio::test]
async fn l05_1_queue_exact_target_valid_move_after_capture() {
    let handler = RequestHandler::new();
    let source = create_session(&handler, "move-capture-source").await;
    let destination = create_session(&handler, "move-capture-destination").await;
    let split = handler
        .handle(Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Session(source.clone()),
            direction: SplitDirection::Vertical,
            before: false,
            environment: None,
        }))
        .await;
    assert!(matches!(split, Response::SplitWindow(_)), "{split:?}");
    let pane_id = handler
        .state
        .lock()
        .await
        .sessions
        .session(&source)
        .unwrap()
        .window()
        .pane(0)
        .unwrap()
        .id();
    let commands = CommandParser::new()
        .parse(&format!("kill-pane -t {pane_id}"))
        .unwrap();
    let pause = install_queue_exact_target_capture_pause(&handler, "kill-pane");
    let executing = handler.clone();
    let context = pane_context(&source, 0);
    let queued = tokio::spawn(async move {
        executing
            .execute_parsed_commands(std::process::id(), commands, context)
            .await
    });
    wait_for_pause(&pause).await;
    let moved = handler
        .handle(Request::MovePane(rmux_proto::MovePaneRequest {
            source: PaneTarget::with_window(source.clone(), 0, 0),
            target: PaneTarget::with_window(destination.clone(), 0, 0),
            direction: SplitDirection::Vertical,
            detached: true,
            before: false,
            full_size: false,
            size: None,
        }))
        .await;
    let Response::MovePane(moved) = moved else {
        panic!("{moved:?}");
    };
    let replacement_id = handler
        .state
        .lock()
        .await
        .sessions
        .session(&source)
        .unwrap()
        .window()
        .pane(0)
        .unwrap()
        .id();
    assert_ne!(replacement_id, pane_id);
    pause.release.notify_one();
    // The existing capture contract pins the window link occurrence, slot,
    // PaneId and output generation. A legitimate transfer does not rebase an
    // already captured destructive operation onto either location.
    let error = tokio::time::timeout(Duration::from_secs(2), queued)
        .await
        .unwrap()
        .unwrap()
        .expect_err("owner transfer invalidates the captured occurrence");
    assert!(
        error.to_string().contains("target identity changed"),
        "{error}"
    );
    let state = handler.state.lock().await;
    assert_eq!(
        state
            .sessions
            .session(&source)
            .unwrap()
            .window()
            .pane(0)
            .unwrap()
            .id(),
        replacement_id
    );
    assert_eq!(
        state
            .sessions
            .session(&destination)
            .unwrap()
            .window()
            .pane(moved.target.pane_index())
            .unwrap()
            .id(),
        pane_id
    );
}
